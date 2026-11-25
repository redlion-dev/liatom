#[cfg(not(feature = "library"))]
use cosmwasm_schema::cw_serde;
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    coin, from_json, to_json_binary, BankMsg, Binary, Coin, CosmosMsg, Decimal, Deps, DepsMut, DistributionMsg, Env,
    MessageInfo,
    Reply, Response, StakingMsg, StdError, StdResult, SubMsg, Timestamp, Uint128,
};

use crate::error::ContractError;
use crate::msg::{ExchangeRateResponse, ExecuteMsg, InstantiateMsg, PoolResponse, QueryMsg};
use crate::rate::{self, validate_weights};
use crate::state::{
    Batch, BatchStatus, Config, PauseMode, ValidatorWeight, BATCHES, CLAIMS, CONFIG, CURRENT_BATCH,
    FEE_REPLY_ID, FINALIZE_DELAY_SECS, LSM_OVERWEIGHT_BAND_BPS, LSM_REPLY_ID, MAX_VALIDATORS, MAX_VALIDATOR_COMMISSION_PERCENT, MIN_TOKENIZE_PART, TOKENIZE_REPLY_ID, LAST_SUBMIT, ROUNDING_TOLERANCE_PER_UNDELEGATE, MAX_FEE_BPS, OWED_TO_REDEEMERS, PENDING_LIATOM, SHORTFALL_GRACE_SECS, SUBDENOM,
    UNBONDING, VALIDATORS,
};
use crate::lsm;
use crate::tokenfactory::{self, full_denom, TokenFactoryMsg};

type Resp = Response<TokenFactoryMsg>;

/// Smallest redelegation `Rebalance` makes by default: 1 ATOM.
const DEFAULT_MIN_MOVE: u128 = 1_000_000;
/// Upper bound on redelegations per `Rebalance` call.
const MAX_REBALANCE_MOVES: usize = 8;

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Resp, ContractError> {
    if msg.fee_bps > MAX_FEE_BPS {
        return Err(ContractError::FeeTooHigh {});
    }
    // The chain reports delegations and share denoms with lowercase addresses, and every comparison here
    // is on the raw string: an uppercase address from the admin would never match them.
    let validators = canonical_weights(&msg.validators);
    validate_weights(&validators)?;
    rate::validate_timing(msg.epoch_secs, msg.unbonding_secs)?;
    if msg.min_deposit.is_zero() {
        return Err(ContractError::InvalidMinDeposit {});
    }
    if msg.max_tvl < msg.min_deposit {
        return Err(ContractError::InvalidMaxTvl {});
    }
    for w in &validators {
        ensure_validator_exists(deps.as_ref(), &w.validator)?;
    }

    let bond_denom = deps.querier.query_bonded_denom()?;
    let attached = one_coin(&info, &bond_denom)?;
    // The chain charges the tokenfactory denom creation fee to the contract balance,
    // so part of the attached funds pays it and only the rest is the seed deposit.
    let seed = coin(
        attached
            .amount
            .checked_sub(msg.denom_creation_fee)
            .map_err(|_| ContractError::BelowMinDeposit {
                got: Uint128::zero(),
                min: msg.min_deposit,
            })?
            .u128(),
        &bond_denom,
    );
    if seed.amount < msg.min_deposit {
        return Err(ContractError::BelowMinDeposit {
            got: seed.amount,
            min: msg.min_deposit,
        });
    }
    if seed.amount > msg.max_tvl {
        return Err(ContractError::TvlCapExceeded {
            max: msg.max_tvl,
            after: seed.amount,
            room: msg.max_tvl,
        });
    }

    let liatom_denom = full_denom(env.contract.address.as_str(), SUBDENOM);
    let cfg = Config {
        admin: deps.api.addr_validate(&msg.admin)?,
        pending_admin: None,
        treasury: {
            let t = deps.api.addr_validate(&msg.treasury)?;
            if t == env.contract.address {
                return Err(ContractError::InvalidTreasury {});
            }
            t
        },
        fee_bps: msg.fee_bps,
        epoch_secs: msg.epoch_secs,
        unbonding_secs: msg.unbonding_secs,
        min_unbonding_secs: msg.unbonding_secs / 8 * 7,
        min_deposit: msg.min_deposit,
        max_tvl: msg.max_tvl,
        pause: None,
        liatom_denom: liatom_denom.clone(),
        bond_denom: bond_denom.clone(),
    };
    CONFIG.save(deps.storage, &cfg)?;
    VALIDATORS.save(deps.storage, &validators)?;
    OWED_TO_REDEEMERS.save(deps.storage, &Uint128::zero())?;
    PENDING_LIATOM.save(deps.storage, &Uint128::zero())?;
    UNBONDING.save(deps.storage, &vec![])?;
    LAST_SUBMIT.save(deps.storage, &Timestamp::from_seconds(0))?;
    CURRENT_BATCH.save(deps.storage, &1)?;
    BATCHES.save(deps.storage, 1, &new_batch(1))?;

    // The seed deposit is minted 1:1 to the contract itself and never released.
    // It keeps supply and pool value non-zero, which blocks first-depositor inflation attacks.
    let validator = rate::pick_validator(&validators, &[], seed.amount)
        .ok_or(ContractError::InvalidWeights {})?;

    Ok(Response::new()
        .add_message(tokenfactory::create_denom(SUBDENOM))
        .add_message(tokenfactory::set_metadata(&liatom_denom))
        .add_message(tokenfactory::mint(
            &liatom_denom,
            seed.amount,
            env.contract.address.as_str(),
        ))
        .add_message(StakingMsg::Delegate {
            validator,
            amount: seed.clone(),
        })
        .add_attribute("action", "instantiate")
        .add_attribute("liatom_denom", liatom_denom)
        .add_attribute("seed", seed.amount))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Resp, ContractError> {
    // Only the four messages that take funds accept them; every other one refuses them: attached ATOM would become ownerless balance and attached
    // liATOM would be locked in the contract.
    if !info.funds.is_empty()
        && !matches!(
            msg,
            ExecuteMsg::Deposit {}
                | ExecuteMsg::DepositTokenizedShares {}
                | ExecuteMsg::Redeem {}
                | ExecuteMsg::RedeemToShares {}
        )
    {
        return Err(ContractError::UnexpectedFunds {});
    }
    match msg {
        ExecuteMsg::Deposit {} => execute_deposit(deps, env, info),
        ExecuteMsg::DepositTokenizedShares {} => execute_deposit_tokenized(deps, env, info),
        ExecuteMsg::Redeem {} => execute_redeem(deps, info),
        ExecuteMsg::RedeemToShares {} => execute_redeem_to_shares(deps, env, info),
        ExecuteMsg::Claim { batch_id } => execute_claim(deps, info, batch_id),
        ExecuteMsg::Harvest {} => execute_harvest(deps, env),
        ExecuteMsg::SubmitUnbonding {} => execute_submit_unbonding(deps, env),
        ExecuteMsg::FinalizeBatch { batch_id } => execute_finalize_batch(deps, env, batch_id),
        ExecuteMsg::UpdateValidators { validators } => {
            execute_update_validators(deps, env, info, validators)
        }
        ExecuteMsg::UpdateConfig {
            treasury,
            fee_bps,
            epoch_secs,
            unbonding_secs,
            min_deposit,
            max_tvl,
        } => execute_update_config(
            deps,
            env,
            info,
            treasury,
            fee_bps,
            epoch_secs,
            unbonding_secs,
            min_deposit,
            max_tvl,
        ),
        ExecuteMsg::Rebalance { min_move } => execute_rebalance(deps, env, info, min_move),
        ExecuteMsg::Pause { mode } => set_pause(deps, info, Some(mode)),
        ExecuteMsg::Unpause {} => set_pause(deps, info, None),
        ExecuteMsg::ProposeAdmin { new_admin } => propose_admin(deps, info, new_admin),
        ExecuteMsg::AcceptAdmin {} => accept_admin(deps, info),
    }
}

/// The payload of a fee transfer that may fail: how much, and where to delegate it if it does.
#[cw_serde]
struct UnsentFee {
    amount: Uint128,
    validator: String,
}

/// A fee transfer that was allowed to fail did fail (the transfer itself was rolled back). The fee is
/// delegated to the validator named in the payload, so it stays in the pool instead of sitting
/// on the balance as ownerless ATOM.
#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, env: Env, msg: Reply) -> Result<Resp, ContractError> {
    match msg.id {
        FEE_REPLY_ID => {
            let cfg = CONFIG.load(deps.storage)?;
            let mut res = Response::new().add_attribute("action", "fee_transfer_failed");
            // This reply is on the exit path, so it must not fail for a reason of its own: a payload that
            // cannot be read simply leaves the fee on the balance (the delegation it sends is the one thing
            // that could still be refused by the chain, and that fails the call like any other message).
            if let Ok(unsent) = from_json::<UnsentFee>(&msg.payload) {
                if !unsent.amount.is_zero() {
                    res = res.add_message(StakingMsg::Delegate {
                        validator: unsent.validator,
                        amount: coin(unsent.amount.u128(), &cfg.bond_denom),
                    });
                }
            }
            Ok(res)
        }
        LSM_REPLY_ID => reply_tokenized_shares(deps, env, msg),
        TOKENIZE_REPLY_ID => reply_exit_shares(deps, env, msg),
        _ => Err(ContractError::Std(StdError::generic_err("unknown reply id"))),
    }
}

