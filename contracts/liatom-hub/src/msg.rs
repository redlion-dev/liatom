use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::{Decimal, Uint128};

use crate::state::{PauseMode, ValidatorWeight};

#[cw_serde]
#[serde(deny_unknown_fields)]
pub struct InstantiateMsg {
    pub admin: String,
    pub treasury: String,
    pub fee_bps: u16,
    pub epoch_secs: u64,
    /// Chain unbonding time in seconds (1_814_400 on Cosmos Hub mainnet).
    pub unbonding_secs: u64,
    pub min_deposit: Uint128,
    /// Cap on the pool value in `uatom`; `Uint128::MAX` for no cap. Instantiate funds must fit under it.
    pub max_tvl: Uint128,
    /// Hub tokenfactory `denom_creation_fee` in `uatom` (10_000_000 on mainnet).
    /// Instantiate funds must cover it plus the seed deposit.
    pub denom_creation_fee: Uint128,
    pub validators: Vec<ValidatorWeight>,
}

#[cw_serde]
#[serde(deny_unknown_fields)]
pub enum ExecuteMsg {
    // --- users ---
    /// Attach `uatom`, receive liATOM at the current exchange rate.
    Deposit {},
    /// Attach one LSM share token (`<validator>/<record id>`, made with `MsgTokenizeShares`) of a validator
    /// in the weight list and receive liATOM for the ATOM it redeems to. The delegation moves to the
    /// contract without unbonding. Priced like a deposit of that amount (same rate, minimum and TVL cap).
    DepositTokenizedShares {},
    /// Attach liATOM, join the open batch (it is sent for unbonding at the next epoch).
    Redeem {},
    /// Attach liATOM and leave as ordinary staked ATOM, at once: the liATOM is burned at the current rate and
    /// the contract tokenizes that much of its delegations (in proportion to their stake, so the leaver shares
    /// the risk of every validator like the holders who stay) into LSM share tokens, one per validator, that
    /// are sent to the caller, who can redeem them into delegations of their own (or sell them). No unbonding
    /// wait, no batch. Fails if the chain refuses to tokenize (LSM caps, a redelegation in progress); the
    /// normal `Redeem` always remains.
    RedeemToShares {},
    /// Withdraw ATOM of a matured batch.
    Claim { batch_id: u64 },

    // --- public, called by a keeper bot ---
    /// Withdraw rewards, take the fee, re-delegate the rest.
    Harvest {},
    /// Close the open batch and send `Undelegate` messages.
    SubmitUnbonding {},
    /// Record what the batch actually received after unbonding.
    FinalizeBatch { batch_id: u64 },

    // --- admin ---
    UpdateValidators { validators: Vec<ValidatorWeight> },
    /// Change protocol parameters. Raising or lowering `fee_bps` first harvests at the old fee,
    /// so pending rewards are never repriced retroactively.
    UpdateConfig {
        treasury: Option<String>,
        fee_bps: Option<u16>,
        epoch_secs: Option<u64>,
        unbonding_secs: Option<u64>,
        min_deposit: Option<Uint128>,
        /// New cap on the pool value. Lowering it below the current value only stops new deposits.
        max_tvl: Option<Uint128>,
    },
    /// Move stake between validators with `Redelegate` so delegations match the target weights.
    /// Validators dropped from the weight list are drained completely. Moves below `min_move`
    /// (default 1 ATOM) are skipped, at most 8 moves per call.
    Rebalance { min_move: Option<Uint128> },
    /// Block new deposits (and, with `full`, also new redemptions). `claim`, `finalize_batch`,
    /// `harvest` and `submit_unbonding` keep working in every mode.
    Pause { mode: PauseMode },
    Unpause {},
    ProposeAdmin { new_admin: String },
    AcceptAdmin {},
}

#[cw_serde]
#[serde(deny_unknown_fields)]
#[derive(QueryResponses)]
pub enum QueryMsg {
    #[returns(crate::state::Config)]
    Config {},
    #[returns(Vec<ValidatorWeight>)]
    Validators {},
    #[returns(PoolResponse)]
    Pool {},
    #[returns(ExchangeRateResponse)]
    ExchangeRate {},
    #[returns(crate::state::Batch)]
    Batch { batch_id: u64 },
    #[returns(Uint128)]
    Claim { batch_id: u64, address: String },
}

#[cw_serde]
pub struct PoolResponse {
    pub delegated: Uint128,
    /// Rewards not yet withdrawn from the distribution module (gross, before the protocol fee).
    pub unclaimed_rewards: Uint128,
    /// Protocol fee accrued on `unclaimed_rewards`, excluded from `pool_value`.
    pub accrued_fee: Uint128,
    /// ATOM reserved for batches that matured or were finalized and are not claimed yet.
    pub owed_to_redeemers: Uint128,
    /// liATOM burned into the open batch, still counted as supply for pricing.
    pub pending_liatom: Uint128,
    pub pool_value: Uint128,
    /// On-chain supply of the liATOM denom.
    pub liatom_supply: Uint128,
}

#[cw_serde]
pub struct ExchangeRateResponse {
    /// ATOM per 1 liATOM.
    pub rate: Decimal,
}
