//! Contract-level tests against cosmwasm-std's mock querier (staking, bank, distribution).
//! Behaviour that needs real tokenfactory or unbonding is covered on the local testchain.

use cosmwasm_std::testing::{
    message_info, mock_dependencies, mock_env, MockApi, MockQuerier, MockStorage,
};
use cosmwasm_std::{
    coin, coins, Addr, BankMsg, CosmosMsg, DecCoin, Decimal, Decimal256, DistributionMsg, Env,
    FullDelegation, OwnedDeps, StakingMsg, Uint128, Validator,
};

use crate::contract::{execute, instantiate, query};
use crate::error::ContractError;
use crate::msg::{ExchangeRateResponse, ExecuteMsg, InstantiateMsg, QueryMsg};
use crate::state::{Batch, BatchStatus, Config, ValidatorWeight, SUBDENOM};
use crate::tokenfactory::{full_denom, TokenFactoryMsg};

type Deps = OwnedDeps<MockStorage, MockApi, MockQuerier>;

const ATOM: &str = "uatom";

fn validator(addr: &str) -> Validator {
    Validator::create(
        addr.to_string(),
        Decimal::percent(5),
        Decimal::percent(20),
        Decimal::percent(1),
    )
}

fn delegation(env: &Env, val: &str, amount: u128) -> FullDelegation {
    FullDelegation::create(
        env.contract.address.clone(),
        val.to_string(),
        coin(amount, ATOM),
        coin(amount, ATOM),
        vec![],
    )
}

fn weights() -> Vec<ValidatorWeight> {
    vec![
        ValidatorWeight {
            validator: "val1".into(),
            weight_bps: 6000,
        },
        ValidatorWeight {
            validator: "val2".into(),
            weight_bps: 4000,
        },
    ]
}

fn liatom(env: &Env) -> String {
    full_denom(env.contract.address.as_str(), SUBDENOM)
}

fn init_msg(deps: &Deps, admin: &Addr) -> InstantiateMsg {
    InstantiateMsg {
        admin: admin.to_string(),
        treasury: deps.api.addr_make("treasury").to_string(),
        fee_bps: 1000,
        epoch_secs: 345_600,
        unbonding_secs: 1_814_400,
        min_deposit: Uint128::new(1_000_000),
        max_tvl: Uint128::MAX,
        denom_creation_fee: Uint128::new(10_000_000),
        validators: weights(),
    }
}

/// Deps with two validators and a freshly instantiated contract holding a 10 ATOM seed.
fn setup() -> (Deps, Env, Addr) {
    let mut deps = mock_dependencies();
    let env = mock_env();
    deps.querier
        .staking
        .update(ATOM, &[validator("val1"), validator("val2")], &[]);
    let admin = deps.api.addr_make("admin");
    let msg = init_msg(&deps, &admin);
    let res = instantiate(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &coins(20_000_000, ATOM)),
        msg,
    )
    .unwrap();
    assert_eq!(res.messages.len(), 4);

    // Mirror the chain state after the instantiate messages ran.
    deps.querier.staking.update(
        ATOM,
        &[validator("val1"), validator("val2")],
        &[delegation(&env, "val1", 10_000_000)],
    );
    deps.querier.bank.update_balance(
        env.contract.address.to_string(),
        coins(10_000_000, liatom(&env)),
    );
    (deps, env, admin)
}

fn set_contract_balance(deps: &mut Deps, env: &Env, atom: u128, liatom_amount: u128) {
    let mut balance = vec![];
    if atom > 0 {
        balance.push(coin(atom, ATOM));
    }
    if liatom_amount > 0 {
        balance.push(coin(liatom_amount, liatom(env)));
    }
    deps.querier
        .bank
        .update_balance(env.contract.address.to_string(), balance);
}

#[test]
fn instantiate_creates_denom_mints_seed_and_delegates() {
    let mut deps = mock_dependencies();
    let env = mock_env();
    deps.querier
        .staking
        .update(ATOM, &[validator("val1"), validator("val2")], &[]);
    let admin = deps.api.addr_make("admin");
    let msg = init_msg(&deps, &admin);
    let res = instantiate(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &coins(20_000_000, ATOM)),
        msg,
    )
    .unwrap();

    assert_eq!(
        res.messages[0].msg,
        CosmosMsg::Custom(TokenFactoryMsg::CreateDenom {
            subdenom: "liatom".into()
        })
    );
    // Metadata is set right after the denom exists, before anything is minted.
    match &res.messages[1].msg {
        CosmosMsg::Custom(TokenFactoryMsg::SetMetadata { denom, metadata }) => {
            assert_eq!(denom, &liatom(&env));
            // The chain rejects metadata that breaks these bank rules.
            assert_eq!(metadata.base, *denom);
            assert_eq!(metadata.denom_units[0].denom, metadata.base);
            assert_eq!(metadata.denom_units[0].exponent, 0);
            let display = metadata
                .denom_units
                .iter()
                .find(|u| u.denom == metadata.display)
                .expect("display must be one of the denom units");
            assert_eq!(display.exponent, 6);
            assert_eq!(metadata.symbol, "liATOM");
            assert!(!metadata.name.is_empty() && !metadata.description.is_empty());
        }
        other => panic!("expected SetMetadata, got {other:?}"),
    }
    assert_eq!(
        res.messages[2].msg,
        CosmosMsg::Custom(TokenFactoryMsg::MintTokens {
            denom: liatom(&env),
            amount: Uint128::new(10_000_000),
            mint_to_address: env.contract.address.to_string(),
        })
    );
    assert_eq!(
        res.messages[3].msg,
        CosmosMsg::Staking(StakingMsg::Delegate {
            validator: "val1".into(),
            amount: coin(10_000_000, ATOM),
        })
    );
}

#[test]
fn instantiate_rejects_bad_input() {
    let mut deps = mock_dependencies();
    deps.querier
        .staking
        .update(ATOM, &[validator("val1"), validator("val2")], &[]);
    let admin = deps.api.addr_make("admin");
    let base = init_msg(&deps, &admin);
    let info = message_info(&admin, &coins(20_000_000, ATOM));

    let mut m = base.clone();
    m.fee_bps = 2001;
    assert_eq!(
        instantiate(deps.as_mut(), mock_env(), info.clone(), m).unwrap_err(),
        ContractError::FeeTooHigh {}
    );

    let mut m = base.clone();
    m.validators[0].validator = "ghost".into();
    assert!(instantiate(deps.as_mut(), mock_env(), info, m).is_err());

    let low = message_info(&admin, &coins(1, ATOM));
    assert!(matches!(
        instantiate(deps.as_mut(), mock_env(), low, base).unwrap_err(),
        ContractError::BelowMinDeposit { .. }
    ));
}

#[test]
fn deposit_at_par_mints_one_to_one_and_fills_the_underweight_validator() {
    let (mut deps, env, _) = setup();
    let user = deps.api.addr_make("user");
    // attached funds are already part of the contract balance during execution
    set_contract_balance(&mut deps, &env, 5_000_000, 10_000_000);

    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&user, &coins(5_000_000, ATOM)),
        ExecuteMsg::Deposit {},
    )
    .unwrap();

    assert_eq!(
        res.messages[0].msg,
        CosmosMsg::Custom(TokenFactoryMsg::MintTokens {
            denom: liatom(&env),
            amount: Uint128::new(5_000_000),
            mint_to_address: user.to_string(),
        })
    );
    // val1 holds everything, val2 (40%) is underweight
    assert_eq!(
        res.messages[1].msg,
        CosmosMsg::Staking(StakingMsg::Delegate {
            validator: "val2".into(),
            amount: coin(5_000_000, ATOM),
        })
    );
}

#[test]
fn rewards_raise_the_rate_and_reduce_mint() {
    let (mut deps, env, _) = setup();
    let user = deps.api.addr_make("user");

    // 1 ATOM of unclaimed rewards, 10% fee on it: pool 10.9, supply 10 -> rate 1.09
    let delegator = env.contract.address.to_string();
    deps.querier.distribution.set_rewards(
        "val1",
        delegator.clone(),
        vec![DecCoin::new(
            Decimal256::from_atomics(1_000_000u128, 0).unwrap(),
            ATOM,
        )],
    );
    deps.querier
        .distribution
        .set_validators(delegator, ["val1"]);

    let rate: ExchangeRateResponse = cosmwasm_std::from_json(
        query(deps.as_ref(), env.clone(), QueryMsg::ExchangeRate {}).unwrap(),
    )
    .unwrap();
    assert_eq!(rate.rate, Decimal::percent(109));

    set_contract_balance(&mut deps, &env, 10_900_000, 10_000_000);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&user, &coins(10_900_000, ATOM)),
        ExecuteMsg::Deposit {},
    )
    .unwrap();
    assert_eq!(
        res.messages[0].msg,
        CosmosMsg::Custom(TokenFactoryMsg::MintTokens {
            denom: liatom(&env),
            amount: Uint128::new(10_000_000),
            mint_to_address: user.to_string(),
        })
    );
}

#[test]
fn deposit_validations() {
    let (mut deps, env, admin) = setup();
    let user = deps.api.addr_make("user");
    set_contract_balance(&mut deps, &env, 10_000_000, 10_000_000);

    let wrong_denom = message_info(&user, &coins(2_000_000, "ibc/xyz"));
    assert!(matches!(
        execute(deps.as_mut(), env.clone(), wrong_denom, ExecuteMsg::Deposit {}).unwrap_err(),
        ContractError::WrongFunds { .. }
    ));
    let dust = message_info(&user, &coins(10, ATOM));
    assert!(matches!(
        execute(deps.as_mut(), env.clone(), dust, ExecuteMsg::Deposit {}).unwrap_err(),
        ContractError::BelowMinDeposit { .. }
    ));

    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::Pause { mode: crate::state::PauseMode::Full },
    )
    .unwrap();
    let ok_funds = message_info(&user, &coins(2_000_000, ATOM));
    assert_eq!(
        execute(deps.as_mut(), env, ok_funds, ExecuteMsg::Deposit {}).unwrap_err(),
        ContractError::Paused {}
    );
}

#[test]
fn redeem_records_claim_and_burns() {
    let (mut deps, env, _) = setup();
    let user = deps.api.addr_make("user");
    let denom = liatom(&env);

    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&user, &coins(3_000_000, denom.clone())),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    assert_eq!(
        res.messages[0].msg,
        CosmosMsg::Custom(TokenFactoryMsg::BurnTokens {
            denom: denom.clone(),
            amount: Uint128::new(3_000_000),
            burn_from_address: String::new(),
        })
    );

    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&user, &coins(1_000_000, denom)),
        ExecuteMsg::Redeem {},
    )
    .unwrap();

    let claim: Uint128 = cosmwasm_std::from_json(
        query(
            deps.as_ref(),
            env.clone(),
            QueryMsg::Claim {
                batch_id: 1,
                address: user.to_string(),
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(claim, Uint128::new(4_000_000));
    let batch: Batch = cosmwasm_std::from_json(
        query(deps.as_ref(), env, QueryMsg::Batch { batch_id: 1 }).unwrap(),
    )
    .unwrap();
    assert_eq!(batch.liatom_burned, Uint128::new(4_000_000));
    assert_eq!(batch.status, BatchStatus::Open);
}

#[test]
fn redeem_rejects_foreign_denom() {
    let (mut deps, env, _) = setup();
    let user = deps.api.addr_make("user");
    assert!(matches!(
        execute(
            deps.as_mut(),
            env,
            message_info(&user, &coins(5, ATOM)),
            ExecuteMsg::Redeem {},
        )
        .unwrap_err(),
        ContractError::WrongFunds { .. }
    ));
}

#[test]
fn admin_controls_and_two_step_transfer() {
    let (mut deps, env, admin) = setup();
    let user = deps.api.addr_make("user");
    let new_admin = deps.api.addr_make("new_admin");

    assert_eq!(
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&user, &[]),
            ExecuteMsg::Pause { mode: crate::state::PauseMode::Full }
        )
        .unwrap_err(),
        ContractError::Unauthorized {}
    );
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::ProposeAdmin {
            new_admin: new_admin.to_string(),
        },
    )
    .unwrap();
    // proposing alone changes nothing, only the nominee can accept
    assert_eq!(
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&user, &[]),
            ExecuteMsg::AcceptAdmin {}
        )
        .unwrap_err(),
        ContractError::Unauthorized {}
    );
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&new_admin, &[]),
        ExecuteMsg::AcceptAdmin {},
    )
    .unwrap();
    let cfg: Config =
        cosmwasm_std::from_json(query(deps.as_ref(), env, QueryMsg::Config {}).unwrap()).unwrap();
    assert_eq!(cfg.admin, new_admin);
    assert_eq!(cfg.pending_admin, None);
}

#[test]
fn update_validators_requires_admin_and_valid_weights() {
    let (mut deps, env, admin) = setup();
    let user = deps.api.addr_make("user");
    let msg = ExecuteMsg::UpdateValidators {
        validators: vec![ValidatorWeight {
            validator: "val2".into(),
            weight_bps: 10_000,
        }],
    };
    assert_eq!(
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&user, &[]),
            msg.clone()
        )
        .unwrap_err(),
        ContractError::Unauthorized {}
    );
    execute(deps.as_mut(), env.clone(), message_info(&admin, &[]), msg).unwrap();

    let bad = ExecuteMsg::UpdateValidators {
        validators: vec![ValidatorWeight {
            validator: "val2".into(),
            weight_bps: 9_000,
        }],
    };
    assert_eq!(
        execute(deps.as_mut(), env, message_info(&admin, &[]), bad).unwrap_err(),
        ContractError::InvalidWeights {}
    );
}

fn rate_of(deps: &Deps, env: &Env) -> Decimal {
    let r: ExchangeRateResponse = cosmwasm_std::from_json(
        query(deps.as_ref(), env.clone(), QueryMsg::ExchangeRate {}).unwrap(),
    )
    .unwrap();
    r.rate
}

fn batch_of(deps: &Deps, env: &Env, id: u64) -> Batch {
    cosmwasm_std::from_json(
        query(deps.as_ref(), env.clone(), QueryMsg::Batch { batch_id: id }).unwrap(),
    )
    .unwrap()
}

fn set_delegations(deps: &mut Deps, env: &Env, parts: &[(&str, u128)]) {
    let ds: Vec<FullDelegation> = parts.iter().map(|(v, a)| delegation(env, v, *a)).collect();
    deps.querier
        .staking
        .update(ATOM, &[validator("val1"), validator("val2")], &ds);
}

#[test]
fn harvest_takes_fee_and_reinvests_rewards_and_ignores_the_balance() {
    let (mut deps, env, _) = setup();
    let anyone = deps.api.addr_make("anyone");
    let treasury = deps.api.addr_make("treasury");

    // 1 ATOM still in distribution. The 0.5 ATOM on the balance is not the pool's (a donation or
    // unbonded ATOM) and is left alone.
    let delegator = env.contract.address.to_string();
    deps.querier.distribution.set_rewards(
        "val1",
        delegator.clone(),
        vec![DecCoin::new(
            Decimal256::from_atomics(1_000_000u128, 0).unwrap(),
            ATOM,
        )],
    );
    deps.querier
        .distribution
        .set_validators(delegator, ["val1"]);
    set_contract_balance(&mut deps, &env, 500_000, 10_000_000);

    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&anyone, &[]),
        ExecuteMsg::Harvest {},
    )
    .unwrap();

    // gross 1.0, fee 10% = 0.1, reinvested 0.9 into the underweight validator
    assert_eq!(res.messages.len(), 3);
    assert_eq!(
        res.messages[0].msg,
        CosmosMsg::Distribution(DistributionMsg::WithdrawDelegatorReward {
            validator: "val1".into()
        })
    );
    assert_eq!(
        res.messages[1].msg,
        CosmosMsg::Bank(BankMsg::Send {
            to_address: treasury.to_string(),
            amount: coins(100_000, ATOM),
        })
    );
    assert_eq!(
        res.messages[2].msg,
        CosmosMsg::Staking(StakingMsg::Delegate {
            validator: "val2".into(),
            amount: coin(900_000, ATOM),
        })
    );
}

#[test]
fn harvest_with_nothing_to_collect_fails() {
    let (mut deps, env, _) = setup();
    let anyone = deps.api.addr_make("anyone");
    assert_eq!(
        execute(
            deps.as_mut(),
            env,
            message_info(&anyone, &[]),
            ExecuteMsg::Harvest {}
        )
        .unwrap_err(),
        ContractError::NothingToHarvest {}
    );
}