fn execute_deposit(deps: DepsMut, env: Env, info: MessageInfo) -> Result<Resp, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if cfg.pause.is_some() {
        return Err(ContractError::Paused {});
    }
    let funds = one_coin(&info, &cfg.bond_denom)?;
    if funds.amount < cfg.min_deposit {
        return Err(ContractError::BelowMinDeposit {
            got: funds.amount,
            min: cfg.min_deposit,
        });
    }

    // Funds are already on the contract balance, so price the pool without them.
    let pool = compute_pool(deps.as_ref(), &env, &cfg)?;
    let after = pool.pool_value.checked_add(funds.amount)?;
    if after > cfg.max_tvl {
        return Err(ContractError::TvlCapExceeded {
            max: cfg.max_tvl,
            after,
            room: cfg.max_tvl.saturating_sub(pool.pool_value),
        });
    }
    let minted = rate::mint_amount(
        funds.amount,
        pool.effective_supply()?,
        pool.pool_value,
    )?;

    let validators = VALIDATORS.load(deps.storage)?;
    let validator = rate::pick_validator(&validators, &pool.delegations, funds.amount)
        .ok_or(ContractError::InvalidWeights {})?;

    // Delegating pays this validator's pending rewards to the balance; collect them right away.
    let after = with_added(&pool.delegations, &validator, funds.amount);
    let sweep = sweep_rewards(deps.as_ref(), &cfg, &pool, &[validator.clone()], &after, false)?;

    Ok(Response::new()
        .add_message(tokenfactory::mint(
            &cfg.liatom_denom,
            minted,
            info.sender.as_str(),
        ))
        .add_message(StakingMsg::Delegate {
            validator: validator.clone(),
            amount: funds.clone(),
        })
        .add_submessages(sweep)
        .add_attribute("action", "deposit")
        .add_attribute("sender", info.sender)
        .add_attribute("amount", funds.amount)
        .add_attribute("minted", minted)
        .add_attribute("validator", validator))
}

/// What `DepositTokenizedShares` prices with, fixed before the shares are redeemed: the pool and the supply
/// as they are without the new delegation (exactly what `Deposit` prices with).
#[cw_serde]
struct PendingShares {
    sender: String,
    validator: String,
    delegation_before: Uint128,
    supply: Uint128,
    pool_value: Uint128,
    /// All delegations before the redemption, and the validator's target weight: the cap on how far the
    /// deposit may leave the validator above it.
    delegated_before: Uint128,
    weight_bps: u16,
}

fn execute_deposit_tokenized(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Resp, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if cfg.pause.is_some() {
        return Err(ContractError::Paused {});
    }
    let share = match info.funds.as_slice() {
        [c] if !c.amount.is_zero() => c.clone(),
        _ => {
            return Err(ContractError::WrongFunds {
                denom: "<validator>/<record id>".to_string(),
            })
        }
    };
    let validator = lsm::share_token_validator(&share.denom)
        .ok_or_else(|| ContractError::NotAShareToken { denom: share.denom.clone() })?
        .to_string();
    // Only validators the admin chose: a delegation taken over from an arbitrary validator would hand
    // the pool that validator's slashing risk at the depositor's choice.
    let validators = VALIDATORS.load(deps.storage)?;
    let weight_bps = validators
        .iter()
        .find(|w| w.validator == validator)
        .map(|w| w.weight_bps)
        .ok_or_else(|| ContractError::ValidatorNotInSet { validator: validator.clone() })?;

    // The delegation that comes out of the redemption is the deposit: price the pool without it. The
    // amount is only known after the redemption (it depends on the validator's tokens per share), so
    // the minting happens in the reply, from what the delegation actually grew by.
    let pool = compute_pool(deps.as_ref(), &env, &cfg)?;
    let delegation_before = pool
        .delegations
        .iter()
        .find(|(v, _)| *v == validator)
        .map(|(_, a)| *a)
        .unwrap_or_default();
    let payload = PendingShares {
        sender: info.sender.to_string(),
        validator: validator.clone(),
        delegation_before,
        supply: pool.effective_supply()?,
        pool_value: pool.pool_value,
        delegated_before: pool.delegated,
        weight_bps,
    };
    let redeem: CosmosMsg<TokenFactoryMsg> =
        CosmosMsg::Any(lsm::redeem_msg(env.contract.address.as_str(), &share));
    // Redeeming delegates to this validator, which pays out its pending rewards: collect them now.
    let sweep = sweep_rewards(deps.as_ref(), &cfg, &pool, &[validator.clone()], &pool.delegations, false)?;

    Ok(Response::new()
        .add_submessage(
            SubMsg::reply_on_success(redeem, LSM_REPLY_ID).with_payload(to_json_binary(&payload)?),
        )
        .add_submessages(sweep)
        .add_attribute("action", "deposit_tokenized_shares")
        .add_attribute("sender", info.sender)
        .add_attribute("share_token", share.to_string())
        .add_attribute("validator", validator))
}

