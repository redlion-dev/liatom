use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Addr, Timestamp, Uint128};
use cw_storage_plus::{Item, Map};

pub const SUBDENOM: &str = "liatom";
pub const MAX_BPS: u16 = 10_000;
pub const MAX_FEE_BPS: u16 = 2_000;
/// Unbonded ATOM return to the balance in the EndBlocker of the first block at or after
/// `mature_at`, i.e. after the transactions of that block. A `FinalizeBatch` in that block would see
/// a balance that is still short. A shortfall is therefore only accepted as a real slash this
/// long after `mature_at`; a full payout can be finalized right away.
pub const SHORTFALL_GRACE_SECS: u64 = 7 * 86_400;

/// `FinalizeBatch` is accepted only this long after `mature_at`, so at least one block has run
/// since the chain released the unbonded ATOM. In the block of maturity itself the ATOM is not on
/// the balance yet, and finalizing then would pay the batch out of unrelated free rewards.
pub const FINALIZE_DELAY_SECS: u64 = 60;

/// A deficit of at most this many uatom per `Undelegate` message of a batch is chain rounding, not
/// slashing: it is paid out at once instead of waiting for `SHORTFALL_GRACE_SECS`. Without it a
/// validator that was ever slashed (tokens per share below one) would hold every batch back by a week.
pub const ROUNDING_TOLERANCE_PER_UNDELEGATE: u128 = 10;

/// Upper bounds for the timing parameters. They keep `mature_at` and the next-submit time far
/// from timestamp overflow and stop a misconfiguration from locking redemptions for years.
pub const MAX_EPOCH_SECS: u64 = 30 * 86_400;
pub const MAX_UNBONDING_SECS: u64 = 42 * 86_400;

/// The weight list is at most this long, and so is the number of validators the contract holds a
/// delegation at (`UpdateValidators` refuses a list that would push it over), because every exit and
/// every price iterates them. More validators than this would make Deposit and SubmitUnbonding (one Undelegate per staked
/// validator) risk the block gas limit.
pub const MAX_VALIDATORS: usize = 50;

/// The highest commission (percent) a validator may have when the admin puts it in the set. Rewards the
/// validator keeps are rewards the pool never sees, and the 20% protocol fee cap means nothing if the
/// set can point at a validator that takes most of them. (A validator can still raise its commission
/// later, up to its own max rate; the admin has to watch that.)
pub const MAX_VALIDATOR_COMMISSION_PERCENT: u64 = 25;

/// Reply id of the fee transfer that SubmitUnbonding may lose without failing.
pub const FEE_REPLY_ID: u64 = 1;

/// Reply id of the redemption of tokenized shares (`DepositTokenizedShares`).
pub const LSM_REPLY_ID: u64 = 2;

/// How far above its target weight an LSM deposit may leave a validator, relative to the weight (20%).
/// The instant LSM paths are a slash-free way to move stake between validators; this keeps a deposit and
/// exit loop from concentrating the pool on a validator that is about to be slashed.
pub const LSM_OVERWEIGHT_BAND_BPS: u128 = 2000;

/// A tokenization of fewer uatom than this is not sent: on a slashed validator the chain can turn it into
/// 0 uatom and refuse it, which would fail the whole exit. Such parts are folded into a larger one.
pub const MIN_TOKENIZE_PART: u128 = 10;

/// Reply id of the tokenization of a delegation (`RedeemToShares`).
pub const TOKENIZE_REPLY_ID: u64 = 3;