#[test]
fn full_redemption_lifecycle_keeps_the_rate_flat() {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    let bob = deps.api.addr_make("bob");
    let denom = liatom(&env);

    // 15 ATOM delegated: 10 seed + 5 deposited by alice.
    set_delegations(&mut deps, &env, &[("val1", 9_000_000), ("val2", 6_000_000)]);
    deps.querier.bank.update_balance(
        alice.to_string(),
        coins(5_000_000, denom.clone()),
    );
    assert_eq!(rate_of(&deps, &env), Decimal::one());

    // alice redeems 3 liATOM; the chain burns them, so supply drops to 12.
    deps.querier
        .bank
        .update_balance(alice.to_string(), coins(2_000_000, denom.clone()));
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(3_000_000, denom.clone())),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    // 12 minted + 3 pending against 15 ATOM: unchanged.
    assert_eq!(rate_of(&deps, &env), Decimal::one());

    // Submit: 3 ATOM undelegated pro rata (60% / 40%).
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&bob, &[]),
        ExecuteMsg::SubmitUnbonding {},
    )
    .unwrap();
    assert_eq!(
        res.messages
            .iter()
            .map(|m| m.msg.clone())
            .collect::<Vec<_>>(),
        vec![
            CosmosMsg::Staking(StakingMsg::Undelegate {
                validator: "val1".into(),
                amount: coin(1_800_000, ATOM),
            }),
            CosmosMsg::Staking(StakingMsg::Undelegate {
                validator: "val2".into(),
                amount: coin(1_200_000, ATOM),
            }),
        ]
    );
    let b1 = batch_of(&deps, &env, 1);
    assert_eq!(b1.status, BatchStatus::Unbonding);
    assert_eq!(b1.atom_expected, Some(Uint128::new(3_000_000)));
    let mature_at = env.block.time.plus_seconds(1_814_400);
    assert_eq!(b1.mature_at, Some(mature_at));
    assert_eq!(batch_of(&deps, &env, 2).status, BatchStatus::Open);

    // The chain applied the undelegation: delegations shrink, rate stays 1.0.
    set_delegations(&mut deps, &env, &[("val1", 7_200_000), ("val2", 4_800_000)]);
    assert_eq!(rate_of(&deps, &env), Decimal::one());

    // Nothing queued in the new batch, and a second epoch has not elapsed.
    assert_eq!(
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&bob, &[]),
            ExecuteMsg::SubmitUnbonding {}
        )
        .unwrap_err(),
        ContractError::NothingToUnbond {}
    );
    deps.querier
        .bank
        .update_balance(alice.to_string(), coins(1_000_000, denom.clone()));
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(1_000_000, denom)),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    assert_eq!(
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&bob, &[]),
            ExecuteMsg::SubmitUnbonding {}
        )
        .unwrap_err(),
        ContractError::EpochNotElapsed {
            remaining_secs: 345_600
        }
    );

    // Too early to finalize or claim.
    assert!(matches!(
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&bob, &[]),
            ExecuteMsg::FinalizeBatch { batch_id: 1 }
        )
        .unwrap_err(),
        ContractError::NotMatured { .. }
    ));
    assert_eq!(
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&alice, &[]),
            ExecuteMsg::Claim { batch_id: 1 }
        )
        .unwrap_err(),
        ContractError::WrongBatchStatus { batch_id: 1 }
    );

    // After the unbonding period the 3 ATOM are back on the contract. Until someone finalizes,
    // they are owed to redeemers and must not lift the rate.
    let mut arrival = env.clone();
    arrival.block.time = mature_at.plus_seconds(1);
    set_contract_balance(&mut deps, &arrival, 3_000_000, 10_000_000);
    assert_eq!(rate_of(&deps, &arrival), Decimal::one());
    // Finalizing waits one block past maturity (the ATOM arrives at the end of that block).
    assert!(matches!(
        execute(
            deps.as_mut(),
            arrival.clone(),
            message_info(&bob, &[]),
            ExecuteMsg::FinalizeBatch { batch_id: 1 }
        )
        .unwrap_err(),
        ContractError::NotMatured { .. }
    ));
    let mut later = env.clone();
    later.block.time = mature_at.plus_seconds(crate::state::FINALIZE_DELAY_SECS);

    execute(
        deps.as_mut(),
        later.clone(),
        message_info(&bob, &[]),
        ExecuteMsg::FinalizeBatch { batch_id: 1 },
    )
    .unwrap();
    let b1 = batch_of(&deps, &later, 1);
    assert_eq!(b1.status, BatchStatus::Matured);
    assert_eq!(b1.atom_received, Some(Uint128::new(3_000_000)));
    assert_eq!(rate_of(&deps, &later), Decimal::one());

    // Only the redeemer can claim, and only once.
    assert_eq!(
        execute(
            deps.as_mut(),
            later.clone(),
            message_info(&bob, &[]),
            ExecuteMsg::Claim { batch_id: 1 }
        )
        .unwrap_err(),
        ContractError::NoClaim { batch_id: 1 }
    );
    let res = execute(
        deps.as_mut(),
        later.clone(),
        message_info(&alice, &[]),
        ExecuteMsg::Claim { batch_id: 1 },
    )
    .unwrap();
    assert_eq!(
        res.messages[0].msg,
        CosmosMsg::Bank(BankMsg::Send {
            to_address: alice.to_string(),
            amount: coins(3_000_000, ATOM),
        })
    );
    assert_eq!(
        execute(
            deps.as_mut(),
            later,
            message_info(&alice, &[]),
            ExecuteMsg::Claim { batch_id: 1 }
        )
        .unwrap_err(),
        ContractError::NoClaim { batch_id: 1 }
    );
}

#[test]
fn slashed_unbonding_pays_out_what_actually_came_back() {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    let denom = liatom(&env);

    // 10 seed + 4 minted for alice, then alice's 4 are burned on redeem: supply 10, pending 4.
    set_delegations(&mut deps, &env, &[("val1", 14_000_000)]);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(4_000_000, denom)),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &[]),
        ExecuteMsg::SubmitUnbonding {},
    )
    .unwrap();

    // 4 ATOM expected, a slash left only 3.6 on the balance.
    let mut early = env.clone();
    early.block.time = env.block.time.plus_seconds(1_814_400 + crate::state::FINALIZE_DELAY_SECS);
    set_contract_balance(&mut deps, &early, 3_600_000, 10_000_000);
    // Right after maturity a shortfall may be funds the chain releases at the end of this block,
    // so it is not accepted yet (anyone could otherwise underpay the batch by finalizing early).
    let err = execute(
        deps.as_mut(),
        early.clone(),
        message_info(&alice, &[]),
        ExecuteMsg::FinalizeBatch { batch_id: 1 },
    )
    .unwrap_err();
    assert!(matches!(err, ContractError::NotReceivedYet { .. }), "{err:?}");

    let mut later = env.clone();
    later.block.time = env
        .block
        .time
        .plus_seconds(1_814_400 + crate::state::SHORTFALL_GRACE_SECS);
    set_contract_balance(&mut deps, &later, 3_600_000, 10_000_000);
    finalize_noted(&mut deps, &later, &alice, 1).unwrap();
    assert_eq!(
        batch_of(&deps, &later, 1).atom_received,
        Some(Uint128::new(3_600_000))
    );

    let res = execute(
        deps.as_mut(),
        later,
        message_info(&alice, &[]),
        ExecuteMsg::Claim { batch_id: 1 },
    )
    .unwrap();
    assert_eq!(
        res.messages[0].msg,
        CosmosMsg::Bank(BankMsg::Send {
            to_address: alice.to_string(),
            amount: coins(3_600_000, ATOM),
        })
    );
}

#[test]
fn unbonding_cannot_exceed_delegations() {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    let denom = liatom(&env);

    // Almost everything is unclaimed rewards, only 1 ATOM is delegated.
    set_delegations(&mut deps, &env, &[("val1", 1_000_000)]);
    set_val1_rewards(&mut deps, &env, 9_000_000);
    set_contract_balance(&mut deps, &env, 0, 10_000_000);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(5_000_000, denom)),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    assert!(matches!(
        execute(
            deps.as_mut(),
            env,
            message_info(&alice, &[]),
            ExecuteMsg::SubmitUnbonding {}
        )
        .unwrap_err(),
        ContractError::InsufficientDelegations { .. }
    ));
}

fn config_of(deps: &Deps, env: &Env) -> Config {
    cosmwasm_std::from_json(query(deps.as_ref(), env.clone(), QueryMsg::Config {}).unwrap())
        .unwrap()
}

fn update_config(
    fee_bps: Option<u16>,
    epoch_secs: Option<u64>,
    unbonding_secs: Option<u64>,
    min_deposit: Option<u128>,
) -> ExecuteMsg {
    ExecuteMsg::UpdateConfig {
        treasury: None,
        fee_bps,
        epoch_secs,
        unbonding_secs,
        min_deposit: min_deposit.map(Uint128::new),
        max_tvl: None,
    }
}

#[test]
fn update_config_is_admin_only_and_validated() {
    let (mut deps, env, admin) = setup();
    let user = deps.api.addr_make("user");
    let new_treasury = deps.api.addr_make("new_treasury");

    assert_eq!(
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&user, &[]),
            update_config(Some(500), None, None, None)
        )
        .unwrap_err(),
        ContractError::Unauthorized {}
    );
    let admin_info = message_info(&admin, &[]);
    assert_eq!(
        execute(
            deps.as_mut(),
            env.clone(),
            admin_info.clone(),
            update_config(None, None, None, None)
        )
        .unwrap_err(),
        ContractError::EmptyConfigUpdate {}
    );
    assert_eq!(
        execute(
            deps.as_mut(),
            env.clone(),
            admin_info.clone(),
            update_config(Some(2001), None, None, None)
        )
        .unwrap_err(),
        ContractError::FeeTooHigh {}
    );
    assert_eq!(
        execute(
            deps.as_mut(),
            env.clone(),
            admin_info.clone(),
            update_config(None, None, None, Some(0))
        )
        .unwrap_err(),
        ContractError::InvalidMinDeposit {}
    );
    // 21 days of unbonding cannot be spread over 1000 s epochs without exceeding 7 entries
    assert!(matches!(
        execute(
            deps.as_mut(),
            env.clone(),
            admin_info.clone(),
            update_config(None, Some(1_000), None, None)
        )
        .unwrap_err(),
        ContractError::InvalidTiming { .. }
    ));

    // a rejected update leaves the config untouched
    assert_eq!(config_of(&deps, &env).epoch_secs, 345_600);

    execute(
        deps.as_mut(),
        env.clone(),
        admin_info,
        ExecuteMsg::UpdateConfig {
            treasury: Some(new_treasury.to_string()),
            fee_bps: None,
            epoch_secs: Some(400_000),
            unbonding_secs: Some(1_900_000),
            min_deposit: Some(Uint128::new(2_000_000)),
            max_tvl: None,
        },
    )
    .unwrap();
    let cfg = config_of(&deps, &env);
    assert_eq!(cfg.treasury, new_treasury);
    assert_eq!(cfg.epoch_secs, 400_000);
    assert_eq!(cfg.unbonding_secs, 1_900_000);
    assert_eq!(cfg.min_deposit, Uint128::new(2_000_000));
}

#[test]
fn changing_the_fee_harvests_under_the_old_fee_first() {
    let (mut deps, env, admin) = setup();
    let treasury = deps.api.addr_make("treasury");

    let delegator = env.contract.address.to_string();
    deps.querier.distribution.set_rewards(
        "val1",
        delegator.clone(),
        vec![DecCoin::new(
            Decimal256::from_atomics(1_000_000u128, 0).unwrap(),
            ATOM,
        )],
    );
    deps.querier
        .distribution
        .set_validators(delegator, ["val1"]);

    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        update_config(Some(500), None, None, None),
    )
    .unwrap();
    // old fee 10% of 1 ATOM goes to the treasury, the rest is reinvested
    assert_eq!(
        res.messages[1].msg,
        CosmosMsg::Bank(BankMsg::Send {
            to_address: treasury.to_string(),
            amount: coins(100_000, ATOM),
        })
    );
    assert_eq!(
        res.messages[2].msg,
        CosmosMsg::Staking(StakingMsg::Delegate {
            validator: "val2".into(),
            amount: coin(900_000, ATOM),
        })
    );
    assert_eq!(config_of(&deps, &env).fee_bps, 500);

    // setting the same fee again does not touch rewards
    let res = execute(
        deps.as_mut(),
        env,
        message_info(&admin, &[]),
        update_config(Some(500), None, None, None),
    )
    .unwrap();
    assert!(res.messages.is_empty());
}

#[test]
fn instantiate_rejects_bad_timing_and_zero_min_deposit() {
    let mut deps = mock_dependencies();
    deps.querier
        .staking
        .update(ATOM, &[validator("val1"), validator("val2")], &[]);
    let admin = deps.api.addr_make("admin");
    let info = message_info(&admin, &coins(20_000_000, ATOM));

    let mut m = init_msg(&deps, &admin);
    m.epoch_secs = 3_600;
    assert!(matches!(
        instantiate(deps.as_mut(), mock_env(), info.clone(), m).unwrap_err(),
        ContractError::InvalidTiming { .. }
    ));
    let mut m = init_msg(&deps, &admin);
    m.min_deposit = Uint128::zero();
    assert_eq!(
        instantiate(deps.as_mut(), mock_env(), info, m).unwrap_err(),
        ContractError::InvalidMinDeposit {}
    );
}

#[test]
fn rebalance_redelegates_towards_target_weights() {
    let (mut deps, env, admin) = setup();
    let user = deps.api.addr_make("user");
    // 10 ATOM all on val1, targets are 60% / 40%
    assert_eq!(
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&user, &[]),
            ExecuteMsg::Rebalance { min_move: None }
        )
        .unwrap_err(),
        ContractError::Unauthorized {}
    );

    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::Rebalance { min_move: None },
    )
    .unwrap();
    assert_eq!(
        res.messages
            .iter()
            .map(|m| m.msg.clone())
            .collect::<Vec<_>>(),
        vec![CosmosMsg::Staking(StakingMsg::Redelegate {
            src_validator: "val1".into(),
            dst_validator: "val2".into(),
            amount: coin(4_000_000, ATOM),
        })]
    );

    // once the chain applied it there is nothing left to do
    set_delegations(&mut deps, &env, &[("val1", 6_000_000), ("val2", 4_000_000)]);
    assert_eq!(
        execute(
            deps.as_mut(),
            env,
            message_info(&admin, &[]),
            ExecuteMsg::Rebalance { min_move: None }
        )
        .unwrap_err(),
        ContractError::NothingToRebalance {}
    );
}

#[test]
fn dropping_a_validator_from_the_weights_drains_it() {
    let (mut deps, env, admin) = setup();
    set_delegations(&mut deps, &env, &[("val1", 6_000_000), ("val2", 4_000_000)]);

    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::UpdateValidators {
            validators: vec![ValidatorWeight {
                validator: "val2".into(),
                weight_bps: 10_000,
            }],
        },
    )
    .unwrap();
    let res = execute(
        deps.as_mut(),
        env,
        message_info(&admin, &[]),
        ExecuteMsg::Rebalance { min_move: None },
    )
    .unwrap();
    assert_eq!(
        res.messages[0].msg,
        CosmosMsg::Staking(StakingMsg::Redelegate {
            src_validator: "val1".into(),
            dst_validator: "val2".into(),
            amount: coin(6_000_000, ATOM),
        })
    );
}

#[test]
fn pause_modes_block_what_they_promise() {
    use crate::state::PauseMode;
    let (mut deps, env, admin) = setup();
    let alice = deps.api.addr_make("alice");
    let denom = liatom(&env);
    set_delegations(&mut deps, &env, &[("val1", 14_000_000)]);

    let deposit = |deps: &mut Deps| {
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&alice, &coins(2_000_000, ATOM)),
            ExecuteMsg::Deposit {},
        )
    };
    let redeem = |deps: &mut Deps| {
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&alice, &coins(1_000_000, denom.clone())),
            ExecuteMsg::Redeem {},
        )
    };
    let set = |deps: &mut Deps, m: Option<PauseMode>| {
        let msg = match m {
            Some(mode) => ExecuteMsg::Pause { mode },
            None => ExecuteMsg::Unpause {},
        };
        execute(deps.as_mut(), env.clone(), message_info(&admin, &[]), msg).unwrap();
    };

    // Deposits only: users can still leave.
    set(&mut deps, Some(PauseMode::DepositsOnly));
    assert_eq!(deposit(&mut deps).unwrap_err(), ContractError::Paused {});
    redeem(&mut deps).unwrap();

    // Full: nobody can enter or queue to leave.
    set(&mut deps, Some(PauseMode::Full));
    assert_eq!(deposit(&mut deps).unwrap_err(), ContractError::Paused {});
    assert_eq!(redeem(&mut deps).unwrap_err(), ContractError::Paused {});

    // Back to normal.
    set(&mut deps, None);
    redeem(&mut deps).unwrap();
}

/// Two batches that each expect 100 uatom, both matured. Returns the deps, the env after the
/// grace period and the redeemer.
fn two_matured_batches() -> (Deps, Env, Addr) {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    let denom = liatom(&env);

    // Seed 10 ATOM supply held by the contract; alice redeems 100 uatom of liATOM in each batch.
    set_delegations(&mut deps, &env, &[("val1", 10_000_200)]);
    let redeem = |deps: &mut Deps, env: &Env| {
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&alice, &coins(100, denom.clone())),
            ExecuteMsg::Redeem {},
        )
        .unwrap();
    };
    let submit = |deps: &mut Deps, env: &Env| {
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&alice, &[]),
            ExecuteMsg::SubmitUnbonding {},
        )
        .unwrap();
    };

    redeem(&mut deps, &env);
    submit(&mut deps, &env);
    assert_eq!(batch_of(&deps, &env, 1).atom_expected, Some(Uint128::new(100)));
    set_delegations(&mut deps, &env, &[("val1", 10_000_100)]);

    let mut second = env.clone();
    second.block.time = env.block.time.plus_seconds(345_600 + 10);
    redeem(&mut deps, &second);
    submit(&mut deps, &second);
    assert_eq!(batch_of(&deps, &second, 2).atom_expected, Some(Uint128::new(100)));
    set_delegations(&mut deps, &second, &[("val1", 10_000_000)]);

    // Both unbonding periods and the grace period are over.
    let mut later = second.clone();
    later.block.time = second
        .block
        .time
        .plus_seconds(1_814_400 + crate::state::SHORTFALL_GRACE_SECS + 1);
    (deps, later, alice)
}