/// The shares were redeemed: the delegation grew by the deposit. Mint for exactly that growth.
fn reply_tokenized_shares(deps: DepsMut, env: Env, msg: Reply) -> Result<Resp, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    let p: PendingShares = from_json(&msg.payload)?;
    let after = deps
        .querier
        .query_delegation(&env.contract.address, &p.validator)?
        .map(|d| d.amount.amount)
        .unwrap_or_default();
    let deposited = after.saturating_sub(p.delegation_before);
    if deposited < cfg.min_deposit {
        return Err(ContractError::BelowMinDeposit {
            got: deposited,
            min: cfg.min_deposit,
        });
    }
    let total = p.pool_value.checked_add(deposited)?;
    if total > cfg.max_tvl {
        return Err(ContractError::TvlCapExceeded {
            max: cfg.max_tvl,
            after: total,
            room: cfg.max_tvl.saturating_sub(p.pool_value),
        });
    }
    // The instant LSM paths move stake between validators without the slash entries a redelegation carries,
    // so a deposit may not leave its validator far above its target weight (see LSM_OVERWEIGHT_BAND_BPS).
    let total_after = p.delegated_before.checked_add(deposited)?;
    let max_stake = total_after.multiply_ratio(
        u128::from(p.weight_bps) * (10_000 + LSM_OVERWEIGHT_BAND_BPS),
        10_000u128 * 10_000u128,
    );
    if after > max_stake {
        return Err(ContractError::ValidatorOverweight { validator: p.validator, after, max_stake });
    }
    let minted = rate::mint_amount(deposited, p.supply, p.pool_value)?;
    Ok(Response::new()
        .add_message(tokenfactory::mint(&cfg.liatom_denom, minted, &p.sender))
        .add_attribute("action", "deposit_tokenized_shares_minted")
        .add_attribute("deposited", deposited)
        .add_attribute("minted", minted))
}

/// What the reply of `RedeemToShares` needs, fixed before the tokenization.
#[cw_serde]
struct PendingExit {
    sender: String,
    validator: String,
    delegation_before: Uint128,
    amount: Uint128,
}

fn execute_redeem_to_shares(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Resp, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if cfg.pause == Some(PauseMode::Full) {
        return Err(ContractError::Paused {});
    }
    let funds = one_coin(&info, &cfg.liatom_denom)?;

    // The liATOM is burned now and worth `amount` ATOM at the current rate (the same price a deposit
    // pays and a batch is fixed at, rounded down). That much of the delegations leaves the pool as share tokens.
    let pool = compute_pool(deps.as_ref(), &env, &cfg)?;
    let amount = rate::atom_for_liatom(funds.amount, pool.pool_value, pool.effective_supply()?);
    if amount < cfg.min_deposit {
        return Err(ContractError::ExitTooSmall { amount, min: cfg.min_deposit });
    }
    // Taken from every validator in proportion to its stake, like a batch: the leaver cannot choose a
    // healthy validator and leave the holders who stay with one that is about to be slashed.
    let mut parts = rate::split_by_stake(amount, &pool.delegations).ok_or(
        ContractError::InsufficientDelegations { needed: amount, delegated: pool.delegated },
    )?;
    // A part of a few uatom can be turned into 0 by the chain's rounding on a slashed validator, which
    // would fail the whole exit: fold such parts into one with room (the user gets at most a few uatom less).
    let floor = Uint128::new(MIN_TOKENIZE_PART);
    let mut dust = Uint128::zero();
    parts.retain(|(_, p)| {
        if *p < floor {
            dust += *p;
            false
        } else {
            true
        }
    });
    if !dust.is_zero() {
        for (v, p) in parts.iter_mut() {
            let stake = pool.delegations.iter().find(|(d, _)| d == v).map(|(_, a)| *a).unwrap_or_default();
            if stake - *p >= dust {
                *p += dust;
                break;
            }
        }
    }
    if parts.is_empty() {
        return Err(ContractError::ExitTooSmall { amount, min: cfg.min_deposit });
    }

    let mut res = Response::new().add_message(tokenfactory::burn_own(&cfg.liatom_denom, funds.amount));
    let mut after = pool.delegations.clone();
    let mut touched: Vec<String> = vec![];
    for (validator, part) in parts.iter() {
        let have = pool
            .delegations
            .iter()
            .find(|(v, _)| v == validator)
            .map(|(_, a)| *a)
            .unwrap_or_default();
        if let Some((_, a)) = after.iter_mut().find(|(v, _)| v == validator) {
            *a -= *part;
        }
        let tokenize: CosmosMsg<TokenFactoryMsg> = CosmosMsg::Any(lsm::tokenize_msg(
            env.contract.address.as_str(),
            validator,
            &coin(part.u128(), &cfg.bond_denom),
            info.sender.as_str(),
        ));
        let payload = PendingExit {
            sender: info.sender.to_string(),
            validator: validator.clone(),
            delegation_before: have,
            amount: *part,
        };
        res = res.add_submessage(
            SubMsg::reply_on_success(tokenize, TOKENIZE_REPLY_ID).with_payload(to_json_binary(&payload)?),
        );
        touched.push(validator.clone());
    }
    // Tokenizing takes shares out of these delegations, which pays out their rewards: collect them now.
    // The fee transfer may fail without stopping the exit (its reply delegates the unsent fee).
    let sweep = sweep_rewards(deps.as_ref(), &cfg, &pool, &touched, &after, true)?;

    Ok(res
        .add_submessages(sweep)
        .add_attribute("action", "redeem_to_shares")
        .add_attribute("sender", info.sender)
        .add_attribute("liatom", funds.amount)
        .add_attribute("amount", amount))
}

/// The delegation was tokenized: hand the share token to the caller, after checking that the pool
/// gave up what the burned liATOM was worth and no more.
fn reply_exit_shares(deps: DepsMut, env: Env, msg: Reply) -> Result<Resp, ContractError> {
    let p: PendingExit = from_json(&msg.payload)?;
    let response = msg.result.into_result().map_err(StdError::generic_err)?;
    let share = response
        .msg_responses
        .iter()
        .find_map(|r| lsm::parse_tokenize_response(r.value.as_slice()))
        .ok_or(ContractError::BadTokenization {})?;
    // the token must belong to the validator the delegation was taken from
    if lsm::share_token_validator(&share.denom) != Some(p.validator.as_str()) || share.amount.is_zero() {
        return Err(ContractError::BadTokenization {});
    }
    let after = deps
        .querier
        .query_delegation(&env.contract.address, &p.validator)?
        .map(|d| d.amount.amount)
        .unwrap_or_default();
    let taken = p.delegation_before.saturating_sub(after);
    // up to 2 uatom of decimal rounding between tokens and shares
    if taken > p.amount.checked_add(Uint128::new(2))? {
        return Err(ContractError::BadTokenization {});
    }
    Ok(Response::new()
        .add_message(BankMsg::Send {
            to_address: p.sender.clone(),
            amount: vec![share.clone()],
        })
        .add_attribute("action", "redeem_to_shares_sent")
        .add_attribute("share_token", share.to_string())
        .add_attribute("taken", taken))
}

