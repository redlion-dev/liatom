use cosmwasm_std::{Decimal, OverflowError, StdError, Uint128};
use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("{0}")]
    Overflow(#[from] OverflowError),

    #[error("unauthorized")]
    Unauthorized {},

    #[error("contract is paused")]
    Paused {},

    #[error("expected exactly one coin of denom {denom} attached")]
    WrongFunds { denom: String },

    #[error("{denom} is not a tokenized-share denom (<validator>/<record id>)")]
    NotAShareToken { denom: String },

    #[error("validator {validator} is not in the contract's validator set")]
    ValidatorNotInSet { validator: String },

    #[error("{amount} uatom is below the minimum exit {min}")]
    ExitTooSmall { amount: Uint128, min: Uint128 },

    #[error("the tokenization did not return a share token, or took more than it should")]
    BadTokenization {},

    #[error("the deposit would leave {validator} with {after} uatom, above the {max_stake} its weight allows; use Deposit, or a share token of a validator below its weight")]
    ValidatorOverweight { validator: String, after: Uint128, max_stake: Uint128 },

    #[error("the list would leave the contract with {would_be} validators (the weight list plus the ones it still holds stake at), over the limit of {max}; drain removed validators with Rebalance first")]
    TooManyValidators { would_be: usize, max: usize },

    #[error("validator {validator} takes {commission} commission, above the {max} the set allows")]
    CommissionTooHigh { validator: String, commission: Decimal, max: Decimal },

    #[error("this message takes no funds")]
    UnexpectedFunds {},

    #[error("deposit {got} is below the minimum {min}")]
    BelowMinDeposit { got: Uint128, min: Uint128 },

    #[error("amount is too small, would mint zero liATOM")]
    ZeroMint {},

    #[error("pool value is zero while liATOM supply is not, refusing to price deposits")]
    EmptyPool {},

    #[error("validator weights must be unique, non-empty and sum to 10000 bps")]
    InvalidWeights {},

    #[error("fee_bps must be at most 2000")]
    FeeTooHigh {},

    #[error("unbonding_secs must be positive and at most {max_ratio} epochs long (epoch_secs > 0), so a validator never holds more than 7 unbonding entries")]
    InvalidTiming { max_ratio: u64 },

    #[error("deposit would lift the pool to {after} uatom, over the cap of {max} (room: {room})")]
    TvlCapExceeded {
        max: Uint128,
        after: Uint128,
        room: Uint128,
    },

    #[error("the treasury cannot be the contract itself: the fee would stay on its balance")]
    InvalidTreasury {},

    #[error("min_deposit must be positive")]
    InvalidMinDeposit {},

    #[error("max_tvl must be at least min_deposit")]
    InvalidMaxTvl {},

    #[error("UpdateConfig needs at least one field")]
    EmptyConfigUpdate {},

    #[error("delegations already match the target weights, nothing to rebalance")]
    NothingToRebalance {},

    #[error("nothing to harvest")]
    NothingToHarvest {},

    #[error("the open batch is empty, nothing to unbond")]
    NothingToUnbond {},

    #[error("epoch_secs must be at most {max_epoch} s and unbonding_secs at most {max_unbonding} s")]
    TimingTooLong { max_epoch: u64, max_unbonding: u64 },

    #[error("unbonding_secs cannot go below {min} (7/8 of its value at instantiate)")]
    UnbondingBelowFloor { min: u64 },

    #[error("epoch has not elapsed yet, {remaining_secs} s left")]
    EpochNotElapsed { remaining_secs: u64 },

    #[error("batch needs {needed} uatom but only {delegated} uatom is delegated, harvest first or retry later")]
    InsufficientDelegations { needed: Uint128, delegated: Uint128 },

    #[error("batch {batch_id} is not in the expected status")]
    WrongBatchStatus { batch_id: u64 },

    #[error("batch {batch_id} has not matured yet, {remaining_secs} s left")]
    NotMatured {
        batch_id: u64,
        remaining_secs: u64,
    },

    #[error(
        "batch {batch_id}: only {free} of {expected} uatom are on the balance, \
         the rest may still be in transit, retry in {remaining_secs} s"
    )]
    NotReceivedYet {
        batch_id: u64,
        free: Uint128,
        expected: Uint128,
        remaining_secs: u64,
    },

    #[error("no claim for this address in batch {batch_id}")]
    NoClaim { batch_id: u64 },
}