/// FinalizeBatch where a real shortfall is settled: the first call only notes it (Ok, nothing paid),
/// a call in a later block settles.
fn finalize_noted(deps: &mut Deps, env: &Env, who: &Addr, id: u64) -> Result<(), ContractError> {
    finalize(deps, env, who, id)?;
    let mut next = env.clone();
    next.block.height += 1;
    finalize(deps, &next, who, id)
}

fn finalize(deps: &mut Deps, env: &Env, who: &Addr, id: u64) -> Result<(), ContractError> {
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(who, &[]),
        ExecuteMsg::FinalizeBatch { batch_id: id },
    )
    .map(|_| ())
}

fn received(deps: &Deps, env: &Env) -> (u128, u128) {
    (
        batch_of(deps, env, 1).atom_received.unwrap().u128(),
        batch_of(deps, env, 2).atom_received.unwrap().u128(),
    )
}

#[test]
fn shortfall_is_shared_pro_rata_whichever_batch_is_finalized_first() {
    // 200 expected, only 150 came back (a slash). Finalizing batch 1 first or batch 2 first,
    // by anyone, must give the same split: nobody can pick who absorbs the loss.
    let outcomes: Vec<(u128, u128)> = [1u64, 2]
        .into_iter()
        .map(|first| {
            let (mut deps, later, _) = two_matured_batches();
            set_contract_balance(&mut deps, &later, 150, 10_000_000);
            let stranger = deps.api.addr_make("stranger");
            finalize_noted(&mut deps, &later, &stranger, first).unwrap();
            received(&deps, &later)
        })
        .collect();
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(outcomes[0], (75, 75));
}

#[test]
fn one_finalize_settles_every_matured_batch() {
    let (mut deps, later, alice) = two_matured_batches();
    set_contract_balance(&mut deps, &later, 150, 10_000_000);
    finalize_noted(&mut deps, &later, &alice, 2).unwrap();
    // Batch 1 was settled by the same call, so finalizing it again is refused.
    assert_eq!(
        finalize(&mut deps, &later, &alice, 1).unwrap_err(),
        ContractError::WrongBatchStatus { batch_id: 1 }
    );
    assert_eq!(received(&deps, &later), (75, 75));
}

#[test]
fn full_payout_does_not_depend_on_order_either() {
    for first in [1u64, 2] {
        let (mut deps, later, alice) = two_matured_batches();
        set_contract_balance(&mut deps, &later, 200, 10_000_000);
        finalize(&mut deps, &later, &alice, first).unwrap();
        assert_eq!(received(&deps, &later), (100, 100));
    }
}

#[test]
fn each_batch_has_its_own_grace_period() {
    let (mut deps, later, alice) = two_matured_batches();
    set_contract_balance(&mut deps, &later, 150, 10_000_000);
    // Batch 1 matured four days (and ten seconds) before batch 2.
    let four_days = 345_600 + 10;
    let mut within_grace = later.clone();
    within_grace.block.time = later.block.time.minus_seconds(four_days + 3600);
    let err = finalize(&mut deps, &within_grace, &alice, 1).unwrap_err();
    assert!(matches!(err, ContractError::NotReceivedYet { .. }), "{err:?}");

    // Batch 1's grace is over, batch 2's is not: only batch 1 is settled, and a batch that matured
    // later cannot push its deadline away. Batch 2 is left alone.
    let mut first_only = later.clone();
    first_only.block.time = later.block.time.minus_seconds(four_days);
    finalize_noted(&mut deps, &first_only, &alice, 1).unwrap();
    assert_eq!(batch_of(&deps, &first_only, 1).atom_received, Some(Uint128::new(100)));
    assert_eq!(batch_of(&deps, &first_only, 2).status, BatchStatus::Unbonding);
    let err = finalize(&mut deps, &first_only, &alice, 2).unwrap_err();
    assert!(matches!(err, ContractError::NotReceivedYet { .. }), "{err:?}");

    // Once its own grace is over, batch 2 takes what is left.
    finalize_noted(&mut deps, &later, &alice, 2).unwrap();
    assert_eq!(batch_of(&deps, &later, 2).atom_received, Some(Uint128::new(50)));
}

#[test]
fn a_late_batch_is_not_written_off_while_its_own_grace_runs() {
    // Review finding (haiku, opus): the chain pays batch 2 later than the configured period. The
    // call that settles batch 1 must not treat batch 2's missing ATOM as slashing, or the ATOM
    // that arrives later is free balance that anyone can harvest.
    let (mut deps, later, alice) = two_matured_batches();
    let four_days = 345_600 + 10;
    let mut at = later.clone();
    at.block.time = later.block.time.minus_seconds(four_days); // batch 1's grace over, batch 2's not
    set_contract_balance(&mut deps, &at, 100, 10_000_000); // only batch 1's ATOM is there
    finalize_noted(&mut deps, &at, &alice, 1).unwrap();
    assert_eq!(batch_of(&deps, &at, 1).atom_received, Some(Uint128::new(100)));
    assert_eq!(batch_of(&deps, &at, 2).status, BatchStatus::Unbonding);
    // Batch 2 has not matured: nothing to harvest.
    assert_eq!(
        execute(
            deps.as_mut(),
            at.clone(),
            message_info(&alice, &[]),
            ExecuteMsg::Harvest {},
        )
        .unwrap_err(),
        ContractError::NothingToHarvest {}
    );
    // It arrives, and is paid in full.
    set_contract_balance(&mut deps, &at, 200, 10_000_000);
    finalize(&mut deps, &at, &alice, 2).unwrap();
    assert_eq!(batch_of(&deps, &at, 2).atom_received, Some(Uint128::new(100)));
}

#[test]
fn rounding_dust_is_not_treated_as_slashing() {
    // Review finding (opus, sonnet): on a validator that was slashed once, Undelegate returns a
    // uatom less than asked. With no free rewards left on the balance (they are swept), that used
    // to look like slashing and held every batch back for a week, then for weeks as later batches
    // matured. A deficit of up to ROUNDING_TOLERANCE_PER_UNDELEGATE per undelegation is paid out now.
    let (mut deps, at, alice) = arrival_window();
    let mut ready = at.clone();
    ready.block.time = at.block.time.plus_seconds(crate::state::FINALIZE_DELAY_SECS);
    set_contract_balance(&mut deps, &ready, 99, 0); // 100 expected, 1 uatom lost to rounding
    finalize(&mut deps, &ready, &alice, 1).unwrap();
    assert_eq!(batch_of(&deps, &ready, 1).atom_received, Some(Uint128::new(99)));

    // A real loss (here 20 uatom, more than the tolerance of 10) still waits for the grace period.
    let (mut deps, at, alice) = arrival_window();
    let mut ready = at.clone();
    ready.block.time = at.block.time.plus_seconds(crate::state::FINALIZE_DELAY_SECS);
    set_contract_balance(&mut deps, &ready, 80, 0);
    let err = finalize(&mut deps, &ready, &alice, 1).unwrap_err();
    assert!(matches!(err, ContractError::NotReceivedYet { .. }), "{err:?}");
}

#[test]
fn a_batch_that_matured_moments_ago_is_left_for_the_next_call() {
    // Its ATOM is released at the end of its block, so the call must not count it as a shortfall.
    let (mut deps, later, alice) = two_matured_batches();
    // Only batch 1's ATOM is on the balance; batch 2 matured 10 seconds ago.
    set_contract_balance(&mut deps, &later, 100, 10_000_000);
    let mut just_matured = later.clone();
    just_matured.block.time = later.block.time.minus_seconds(crate::state::SHORTFALL_GRACE_SECS + 1)
        .plus_seconds(0);
    // At this time batch 2 matured exactly GRACE+1 seconds before 'later'; make batch 2 only 10 s old.
    let batch2_mature = later.block.time.minus_seconds(crate::state::SHORTFALL_GRACE_SECS + 1);
    just_matured.block.time = batch2_mature.plus_seconds(10);
    finalize(&mut deps, &just_matured, &alice, 1).unwrap();
    assert_eq!(batch_of(&deps, &just_matured, 1).atom_received, Some(Uint128::new(100)));
    // Batch 2 is still waiting and is paid once its ATOM is there.
    assert_eq!(batch_of(&deps, &just_matured, 2).status, BatchStatus::Unbonding);
}

fn set_max_tvl(deps: &mut Deps, env: &Env, admin: &Addr, max: u128) -> Result<(), ContractError> {
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(admin, &[]),
        ExecuteMsg::UpdateConfig {
            treasury: None,
            fee_bps: None,
            epoch_secs: None,
            unbonding_secs: None,
            min_deposit: None,
            max_tvl: Some(Uint128::new(max)),
        },
    )
    .map(|_| ())
}

#[test]
fn deposits_stop_at_the_tvl_cap_but_exits_stay_open() {
    let (mut deps, env, admin) = setup();
    let alice = deps.api.addr_make("alice");
    let denom = liatom(&env);
    set_delegations(&mut deps, &env, &[("val1", 10_000_000)]);

    // Pool is 10 ATOM. A cap of 15 leaves room for exactly 5.
    set_max_tvl(&mut deps, &env, &admin, 15_000_000).unwrap();
    let deposit = |deps: &mut Deps, amount: u128| {
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&alice, &coins(amount, ATOM)),
            ExecuteMsg::Deposit {},
        )
    };
    assert_eq!(
        deposit(&mut deps, 5_000_001).unwrap_err(),
        ContractError::TvlCapExceeded {
            max: Uint128::new(15_000_000),
            after: Uint128::new(15_000_001),
            room: Uint128::new(5_000_000),
        }
    );
    deposit(&mut deps, 5_000_000).unwrap(); // exactly at the cap

    // Lowering the cap below the current value stops deposits only.
    set_max_tvl(&mut deps, &env, &admin, 2_000_000).unwrap();
    assert!(matches!(
        deposit(&mut deps, 1_000_000).unwrap_err(),
        ContractError::TvlCapExceeded { room, .. } if room.is_zero()
    ));
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(1_000_000, denom)),
        ExecuteMsg::Redeem {},
    )
    .unwrap();

    // Lifting the cap reopens deposits.
    set_max_tvl(&mut deps, &env, &admin, u128::MAX).unwrap();
    deposit(&mut deps, 1_000_000).unwrap();
}

#[test]
fn tvl_cap_validation() {
    let (mut deps, env, admin) = setup();
    // below min_deposit (1 ATOM)
    assert_eq!(
        set_max_tvl(&mut deps, &env, &admin, 999_999).unwrap_err(),
        ContractError::InvalidMaxTvl {}
    );
    // only the admin may change it
    let user = deps.api.addr_make("user");
    assert_eq!(
        set_max_tvl(&mut deps, &env, &user, 50_000_000).unwrap_err(),
        ContractError::Unauthorized {}
    );

    // instantiate: cap must fit the seed and min_deposit
    let mut deps = mock_dependencies();
    deps.querier
        .staking
        .update(ATOM, &[validator("val1"), validator("val2")], &[]);
    let admin = deps.api.addr_make("admin");
    let mut m = init_msg(&deps, &admin);
    m.max_tvl = Uint128::new(500_000);
    assert_eq!(
        instantiate(deps.as_mut(), mock_env(), message_info(&admin, &coins(20_000_000, ATOM)), m)
            .unwrap_err(),
        ContractError::InvalidMaxTvl {}
    );
    let mut m = init_msg(&deps, &admin);
    m.max_tvl = Uint128::new(5_000_000); // seed is 10 ATOM
    assert!(matches!(
        instantiate(deps.as_mut(), mock_env(), message_info(&admin, &coins(20_000_000, ATOM)), m)
            .unwrap_err(),
        ContractError::TvlCapExceeded { .. }
    ));
}

/// One batch of 100 uatom unbonding, 50 uatom of free rewards on the balance, and the clock at
/// `mature_at`: the chain releases the 100 only at the end of this very block.
fn arrival_window() -> (Deps, Env, Addr) {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    set_delegations(&mut deps, &env, &[("val1", 10_000_100)]);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(100, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &[]),
        ExecuteMsg::SubmitUnbonding {},
    )
    .unwrap();
    set_delegations(&mut deps, &env, &[("val1", 10_000_000)]);
    let mut at = env.clone();
    at.block.time = env.block.time.plus_seconds(1_814_400);
    set_contract_balance(&mut deps, &at, 50, 0);
    (deps, at, alice)
}

#[test]
fn a_deposit_is_priced_like_a_redemption_whatever_arrived() {
    // Review findings (sonnet6): two different prices let a donor profit from users who deposit and
    // then redeem (the deposit paid for a donation, the redemption forfeited it), and let a
    // holder tax depositors by lengthening unbonding_secs. Deposits and redemptions now use the same
    // price. Without free ATOM on the balance it does not depend on whether the unbonded ATOM of a
    // matured batch has already arrived (with a real supply, which an older version of this test lacked).
    let (mut deps, at, alice) = arrival_window();
    let mint_for = |deps: &mut Deps, env: &Env, balance: u128| -> Uint128 {
        // The chain adds attached funds to the balance before the contract runs.
        set_contract_balance(deps, env, balance + 2_000_000, 10_000_000);
        let res = execute(
            deps.as_mut(),
            env.clone(),
            message_info(&alice, &coins(2_000_000, ATOM)),
            ExecuteMsg::Deposit {},
        )
        .unwrap();
        res.messages
            .iter()
            .find_map(|m| match &m.msg {
                CosmosMsg::Custom(TokenFactoryMsg::MintTokens { amount, .. }) => Some(*amount),
                _ => None,
            })
            .unwrap()
    };
    let not_arrived = mint_for(&mut deps, &at, 0);
    let arrived = mint_for(&mut deps, &at, 100); // the batch's 100 uatom reached the contract
    assert_eq!(not_arrived, arrived);
}

#[test]
fn the_quoted_rate_does_not_depend_on_whether_unbonded_atom_has_arrived() {
    // With real supply (haiku6 pointed out that the old version of this test had none): without
    // free ATOM on the balance the redemption price is the same before and after the unbonded ATOM
    // reaches the contract. (A donation never enters it.)
    let (mut deps, at, _alice) = arrival_window();
    set_contract_balance(&mut deps, &at, 0, 10_000_000); // nothing arrived yet
    let before = rate_of(&deps, &at);
    set_contract_balance(&mut deps, &at, 100, 10_000_000); // the batch's 100 uatom arrived
    let after = rate_of(&deps, &at);
    assert_eq!(before, after);
}

fn set_val1_rewards(deps: &mut Deps, env: &Env, uatom: u128) {
    let delegator = env.contract.address.to_string();
    deps.querier.distribution.set_rewards(
        "val1",
        delegator.clone(),
        vec![DecCoin::new(Decimal256::from_atomics(uatom, 0).unwrap(), ATOM)],
    );
    deps.querier.distribution.set_validators(delegator, ["val1"]);
}

#[test]
fn deposit_collects_the_rewards_its_delegation_pays_out() {
    // Delegating makes the chain pay val1's pending rewards to the balance. If they stayed there
    // they could not be told from unbonded ATOM that arrived early, so the deposit collects them.
    let (mut deps, env, _) = setup();
    set_val1_rewards(&mut deps, &env, 1_000_000);
    let user = deps.api.addr_make("user");
    set_contract_balance(&mut deps, &env, 2_000_000, 10_000_000);
    // The deposit goes to the validator that is below its weight: make that val1.
    set_delegations(&mut deps, &env, &[("val1", 4_000_000), ("val2", 6_000_000)]);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&user, &coins(2_000_000, ATOM)),
        ExecuteMsg::Deposit {},
    )
    .unwrap();
    assert_eq!(res.messages.len(), 4, "{:?}", res.messages);
    assert_eq!(
        res.messages[1].msg,
        CosmosMsg::Staking(StakingMsg::Delegate {
            validator: "val1".into(),
            amount: coin(2_000_000, ATOM),
        })
    );
    assert_eq!(
        res.messages[2].msg,
        CosmosMsg::Bank(cosmwasm_std::BankMsg::Send {
            to_address: config_of(&deps, &env).treasury.to_string(),
            amount: coins(100_000, ATOM),
        })
    );
    assert_eq!(
        res.messages[3].msg,
        CosmosMsg::Staking(StakingMsg::Delegate {
            validator: "val1".into(),
            amount: coin(900_000, ATOM),
        })
    );
}