fn execute_redeem(deps: DepsMut, info: MessageInfo) -> Result<Resp, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if cfg.pause == Some(PauseMode::Full) {
        return Err(ContractError::Paused {});
    }
    let funds = one_coin(&info, &cfg.liatom_denom)?;

    let batch_id = CURRENT_BATCH.load(deps.storage)?;
    let mut batch = BATCHES.load(deps.storage, batch_id)?;
    batch.liatom_burned = batch.liatom_burned.checked_add(funds.amount)?;
    BATCHES.save(deps.storage, batch_id, &batch)?;
    let pending = PENDING_LIATOM.load(deps.storage)?.checked_add(funds.amount)?;
    PENDING_LIATOM.save(deps.storage, &pending)?;

    let key = (batch_id, &info.sender);
    let prev = CLAIMS.may_load(deps.storage, key)?.unwrap_or_default();
    CLAIMS.save(deps.storage, key, &prev.checked_add(funds.amount)?)?;

    // The ATOM amount is fixed when the batch is submitted, not now.
    Ok(Response::new()
        .add_message(tokenfactory::burn_own(&cfg.liatom_denom, funds.amount))
        .add_attribute("action", "redeem")
        .add_attribute("sender", info.sender)
        .add_attribute("batch_id", batch_id.to_string())
        .add_attribute("liatom", funds.amount))
}

/// Collects rewards, takes the protocol fee and re-delegates the rest.
///
/// Rewards reach the contract balance two ways: by `WithdrawDelegatorReward` and implicitly
/// whenever a delegation changes (the distribution hook pays them out). Both count as
/// `gross`; the fee goes to the treasury and the remainder is delegated.
fn execute_harvest(deps: DepsMut, env: Env) -> Result<Resp, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    let plan = plan_harvest(deps.as_ref(), &env, &cfg)?;
    if plan.gross.is_zero() {
        return Err(ContractError::NothingToHarvest {});
    }
    Ok(Response::new()
        .add_messages(plan.msgs)
        .add_attribute("action", "harvest")
        .add_attribute("gross", plan.gross)
        .add_attribute("fee", plan.fee)
        .add_attribute("reinvested", plan.net))
}

/// What a harvest would do right now. `gross == 0` means there is nothing to collect and `msgs` is empty.
struct HarvestPlan {
    msgs: Vec<CosmosMsg<TokenFactoryMsg>>,
    gross: Uint128,
    fee: Uint128,
    net: Uint128,
}

fn plan_harvest(deps: Deps, env: &Env, cfg: &Config) -> Result<HarvestPlan, ContractError> {
    let pool = compute_pool(deps, env, cfg)?;
    let gross = pool.unclaimed_rewards;
    let (fee, net) = rate::split_fee(gross, cfg.fee_bps);
    let mut msgs: Vec<CosmosMsg<TokenFactoryMsg>> = vec![];
    if gross.is_zero() {
        return Ok(HarvestPlan { msgs, gross, fee, net });
    }

    for (validator, amount) in &pool.rewards_by_validator {
        if !amount.is_zero() {
            msgs.push(
                DistributionMsg::WithdrawDelegatorReward {
                    validator: validator.clone(),
                }
                .into(),
            );
        }
    }
    if !fee.is_zero() {
        msgs.push(
            BankMsg::Send {
                to_address: cfg.treasury.to_string(),
                amount: vec![coin(fee.u128(), &cfg.bond_denom)],
            }
            .into(),
        );
    }
    if !net.is_zero() {
        let validators = VALIDATORS.load(deps.storage)?;
        let validator = rate::pick_validator(&validators, &pool.delegations, net)
            .ok_or(ContractError::InvalidWeights {})?;
        msgs.push(
            StakingMsg::Delegate {
                validator,
                amount: coin(net.u128(), &cfg.bond_denom),
            }
            .into(),
        );
    }
    Ok(HarvestPlan { msgs, gross, fee, net })
}

fn execute_update_config(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    treasury: Option<String>,
    fee_bps: Option<u16>,
    epoch_secs: Option<u64>,
    unbonding_secs: Option<u64>,
    min_deposit: Option<Uint128>,
    max_tvl: Option<Uint128>,
) -> Result<Resp, ContractError> {
    let mut cfg = only_admin(deps.as_ref(), &info)?;
    if treasury.is_none()
        && fee_bps.is_none()
        && epoch_secs.is_none()
        && unbonding_secs.is_none()
        && min_deposit.is_none()
        && max_tvl.is_none()
    {
        return Err(ContractError::EmptyConfigUpdate {});
    }

    let mut res = Response::new().add_attribute("action", "update_config");

    if let Some(new_fee) = fee_bps {
        if new_fee > MAX_FEE_BPS {
            return Err(ContractError::FeeTooHigh {});
        }
        if new_fee != cfg.fee_bps {
            // Pending rewards were priced with the old fee. Collect them under it first, so the
            // new fee only applies to rewards earned from now on and the exchange rate does not jump.
            let plan = plan_harvest(deps.as_ref(), &env, &cfg)?;
            res = res.add_messages(plan.msgs);
            res = res.add_attribute("harvested", plan.gross);
            cfg.fee_bps = new_fee;
            res = res.add_attribute("fee_bps", new_fee.to_string());
        }
    }
    if let Some(t) = treasury {
        cfg.treasury = deps.api.addr_validate(&t)?;
        if cfg.treasury == env.contract.address {
            return Err(ContractError::InvalidTreasury {});
        }
        res = res.add_attribute("treasury", t);
    }
    if let Some(m) = min_deposit {
        if m.is_zero() {
            return Err(ContractError::InvalidMinDeposit {});
        }
        cfg.min_deposit = m;
        res = res.add_attribute("min_deposit", m);
    }
    if let Some(m) = max_tvl {
        cfg.max_tvl = m;
        res = res.add_attribute("max_tvl", m);
    }
    if cfg.max_tvl < cfg.min_deposit {
        return Err(ContractError::InvalidMaxTvl {});
    }
    if let Some(u) = unbonding_secs {
        if u < cfg.min_unbonding_secs {
            return Err(ContractError::UnbondingBelowFloor { min: cfg.min_unbonding_secs });
        }
    }
    if epoch_secs.is_some() || unbonding_secs.is_some() {
        cfg.epoch_secs = epoch_secs.unwrap_or(cfg.epoch_secs);
        cfg.unbonding_secs = unbonding_secs.unwrap_or(cfg.unbonding_secs);
        rate::validate_timing(cfg.epoch_secs, cfg.unbonding_secs)?;
        res = res
            .add_attribute("epoch_secs", cfg.epoch_secs.to_string())
            .add_attribute("unbonding_secs", cfg.unbonding_secs.to_string());
    }

    CONFIG.save(deps.storage, &cfg)?;
    Ok(res)
}