#[cw_serde]
pub struct Config {
    pub admin: Addr,
    pub pending_admin: Option<Addr>,
    pub treasury: Addr,
    pub fee_bps: u16,
    /// Minimum time between two `SubmitUnbonding` calls.
    pub epoch_secs: u64,
    /// Chain unbonding period in seconds (not queryable from a contract, so configured).
    pub unbonding_secs: u64,
    /// Lowest value `unbonding_secs` may be set to later: 7/8 of its value at instantiate. A
    /// configuration longer than the chain's makes deposits overpay, so the admin must be able to
    /// lower it when the chain shortens its period, but not so far that a batch could be settled
    /// before the chain has paid it (the grace period of 7 days covers a chain up to a seventh longer, and
    /// at 6 epochs per unbonding period the chain's 7 entries per validator still suffice).
    #[serde(default)]
    pub min_unbonding_secs: u64,
    pub min_deposit: Uint128,
    /// Deposits are refused when they would lift the pool value above this. `Uint128::MAX` = no cap.
    pub max_tvl: Uint128,
    /// `None` = running. See [`PauseMode`].
    pub pause: Option<PauseMode>,
    /// Full tokenfactory denom of liATOM: `factory/<contract>/liatom`.
    pub liatom_denom: String,
    /// Chain bond denom (`uatom`).
    pub bond_denom: String,
}

/// What the admin pause blocks. `Claim`, `FinalizeBatch`, `Harvest` and `SubmitUnbonding` always work,
/// so a pause never traps ATOM that is already on its way out.
#[cw_serde]
#[derive(Copy)]
pub enum PauseMode {
    /// Blocks `Deposit` only: users can still leave through `Redeem`.
    DepositsOnly,
    /// Blocks `Deposit` and `Redeem`.
    Full,
}

#[cw_serde]
pub struct ValidatorWeight {
    pub validator: String,
    pub weight_bps: u16,
}

#[cw_serde]
pub enum BatchStatus {
    /// Collecting redemptions, not yet sent to the staking module.
    Open,
    /// `Undelegate` messages were sent, waiting for the unbonding period.
    Unbonding,
    /// Funds are back on the contract and can be claimed.
    Matured,
}

#[cw_serde]
pub struct Batch {
    pub id: u64,
    pub status: BatchStatus,
    /// liATOM burned into this batch.
    pub liatom_burned: Uint128,
    /// ATOM owed to the batch, fixed when it is submitted.
    pub atom_expected: Option<Uint128>,
    /// ATOM actually received after unbonding (differs on slashing).
    pub atom_received: Option<Uint128>,
    pub mature_at: Option<Timestamp>,
    /// How many `Undelegate` messages the submit sent. Each can come back a uatom short because the
    /// chain converts the amount to shares and back with truncation.
    #[serde(default)]
    pub undelegations: u32,
    /// Height of the first block in which this batch was seen short after its grace period. A
    /// shortfall is only written off in a later block: after a chain halt the first block already
    /// counts as past the grace period, while the unbonded ATOM is only released at its end.
    #[serde(default)]
    pub shortfall_noted_at: Option<u64>,
    /// When the batch was submitted for unbonding (the keeper schedules the next submit from it, which
    /// stays right when `unbonding_secs` is changed while a batch is in flight).
    #[serde(default)]
    pub submitted_at: Option<Timestamp>,
}

pub const CONFIG: Item<Config> = Item::new("config");
pub const VALIDATORS: Item<Vec<ValidatorWeight>> = Item::new("validators");
pub const CURRENT_BATCH: Item<u64> = Item::new("current_batch");
pub const BATCHES: Map<u64, Batch> = Map::new("batches");
/// liATOM a user put into a batch: (batch id, user) -> amount.
pub const CLAIMS: Map<(u64, &Addr), Uint128> = Map::new("claims");
/// ATOM of finalized batches that redeemers have not claimed yet (excluded from the exchange rate).
pub const OWED_TO_REDEEMERS: Item<Uint128> = Item::new("owed_to_redeemers");
/// liATOM burned into the open batch. Still backed by pool ATOM until `SubmitUnbonding`,
/// so it counts as supply when pricing the exchange rate.
pub const PENDING_LIATOM: Item<Uint128> = Item::new("pending_liatom");
/// Batches in `Unbonding` status (small list, at most ~6 live at once).
pub const UNBONDING: Item<Vec<u64>> = Item::new("unbonding");
pub const LAST_SUBMIT: Item<Timestamp> = Item::new("last_submit");