#[test]
fn submit_unbonding_collects_rewards_of_the_undelegated_validator() {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    set_delegations(&mut deps, &env, &[("val1", 10_000_100)]);
    set_val1_rewards(&mut deps, &env, 1_000_000);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(100, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &[]),
        ExecuteMsg::SubmitUnbonding {},
    )
    .unwrap();
    assert!(matches!(
        res.messages[0].msg,
        CosmosMsg::Staking(StakingMsg::Undelegate { .. })
    ));
    assert_eq!(
        res.messages[1].msg,
        CosmosMsg::Bank(cosmwasm_std::BankMsg::Send {
            to_address: config_of(&deps, &env).treasury.to_string(),
            amount: coins(100_000, ATOM),
        })
    );
    assert_eq!(
        res.messages[2].msg,
        CosmosMsg::Staking(StakingMsg::Delegate {
            validator: "val1".into(),
            amount: coin(900_000, ATOM),
        })
    );
}

#[test]
fn unbonded_atom_that_arrives_early_cannot_be_harvested() {
    // Review finding (opus): if the configured unbonding period is longer than the chain's, the
    // unbonded ATOM lands on the balance before `mature_at`. It must stay the redeemers' money:
    // nobody may harvest it as rewards, whatever the chain's real unbonding time is.
    let (mut deps, at, alice) = arrival_window();
    let mut early = at.clone();
    early.block.time = at.block.time.minus_seconds(86_400);
    set_contract_balance(&mut deps, &early, 100, 0); // the 100 uatom of the batch are back
    let rate_before = rate_of(&deps, &early);
    assert_eq!(
        execute(
            deps.as_mut(),
            early.clone(),
            message_info(&alice, &[]),
            ExecuteMsg::Harvest {},
        )
        .unwrap_err(),
        ContractError::NothingToHarvest {}
    );
    assert_eq!(rate_before, Decimal::one());

    // At maturity the batch is paid in full.
    let mut ready = at.clone();
    ready.block.time = at.block.time.plus_seconds(crate::state::FINALIZE_DELAY_SECS);
    finalize(&mut deps, &ready, &alice, 1).unwrap();
    assert_eq!(batch_of(&deps, &ready, 1).atom_received, Some(Uint128::new(100)));
}

#[test]
fn only_the_submit_fee_transfer_may_fail_without_stopping_the_call() {
    // Review finding (sonnet): a treasury that cannot receive (a blocked module account, a typo)
    // made every fee transfer fail and with it SubmitUnbonding, so redeemed liATOM could never be
    // sent to unbonding. Exits must not depend on the treasury: in SubmitUnbonding the fee transfer
    // is a sub-message whose failure is ignored; deposits stay strict.
    use cosmwasm_std::ReplyOn;
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    set_delegations(&mut deps, &env, &[("val1", 10_000_100)]);
    set_val1_rewards(&mut deps, &env, 1_000_000);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(100, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &[]),
        ExecuteMsg::SubmitUnbonding {},
    )
    .unwrap();
    assert!(matches!(res.messages[1].msg, CosmosMsg::Bank(_)));
    assert_eq!(res.messages[1].reply_on, ReplyOn::Error);
    assert_eq!(res.messages[1].id, crate::state::FEE_REPLY_ID);

    // The reply handler accepts the failure and delegates the unsent fee, so it cannot sit
    // on the balance as ownerless ATOM.
    let reply = cosmwasm_std::Reply {
        id: crate::state::FEE_REPLY_ID,
        payload: res.messages[1].payload.clone(),
        gas_used: 0,
        result: cosmwasm_std::SubMsgResult::Err("blocked address".into()),
    };
    let out = crate::contract::reply(deps.as_mut(), env.clone(), reply).unwrap();
    assert_eq!(
        out.messages[0].msg,
        CosmosMsg::Staking(StakingMsg::Delegate {
            validator: "val1".into(),
            amount: coin(100_000, ATOM),
        })
    );
    // An unknown reply id is refused.
    let bogus = cosmwasm_std::Reply {
        id: 99,
        payload: cosmwasm_std::Binary::default(),
        gas_used: 0,
        result: cosmwasm_std::SubMsgResult::Err("x".into()),
    };
    assert!(crate::contract::reply(deps.as_mut(), env.clone(), bogus).is_err());

    // A deposit keeps its fee transfer strict.
    let (mut deps, env, _) = setup();
    set_val1_rewards(&mut deps, &env, 1_000_000);
    let user = deps.api.addr_make("user");
    set_contract_balance(&mut deps, &env, 2_000_000, 10_000_000);
    set_delegations(&mut deps, &env, &[("val1", 4_000_000), ("val2", 6_000_000)]);
    let res = execute(
        deps.as_mut(),
        env,
        message_info(&user, &coins(2_000_000, ATOM)),
        ExecuteMsg::Deposit {},
    )
    .unwrap();
    assert_eq!(res.messages[2].reply_on, ReplyOn::Never);
}

#[test]
fn unbonding_period_cannot_go_below_the_floor() {
    // Review findings (sonnet3, opus6): lowering unbonding_secs to a minute made the contract settle a
    // batch before the chain had paid it and write it off; but a configuration longer than the chain's
    // makes deposits overpay and must be correctable. So it may go down, but only to 3/4 of its
    // value at instantiate (a chain up to a seventh longer is covered by the 7-day grace period).
    let (mut deps, env, admin) = setup();
    let initial = config_of(&deps, &env).unbonding_secs;
    let floor = initial / 8 * 7;
    assert_eq!(config_of(&deps, &env).min_unbonding_secs, floor);
    let below = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        update_config(None, None, Some(floor - 1), None),
    )
    .unwrap_err();
    assert_eq!(below, ContractError::UnbondingBelowFloor { min: floor });
    // the floor itself, and raising, are fine
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        update_config(None, Some(345_600), Some(floor), None),
    )
    .unwrap();
    execute(
        deps.as_mut(),
        env,
        message_info(&admin, &[]),
        update_config(None, Some(400_000), Some(2_000_000), None),
    )
    .unwrap();
}

#[test]
fn a_chain_halt_longer_than_the_grace_period_cannot_write_a_batch_off() {
    // Review finding (opus5): the chain halts across a batch's maturity for more than the grace
    // period. The first block after the halt already counts as past the grace period, but the
    // unbonded ATOM is only released at the end of that block. Writing the batch off there would
    // hand its ATOM to the holders. A shortfall must be seen in an earlier block first.
    let (mut deps, at, alice) = arrival_window();
    let mut first_block = at.clone();
    first_block.block.time = at.block.time.plus_seconds(crate::state::SHORTFALL_GRACE_SECS + 61);
    set_contract_balance(&mut deps, &first_block, 0, 0); // the ATOM has not been released yet
    finalize(&mut deps, &first_block, &alice, 1).unwrap(); // notes the shortfall, pays nothing
    assert_eq!(batch_of(&deps, &first_block, 1).status, BatchStatus::Unbonding);
    assert_eq!(batch_of(&deps, &first_block, 1).shortfall_noted_at, Some(first_block.block.height));

    // The same block cannot settle it either.
    finalize(&mut deps, &first_block, &alice, 1).unwrap();
    assert_eq!(batch_of(&deps, &first_block, 1).status, BatchStatus::Unbonding);

    // The ATOM arrives at the end of the block; the next block pays the batch in full.
    let mut next_block = first_block.clone();
    next_block.block.height += 1;
    next_block.block.time = first_block.block.time.plus_seconds(6);
    set_contract_balance(&mut deps, &next_block, 100, 0);
    finalize(&mut deps, &next_block, &alice, 1).unwrap();
    assert_eq!(batch_of(&deps, &next_block, 1).atom_received, Some(Uint128::new(100)));
}



#[test]
fn the_treasury_cannot_be_the_contract_itself() {
    // Review finding (sonnet5, opus5): with the treasury set to the contract the fee never leaves
    // the balance, where it is counted as rewards again and again.
    let (mut deps, env, admin) = setup();
    let err = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::UpdateConfig {
            treasury: Some(env.contract.address.to_string()),
            fee_bps: None,
            epoch_secs: None,
            unbonding_secs: None,
            min_deposit: None,
            max_tvl: None,
        },
    )
    .unwrap_err();
    assert_eq!(err, ContractError::InvalidTreasury {});
}


#[test]
fn a_donation_never_changes_any_price() {
    // The contract's bank balance is not part of any price, so ATOM sent to it by anyone cannot move
    // the exchange rate, what a deposit mints, what a batch is owed, or what a harvest does. (Reading
    // the balance and telling donations from unbonded ATOM was the source of every pricing bug found
    // in review: hidden donations, double pricing, early and late arrival.)
    let run = |donation: u128| -> (Decimal, u128, u128) {
        let (mut deps, env, _) = setup();
        let alice = deps.api.addr_make("alice");
        let carol = deps.api.addr_make("carol");
        set_delegations(&mut deps, &env, &[("val1", 100_000_000)]);
        set_val1_rewards(&mut deps, &env, 2_000_000);
        set_contract_balance(&mut deps, &env, donation, 100_000_000);
        let rate = rate_of(&deps, &env);
        // a deposit
        let res = execute(
            deps.as_mut(),
            env.clone(),
            message_info(&carol, &coins(5_000_000, ATOM)),
            ExecuteMsg::Deposit {},
        )
        .unwrap();
        let minted = res
            .messages
            .iter()
            .find_map(|m| match &m.msg {
                CosmosMsg::Custom(TokenFactoryMsg::MintTokens { amount, .. }) => Some(amount.u128()),
                _ => None,
            })
            .unwrap();
        // a batch
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&alice, &coins(40_000_000, liatom(&env))),
            ExecuteMsg::Redeem {},
        )
        .unwrap();
        execute(deps.as_mut(), env.clone(), message_info(&alice, &[]), ExecuteMsg::SubmitUnbonding {})
            .unwrap();
        let expected = batch_of(&deps, &env, 1).atom_expected.unwrap().u128();
        (rate, minted, expected)
    };
    let none = run(0);
    assert_eq!(run(1_000_000), none);
    assert_eq!(run(900_000_000_000), none);
}

#[test]
fn messages_that_take_no_funds_refuse_them() {
    let (mut deps, env, _) = setup();
    let who = deps.api.addr_make("who");
    let msgs = [
        ExecuteMsg::Claim { batch_id: 1 },
        ExecuteMsg::Harvest {},
        ExecuteMsg::SubmitUnbonding {},
        ExecuteMsg::FinalizeBatch { batch_id: 1 },
        // the admin messages take no funds either (attached ATOM would become ownerless balance)
        ExecuteMsg::Pause { mode: crate::state::PauseMode::DepositsOnly },
        ExecuteMsg::Unpause {},
        ExecuteMsg::Rebalance { min_move: None },
        ExecuteMsg::UpdateValidators { validators: weights() },
        ExecuteMsg::ProposeAdmin { new_admin: "someone".to_string() },
        ExecuteMsg::AcceptAdmin {},
    ];
    for msg in msgs {
        // from the admin, so that only the funds check can be what refuses them
        let admin = deps.api.addr_make("admin");
        let err = execute(deps.as_mut(), env.clone(), message_info(&admin, &coins(5, ATOM)), msg.clone())
            .unwrap_err();
        assert!(matches!(err, ContractError::UnexpectedFunds {}), "{msg:?}: {err:?}");
        let err = execute(deps.as_mut(), env.clone(), message_info(&who, &coins(5, ATOM)), msg)
            .unwrap_err();
        assert!(matches!(err, ContractError::UnexpectedFunds {}), "{err:?}");
    }
}

#[test]
fn validator_addresses_are_stored_in_the_form_the_chain_uses() {
    // bech32 decoding lowercases an address, so the chain accepts an uppercase one but reports
    // delegations and share denoms in lowercase; the weights must match them as strings.
    let (mut deps, env, admin) = setup();
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::UpdateValidators {
            validators: vec![
                ValidatorWeight { validator: "VAL1".into(), weight_bps: 6000 },
                ValidatorWeight { validator: "Val2".into(), weight_bps: 4000 },
            ],
        },
    )
    .unwrap();
    let stored: Vec<ValidatorWeight> = cosmwasm_std::from_json(
        query(deps.as_ref(), env.clone(), QueryMsg::Validators {}).unwrap(),
    )
    .unwrap();
    assert_eq!(stored[0].validator, "val1");
    assert_eq!(stored[1].validator, "val2");
}

// ---- DepositTokenizedShares (LSM) ----

const SHARE: &str = "val1/7";

fn minted_in(res: &cosmwasm_std::Response<TokenFactoryMsg>) -> Option<(u128, String)> {
    res.messages.iter().find_map(|m| match &m.msg {
        CosmosMsg::Custom(TokenFactoryMsg::MintTokens { amount, mint_to_address, .. }) => {
            Some((amount.u128(), mint_to_address.clone()))
        }
        _ => None,
    })
}

/// Runs `DepositTokenizedShares` and then the reply, as the chain would after the redemption grew the
/// delegation of `val1` by `grew_by`.
fn tokenized_deposit(
    deps: &mut Deps,
    env: &Env,
    who: &Addr,
    share: &str,
    amount: u128,
    grew_by: u128,
    current_val1: u128,
) -> Result<cosmwasm_std::Response<TokenFactoryMsg>, ContractError> {
    // (a failed reply rolls the chain back; the mock keeps its state, so reset it)
    set_delegations(deps, env, &[("val1", current_val1)]);
    let admin = deps.api.addr_make("admin");
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::UpdateValidators {
            validators: vec![ValidatorWeight { validator: "val1".into(), weight_bps: 10_000 }],
        },
    )
    .ok();
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(who, &coins(amount, share)),
        ExecuteMsg::DepositTokenizedShares {},
    )?;
    let first = &res.messages[0];
    assert!(matches!(first.msg, CosmosMsg::Any(_)));
    assert_eq!(first.id, crate::state::LSM_REPLY_ID);
    assert_eq!(first.reply_on, cosmwasm_std::ReplyOn::Success);
    set_delegations(deps, env, &[("val1", current_val1 + grew_by)]);
    crate::contract::reply(
        deps.as_mut(),
        env.clone(),
        cosmwasm_std::Reply {
            id: crate::state::LSM_REPLY_ID,
            payload: first.payload.clone(),
            gas_used: 0,
            result: cosmwasm_std::SubMsgResult::Ok(cosmwasm_std::SubMsgResponse {
                events: vec![],
                #[allow(deprecated)]
                data: None,
                msg_responses: vec![],
            }),
        },
    )
}

#[test]
fn a_tokenized_deposit_mints_exactly_what_a_plain_deposit_of_the_same_amount_would() {
    // Same state, with rewards, so that the rate is not 1: the two ways in must agree to the unit.
    let make = || {
        let (mut deps, env, _) = setup();
        set_delegations(&mut deps, &env, &[("val1", 10_000_000)]);
        set_val1_rewards(&mut deps, &env, 2_000_000);
        (deps, env)
    };
    let (mut plain, env) = make();
    let carol = plain.api.addr_make("carol");
    let res = execute(
        plain.as_mut(),
        env.clone(),
        message_info(&carol, &coins(5_000_000, ATOM)),
        ExecuteMsg::Deposit {},
    )
    .unwrap();
    let (want, _) = minted_in(&res).unwrap();

    let (mut lsm, env) = make();
    let res = tokenized_deposit(&mut lsm, &env, &carol, SHARE, 5_000_000, 5_000_000, 10_000_000).unwrap();
    let (got, to) = minted_in(&res).unwrap();
    assert_eq!(got, want);
    assert_eq!(to, carol.to_string());
}

#[test]
fn a_tokenized_deposit_is_priced_on_what_the_delegation_grew_by_not_on_the_shares_sent() {
    // Tokens per share can be below 1 after a slash: 5 million shares redeem to less.
    let (mut deps, env, _) = setup();
    let carol = deps.api.addr_make("carol");
    let res = tokenized_deposit(&mut deps, &env, &carol, SHARE, 5_000_000, 4_000_000, 10_000_000).unwrap();
    // pool 10 ATOM / supply 10 liATOM: rate 1
    assert_eq!(minted_in(&res).unwrap().0, 4_000_000);
}

#[test]
fn a_tokenized_deposit_collects_the_rewards_the_redemption_pays_out() {
    let (mut deps, env, _) = setup();
    set_val1_rewards(&mut deps, &env, 1_000_000);
    let carol = deps.api.addr_make("carol");
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&carol, &coins(5_000_000, SHARE)),
        ExecuteMsg::DepositTokenizedShares {},
    )
    .unwrap();
    // fee to the treasury and the rest delegated again, as for a plain deposit
    assert!(res.messages.iter().any(|m| matches!(m.msg, CosmosMsg::Bank(BankMsg::Send { .. }))));
    assert!(res
        .messages
        .iter()
        .any(|m| matches!(m.msg, CosmosMsg::Staking(StakingMsg::Delegate { .. }))));
}

#[test]
fn a_tokenized_deposit_refuses_what_it_cannot_price_or_trust() {
    let (mut deps, env, admin) = setup();
    let carol = deps.api.addr_make("carol");
    let go = |deps: &mut Deps, funds: Vec<cosmwasm_std::Coin>| {
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&carol, &funds),
            ExecuteMsg::DepositTokenizedShares {},
        )
        .unwrap_err()
    };
    assert!(matches!(go(&mut deps, vec![]), ContractError::WrongFunds { .. }));
    assert!(matches!(
        go(&mut deps, vec![coin(1, "val1/1"), coin(1, "val1/2")]),
        ContractError::WrongFunds { .. }
    ));
    assert!(matches!(go(&mut deps, coins(5_000_000, ATOM)), ContractError::NotAShareToken { .. }));
    assert!(matches!(
        go(&mut deps, coins(5_000_000, "factory/x/liatom")),
        ContractError::NotAShareToken { .. }
    ));
    // a validator outside the weight list
    assert!(matches!(
        go(&mut deps, coins(5_000_000, "val9/1")),
        ContractError::ValidatorNotInSet { .. }
    ));
    // paused
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::Pause { mode: crate::state::PauseMode::DepositsOnly },
    )
    .unwrap();
    assert!(matches!(go(&mut deps, coins(5_000_000, SHARE)), ContractError::Paused {}));
}