/// Moves stake with `Redelegate` towards the current target weights. Redelegation needs no
/// unbonding wait, so this never touches liquidity, but the chain limits it: each (src, dst) pair
/// holds at most 7 redelegation entries, and a validator that just received a redelegation cannot
/// be redelegated from until it matures. The chain rejects violations and the whole call reverts.
fn execute_rebalance(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    min_move: Option<Uint128>,
) -> Result<Resp, ContractError> {
    only_admin(deps.as_ref(), &info)?;
    let weights = VALIDATORS.load(deps.storage)?;
    let delegations: Vec<(String, Uint128)> = deps
        .querier
        .query_all_delegations(&env.contract.address)?
        .into_iter()
        .map(|d| (d.validator, d.amount.amount))
        .collect();
    let cfg = CONFIG.load(deps.storage)?;

    let moves = rate::plan_rebalance(
        &weights,
        &delegations,
        min_move.unwrap_or(Uint128::new(DEFAULT_MIN_MOVE)),
        MAX_REBALANCE_MOVES,
    );
    if moves.is_empty() {
        return Err(ContractError::NothingToRebalance {});
    }

    let pool = compute_pool(deps.as_ref(), &env, &cfg)?;
    let mut res = Response::new().add_attribute("action", "rebalance");
    let mut moved = Uint128::zero();
    let mut after = delegations.clone();
    let mut touched: Vec<String> = vec![];
    for (from, to, amount) in moves {
        moved = moved.checked_add(amount)?;
        if let Some((_, a)) = after.iter_mut().find(|(v, _)| *v == from) {
            *a = a.saturating_sub(amount);
        }
        after = with_added(&after, &to, amount);
        touched.push(from.clone());
        touched.push(to.clone());
        res = res.add_message(StakingMsg::Redelegate {
            src_validator: from,
            dst_validator: to,
            amount: coin(amount.u128(), &cfg.bond_denom),
        });
    }
    // Redelegating pays both validators' pending rewards to the balance; collect them right away.
    res = res.add_submessages(sweep_rewards(deps.as_ref(), &cfg, &pool, &touched, &after, false)?);
    Ok(res.add_attribute("moved", moved))
}


/// Closes the open batch: fixes the ATOM it is owed at the current rate and undelegates that
/// amount, at most once per validator so the 7-entry unbonding limit is never reached.
fn execute_submit_unbonding(deps: DepsMut, env: Env) -> Result<Resp, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    let batch_id = CURRENT_BATCH.load(deps.storage)?;
    let mut batch = BATCHES.load(deps.storage, batch_id)?;
    if batch.liatom_burned.is_zero() {
        return Err(ContractError::NothingToUnbond {});
    }

    let last = LAST_SUBMIT.load(deps.storage)?;
    let next_allowed = last.plus_seconds(cfg.epoch_secs);
    if env.block.time < next_allowed {
        return Err(ContractError::EpochNotElapsed {
            remaining_secs: next_allowed.seconds() - env.block.time.seconds(),
        });
    }

    let pool = compute_pool(deps.as_ref(), &env, &cfg)?;
    let atom_expected = rate::atom_for_liatom(
        batch.liatom_burned,
        pool.pool_value,
        pool.effective_supply()?,
    );
    let parts = rate::split_by_stake(atom_expected, &pool.delegations).ok_or(
        ContractError::InsufficientDelegations {
            needed: atom_expected,
            delegated: pool.delegated,
        },
    )?;

    let mut res = Response::new();
    let mut after = pool.delegations.clone();
    let mut touched: Vec<String> = vec![];
    for (validator, amount) in parts {
        if let Some((_, a)) = after.iter_mut().find(|(v, _)| *v == validator) {
            *a = a.saturating_sub(amount);
        }
        touched.push(validator.clone());
        res = res.add_message(StakingMsg::Undelegate {
            validator,
            amount: coin(amount.u128(), &cfg.bond_denom),
        });
    }
    // Undelegating pays those validators' pending rewards to the balance; collect them right away.
    res = res.add_submessages(sweep_rewards(deps.as_ref(), &cfg, &pool, &touched, &after, true)?);

    batch.status = BatchStatus::Unbonding;
    batch.undelegations = touched.len() as u32;
    batch.atom_expected = Some(atom_expected);
    batch.mature_at = Some(env.block.time.plus_seconds(cfg.unbonding_secs));
    batch.submitted_at = Some(env.block.time);
    BATCHES.save(deps.storage, batch_id, &batch)?;
    UNBONDING.update(deps.storage, |mut ids| -> StdResult<_> {
        ids.push(batch_id);
        Ok(ids)
    })?;
    let pending = PENDING_LIATOM
        .load(deps.storage)?
        .checked_sub(batch.liatom_burned)?;
    PENDING_LIATOM.save(deps.storage, &pending)?;
    LAST_SUBMIT.save(deps.storage, &env.block.time)?;
    let next = batch_id + 1;
    CURRENT_BATCH.save(deps.storage, &next)?;
    BATCHES.save(deps.storage, next, &new_batch(next))?;

    Ok(res
        .add_attribute("action", "submit_unbonding")
        .add_attribute("batch_id", batch_id.to_string())
        .add_attribute("liatom", batch.liatom_burned)
        .add_attribute("atom_expected", atom_expected))
}