#[test]
fn a_tokenized_deposit_keeps_the_minimum_and_the_tvl_cap() {
    let (mut deps, env, admin) = setup();
    let carol = deps.api.addr_make("carol");
    // below min_deposit (1 ATOM): the redemption grew the delegation by 0.5 ATOM
    assert!(matches!(
        tokenized_deposit(&mut deps, &env, &carol, SHARE, 500_000, 500_000, 10_000_000).unwrap_err(),
        ContractError::BelowMinDeposit { .. }
    ));
    // nothing grew (a share token that redeems to nothing)
    let e = tokenized_deposit(&mut deps, &env, &carol, SHARE, 5_000_000, 0, 10_000_000).unwrap_err();
    assert!(matches!(e, ContractError::BelowMinDeposit { .. }), "{e:?}");
    // the cap: pool is 10 ATOM, cap 12
    set_max_tvl(&mut deps, &env, &admin, 12_000_000).unwrap();
    assert!(matches!(
        tokenized_deposit(&mut deps, &env, &carol, SHARE, 5_000_000, 5_000_000, 10_000_000).unwrap_err(),
        ContractError::TvlCapExceeded { .. }
    ));
    assert!(tokenized_deposit(&mut deps, &env, &carol, SHARE, 2_000_000, 2_000_000, 10_000_000).is_ok());
}

#[test]
fn a_donation_does_not_change_what_a_tokenized_deposit_mints() {
    let run = |donation: u128| {
        let (mut deps, env, _) = setup();
        set_contract_balance(&mut deps, &env, donation, 10_000_000);
        let carol = deps.api.addr_make("carol");
        let res = tokenized_deposit(&mut deps, &env, &carol, SHARE, 5_000_000, 5_000_000, 10_000_000).unwrap();
        minted_in(&res).unwrap().0
    };
    assert_eq!(run(0), run(900_000_000_000));
}

// ---- RedeemToShares (leave as LSM share tokens) ----

fn tokenize_response(denom: &str, amount: u128) -> cosmwasm_std::SubMsgResponse {
    let coin_bytes = {
        let mut c = vec![0x0a, denom.len() as u8];
        c.extend_from_slice(denom.as_bytes());
        let a = amount.to_string();
        c.push(0x12);
        c.push(a.len() as u8);
        c.extend_from_slice(a.as_bytes());
        c
    };
    let mut value = vec![0x0a, coin_bytes.len() as u8];
    value.extend_from_slice(&coin_bytes);
    #[allow(deprecated)]
    cosmwasm_std::SubMsgResponse {
        events: vec![],
        data: None,
        msg_responses: vec![cosmwasm_std::MsgResponse {
            type_url: "/gaia.liquid.v1beta1.MsgTokenizeSharesResponse".to_string(),
            value: cosmwasm_std::Binary::from(value),
        }],
    }
}

fn tokenize_submsgs(
    res: &cosmwasm_std::Response<TokenFactoryMsg>,
) -> Vec<&cosmwasm_std::SubMsg<TokenFactoryMsg>> {
    res.messages.iter().filter(|m| m.id == crate::state::TOKENIZE_REPLY_ID).collect()
}

/// Runs `RedeemToShares` for `liatom_amount` with the given delegations and then the reply of the
/// first tokenization, as the chain would after it took `took` from that delegation and returned
/// `share`.
fn exit_to_shares(
    deps: &mut Deps,
    env: &Env,
    who: &Addr,
    liatom_amount: u128,
    took: u128,
    share: (&str, u128),
) -> Result<cosmwasm_std::Response<TokenFactoryMsg>, ContractError> {
    set_delegations(deps, env, &[("val1", 10_000_000)]);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(who, &coins(liatom_amount, liatom(env))),
        ExecuteMsg::RedeemToShares {},
    )?;
    let sub = tokenize_submsgs(&res)[0];
    assert!(matches!(sub.msg, CosmosMsg::Any(_)));
    assert_eq!(sub.reply_on, cosmwasm_std::ReplyOn::Success);
    set_delegations(deps, env, &[("val1", 10_000_000 - took)]);
    crate::contract::reply(
        deps.as_mut(),
        env.clone(),
        cosmwasm_std::Reply {
            id: crate::state::TOKENIZE_REPLY_ID,
            payload: sub.payload.clone(),
            gas_used: 0,
            result: cosmwasm_std::SubMsgResult::Ok(tokenize_response(share.0, share.1)),
        },
    )
}

#[test]
fn leaving_as_shares_burns_at_the_current_rate_and_sends_the_share_token() {
    let (mut deps, env, _) = setup();
    set_val1_rewards(&mut deps, &env, 2_000_000);
    let alice = deps.api.addr_make("alice");
    let rate = rate_of(&deps, &env);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(4_000_000, liatom(&env))),
        ExecuteMsg::RedeemToShares {},
    )
    .unwrap();
    assert!(res.messages.iter().any(|m| matches!(
        m.msg,
        CosmosMsg::Custom(TokenFactoryMsg::BurnTokens { .. })
    )));
    let want = (Decimal::from_ratio(4_000_000u128, 1u128) * rate).to_uint_floor();
    let subs = tokenize_submsgs(&res);
    assert_eq!(subs.len(), 1);
    let CosmosMsg::Any(any) = &subs[0].msg else { panic!() };
    assert_eq!(any.type_url, crate::lsm::TOKENIZE_TYPE_URL);
    // the amount in the message is the burned liATOM worth, the owner is the caller
    let bytes = any.value.to_vec();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains(&want.to_string()), "{text} must carry {want}");
    assert!(text.contains(alice.as_str()));
    // rewards of the validator are collected (fee to the treasury, rest delegated again), and a
    // treasury that refuses the fee must not stop the exit: that transfer is allowed to fail
    let fee = res
        .messages
        .iter()
        .find(|m| matches!(m.msg, CosmosMsg::Bank(BankMsg::Send { .. })))
        .expect("fee transfer");
    assert_eq!(fee.reply_on, cosmwasm_std::ReplyOn::Error);

    let out = exit_to_shares(&mut deps, &env, &alice, 4_000_000, want.u128(), ("val1/9", want.u128() - 1))
        .unwrap();
    assert_eq!(
        out.messages[0].msg,
        CosmosMsg::Bank(BankMsg::Send {
            to_address: alice.to_string(),
            amount: coins(want.u128() - 1, "val1/9"),
        })
    );
}

#[test]
fn leaving_as_shares_takes_from_every_validator_in_proportion_to_its_stake() {
    // The leaver must not be able to pick a healthy validator and leave the risky one to the holders
    // who stay (the pending-slash dodge found in review): one tokenization per validator, pro rata.
    let (mut deps, env, _) = setup();
    set_delegations(&mut deps, &env, &[("val1", 6_000_000), ("val2", 4_000_000)]);
    let alice = deps.api.addr_make("alice");
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(5_000_000, liatom(&env))),
        ExecuteMsg::RedeemToShares {},
    )
    .unwrap();
    let subs = tokenize_submsgs(&res);
    assert_eq!(subs.len(), 2);
    let amounts: Vec<String> = subs
        .iter()
        .map(|s| match &s.msg {
            CosmosMsg::Any(a) => String::from_utf8_lossy(&a.value.to_vec()).to_string(),
            _ => panic!(),
        })
        .collect();
    // pool 10 ATOM / supply 10 liATOM: 5 liATOM = 5 ATOM, 3 from val1 and 2 from val2
    assert!(amounts[0].contains("val1") && amounts[0].contains("3000000"), "{}", amounts[0]);
    assert!(amounts[1].contains("val2") && amounts[1].contains("2000000"), "{}", amounts[1]);
}

#[test]
fn leaving_as_shares_refuses_what_it_cannot_do() {
    let (mut deps, env, admin) = setup();
    let alice = deps.api.addr_make("alice");
    let run = |deps: &mut Deps, liatom_amount: u128| {
        execute(
            deps.as_mut(),
            env.clone(),
            message_info(&alice, &coins(liatom_amount, liatom(&env))),
            ExecuteMsg::RedeemToShares {},
        )
        .unwrap_err()
    };
    // below the minimum exit (1 ATOM)
    assert!(matches!(run(&mut deps, 500_000), ContractError::ExitTooSmall { .. }));
    // wrong funds
    assert!(execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(4_000_000, ATOM)),
        ExecuteMsg::RedeemToShares {},
    )
    .is_err());
    // full pause blocks it, a deposits-only pause does not
    execute(deps.as_mut(), env.clone(), message_info(&admin, &[]),
        ExecuteMsg::Pause { mode: crate::state::PauseMode::DepositsOnly }).unwrap();
    assert!(exit_to_shares(&mut deps, &env, &alice, 4_000_000, 4_000_000, ("val1/9", 4_000_000)).is_ok());
    execute(deps.as_mut(), env.clone(), message_info(&admin, &[]),
        ExecuteMsg::Pause { mode: crate::state::PauseMode::Full }).unwrap();
    assert!(matches!(run(&mut deps, 4_000_000), ContractError::Paused {}));
}

#[test]
fn leaving_as_shares_checks_the_reply_before_paying_out() {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    // the pool gave up much more than the burned liATOM was worth
    assert!(matches!(
        exit_to_shares(&mut deps, &env, &alice, 4_000_000, 6_000_000, ("val1/9", 4_000_000)).unwrap_err(),
        ContractError::BadTokenization {}
    ));
    // a share token of another validator
    assert!(matches!(
        exit_to_shares(&mut deps, &env, &alice, 4_000_000, 4_000_000, ("val2/9", 4_000_000)).unwrap_err(),
        ContractError::BadTokenization {}
    ));
    // not a share token at all
    assert!(matches!(
        exit_to_shares(&mut deps, &env, &alice, 4_000_000, 4_000_000, ("uatom", 4_000_000)).unwrap_err(),
        ContractError::BadTokenization {}
    ));
}

#[test]
fn a_donation_does_not_change_what_leaving_as_shares_costs() {
    let run = |donation: u128| {
        let (mut deps, env, _) = setup();
        set_contract_balance(&mut deps, &env, donation, 10_000_000);
        let alice = deps.api.addr_make("alice");
        let res = execute(
            deps.as_mut(),
            env.clone(),
            message_info(&alice, &coins(4_000_000, liatom(&env))),
            ExecuteMsg::RedeemToShares {},
        )
        .unwrap();
        match &tokenize_submsgs(&res)[0].msg {
            CosmosMsg::Any(a) => a.value.to_vec(),
            _ => panic!(),
        }
    };
    assert_eq!(run(0), run(900_000_000_000));
}

#[test]
fn a_tokenized_deposit_may_not_leave_its_validator_far_above_its_weight() {
    // The instant LSM paths are a slash-free way to move stake between validators. With weights 60/40 a
    // deposit may leave a validator at most 20% (relative) above its weight, so a deposit and exit loop
    // cannot concentrate the pool on a validator that is about to be slashed.
    let run = |amount: u128| {
        let (mut deps, env, _) = setup();
        let carol = deps.api.addr_make("carol");
        set_delegations(&mut deps, &env, &[("val1", 6_000_000), ("val2", 4_000_000)]);
        let res = execute(
            deps.as_mut(),
            env.clone(),
            message_info(&carol, &coins(amount, SHARE)),
            ExecuteMsg::DepositTokenizedShares {},
        )
        .unwrap();
        set_delegations(&mut deps, &env, &[("val1", 6_000_000 + amount), ("val2", 4_000_000)]);
        crate::contract::reply(
            deps.as_mut(),
            env.clone(),
            cosmwasm_std::Reply {
                id: crate::state::LSM_REPLY_ID,
                payload: res.messages[0].payload.clone(),
                gas_used: 0,
                result: cosmwasm_std::SubMsgResult::Ok(cosmwasm_std::SubMsgResponse {
                    events: vec![],
                    #[allow(deprecated)]
                    data: None,
                    msg_responses: vec![],
                }),
            },
        )
    };
    // 3 ATOM into val1: 9 of 13 ATOM, the cap is 13 * 0.6 * 1.2 = 9.36
    assert!(run(3_000_000).is_ok());
    // 5 ATOM: 11 of 15, the cap is 10.8
    assert!(matches!(run(5_000_000).unwrap_err(), ContractError::ValidatorOverweight { .. }));
}

#[test]
fn leaving_as_shares_folds_dust_parts_into_a_larger_one() {
    // A part of 1 uatom can be turned into 0 by the chain on a slashed validator, which fails the whole exit.
    let (mut deps, env, _) = setup();
    set_delegations(&mut deps, &env, &[("val1", 10_000_000), ("val2", 50)]);
    let alice = deps.api.addr_make("alice");
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(1_000_000, liatom(&env))),
        ExecuteMsg::RedeemToShares {},
    )
    .unwrap();
    // one tokenization only: the part of val2 (5 uatom) is folded into val1
    let subs = tokenize_submsgs(&res);
    assert_eq!(subs.len(), 1);
    let CosmosMsg::Any(a) = &subs[0].msg else { panic!() };
    assert!(String::from_utf8_lossy(&a.value.to_vec()).contains("val1"));
}

// ===== Mutation-kill tests (added by the independent review, commit 62f289b) =====

fn minted_of(res: &cosmwasm_std::Response<TokenFactoryMsg>) -> u128 {
    minted_in(res).expect("a mint message").0
}

#[test]
fn mk_protocol_constants_have_their_documented_values() {
    use crate::state as st;
    assert_eq!(st::MAX_BPS, 10_000);
    assert_eq!(st::MAX_FEE_BPS, 2_000);
    assert_eq!(st::SHORTFALL_GRACE_SECS, 7 * 86_400);
    assert_eq!(st::FINALIZE_DELAY_SECS, 60);
    assert_eq!(st::ROUNDING_TOLERANCE_PER_UNDELEGATE, 10);
    assert_eq!(st::MAX_EPOCH_SECS, 30 * 86_400);
    assert_eq!(st::MAX_UNBONDING_SECS, 42 * 86_400);
    assert_eq!(st::MAX_VALIDATORS, 50);
    assert_eq!(st::LSM_OVERWEIGHT_BAND_BPS, 2_000);
    assert_eq!(st::MIN_TOKENIZE_PART, 10);
    assert_eq!(st::TOKENIZE_REPLY_ID, 3);
    assert_eq!(st::LSM_REPLY_ID, 2);
    assert_eq!(st::FEE_REPLY_ID, 1);
    assert_eq!(st::SUBDENOM, "liatom");
    assert_eq!(crate::rate::MAX_UNBONDING_EPOCH_RATIO, 6);
}

#[test]
fn mk_seven_unbonding_entries_rule_is_a_ratio_of_six() {
    // 7 entries per delegation on the chain: unbonding may be at most 6 epochs long.
    assert!(crate::rate::validate_timing(1_814_400 / 6, 1_814_400).is_ok());
    assert!(crate::rate::validate_timing(1_814_400 / 7, 1_814_400).is_err());
    assert!(crate::rate::validate_timing(1_000_000, 6_000_001).is_err());
    assert!(crate::rate::validate_timing(1_000_000, 3_000_000).is_ok());
}

#[test]
fn mk_fee_of_exactly_twenty_percent_is_allowed() {
    let (mut deps, env, admin) = setup();
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        update_config(Some(2_000), None, None, None),
    )
    .unwrap();
    assert_eq!(config_of(&deps, &env).fee_bps, 2_000);
}

#[test]
fn mk_instantiate_accepts_seed_and_cap_exactly_at_the_minimum() {
    let mut deps = mock_dependencies();
    deps.querier
        .staking
        .update(ATOM, &[validator("val1"), validator("val2")], &[]);
    let admin = deps.api.addr_make("admin");
    let mut m = init_msg(&deps, &admin);
    m.min_deposit = Uint128::new(1_000_000);
    m.max_tvl = Uint128::new(1_000_000);
    // 10 ATOM creation fee plus a 1 ATOM seed: the seed equals min_deposit and max_tvl
    instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&admin, &coins(11_000_000, ATOM)),
        m,
    )
    .unwrap();
}

#[test]
fn mk_instantiate_refuses_the_contract_as_treasury() {
    let mut deps = mock_dependencies();
    deps.querier
        .staking
        .update(ATOM, &[validator("val1"), validator("val2")], &[]);
    let admin = deps.api.addr_make("admin");
    let env = mock_env();
    let mut m = init_msg(&deps, &admin);
    m.treasury = env.contract.address.to_string();
    assert_eq!(
        instantiate(
            deps.as_mut(),
            env,
            message_info(&admin, &coins(20_000_000, ATOM)),
            m
        )
        .unwrap_err(),
        ContractError::InvalidTreasury {}
    );
}