/// After the unbonding period, records what the matured batches actually received.
///
/// One call settles *every* matured batch at once, whichever batch id is passed, so the outcome
/// never depends on who finalizes first or in which order. Without a shortfall each batch gets
/// its `atom_expected`. With a slash the shortfall is shared pro rata over all matured batches
/// (see [`rate::settle_matured`]); a batch never receives more than it expected.
fn execute_finalize_batch(
    deps: DepsMut,
    env: Env,
    batch_id: u64,
) -> Result<Resp, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    let requested = BATCHES.load(deps.storage, batch_id)?;
    if requested.status != BatchStatus::Unbonding {
        return Err(ContractError::WrongBatchStatus { batch_id });
    }
    let mature_at = requested
        .mature_at
        .ok_or(ContractError::WrongBatchStatus { batch_id })?;
    // Wait one block past maturity: the chain releases the ATOM at the end of the block of
    // `mature_at`, and before that the balance holds only unrelated free rewards.
    let finalizable_at = mature_at.plus_seconds(FINALIZE_DELAY_SECS);
    if env.block.time < finalizable_at {
        return Err(ContractError::NotMatured {
            batch_id,
            remaining_secs: finalizable_at.seconds() - env.block.time.seconds(),
        });
    }

    // Every unbonding batch whose period and one-block delay are over, in id order. A batch that
    // matured moments ago may still have its ATOM in transit, so it waits for the next call.
    let mut ids = UNBONDING.load(deps.storage)?;
    ids.sort_unstable();
    let mut matured: Vec<Batch> = vec![];
    for id in &ids {
        let b = BATCHES.load(deps.storage, *id)?;
        if b.status == BatchStatus::Unbonding
            && b.mature_at.is_some_and(|m| env.block.time >= m.plus_seconds(FINALIZE_DELAY_SECS))
        {
            matured.push(b);
        }
    }
    let expected: Vec<Uint128> = matured
        .iter()
        .map(|b| b.atom_expected.ok_or(ContractError::WrongBatchStatus { batch_id: b.id }))
        .collect::<Result<_, _>>()?;

    // ATOM on the balance that is not already promised to finalized batches.
    let balance = deps
        .querier
        .query_balance(&env.contract.address, &cfg.bond_denom)?
        .amount;
    let owed = OWED_TO_REDEEMERS.load(deps.storage)?;
    let available = balance.saturating_sub(owed);

    // The chain returns a uatom or so less per `Undelegate` on a validator that was slashed before;
    // that rounding is paid out at once and does not count as slashing.
    let tolerance_of = |set: &[Batch]| {
        Uint128::new(
            set.iter().map(|b| b.undelegations as u128).sum::<u128>() * ROUNDING_TOLERANCE_PER_UNDELEGATE,
        )
    };
    let total_expected = expected.iter().fold(Uint128::zero(), |a, e| a.saturating_add(*e));
    if total_expected.saturating_sub(available) > tolerance_of(&matured) {
        // A real shortfall. A batch whose ATOM is merely late must not be written off, so each batch
        // has its own grace period counted from its own `mature_at`: only batches past it are settled
        // short, younger ones stay unbonding (and a stream of new batches cannot push an old
        // batch's deadline away either).
        let grace_over = |b: &Batch| {
            b.mature_at
                .is_some_and(|m| env.block.time >= m.plus_seconds(SHORTFALL_GRACE_SECS))
        };
        if !matured.iter().any(|b| b.id == batch_id && grace_over(b)) {
            let next_deadline = matured
                .iter()
                .filter_map(|b| b.mature_at)
                .map(|m| m.plus_seconds(SHORTFALL_GRACE_SECS).seconds())
                .filter(|d| *d > env.block.time.seconds())
                .min()
                .unwrap_or(0);
            return Err(ContractError::NotReceivedYet {
                batch_id,
                free: available,
                expected: total_expected,
                remaining_secs: next_deadline.saturating_sub(env.block.time.seconds()),
            });
        }
        matured.retain(|b| grace_over(b));

        // Do not write a batch off in the same block in which the shortfall is first seen: after a
        // chain halt longer than the grace period the first block already counts as past it, and the
        // unbonded ATOM is only released at the end of that block. Note the block; a later call
        // settles. (Ok, not Err: an error would revert the note.)
        let height = env.block.height;
        for b in matured.iter_mut() {
            if b.shortfall_noted_at.is_none() {
                b.shortfall_noted_at = Some(height);
                BATCHES.save(deps.storage, b.id, b)?;
            }
        }
        matured.retain(|b| b.shortfall_noted_at.is_some_and(|h| h < height));
        if !matured.iter().any(|b| b.id == batch_id) {
            return Ok(Response::new()
                .add_attribute("action", "finalize_batch")
                .add_attribute("shortfall_noted", batch_id.to_string()));
        }
    }
    let expected: Vec<Uint128> = matured
        .iter()
        .map(|b| b.atom_expected.ok_or(ContractError::WrongBatchStatus { batch_id: b.id }))
        .collect::<Result<_, _>>()?;
    let (shares, _) = rate::settle_matured(available, &expected);

    let settled: Vec<u64> = matured.iter().map(|b| b.id).collect();
    let mut owed = owed;
    let mut res = Response::new().add_attribute("action", "finalize_batch");
    for (mut b, received) in matured.into_iter().zip(shares) {
        b.status = BatchStatus::Matured;
        b.atom_received = Some(received);
        BATCHES.save(deps.storage, b.id, &b)?;
        owed = owed.checked_add(received)?;
        res = res
            .add_attribute("batch_id", b.id.to_string())
            .add_attribute("atom_received", received);
    }
    OWED_TO_REDEEMERS.save(deps.storage, &owed)?;
    let still_unbonding: Vec<u64> = ids.into_iter().filter(|id| !settled.contains(id)).collect();
    UNBONDING.save(deps.storage, &still_unbonding)?;
    Ok(res)
}

fn execute_claim(deps: DepsMut, info: MessageInfo, batch_id: u64) -> Result<Resp, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    let batch = BATCHES.load(deps.storage, batch_id)?;
    if batch.status != BatchStatus::Matured {
        return Err(ContractError::WrongBatchStatus { batch_id });
    }
    let liatom = CLAIMS
        .may_load(deps.storage, (batch_id, &info.sender))?
        .ok_or(ContractError::NoClaim { batch_id })?;
    CLAIMS.remove(deps.storage, (batch_id, &info.sender));

    let received = batch
        .atom_received
        .ok_or(ContractError::WrongBatchStatus { batch_id })?;
    // Rounds down. The dust stays in OWED_TO_REDEEMERS, so it is never paid out and not repriced
    // into the pool either (at most one uatom per claim).
    let payout = received.multiply_ratio(liatom, batch.liatom_burned);
    let owed = OWED_TO_REDEEMERS.load(deps.storage)?.checked_sub(payout)?;
    OWED_TO_REDEEMERS.save(deps.storage, &owed)?;

    let mut res = Response::new()
        .add_attribute("action", "claim")
        .add_attribute("batch_id", batch_id.to_string())
        .add_attribute("payout", payout);
    if !payout.is_zero() {
        res = res.add_message(BankMsg::Send {
            to_address: info.sender.to_string(),
            amount: vec![coin(payout.u128(), &cfg.bond_denom)],
        });
    }
    Ok(res)
}

/// The weights with the validator addresses in the form the chain uses (bech32 is lowercase).
fn canonical_weights(weights: &[ValidatorWeight]) -> Vec<ValidatorWeight> {
    weights
        .iter()
        .map(|w| ValidatorWeight { validator: w.validator.to_lowercase(), weight_bps: w.weight_bps })
        .collect()
}

fn execute_update_validators(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    validators: Vec<ValidatorWeight>,
) -> Result<Resp, ContractError> {
    only_admin(deps.as_ref(), &info)?;
    let validators = canonical_weights(&validators);
    validate_weights(&validators)?;
    for w in &validators {
        ensure_validator_exists(deps.as_ref(), &w.validator)?;
    }
    // Every exit and every price iterates the delegations, so their number is bounded too: the new list
    // plus the validators the contract still holds stake at (removed ones stay until Rebalance drains them).
    let mut names: Vec<String> = validators.iter().map(|w| w.validator.clone()).collect();
    for d in deps.querier.query_all_delegations(&env.contract.address)? {
        if !names.contains(&d.validator) {
            names.push(d.validator);
        }
    }
    if names.len() > MAX_VALIDATORS {
        return Err(ContractError::TooManyValidators { would_be: names.len(), max: MAX_VALIDATORS });
    }
    VALIDATORS.save(deps.storage, &validators)?;
    Ok(Response::new().add_attribute("action", "update_validators"))
}

fn set_pause(
    deps: DepsMut,
    info: MessageInfo,
    mode: Option<PauseMode>,
) -> Result<Resp, ContractError> {
    let mut cfg = only_admin(deps.as_ref(), &info)?;
    cfg.pause = mode;
    CONFIG.save(deps.storage, &cfg)?;
    let label = match mode {
        None => "none",
        Some(PauseMode::DepositsOnly) => "deposits_only",
        Some(PauseMode::Full) => "full",
    };
    Ok(Response::new()
        .add_attribute("action", "set_pause")
        .add_attribute("mode", label))
}

fn propose_admin(
    deps: DepsMut,
    info: MessageInfo,
    new_admin: String,
) -> Result<Resp, ContractError> {
    let mut cfg = only_admin(deps.as_ref(), &info)?;
    cfg.pending_admin = Some(deps.api.addr_validate(&new_admin)?);
    CONFIG.save(deps.storage, &cfg)?;
    Ok(Response::new()
        .add_attribute("action", "propose_admin")
        .add_attribute("pending_admin", new_admin))
}

fn accept_admin(deps: DepsMut, info: MessageInfo) -> Result<Resp, ContractError> {
    let mut cfg = CONFIG.load(deps.storage)?;
    if cfg.pending_admin.as_ref() != Some(&info.sender) {
        return Err(ContractError::Unauthorized {});
    }
    cfg.admin = info.sender.clone();
    cfg.pending_admin = None;
    CONFIG.save(deps.storage, &cfg)?;
    Ok(Response::new()
        .add_attribute("action", "accept_admin")
        .add_attribute("admin", info.sender))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_json_binary(&CONFIG.load(deps.storage)?),
        QueryMsg::Validators {} => to_json_binary(&VALIDATORS.load(deps.storage)?),
        QueryMsg::Pool {} => {
            let cfg = CONFIG.load(deps.storage)?;
            let pool = compute_pool(deps, &env, &cfg).map_err(to_std)?;
            to_json_binary(&pool.response())
        }
        QueryMsg::ExchangeRate {} => {
            let cfg = CONFIG.load(deps.storage)?;
            let pool = compute_pool(deps, &env, &cfg).map_err(to_std)?;
            let supply = pool.effective_supply().map_err(to_std)?;
            to_json_binary(&ExchangeRateResponse {
                rate: rate::exchange_rate(pool.pool_value, supply),
            })
        }
        QueryMsg::Batch { batch_id } => to_json_binary(&BATCHES.load(deps.storage, batch_id)?),
        QueryMsg::Claim { batch_id, address } => {
            let addr = deps.api.addr_validate(&address)?;
            to_json_binary(
                &CLAIMS
                    .may_load(deps.storage, (batch_id, &addr))?
                    .unwrap_or_default(),
            )
        }
    }
}

fn to_std(e: ContractError) -> StdError {
    StdError::generic_err(e.to_string())
}

/// Everything the pricing logic needs, read once per call.
struct Pool {
    delegations: Vec<(String, Uint128)>,
    delegated: Uint128,
    rewards_by_validator: Vec<(String, Uint128)>,
    unclaimed_rewards: Uint128,
    accrued_fee: Uint128,
    owed_to_redeemers: Uint128,
    pending_liatom: Uint128,
    pool_value: Uint128,
    liatom_supply: Uint128,
}

impl Pool {
    /// Supply used for pricing: minted liATOM plus liATOM burned into the open batch,
    /// whose ATOM is still in the pool until the batch is submitted.
    fn effective_supply(&self) -> Result<Uint128, ContractError> {
        Ok(self.liatom_supply.checked_add(self.pending_liatom)?)
    }

    fn response(&self) -> PoolResponse {
        PoolResponse {
            delegated: self.delegated,
            unclaimed_rewards: self.unclaimed_rewards,
            accrued_fee: self.accrued_fee,
            owed_to_redeemers: self.owed_to_redeemers,
            pending_liatom: self.pending_liatom,
            pool_value: self.pool_value,
            liatom_supply: self.liatom_supply,
        }
    }
}

/// pool_value = delegations + (rewards, withdrawn or not) - protocol fee on those rewards.
/// The contract's bank balance is not part of it.
fn compute_pool(deps: Deps, env: &Env, cfg: &Config) -> Result<Pool, ContractError> {
    let delegations: Vec<(String, Uint128)> = deps
        .querier
        .query_all_delegations(&env.contract.address)?
        .into_iter()
        .map(|d| (d.validator, d.amount.amount))
        .collect();
    let delegated = delegations
        .iter()
        .try_fold(Uint128::zero(), |acc, (_, a)| acc.checked_add(*a))?;

    let rewards = deps
        .querier
        .query_delegation_total_rewards(env.contract.address.as_str())?;
    let mut rewards_by_validator = vec![];
    let mut unclaimed_rewards = Uint128::zero();
    for r in &rewards.rewards {
        let mut floored = Uint128::zero();
        for c in r.reward.iter().filter(|c| c.denom == cfg.bond_denom) {
            let part = Uint128::try_from(c.amount.to_uint_floor())
                .map_err(|_| StdError::generic_err("rewards overflow"))?;
            floored = floored.checked_add(part)?;
        }
        unclaimed_rewards = unclaimed_rewards.checked_add(floored)?;
        rewards_by_validator.push((r.validator_address.clone(), floored));
    }

    // The contract's bank balance is NEVER part of any price. What the pool owns is what is
    // delegated plus the rewards the chain owes it (read with a query, floored exactly as the chain pays
    // them out, and collected in the same transaction as every delegation change). Everything else on the
    // balance is either unbonded ATOM that belongs to redeemers, whenever it arrives, or a donation, which
    // belongs to nobody: it cannot change a price, a batch or a rate. (Before this, reading the balance
    // and trying to tell those apart was the source of every pricing bug found in review.)
    let matured = matured_unfinalized(deps, env)?;
    let owed_to_redeemers = OWED_TO_REDEEMERS.load(deps.storage)?.checked_add(matured)?;
    let (accrued_fee, net_rewards) = rate::split_fee(unclaimed_rewards, cfg.fee_bps);
    let pool_value = delegated.checked_add(net_rewards)?;

    Ok(Pool {
        delegations,
        delegated,
        rewards_by_validator,
        unclaimed_rewards,
        accrued_fee,
        owed_to_redeemers,
        pending_liatom: PENDING_LIATOM.load(deps.storage)?,
        pool_value,
        liatom_supply: deps.querier.query_supply(&cfg.liatom_denom)?.amount,
    })
}

/// ATOM expected from unbonding batches whose period is over but that nobody finalized yet.
/// It is owed to redeemers (it is added to `owed_to_redeemers` in the pool query).
fn matured_unfinalized(deps: Deps, env: &Env) -> Result<Uint128, ContractError> {
    let mut total = Uint128::zero();
    for id in UNBONDING.load(deps.storage)? {
        let b = BATCHES.load(deps.storage, id)?;
        if let (Some(mature_at), Some(expected)) = (b.mature_at, b.atom_expected) {
            if env.block.time >= mature_at {
                total = total.checked_add(expected)?;
            }
        }
    }
    Ok(total)
}