#[test]
fn mk_instantiate_stores_validator_addresses_in_chain_form() {
    let mut deps = mock_dependencies();
    deps.querier
        .staking
        .update(ATOM, &[validator("val1"), validator("val2")], &[]);
    let admin = deps.api.addr_make("admin");
    let mut m = init_msg(&deps, &admin);
    m.validators = vec![
        ValidatorWeight { validator: "VAL1".into(), weight_bps: 6000 },
        ValidatorWeight { validator: "Val2".into(), weight_bps: 4000 },
    ];
    let env = mock_env();
    instantiate(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &coins(20_000_000, ATOM)),
        m,
    )
    .unwrap();
    let stored: Vec<ValidatorWeight> = cosmwasm_std::from_json(
        query(deps.as_ref(), env, QueryMsg::Validators {}).unwrap(),
    )
    .unwrap();
    assert_eq!(stored[0].validator, "val1");
    assert_eq!(stored[1].validator, "val2");
}

#[test]
fn mk_deposit_at_exactly_the_minimum_is_accepted() {
    let (mut deps, env, _) = setup();
    let user = deps.api.addr_make("user");
    set_contract_balance(&mut deps, &env, 1_000_000, 10_000_000);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&user, &coins(1_000_000, ATOM)),
        ExecuteMsg::Deposit {},
    )
    .unwrap();
    assert_eq!(minted_of(&res), 1_000_000);
}

#[test]
fn mk_deposit_is_priced_with_liatom_already_burned_for_exits() {
    // Redeem burns liATOM at once; the ATOM only leave the pool at SubmitUnbonding. Until then the
    // burned liATOM still counts in the supply used for pricing (pending_liatom).
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    let carol = deps.api.addr_make("carol");
    let denom = liatom(&env);
    set_delegations(&mut deps, &env, &[("val1", 10_000_000)]);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(2_000_000, denom)),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    // pool 10 ATOM, supply 10 + pending 2 = 12: 4 ATOM mint 4.8 liATOM
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&carol, &coins(4_000_000, ATOM)),
        ExecuteMsg::Deposit {},
    )
    .unwrap();
    assert_eq!(minted_of(&res), 4_800_000);
}

#[test]
fn mk_tokenized_deposit_of_exactly_the_minimum_is_accepted() {
    let (mut deps, env, _) = setup();
    let carol = deps.api.addr_make("carol");
    let res = tokenized_deposit(&mut deps, &env, &carol, SHARE, 1_000_000, 1_000_000, 10_000_000).unwrap();
    assert_eq!(minted_of(&res), 1_000_000);
}

#[test]
fn mk_tokenized_deposit_may_reach_the_overweight_cap_exactly() {
    // Before: val1 10 ATOM, val2 7 ATOM (weights 60/40). An 8 ATOM deposit into val1 leaves it at
    // 18 of 25 ATOM = 0.6 * 1.2 of the total: exactly the cap, which is allowed.
    let (mut deps, env, _) = setup();
    let carol = deps.api.addr_make("carol");
    set_delegations(&mut deps, &env, &[("val1", 10_000_000), ("val2", 7_000_000)]);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&carol, &coins(8_000_000, SHARE)),
        ExecuteMsg::DepositTokenizedShares {},
    )
    .unwrap();
    set_delegations(&mut deps, &env, &[("val1", 18_000_000), ("val2", 7_000_000)]);
    crate::contract::reply(
        deps.as_mut(),
        env.clone(),
        cosmwasm_std::Reply {
            id: crate::state::LSM_REPLY_ID,
            payload: res.messages[0].payload.clone(),
            gas_used: 0,
            result: cosmwasm_std::SubMsgResult::Ok(cosmwasm_std::SubMsgResponse {
                events: vec![],
                #[allow(deprecated)]
                data: None,
                msg_responses: vec![],
            }),
        },
    )
    .unwrap();
}

#[test]
fn mk_tokenized_deposit_is_refused_just_above_the_overweight_cap() {
    // 6 + 4.288 ATOM into val1 of a 14.288 ATOM pool: cap 0.72 * 14.288 = 10.28736 ATOM, delegation 10.288.
    // The tolerance band is 20% of the weight, so a band of 20.01% must not admit it.
    let (mut deps, env, _) = setup();
    let carol = deps.api.addr_make("carol");
    set_delegations(&mut deps, &env, &[("val1", 6_000_000), ("val2", 4_000_000)]);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&carol, &coins(4_288_000, SHARE)),
        ExecuteMsg::DepositTokenizedShares {},
    )
    .unwrap();
    set_delegations(&mut deps, &env, &[("val1", 10_288_000), ("val2", 4_000_000)]);
    let err = crate::contract::reply(
        deps.as_mut(),
        env.clone(),
        cosmwasm_std::Reply {
            id: crate::state::LSM_REPLY_ID,
            payload: res.messages[0].payload.clone(),
            gas_used: 0,
            result: cosmwasm_std::SubMsgResult::Ok(cosmwasm_std::SubMsgResponse {
                events: vec![],
                #[allow(deprecated)]
                data: None,
                msg_responses: vec![],
            }),
        },
    )
    .unwrap_err();
    assert!(matches!(err, ContractError::ValidatorOverweight { .. }), "{err:?}");
}

#[test]
fn mk_tokenized_deposit_sends_its_fee_without_a_reply() {
    // Strict fee transfer on the deposit path (a treasury that refuses funds must fail the deposit).
    let (mut deps, env, _) = setup();
    set_val1_rewards(&mut deps, &env, 1_000_000);
    let carol = deps.api.addr_make("carol");
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&carol, &coins(5_000_000, SHARE)),
        ExecuteMsg::DepositTokenizedShares {},
    )
    .unwrap();
    let fee = res
        .messages
        .iter()
        .find(|m| matches!(m.msg, CosmosMsg::Bank(BankMsg::Send { .. })))
        .expect("fee transfer");
    assert_eq!(fee.reply_on, cosmwasm_std::ReplyOn::Never);
}

#[test]
fn mk_exit_as_shares_burns_exactly_the_liatom_sent() {
    let (mut deps, env, _) = setup();
    set_val1_rewards(&mut deps, &env, 2_000_000);
    let alice = deps.api.addr_make("alice");
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(4_000_000, liatom(&env))),
        ExecuteMsg::RedeemToShares {},
    )
    .unwrap();
    let burned = res
        .messages
        .iter()
        .find_map(|m| match &m.msg {
            CosmosMsg::Custom(TokenFactoryMsg::BurnTokens { amount, .. }) => Some(amount.u128()),
            _ => None,
        })
        .expect("a burn");
    assert_eq!(burned, 4_000_000);
}

#[test]
fn mk_exit_as_shares_is_priced_with_liatom_already_burned() {
    let (mut deps, env, _) = setup();
    let bob = deps.api.addr_make("bob");
    let alice = deps.api.addr_make("alice");
    set_delegations(&mut deps, &env, &[("val1", 10_000_000)]);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&bob, &coins(2_000_000, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    // pool 10, supply 10 + pending 2: 4 liATOM are worth 4 * 10 / 12 = 3.333333 ATOM
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(4_000_000, liatom(&env))),
        ExecuteMsg::RedeemToShares {},
    )
    .unwrap();
    let subs = tokenize_submsgs(&res);
    let text = match &subs[0].msg {
        CosmosMsg::Any(a) => String::from_utf8_lossy(&a.value.to_vec()).to_string(),
        _ => panic!(),
    };
    assert!(text.contains("3333333"), "{text}");
    assert!(!text.contains("4000000"), "{text}");
}

#[test]
fn mk_exit_as_shares_minimum_is_inclusive() {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    // exactly 1 ATOM (min_deposit) at rate 1
    assert!(exit_to_shares(&mut deps, &env, &alice, 1_000_000, 1_000_000, ("val1/9", 1_000_000)).is_ok());
}

#[test]
fn mk_exit_as_shares_keeps_parts_of_exactly_the_dust_floor() {
    // A part of exactly MIN_TOKENIZE_PART (10 uatom) is a real tokenization, not dust.
    let (mut deps, env, _) = setup();
    // 1 ATOM exit: val2 holds 100 of 10 000 000 uatom, so its part is exactly 10 uatom
    set_delegations(&mut deps, &env, &[("val1", 9_999_900), ("val2", 100)]);
    let alice = deps.api.addr_make("alice");
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(1_000_000, liatom(&env))),
        ExecuteMsg::RedeemToShares {},
    )
    .unwrap();
    assert_eq!(tokenize_submsgs(&res).len(), 2);
}

#[test]
fn mk_exit_as_shares_tolerates_exactly_two_uatom_of_rounding() {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    // took = amount + 2: accepted
    assert!(exit_to_shares(&mut deps, &env, &alice, 4_000_000, 4_000_002, ("val1/9", 4_000_000)).is_ok());
    let (mut deps, env, _) = setup();
    // took = amount + 3: refused
    assert!(matches!(
        exit_to_shares(&mut deps, &env, &alice, 4_000_000, 4_000_003, ("val1/9", 4_000_000)).unwrap_err(),
        ContractError::BadTokenization {}
    ));
}

#[test]
fn mk_exit_as_shares_refuses_a_zero_amount_share() {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    assert!(matches!(
        exit_to_shares(&mut deps, &env, &alice, 4_000_000, 4_000_000, ("val1/9", 0)).unwrap_err(),
        ContractError::BadTokenization {}
    ));
}

#[test]
fn mk_record_ids_of_twenty_digits_are_share_tokens() {
    assert_eq!(
        crate::lsm::share_token_validator("val1/12345678901234567890"),
        Some("val1")
    );
    assert_eq!(crate::lsm::share_token_validator("val1/123456789012345678901"), None);
}

#[test]
fn mk_varint_of_length_128_is_encoded_as_two_bytes() {
    let long = "x".repeat(128);
    let m = crate::lsm::redeem_msg(&long, &cosmwasm_std::coin(1, "v/1"));
    assert_eq!(&m.value[..3], &[0x0a, 0x80, 0x01]);
}

#[test]
fn mk_tokenize_response_refuses_non_numeric_or_partial_coins() {
    use crate::lsm::parse_tokenize_response;
    // coin with a non-numeric amount
    let mut c = vec![0x0a, 3];
    c.extend_from_slice(b"v/1");
    c.extend_from_slice(&[0x12, 1, b'x']);
    let mut resp = vec![0x0a, c.len() as u8];
    resp.extend_from_slice(&c);
    assert_eq!(parse_tokenize_response(&resp), None);
    // coin without a denom
    let c = vec![0x12, 1, b'5'];
    let mut resp = vec![0x0a, c.len() as u8];
    resp.extend_from_slice(&c);
    assert_eq!(parse_tokenize_response(&resp), None);
    // an unknown length-delimited field inside the coin is still a valid coin
    let mut c = vec![0x0a, 3];
    c.extend_from_slice(b"v/1");
    c.extend_from_slice(&[0x12, 1, b'5', 0x1a, 1, b'z']);
    let mut resp = vec![0x0a, c.len() as u8];
    resp.extend_from_slice(&c);
    assert_eq!(parse_tokenize_response(&resp), Some(cosmwasm_std::coin(5, "v/1")));
}

#[test]
fn mk_overlong_varints_are_refused() {
    use crate::lsm::parse_tokenize_response;
    // A valid coin whose length prefix is encoded in six bytes (overlong): refused.
    let mut c = vec![0x0a, 3];
    c.extend_from_slice(b"v/1");
    c.extend_from_slice(&[0x12, 1, b'5']);
    assert_eq!(c.len(), 8);
    let mut resp = vec![0x0a, 0x88, 0x80, 0x80, 0x80, 0x80, 0x00];
    resp.extend_from_slice(&c);
    assert_eq!(parse_tokenize_response(&resp), None);
}

#[test]
fn mk_rebalance_moves_at_least_one_atom_by_default() {
    let (mut deps, env, admin) = setup();
    // 0.5 ATOM off target: below the default 1 ATOM minimum move
    set_delegations(&mut deps, &env, &[("val1", 5_500_000), ("val2", 4_500_000)]);
    assert_eq!(
        execute(
            deps.as_mut(),
            env,
            message_info(&admin, &[]),
            ExecuteMsg::Rebalance { min_move: None }
        )
        .unwrap_err(),
        ContractError::NothingToRebalance {}
    );
}

#[test]
fn mk_rebalance_moves_are_capped_at_eight() {
    let (mut deps, env, admin) = setup();
    // ten validators, 10 % each; all 10 ATOM on the first: nine 1 ATOM moves are needed, eight are made
    let names: Vec<String> = (0..10).map(|i| format!("v{i}")).collect();
    let vals: Vec<Validator> = names.iter().map(|n| validator(n)).collect();
    deps.querier
        .staking
        .update(ATOM, &vals, &[delegation(&env, "v0", 10_000_000)]);
    let weights: Vec<ValidatorWeight> = names
        .iter()
        .map(|n| ValidatorWeight { validator: n.clone(), weight_bps: 1000 })
        .collect();
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::UpdateValidators { validators: weights },
    )
    .unwrap();
    let res = execute(
        deps.as_mut(),
        env,
        message_info(&admin, &[]),
        ExecuteMsg::Rebalance { min_move: None },
    )
    .unwrap();
    let redelegations = res
        .messages
        .iter()
        .filter(|m| matches!(m.msg, CosmosMsg::Staking(StakingMsg::Redelegate { .. })))
        .count();
    assert_eq!(redelegations, 8);
}

#[test]
fn mk_rebalance_collects_each_validators_rewards_once() {
    // val1 is the source of two moves: its rewards must be collected once, not per move.
    let (mut deps, env, admin) = setup();
    let vals = [validator("val1"), validator("val2"), validator("val3")];
    deps.querier.staking.update(ATOM, &vals, &[delegation(&env, "val1", 10_000_000)]);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::UpdateValidators {
            validators: vec![
                ValidatorWeight { validator: "val1".into(), weight_bps: 2000 },
                ValidatorWeight { validator: "val2".into(), weight_bps: 4000 },
                ValidatorWeight { validator: "val3".into(), weight_bps: 4000 },
            ],
        },
    )
    .unwrap();
    set_val1_rewards(&mut deps, &env, 1_000_000);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::Rebalance { min_move: None },
    )
    .unwrap();
    let sends: Vec<u128> = res
        .messages
        .iter()
        .filter_map(|m| match &m.msg {
            CosmosMsg::Bank(BankMsg::Send { amount, .. }) => Some(amount[0].amount.u128()),
            _ => None,
        })
        .collect();
    assert_eq!(sends, vec![100_000]);
}

#[test]
fn mk_rebalance_collects_the_rewards_of_its_destination() {
    let (mut deps, env, admin) = setup();
    set_delegations(&mut deps, &env, &[("val1", 10_000_000)]);
    // rewards on val2 (the destination of the move, which is not yet a delegation)
    let delegator = env.contract.address.to_string();
    deps.querier.distribution.set_rewards(
        "val2",
        delegator.clone(),
        vec![DecCoin::new(Decimal256::from_atomics(1_000_000u128, 0).unwrap(), ATOM)],
    );
    deps.querier.distribution.set_validators(delegator, ["val1", "val2"]);
    let res = execute(
        deps.as_mut(),
        env,
        message_info(&admin, &[]),
        ExecuteMsg::Rebalance { min_move: None },
    )
    .unwrap();
    let fee = res
        .messages
        .iter()
        .find(|m| matches!(m.msg, CosmosMsg::Bank(BankMsg::Send { .. })))
        .expect("the destination's rewards are collected");
    assert!(matches!(&fee.msg, CosmosMsg::Bank(BankMsg::Send { amount, .. }) if amount[0].amount.u128() == 100_000));
    assert_eq!(fee.reply_on, cosmwasm_std::ReplyOn::Never);
}

#[test]
fn mk_pause_unlock_reopens_deposits() {
    let (mut deps, env, admin) = setup();
    let alice = deps.api.addr_make("alice");
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin, &[]),
        ExecuteMsg::Pause { mode: crate::state::PauseMode::DepositsOnly },
    )
    .unwrap();
    execute(deps.as_mut(), env.clone(), message_info(&admin, &[]), ExecuteMsg::Unpause {}).unwrap();
    assert!(execute(
        deps.as_mut(),
        env,
        message_info(&alice, &coins(2_000_000, ATOM)),
        ExecuteMsg::Deposit {},
    )
    .is_ok());
}

#[test]
fn mk_pause_attribute_names_the_mode() {
    use crate::state::PauseMode;
    let (mut deps, env, admin) = setup();
    let res = execute(
        deps.as_mut(),
        env,
        message_info(&admin, &[]),
        ExecuteMsg::Pause { mode: PauseMode::Full },
    )
    .unwrap();
    assert!(res.attributes.iter().any(|a| a.key == "mode" && a.value == "full"));
}

#[test]
fn mk_only_the_admin_can_propose_a_new_admin() {
    // otherwise anyone could nominate themselves and accept
    let (mut deps, env, _) = setup();
    let user = deps.api.addr_make("user");
    assert_eq!(
        execute(
            deps.as_mut(),
            env,
            message_info(&user, &[]),
            ExecuteMsg::ProposeAdmin { new_admin: user.to_string() },
        )
        .unwrap_err(),
        ContractError::Unauthorized {}
    );
}