/// Rewards the chain pays out to the contract balance whenever a delegation changes.
///
/// Changing the delegation of a validator (Delegate, Undelegate, Redelegate) makes the distribution
/// module pay that validator's pending rewards to the contract balance, in the middle of the
/// transaction. Left there they would be indistinguishable from unbonded ATOM arriving early, so the
/// same transaction collects them: the fee goes to the treasury and the rest is delegated again.
/// The payout equals the floored reward the query reports for the same state, so the amounts are exact.
///
/// The re-delegation goes to a validator that was touched already (its rewards are paid out now and
/// are zero for the rest of the block). If none is in the weight list, the target's own rewards are
/// paid by that Delegate and collected by a second Send and Delegate.
/// `delegations_after` are the delegations once the transaction's own messages have run.
fn sweep_rewards(
    deps: Deps,
    cfg: &Config,
    pool: &Pool,
    touched: &[String],
    delegations_after: &[(String, Uint128)],
    best_effort_fee: bool,
) -> Result<Vec<SubMsg<TokenFactoryMsg>>, ContractError> {
    let reward_of = |v: &str| -> Uint128 {
        pool.rewards_by_validator
            .iter()
            .find(|(addr, _)| addr == v)
            .map(|(_, a)| *a)
            .unwrap_or_default()
    };
    let mut unique: Vec<&String> = vec![];
    for v in touched {
        if !unique.contains(&v) {
            unique.push(v);
        }
    }
    let mut paid = Uint128::zero();
    for v in &unique {
        paid = paid.checked_add(reward_of(v))?;
    }
    let (fee, net) = rate::split_fee(paid, cfg.fee_bps);
    let mut msgs: Vec<SubMsg<TokenFactoryMsg>> = vec![];
    // The fee transfer can fail (a treasury that cannot receive). Where that must not stop the call
    // (SubmitUnbonding: users' exits), the failure is ignored in `reply`, which delegates the unsent
    // fee to the validator named in the payload, so it never sits on the balance as ownerless ATOM.
    let send = |amount: Uint128, reinvest_into: Option<&str>| -> SubMsg<TokenFactoryMsg> {
        let msg: CosmosMsg<TokenFactoryMsg> = BankMsg::Send {
            to_address: cfg.treasury.to_string(),
            amount: vec![coin(amount.u128(), &cfg.bond_denom)],
        }
        .into();
        match (best_effort_fee, reinvest_into) {
            (true, Some(validator)) => SubMsg::reply_on_error(msg, FEE_REPLY_ID).with_payload(
                to_json_binary(&UnsentFee { amount, validator: validator.to_string() })
                    .expect("serialisable"),
            ),
            // No validator to reinvest into (nothing was swept): fail the call like a strict send.
            _ => SubMsg::new(msg),
        }
    };
    let delegate = |validator: &str, amount: Uint128| -> SubMsg<TokenFactoryMsg> {
        SubMsg::new(StakingMsg::Delegate {
            validator: validator.to_string(),
            amount: coin(amount.u128(), &cfg.bond_denom),
        })
    };
    if net.is_zero() {
        // Nothing to re-delegate. With the fee capped at 20% this only happens when nothing was paid
        // out (no fee either), so the branch is a guard and the transfer is strict.
        if !fee.is_zero() {
            msgs.push(send(fee, None));
        }
        return Ok(msgs);
    }

    let weights = VALIDATORS.load(deps.storage)?;
    let among_touched: Vec<ValidatorWeight> = weights
        .iter()
        .filter(|w| unique.iter().any(|v| **v == w.validator))
        .cloned()
        .collect();
    let (target, already_paid) = match rate::pick_validator(&among_touched, delegations_after, net) {
        Some(v) => (v, true),
        // No touched validator is in the weight list (it was removed, or the list names validators the
        // contract holds no stake at). Re-delegate to the touched validator with the most stake: the
        // contract holds stake there, so it exists on the chain, while an untouched weight may have vanished
        // (an unbonded validator with no shares is deleted) and would make the whole exit revert.
        None => match unique.iter().max_by_key(|v| {
            delegations_after.iter().find(|(d, _)| d == **v).map(|(_, a)| *a).unwrap_or_default()
        }) {
            Some(v) => ((*v).clone(), true),
            None => (
                rate::pick_validator(&weights, delegations_after, net)
                    .ok_or(ContractError::InvalidWeights {})?,
                false,
            ),
        },
    };
    if !fee.is_zero() {
        msgs.push(send(fee, Some(&target)));
    }
    msgs.push(delegate(&target, net));
    if !already_paid {
        // Delegating to `target` paid out its pending rewards as well: collect them too.
        let (fee2, net2) = rate::split_fee(reward_of(&target), cfg.fee_bps);
        if !fee2.is_zero() {
            msgs.push(send(fee2, Some(&target)));
        }
        if !net2.is_zero() {
            msgs.push(delegate(&target, net2));
        }
    }
    Ok(msgs)
}

/// `delegations` with `amount` added to `validator`.
fn with_added(
    delegations: &[(String, Uint128)],
    validator: &str,
    amount: Uint128,
) -> Vec<(String, Uint128)> {
    let mut out = delegations.to_vec();
    match out.iter_mut().find(|(v, _)| v == validator) {
        Some((_, a)) => *a += amount,
        None => out.push((validator.to_string(), amount)),
    }
    out
}

fn ensure_validator_exists(deps: Deps, validator: &str) -> Result<(), ContractError> {
    let Some(v) = deps.querier.query_validator(validator)? else {
        return Err(StdError::generic_err(format!("validator {validator} not found")).into());
    };
    let max = Decimal::percent(MAX_VALIDATOR_COMMISSION_PERCENT);
    if v.commission > max {
        return Err(ContractError::CommissionTooHigh {
            validator: validator.to_string(),
            commission: v.commission,
            max,
        });
    }
    Ok(())
}

fn only_admin(deps: Deps, info: &MessageInfo) -> Result<Config, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    if info.sender != cfg.admin {
        return Err(ContractError::Unauthorized {});
    }
    Ok(cfg)
}

fn one_coin(info: &MessageInfo, denom: &str) -> Result<Coin, ContractError> {
    match info.funds.as_slice() {
        [c] if c.denom == denom && !c.amount.is_zero() => Ok(coin(c.amount.u128(), denom)),
        _ => Err(ContractError::WrongFunds {
            denom: denom.to_string(),
        }),
    }
}

fn new_batch(id: u64) -> Batch {
    Batch {
        id,
        status: BatchStatus::Open,
        liatom_burned: Uint128::zero(),
        atom_expected: None,
        atom_received: None,
        mature_at: None,
        undelegations: 0,
        shortfall_noted_at: None,
        submitted_at: None,
    }
}