#[test]
fn mk_update_validators_refuses_an_unknown_validator() {
    let (mut deps, env, admin) = setup();
    assert!(execute(
        deps.as_mut(),
        env,
        message_info(&admin, &[]),
        ExecuteMsg::UpdateValidators {
            validators: vec![
                ValidatorWeight { validator: "val1".into(), weight_bps: 6000 },
                ValidatorWeight { validator: "ghost".into(), weight_bps: 4000 },
            ],
        },
    )
    .is_err());
}

#[test]
fn mk_fee_transfer_of_zero_is_not_emitted_by_harvest() {
    let (mut deps, env, _) = setup();
    let anyone = deps.api.addr_make("anyone");
    // 9 uatom of rewards: 10% rounds down to a fee of 0, so there is nothing to send to the treasury
    set_val1_rewards(&mut deps, &env, 9);
    let res = execute(deps.as_mut(), env, message_info(&anyone, &[]), ExecuteMsg::Harvest {}).unwrap();
    assert!(!res.messages.iter().any(|m| matches!(m.msg, CosmosMsg::Bank(_))));
}

#[test]
fn mk_harvest_withdraws_only_from_validators_that_paid() {
    let (mut deps, env, _) = setup();
    let anyone = deps.api.addr_make("anyone");
    set_delegations(&mut deps, &env, &[("val1", 4_000_000), ("val2", 6_000_000)]);
    set_val1_rewards(&mut deps, &env, 1_000_000);
    // val2 is in the distribution set with no rewards at all
    let delegator = env.contract.address.to_string();
    deps.querier
        .distribution
        .set_validators(delegator.clone(), ["val1", "val2"]);
    deps.querier.distribution.set_rewards("val2", delegator, vec![]);
    let res = execute(deps.as_mut(), env, message_info(&anyone, &[]), ExecuteMsg::Harvest {}).unwrap();
    let withdraws = res
        .messages
        .iter()
        .filter(|m| matches!(m.msg, CosmosMsg::Distribution(DistributionMsg::WithdrawDelegatorReward { .. })))
        .count();
    assert_eq!(withdraws, 1);
}

#[test]
fn mk_claim_rounds_down_on_every_claimant() {
    // 10 uatom received for 3 liATOM: the claims of 1 and 2 liATOM are 3 and 6 uatom, never rounded up.
    let (mut deps, env, _) = setup();
    let a = deps.api.addr_make("a");
    let b = deps.api.addr_make("b");
    let batch = Batch {
        id: 1,
        status: BatchStatus::Matured,
        liatom_burned: Uint128::new(3),
        atom_expected: Some(Uint128::new(10)),
        atom_received: Some(Uint128::new(10)),
        mature_at: Some(env.block.time),
        undelegations: 1,
        shortfall_noted_at: None,
        submitted_at: Some(env.block.time),
    };
    let st = crate::state::BATCHES;
    st.save(deps.as_mut().storage, 1, &batch).unwrap();
    crate::state::OWED_TO_REDEEMERS.save(deps.as_mut().storage, &Uint128::new(10)).unwrap();
    crate::state::CLAIMS.save(deps.as_mut().storage, (1, &a), &Uint128::new(1)).unwrap();
    crate::state::CLAIMS.save(deps.as_mut().storage, (1, &b), &Uint128::new(2)).unwrap();
    let res = execute(deps.as_mut(), env.clone(), message_info(&a, &[]), ExecuteMsg::Claim { batch_id: 1 }).unwrap();
    assert_eq!(
        res.messages[0].msg,
        CosmosMsg::Bank(BankMsg::Send { to_address: a.to_string(), amount: coins(3, ATOM) })
    );
    let res = execute(deps.as_mut(), env, message_info(&b, &[]), ExecuteMsg::Claim { batch_id: 1 }).unwrap();
    assert_eq!(
        res.messages[0].msg,
        CosmosMsg::Bank(BankMsg::Send { to_address: b.to_string(), amount: coins(6, ATOM) })
    );
}

#[test]
fn mk_claim_reduces_what_the_batches_owe() {
    // After a claim, the balance still owed to other redeemers must shrink by the payout, or the next
    // finalization would deduct too much and underpay them.
    let (mut deps, env, _) = setup();
    let a = deps.api.addr_make("a");
    let batch = Batch {
        id: 1,
        status: BatchStatus::Matured,
        liatom_burned: Uint128::new(100),
        atom_expected: Some(Uint128::new(100)),
        atom_received: Some(Uint128::new(100)),
        mature_at: Some(env.block.time),
        undelegations: 1,
        shortfall_noted_at: None,
        submitted_at: Some(env.block.time),
    };
    crate::state::BATCHES.save(deps.as_mut().storage, 1, &batch).unwrap();
    crate::state::OWED_TO_REDEEMERS.save(deps.as_mut().storage, &Uint128::new(100)).unwrap();
    crate::state::CLAIMS.save(deps.as_mut().storage, (1, &a), &Uint128::new(100)).unwrap();
    execute(deps.as_mut(), env.clone(), message_info(&a, &[]), ExecuteMsg::Claim { batch_id: 1 }).unwrap();
    assert_eq!(
        crate::state::OWED_TO_REDEEMERS.load(deps.as_ref().storage).unwrap(),
        Uint128::zero()
    );
}

#[test]
fn mk_settled_batches_leave_the_unbonding_list() {
    // Both matured batches are settled by one call; they must then leave UNBONDING, otherwise the
    // Pool query would count their 200 uatom as owed twice (once as finalized, once as matured).
    let (mut deps, later, alice) = two_matured_batches();
    set_contract_balance(&mut deps, &later, 200, 10_000_000);
    finalize(&mut deps, &later, &alice, 1).unwrap();
    let pool: crate::msg::PoolResponse = cosmwasm_std::from_json(
        query(deps.as_ref(), later.clone(), QueryMsg::Pool {}).unwrap(),
    )
    .unwrap();
    assert_eq!(pool.owed_to_redeemers, Uint128::new(200));
}

#[test]
fn mk_pool_query_reports_its_fields() {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    set_delegations(&mut deps, &env, &[("val1", 10_000_000)]);
    set_val1_rewards(&mut deps, &env, 1_000_000);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(2_000_000, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    let pool: crate::msg::PoolResponse = cosmwasm_std::from_json(
        query(deps.as_ref(), env, QueryMsg::Pool {}).unwrap(),
    )
    .unwrap();
    assert_eq!(pool.delegated, Uint128::new(10_000_000));
    assert_eq!(pool.pool_value, Uint128::new(10_900_000));
    assert_eq!(pool.pending_liatom, Uint128::new(2_000_000));
    assert_eq!(pool.liatom_supply, Uint128::new(10_000_000));
    assert_eq!(pool.owed_to_redeemers, Uint128::zero());
}

#[test]
fn mk_rewards_in_other_denoms_do_not_count() {
    let (mut deps, env, _) = setup();
    let delegator = env.contract.address.to_string();
    deps.querier.distribution.set_rewards(
        "val1",
        delegator.clone(),
        vec![DecCoin::new(Decimal256::from_atomics(1_000_000u128, 0).unwrap(), "ibc/ABC")],
    );
    deps.querier.distribution.set_validators(delegator, ["val1"]);
    let rate = rate_of(&deps, &env);
    assert_eq!(rate, Decimal::one());
}

#[test]
fn mk_sub_uatom_rewards_are_floored() {
    let (mut deps, env, _) = setup();
    let delegator = env.contract.address.to_string();
    // 1.5 uatom of rewards on the chain are paid as 1 uatom
    deps.querier.distribution.set_rewards(
        "val1",
        delegator.clone(),
        vec![DecCoin::new(Decimal256::from_atomics(1_500_000_000_000_000_000u128, 18).unwrap(), ATOM)],
    );
    deps.querier.distribution.set_validators(delegator, ["val1"]);
    let res: crate::msg::PoolResponse = cosmwasm_std::from_json(
        query(deps.as_ref(), env, QueryMsg::Pool {}).unwrap(),
    )
    .unwrap();
    assert_eq!(res.unclaimed_rewards, Uint128::new(1));
}

#[test]
fn mk_deposit_with_no_rewards_sends_no_zero_messages() {
    // With nothing to collect, a deposit must not emit a zero-amount delegation or fee transfer.
    let (mut deps, env, _) = setup();
    let user = deps.api.addr_make("user");
    set_contract_balance(&mut deps, &env, 2_000_000, 10_000_000);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&user, &coins(2_000_000, ATOM)),
        ExecuteMsg::Deposit {},
    )
    .unwrap();
    assert_eq!(res.messages.len(), 2, "{:?}", res.messages);
    assert!(!res.messages.iter().any(|m| matches!(
        &m.msg,
        CosmosMsg::Staking(StakingMsg::Delegate { amount, .. }) if amount.amount.is_zero()
    )));
}

#[test]
fn an_exit_re_delegates_to_a_validator_it_holds_stake_at_not_to_an_untouched_weight() {
    // opus12 M1: the weight list names a validator that has vanished from the chain (an unbonded validator
    // with no shares is deleted). The exit used to re-delegate the swept rewards to that untouched weight,
    // the Delegate failed and the whole SubmitUnbonding reverted while Redeem kept burning into a batch
    // that could never be sent. The target must be a validator the contract holds stake at.
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    // the stake is at "gone", which is not in the weight list; the list names only "val1", which
    // here stands for the validator that no longer exists on the chain
    set_delegations(&mut deps, &env, &[("gone", 10_000_100)]);
    let delegator = env.contract.address.to_string();
    deps.querier.distribution.set_rewards(
        "gone",
        delegator.clone(),
        vec![DecCoin::new(Decimal256::from_atomics(1_000_000u128, 0).unwrap(), ATOM)],
    );
    deps.querier.distribution.set_validators(delegator, ["gone"]);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(100, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    let res = execute(
        deps.as_mut(),
        env,
        message_info(&alice, &[]),
        ExecuteMsg::SubmitUnbonding {},
    )
    .unwrap();
    let delegates: Vec<String> = res
        .messages
        .iter()
        .filter_map(|m| match &m.msg {
            CosmosMsg::Staking(StakingMsg::Delegate { validator, .. }) => Some(validator.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(delegates, vec!["gone".to_string()], "{:?}", res.messages);
    let sends = res
        .messages
        .iter()
        .filter(|m| matches!(m.msg, CosmosMsg::Bank(BankMsg::Send { .. })))
        .count();
    assert_eq!(sends, 1, "one fee transfer: the target was touched, its rewards are already paid");
}

#[test]
fn the_validator_list_cannot_push_the_delegations_over_the_limit() {
    // opus12 M2: MAX_VALIDATORS bounded the weight list but not the validators the contract holds stake at
    // (removed ones stay until drained), so rotating validators grew every exit without bound.
    let (mut deps, env, admin) = setup();
    let all: Vec<Validator> = (0..60).map(|i| validator(&format!("v{i}"))).collect();
    // the contract holds stake at v0 and v1 (not in the new list)
    deps.querier.staking.update(
        ATOM,
        &all,
        &[delegation(&env, "v0", 5_000_000), delegation(&env, "v1", 5_000_000)],
    );
    let list = |from: usize, n: usize| ExecuteMsg::UpdateValidators {
        validators: (from..from + n)
            .map(|i| ValidatorWeight { validator: format!("v{i}"), weight_bps: (10_000 / n) as u16 })
            .collect(),
    };
    // 48 new names + the 2 held = 50: allowed (10000 is divisible by 50)
    // use 50 names that include none of the held ones would make 52
    let err = execute(deps.as_mut(), env.clone(), message_info(&admin, &[]), list(2, 50)).unwrap_err();
    assert!(matches!(err, ContractError::TooManyValidators { would_be: 52, max: 50 }), "{err:?}");
    // 40 new names + 2 held = 42: allowed
    assert!(execute(deps.as_mut(), env.clone(), message_info(&admin, &[]), list(2, 40)).is_ok());
}

#[test]
fn a_validator_that_keeps_most_of_the_rewards_cannot_join_the_set() {
    // opus12 L1: the 20% fee cap means nothing if the set can point at a validator that takes most rewards.
    let (mut deps, env, admin) = setup();
    deps.querier.staking.update(
        ATOM,
        &[
            validator("val1"),
            Validator::create(
                "greedy".to_string(),
                Decimal::percent(30),
                Decimal::percent(100),
                Decimal::percent(1),
            ),
            Validator::create(
                "fair".to_string(),
                Decimal::percent(25),
                Decimal::percent(100),
                Decimal::percent(1),
            ),
        ],
        &[delegation(&env, "val1", 10_000_000)],
    );
    let one = |v: &str| ExecuteMsg::UpdateValidators {
        validators: vec![ValidatorWeight { validator: v.to_string(), weight_bps: 10_000 }],
    };
    let err = execute(deps.as_mut(), env.clone(), message_info(&admin, &[]), one("greedy")).unwrap_err();
    assert!(matches!(err, ContractError::CommissionTooHigh { .. }), "{err:?}");
    // exactly the limit is allowed
    assert!(execute(deps.as_mut(), env.clone(), message_info(&admin, &[]), one("fair")).is_ok());
}

#[test]
fn mk_no_second_collection_for_a_validator_already_swept() {
    // Deposit to val1 with rewards on val1: the sweep is one fee transfer and one delegation.
    let (mut deps, env, _) = setup();
    set_val1_rewards(&mut deps, &env, 1_000_000);
    set_contract_balance(&mut deps, &env, 2_000_000, 10_000_000);
    set_delegations(&mut deps, &env, &[("val1", 4_000_000), ("val2", 6_000_000)]);
    let user = deps.api.addr_make("user");
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&user, &coins(2_000_000, ATOM)),
        ExecuteMsg::Deposit {},
    )
    .unwrap();
    let sends = res
        .messages
        .iter()
        .filter(|m| matches!(m.msg, CosmosMsg::Bank(BankMsg::Send { .. })))
        .count();
    assert_eq!(sends, 1);
}

#[test]
fn mk_submit_unbonding_atom_expected_includes_pending_rewards() {
    // 100 uatom burned while 1 ATOM of rewards (0.9 ATOM net) is still unclaimed
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    set_delegations(&mut deps, &env, &[("val1", 10_000_100)]);
    set_val1_rewards(&mut deps, &env, 1_000_000);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(100, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    execute(deps.as_mut(), env.clone(), message_info(&alice, &[]), ExecuteMsg::SubmitUnbonding {})
        .unwrap();
    // pool = 10_000_100 delegated + 900_000 net rewards; supply 10_000_000 + 100 pending = 10_000_100:
    // 100 * 10_900_100 / 10_000_100 = 108 (rounded down)
    assert_eq!(batch_of(&deps, &env, 1).atom_expected, Some(Uint128::new(108)));
}

#[test]
fn mk_submit_unbonding_epoch_boundary_is_inclusive() {
    // Exactly epoch_secs after the previous submit is allowed; one second earlier is refused.
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    set_delegations(&mut deps, &env, &[("val1", 10_000_100)]);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(100, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    execute(deps.as_mut(), env.clone(), message_info(&alice, &[]), ExecuteMsg::SubmitUnbonding {})
        .unwrap();
    set_delegations(&mut deps, &env, &[("val1", 10_000_000)]);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(100, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    let mut at_epoch = env.clone();
    at_epoch.block.time = env.block.time.plus_seconds(345_600 - 1);
    assert!(matches!(
        execute(deps.as_mut(), at_epoch, message_info(&alice, &[]), ExecuteMsg::SubmitUnbonding {})
            .unwrap_err(),
        ContractError::EpochNotElapsed { remaining_secs: 1 }
    ));
    let mut exact = env.clone();
    exact.block.time = env.block.time.plus_seconds(345_600);
    assert!(execute(deps.as_mut(), exact, message_info(&alice, &[]), ExecuteMsg::SubmitUnbonding {}).is_ok());
}

#[test]
fn mk_finalize_is_refused_exactly_one_second_before_the_delay() {
    let (mut deps, at, alice) = arrival_window();
    let mut early = at.clone();
    early.block.time = at.block.time.plus_seconds(crate::state::FINALIZE_DELAY_SECS - 1);
    assert!(matches!(
        finalize(&mut deps, &early, &alice, 1).unwrap_err(),
        ContractError::NotMatured { .. }
    ));
}

#[test]
fn mk_grace_period_is_inclusive_at_its_end() {
    // Exactly SHORTFALL_GRACE_SECS after maturity the shortfall is noted (Ok), not refused as "not yet".
    let (mut deps, at, alice) = arrival_window();
    set_contract_balance(&mut deps, &at, 0, 0);
    let mut ready = at.clone();
    ready.block.time = at.block.time.plus_seconds(crate::state::SHORTFALL_GRACE_SECS);
    finalize(&mut deps, &ready, &alice, 1).unwrap();
    assert_eq!(batch_of(&deps, &ready, 1).status, BatchStatus::Unbonding);
    assert!(batch_of(&deps, &ready, 1).shortfall_noted_at.is_some());
}

#[test]
fn mk_deficit_equal_to_the_rounding_tolerance_is_rounding() {
    // One undelegation: a deficit of exactly 10 uatom is chain rounding and paid at once.
    let (mut deps, at, alice) = arrival_window();
    let mut ready = at.clone();
    ready.block.time = at.block.time.plus_seconds(crate::state::FINALIZE_DELAY_SECS);
    set_contract_balance(&mut deps, &ready, 90, 0);
    finalize(&mut deps, &ready, &alice, 1).unwrap();
    assert_eq!(batch_of(&deps, &ready, 1).atom_received, Some(Uint128::new(90)));
}

#[test]
fn mk_each_undelegation_adds_its_own_tolerance() {
    // Two undelegations (two validators): a deficit of 15 uatom is within 2 x 10 and is rounding.
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    let denom = liatom(&env);
    set_delegations(&mut deps, &env, &[("val1", 6_000_050), ("val2", 4_000_050)]);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(100, denom)),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    execute(deps.as_mut(), env.clone(), message_info(&alice, &[]), ExecuteMsg::SubmitUnbonding {})
        .unwrap();
    assert_eq!(batch_of(&deps, &env, 1).undelegations, 2);
    let mut ready = env.clone();
    ready.block.time = env.block.time.plus_seconds(1_814_400 + crate::state::FINALIZE_DELAY_SECS);
    set_contract_balance(&mut deps, &ready, 85, 10_000_000);
    finalize(&mut deps, &ready, &alice, 1).unwrap();
    assert_eq!(batch_of(&deps, &ready, 1).atom_received, Some(Uint128::new(85)));
}

#[test]
fn mk_batch_records_when_it_was_submitted() {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    set_delegations(&mut deps, &env, &[("val1", 10_000_100)]);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(100, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    execute(deps.as_mut(), env.clone(), message_info(&alice, &[]), ExecuteMsg::SubmitUnbonding {})
        .unwrap();
    assert_eq!(batch_of(&deps, &env, 1).submitted_at, Some(env.block.time));
}

#[test]
fn mk_error_messages_name_their_numbers() {
    let e = ContractError::ExitTooSmall { amount: Uint128::new(5), min: Uint128::new(7) };
    assert_eq!(e.to_string(), "5 uatom is below the minimum exit 7");
    let e = ContractError::UnbondingBelowFloor { min: 42 };
    assert!(e.to_string().contains("cannot go below 42"));
    let e = ContractError::EpochNotElapsed { remaining_secs: 9 };
    assert!(e.to_string().contains("9 s left"));
    let e = ContractError::NotReceivedYet {
        batch_id: 4,
        free: Uint128::new(1),
        expected: Uint128::new(2),
        remaining_secs: 3,
    };
    assert_eq!(
        e.to_string(),
        "batch 4: only 1 of 2 uatom are on the balance, the rest may still be in transit, retry in 3 s"
    );
}

#[test]
fn mk_instantiate_attribute_names_are_stable() {
    let mut deps = mock_dependencies();
    deps.querier
        .staking
        .update(ATOM, &[validator("val1"), validator("val2")], &[]);
    let admin = deps.api.addr_make("admin");
    let msg = init_msg(&deps, &admin);
    let res = instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&admin, &coins(20_000_000, ATOM)),
        msg,
    )
    .unwrap();
    assert!(res.attributes.iter().any(|a| a.key == "action" && a.value == "instantiate"));
}


#[test]
fn mk_dust_is_folded_into_a_validator_with_exactly_enough_room() {
    // val2 holds 5 uatom: its 4 uatom part is dust and is folded into val1, which has exactly 4 uatom of room.
    let (mut deps, env, _) = setup();
    set_delegations(&mut deps, &env, &[("val1", 9_999_995), ("val2", 5)]);
    let alice = deps.api.addr_make("alice");
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(9_999_995, liatom(&env))),
        ExecuteMsg::RedeemToShares {},
    )
    .unwrap();
    let subs = tokenize_submsgs(&res);
    assert_eq!(subs.len(), 1);
    let text = match &subs[0].msg {
        CosmosMsg::Any(a) => String::from_utf8_lossy(&a.value.to_vec()).to_string(),
        _ => panic!(),
    };
    assert!(text.contains("9999995"), "{text}");
}

#[test]
fn mk_fee_reply_with_nothing_unsent_delegates_nothing() {
    let (mut deps, env, _) = setup();
    let reply = cosmwasm_std::Reply {
        id: crate::state::FEE_REPLY_ID,
        payload: cosmwasm_std::Binary::from(br#"{"amount":"0","validator":"val1"}"#.to_vec()),
        gas_used: 0,
        result: cosmwasm_std::SubMsgResult::Err("blocked".into()),
    };
    let out = crate::contract::reply(deps.as_mut(), env, reply).unwrap();
    assert!(out.messages.is_empty());
}

#[test]
fn mk_tokenized_deposit_refuses_a_zero_amount_share() {
    let (mut deps, env, _) = setup();
    let carol = deps.api.addr_make("carol");
    let err = execute(
        deps.as_mut(),
        env,
        message_info(&carol, &coins(0, SHARE)),
        ExecuteMsg::DepositTokenizedShares {},
    )
    .unwrap_err();
    assert!(matches!(err, ContractError::WrongFunds { .. }), "{err:?}");
}

#[test]
fn mk_tokenized_deposit_cap_counts_the_pool_rewards() {
    // Delegated 10 ATOM (6 on val1) plus 0.9 ATOM of net rewards. A 4.288 ATOM deposit into val1 leaves it at
    // 10.288 of 14.288 ATOM delegated: over the 0.72 cap (10.287). The rewards are not part of that base.
    let (mut deps, env, _) = setup();
    let carol = deps.api.addr_make("carol");
    set_delegations(&mut deps, &env, &[("val1", 6_000_000), ("val2", 4_000_000)]);
    set_val1_rewards(&mut deps, &env, 1_000_000);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&carol, &coins(4_288_000, SHARE)),
        ExecuteMsg::DepositTokenizedShares {},
    )
    .unwrap();
    set_delegations(&mut deps, &env, &[("val1", 10_288_000), ("val2", 4_000_000)]);
    let err = crate::contract::reply(
        deps.as_mut(),
        env.clone(),
        cosmwasm_std::Reply {
            id: crate::state::LSM_REPLY_ID,
            payload: res.messages[0].payload.clone(),
            gas_used: 0,
            result: cosmwasm_std::SubMsgResult::Ok(cosmwasm_std::SubMsgResponse {
                events: vec![],
                #[allow(deprecated)]
                data: None,
                msg_responses: vec![],
            }),
        },
    )
    .unwrap_err();
    assert!(matches!(err, ContractError::ValidatorOverweight { .. }), "{err:?}");
}

#[test]
fn mk_tokenized_deposit_is_priced_with_liatom_already_burned() {
    let (mut deps, env, _) = setup();
    let bob = deps.api.addr_make("bob");
    let carol = deps.api.addr_make("carol");
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&bob, &coins(2_000_000, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    // pool 10 ATOM, supply 10 + pending 2: 4 ATOM mint 4.8 liATOM
    let res = tokenized_deposit(&mut deps, &env, &carol, SHARE, 4_000_000, 4_000_000, 10_000_000).unwrap();
    assert_eq!(minted_in(&res).unwrap().0, 4_800_000);
}

#[test]
fn mk_shortfall_reports_the_time_until_the_next_grace_period() {
    // now is batch 1's grace deadline; batch 2 (matured 345 610 s later) has its own deadline 345 610 s ahead.
    let (mut deps, later, alice) = two_matured_batches();
    let mut at = later.clone();
    at.block.time = later.block.time.minus_seconds(345_610 + 1);
    set_contract_balance(&mut deps, &at, 0, 10_000_000);
    let err = finalize(&mut deps, &at, &alice, 2).unwrap_err();
    assert!(
        matches!(err, ContractError::NotReceivedYet { remaining_secs: 345_610, .. }),
        "{err:?}"
    );
}

#[test]
fn mk_first_sighting_of_a_shortfall_is_reported_in_the_response() {
    let (mut deps, at, alice) = arrival_window();
    let mut first_block = at.clone();
    first_block.block.time = at.block.time.plus_seconds(crate::state::SHORTFALL_GRACE_SECS + 61);
    set_contract_balance(&mut deps, &first_block, 0, 0);
    let res = execute(
        deps.as_mut(),
        first_block,
        message_info(&alice, &[]),
        ExecuteMsg::FinalizeBatch { batch_id: 1 },
    )
    .unwrap();
    assert!(res.attributes.iter().any(|a| a.key == "shortfall_noted" && a.value == "1"));
}

#[test]
fn mk_unfinalized_matured_atom_is_owed_from_the_moment_it_matures() {
    // Exactly at maturity the 100 uatom of the batch are owed (not yet finalized) and show in the Pool query.
    let (deps, at, _alice) = arrival_window();
    let pool: crate::msg::PoolResponse = cosmwasm_std::from_json(
        query(deps.as_ref(), at, QueryMsg::Pool {}).unwrap(),
    )
    .unwrap();
    assert_eq!(pool.owed_to_redeemers, Uint128::new(100));
}

#[test]
fn mk_a_claim_that_pays_nothing_sends_nothing() {
    let (mut deps, env, _) = setup();
    let a = deps.api.addr_make("a");
    let batch = Batch {
        id: 1,
        status: BatchStatus::Matured,
        liatom_burned: Uint128::new(1_000),
        atom_expected: Some(Uint128::new(10)),
        atom_received: Some(Uint128::new(10)),
        mature_at: Some(env.block.time),
        undelegations: 1,
        shortfall_noted_at: None,
        submitted_at: Some(env.block.time),
    };
    crate::state::BATCHES.save(deps.as_mut().storage, 1, &batch).unwrap();
    crate::state::OWED_TO_REDEEMERS.save(deps.as_mut().storage, &Uint128::new(10)).unwrap();
    crate::state::CLAIMS.save(deps.as_mut().storage, (1, &a), &Uint128::new(1)).unwrap();
    // 10 * 1 / 1000 rounds down to 0
    let res = execute(deps.as_mut(), env, message_info(&a, &[]), ExecuteMsg::Claim { batch_id: 1 }).unwrap();
    assert!(res.messages.is_empty());
}

#[test]
fn mk_update_config_max_tvl_may_equal_min_deposit() {
    let (mut deps, env, admin) = setup();
    set_max_tvl(&mut deps, &env, &admin, 1_000_000).unwrap();
    assert_eq!(config_of(&deps, &env).max_tvl, Uint128::new(1_000_000));
}

#[test]
fn mk_instantiate_accepts_the_maximum_fee() {
    let mut deps = mock_dependencies();
    deps.querier
        .staking
        .update(ATOM, &[validator("val1"), validator("val2")], &[]);
    let admin = deps.api.addr_make("admin");
    let mut m = init_msg(&deps, &admin);
    m.fee_bps = 2_000;
    instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&admin, &coins(20_000_000, ATOM)),
        m,
    )
    .unwrap();
}

#[test]
fn mk_atom_for_liatom_without_supply_is_zero() {
    assert_eq!(
        crate::rate::atom_for_liatom(Uint128::new(100), Uint128::new(100), Uint128::zero()),
        Uint128::zero()
    );
}

#[test]
fn mk_split_by_stake_of_nothing_is_empty() {
    assert_eq!(
        crate::rate::split_by_stake(Uint128::zero(), &[("a".to_string(), Uint128::zero())]),
        Some(vec![])
    );
}

#[test]
fn mk_split_by_stake_gives_the_rounding_unit_to_the_largest_stake() {
    let stakes = [
        ("a".to_string(), Uint128::new(3)),
        ("b".to_string(), Uint128::new(3)),
        ("c".to_string(), Uint128::new(4)),
    ];
    let parts = crate::rate::split_by_stake(Uint128::new(5), &stakes).unwrap();
    assert_eq!(
        parts,
        vec![
            ("a".to_string(), Uint128::new(1)),
            ("b".to_string(), Uint128::new(1)),
            ("c".to_string(), Uint128::new(3)),
        ]
    );
}

#[test]
fn mk_split_by_stake_never_hands_out_more_than_a_stake() {
    // floors are 0 + 0 + 0 + ... with a remainder of 2: each validator may take at most its own stake
    let stakes = [
        ("a".to_string(), Uint128::new(2)),
        ("b".to_string(), Uint128::new(2)),
        ("c".to_string(), Uint128::new(2)),
    ];
    let parts = crate::rate::split_by_stake(Uint128::new(5), &stakes).unwrap();
    assert_eq!(
        parts,
        vec![
            ("a".to_string(), Uint128::new(2)),
            ("b".to_string(), Uint128::new(2)),
            ("c".to_string(), Uint128::new(1)),
        ]
    );
}

#[test]
fn mk_split_by_stake_drops_zero_parts() {
    let stakes = [("a".to_string(), Uint128::new(1)), ("b".to_string(), Uint128::new(99))];
    let parts = crate::rate::split_by_stake(Uint128::new(1), &stakes).unwrap();
    assert_eq!(parts, vec![("b".to_string(), Uint128::new(1))]);
}

#[test]
fn mk_plan_rebalance_moves_exactly_the_minimum() {
    let ws = [
        ValidatorWeight { validator: "a".into(), weight_bps: 5000 },
        ValidatorWeight { validator: "b".into(), weight_bps: 5000 },
    ];
    let del = [("a".to_string(), Uint128::new(1_000_100)), ("b".to_string(), Uint128::new(999_900))];
    let plan = crate::rate::plan_rebalance(&ws, &del, Uint128::new(100), 8);
    assert_eq!(plan, vec![("a".to_string(), "b".to_string(), Uint128::new(100))]);
}

#[test]
fn mk_plan_rebalance_starts_with_the_largest_surplus_and_deficit() {
    let ws: Vec<ValidatorWeight> = ["x", "y", "p", "q"]
        .iter()
        .map(|v| ValidatorWeight { validator: v.to_string(), weight_bps: 2500 })
        .collect();
    // targets 1 000 000 each: x +300, y +100, p -50, q -350
    let del = [
        ("x".to_string(), Uint128::new(1_000_300)),
        ("y".to_string(), Uint128::new(1_000_100)),
        ("p".to_string(), Uint128::new(999_950)),
        ("q".to_string(), Uint128::new(999_650)),
    ];
    let plan = crate::rate::plan_rebalance(&ws, &del, Uint128::one(), 1);
    assert_eq!(plan, vec![("x".to_string(), "q".to_string(), Uint128::new(300))]);
}

#[test]
fn mk_plan_rebalance_gives_rounding_dust_to_the_heaviest_validator() {
    // 1 000 001 uatom at 50/50: the odd uatom goes to "a" (the name that sorts first among equals),
    // so "a" is the destination of the one-uatom move.
    let ws = [
        ValidatorWeight { validator: "a".into(), weight_bps: 5000 },
        ValidatorWeight { validator: "b".into(), weight_bps: 5000 },
    ];
    let del = [("a".to_string(), Uint128::new(500_000)), ("b".to_string(), Uint128::new(500_001))];
    let plan = crate::rate::plan_rebalance(&ws, &del, Uint128::one(), 8);
    assert_eq!(plan, vec![("b".to_string(), "a".to_string(), Uint128::one())]);
}

#[test]
fn mk_sweep_target_is_the_most_underweight_touched_validator() {
    // Undelegating from val1 (60% weight, now 9 ATOM) and val2 (40%, 1 ATOM): the fee is reinvested in val2,
    // the validator that is below its weight after the undelegation, not in the first validator.
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    set_delegations(&mut deps, &env, &[("val1", 9_000_000), ("val2", 1_000_100)]);
    set_val1_rewards(&mut deps, &env, 1_000_000);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(100, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap();
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &[]),
        ExecuteMsg::SubmitUnbonding {},
    )
    .unwrap();
    let target = res
        .messages
        .iter()
        .find_map(|m| match &m.msg {
            CosmosMsg::Staking(StakingMsg::Delegate { validator, .. }) => Some(validator.clone()),
            _ => None,
        })
        .expect("the reinvested part is delegated");
    assert_eq!(target, "val2");
}

#[test]
fn mk_a_zero_amount_of_liatom_is_refused() {
    let (mut deps, env, _) = setup();
    let alice = deps.api.addr_make("alice");
    let err = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&alice, &coins(0, liatom(&env))),
        ExecuteMsg::Redeem {},
    )
    .unwrap_err();
    assert!(matches!(err, ContractError::WrongFunds { .. }), "{err:?}");
}

#[test]
fn mk_rebalance_sweep_counts_the_destinations_stake_before_the_move() {
    // val2 already holds 0.5 ATOM and receives 0.1 ATOM from val1. The 0.9 ATOM net of val1's rewards must go
    // to val1 (the most underweight once both moves are counted), not to val2.
    let (mut deps, env, admin) = setup();
    set_delegations(&mut deps, &env, &[("val1", 1_000_000), ("val2", 500_000)]);
    set_val1_rewards(&mut deps, &env, 1_000_000);
    let res = execute(
        deps.as_mut(),
        env,
        message_info(&admin, &[]),
        ExecuteMsg::Rebalance { min_move: Some(Uint128::one()) },
    )
    .unwrap();
    let target = res
        .messages
        .iter()
        .find_map(|m| match &m.msg {
            CosmosMsg::Staking(StakingMsg::Delegate { validator, .. }) => Some(validator.clone()),
            _ => None,
        })
        .expect("the net rewards are delegated");
    assert_eq!(target, "val1");
}
