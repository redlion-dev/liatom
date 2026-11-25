// sonnet11 audit: extended chain-model proptest (copy of tests/invariants.rs + extra ops: Jump (past 7-day grace, height moves), SetUnbonding, SetMinDeposit, SetTreasury, SettleAll, ClaimAll, tiny amounts;
// new invariants: no slash => every batch receives exactly atom_expected; treasury never holds >20% of accrued rewards; drain_check = liveness (every redeemer can exit after honest admin+keeper actions).
// Run: cp this file to contracts/liatom-hub/tests/ext3.rs; PROPTEST_CASES=2500 cargo test --test ext3 x_
//! Property-based invariant tests.
//!
//! A small chain simulator drives the real `execute`/`reply`/`query` entry points with random
//! operation sequences. After every operation it applies the contract's messages to a model of the
//! chain (staking with validator shares and tokens, so slashing makes shares and tokens differ;
//! distribution; the liquid staking module with tokenize/redeem and share records; bank; tokenfactory),
//! routes every submessage reply back into the contract exactly like wasmd does (depth first, a failing
//! message with `reply_on_error` is rolled back alone, any other failure reverts the whole transaction),
//! feeds that state back into the mock querier and checks the invariants below.
//!
//! Invariants:
//!  1. Conservation: all ATOM in the system (user balances, treasury, contract balance, every delegation
//!     of every delegator including tokenized-share records, pending rewards, unbonding entries) only
//!     changes through reward accrual (+), slashing (-) and the tokenfactory denom fee (-).
//!  2. Solvency: the contract balance always covers what finalized batches have yet to pay out.
//!  3. The exchange rate never decreases, except through slashing. (Rounding always favours the pool.)
//!  4. The seed liATOM minted to the contract at instantiate never leaves it.
//!  5. A user can never claim more than the batch received.
//!  6. (LSM) No actor gains from `Deposit`, `DepositTokenizedShares` or `RedeemToShares`: the value of
//!     everything the actor holds (ATOM, delegations, share tokens, liATOM at the pre-operation rate) does
//!     not grow beyond rounding dust.
//!  7. (LSM) `RedeemToShares` takes from every validator in proportion to its stake.
//!  8. (LSM) Share bookkeeping of the chain model is consistent (delegations add up to validator shares,
//!     every share record has a delegation behind it).

use std::collections::{BTreeMap, BTreeSet};

use cosmwasm_std::testing::{
    message_info, mock_dependencies, mock_env, MockApi, MockQuerier, MockStorage,
};
use cosmwasm_std::{
    coin, Addr, AnyMsg, Binary, Coin, CosmosMsg, DecCoin, Decimal, Decimal256, Env, FullDelegation,
    MsgResponse, Order, OwnedDeps, Reply, ReplyOn, Storage, SubMsg, SubMsgResponse, SubMsgResult,
    Uint128, Uint256, Validator,
};
use proptest::prelude::*;

use liatom_hub::contract::{execute, instantiate, query, reply};
use liatom_hub::msg::{ExchangeRateResponse, ExecuteMsg, InstantiateMsg, PoolResponse, QueryMsg};
use liatom_hub::state::{Batch, BatchStatus, Config, PauseMode, ValidatorWeight, CURRENT_BATCH, OWED_TO_REDEEMERS, UNBONDING};
use liatom_hub::tokenfactory::TokenFactoryMsg;

mod refmodel;

const ATOM: &str = "uatom";
const VALS: [&str; 3] = ["val1", "val2", "val3"];
const USERS: [&str; 3] = ["alice", "bob", "carol"];
const UNBONDING_SECS: u64 = 1_000;
const EPOCH_SECS: u64 = 200;
const DENOM_FEE: u128 = 10_000_000;
const SEED: u128 = 10_000_000;
const MAX_BATCH_PROBE: u64 = 40;
/// Delegator shares are fixed point with 18 decimals, like the SDK's `LegacyDec`.
const SCALE: u128 = 1_000_000_000_000_000_000;
/// What an actor may gain from rounding in one operation (uatom, per validator touched).
const DUST_PER_PART: u128 = 2;
/// Operations of the fixed smoke sequences that are meant to be refused (the second claim of the same
/// batch). Any other failure is a regression.
const SMOKE_EXPECTED_FAILS: u32 = 1;
const LSM_SMOKE_EXPECTED_FAILS: u32 = 0;

static FAILS: std::sync::Mutex<BTreeMap<String, usize>> = std::sync::Mutex::new(BTreeMap::new());
thread_local! {
    /// Failed operations on this thread since the last reset: the fixed smoke sequences pin it, so an
    /// operation that stops working (a Deposit that always errors) fails them. Proptest runs shrug off
    /// failed operations by design, which is why this liveness check lives in the fixed sequences.
    static SEQ_FAILS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
fn note_fail(kind: &str, e: &str) {
    SEQ_FAILS.with(|c| c.set(c.get() + 1));
    let key = format!("{kind}: {}", e.chars().take(110).collect::<String>());
    *FAILS.lock().unwrap().entry(key).or_default() += 1;
}
static OK_EXIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static OK_ENTRY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static TRY_EXIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static TRY_ENTRY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static MULTI_PART_EXIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

type Deps = OwnedDeps<MockStorage, MockApi, MockQuerier>;

fn muldiv(a: u128, b: u128, c: u128) -> u128 {
    Uint128::try_from(Uint256::from(a) * Uint256::from(b) / Uint256::from(c))
        .expect("fits")
        .u128()
}

#[derive(Debug, Clone)]
enum Op {
    Deposit { user: usize, amount: u64 },
    Redeem { user: usize, pct: u64 },
    Harvest,
    Submit,
    Finalize { id: u64 },
    Claim { user: usize, id: u64 },
    Accrue { val: usize, amount: u64 },
    Advance { secs: u64 },
    Slash { val: usize, bps: u64 },
    /// Admin sets new weights (each validator gets `v_i / sum` of 10000 bps) and rebalances.
    Reweight { v: [u64; 3] },
    SetFee { bps: u16 },
    /// Admin changes the epoch length; the contract must refuse values that could exceed 7 entries.
    SetEpoch { secs: u64 },
    // ---- liquid staking module paths ----
    /// `RedeemToShares` with `pct` percent of the user's liATOM.
    RedeemShares { user: usize, pct: u64 },
    /// The user sends one of the share tokens they hold to `DepositTokenizedShares`.
    DepositShares { user: usize, pick: usize },
    /// The user stakes ATOM directly (a delegation of their own that can be tokenized).
    UserDelegate { user: usize, val: usize, amount: u64 },
    /// The user tokenizes `pct` percent of a delegation of their own.
    UserTokenize { user: usize, val: usize, pct: u64 },
    /// The user redeems a share token they hold into a delegation of their own.
    UserRedeem { user: usize, pick: usize },
    /// Admin removes a validator from the weight list (optionally rebalancing with a given minimum move).
    DropValidator { val: usize, rebalance: Option<u64> },
    Pause { mode: u8 },
    /// Admin sets `max_tvl` to `pct` percent of the current pool value (300 or more means no cap).
    SetTvl { pct: u64 },
    /// The treasury refuses (or accepts again) incoming transfers.
    TreasuryBlocked { blocked: bool },
    /// The chain's liquid staking caps refuse (or allow again) tokenization at a validator.
    CapBlocked { val: usize, blocked: bool },
    // ---- audit extension ----
    /// A long pause (past the 7-day shortfall grace) with the block height moving.
    Jump { secs: u64 },
    SetUnbonding { secs: u64 },
    SetMinDeposit { v: u64 },
    SetTreasury { who: u8 },
    /// Finalize every batch id, twice (two blocks).
    SettleAll,
    /// A user claims every batch.
    ClaimAll { user: usize },
    // ---- audit extension (sonnet12) ----
    /// The next operation runs in the same block (same height) as this one.
    SameBlock,
    /// A plain bank send of ATOM to the contract (no message).
    DonateAtom { user: usize, amount: u64 },
    /// A plain bank send of liATOM to the contract (no message).
    DonateLi { user: usize, pct: u64 },
    /// One block, one user: Deposit, Redeem everything, Submit, Finalize all, Claim all.
    Combo { user: usize, amount: u64 },
    /// One block: DepositTokenizedShares then Redeem of the liATOM that just got minted.
    LsmThenRedeem { user: usize, pick: usize },
    /// One block: RedeemToShares, redeem the tokens into a delegation, tokenize it again, deposit it: a wash loop.
    Loop { user: usize, pct: u64 },
    /// Finalize every unbonding batch in three different ways from the same state: the outcomes must agree.
    OrderCheck,
    /// Move one epoch (more than epoch_secs) / one unbonding period + 61 s forward.
    AdvEpoch,
    AdvUnbond,
}

fn op_strategy(with_slash: bool, with_lsm: bool) -> BoxedStrategy<Op> {
    op_strategy_x(with_slash, with_lsm, false)
}

fn op_strategy_x(with_slash: bool, with_lsm: bool, ext: bool) -> BoxedStrategy<Op> {
    // More than 10 arms do not fit prop_oneof!'s tuple union (which also rejects zero weights),
    // so the weighted union is built by hand and arms are simply left out when unused.
    let mut arms: Vec<(u32, BoxedStrategy<Op>)> = vec![
        (5, (0..USERS.len(), 1_000_000u64..5_000_000_000)
            .prop_map(|(user, amount)| Op::Deposit { user, amount })
            .boxed()),
        (4, (0..USERS.len(), 1u64..=100)
            .prop_map(|(user, pct)| Op::Redeem { user, pct })
            .boxed()),
        (3, Just(Op::Harvest).boxed()),
        (3, Just(Op::Submit).boxed()),
        (3, (1u64..6).prop_map(|id| Op::Finalize { id }).boxed()),
        (4, (0..USERS.len(), 1u64..6)
            .prop_map(|(user, id)| Op::Claim { user, id })
            .boxed()),
        (4, (0..VALS.len(), 1u64..200_000_000)
            .prop_map(|(val, amount)| Op::Accrue { val, amount })
            .boxed()),
        (4, (1u64..(2 * UNBONDING_SECS))
            .prop_map(|secs| Op::Advance { secs })
            .boxed()),
        (2, [1u64..=100, 1u64..=100, 1u64..=100]
            .prop_map(|v| Op::Reweight { v })
            .boxed()),
        (2, (0u16..=2_000).prop_map(|bps| Op::SetFee { bps }).boxed()),
        (2, (50u64..=1_000).prop_map(|secs| Op::SetEpoch { secs }).boxed()),
    ];
    if with_slash {
        arms.push((
            2,
            (0..VALS.len(), 1u64..2_000)
                .prop_map(|(val, bps)| Op::Slash { val, bps })
                .boxed(),
        ));
    }
    if with_lsm {
        arms.push((6, (0..USERS.len(), 1u64..=100)
            .prop_map(|(user, pct)| Op::RedeemShares { user, pct })
            .boxed()));
        arms.push((6, (0..USERS.len(), 0usize..4)
            .prop_map(|(user, pick)| Op::DepositShares { user, pick })
            .boxed()));
        arms.push((5, (0..USERS.len(), 0..VALS.len(), 1_000_000u64..3_000_000_000)
            .prop_map(|(user, val, amount)| Op::UserDelegate { user, val, amount })
            .boxed()));
        arms.push((5, (0..USERS.len(), 0..VALS.len(), 1u64..=100)
            .prop_map(|(user, val, pct)| Op::UserTokenize { user, val, pct })
            .boxed()));
        arms.push((3, (0..USERS.len(), 0usize..4)
            .prop_map(|(user, pick)| Op::UserRedeem { user, pick })
            .boxed()));
        arms.push((2, (0..VALS.len(), prop::option::of(prop_oneof![Just(1u64), Just(1_000_000u64)]))
            .prop_map(|(val, rebalance)| Op::DropValidator { val, rebalance })
            .boxed()));
        arms.push((1, (0u8..3).prop_map(|mode| Op::Pause { mode }).boxed()));
        arms.push((1, (20u64..=400).prop_map(|pct| Op::SetTvl { pct }).boxed()));
        arms.push((1, any::<bool>().prop_map(|blocked| Op::TreasuryBlocked { blocked }).boxed()));
        arms.push((1, (0..VALS.len(), any::<bool>())
            .prop_map(|(val, blocked)| Op::CapBlocked { val, blocked })
            .boxed()));
    }
    if ext {
        arms.push((2, (1u64..1_500_000).prop_map(|secs| Op::Jump { secs }).boxed()));
        arms.push((1, (875u64..=1_400).prop_map(|secs| Op::SetUnbonding { secs }).boxed()));
        arms.push((1, (1u64..=3_000_000).prop_map(|v| Op::SetMinDeposit { v }).boxed()));
        arms.push((2, (1u64..=50).prop_map(|v| Op::SetMinDeposit { v }).boxed()));
        arms.push((1, (0u8..2).prop_map(|who| Op::SetTreasury { who }).boxed()));
        arms.push((3, Just(Op::SettleAll).boxed()));
        arms.push((3, (0..USERS.len()).prop_map(|user| Op::ClaimAll { user }).boxed()));
        // tiny amounts: dust folding, ExitTooSmall, rounding
        arms.push((3, (0..USERS.len(), 1u64..3_000).prop_map(|(user, amount)| Op::Deposit { user, amount }).boxed()));
        arms.push((4, (0..USERS.len(), 101u64..3_100).prop_map(|(user, pct)| Op::RedeemShares { user, pct }).boxed()));
        arms.push((2, (0..USERS.len(), 101u64..3_100).prop_map(|(user, pct)| Op::Redeem { user, pct }).boxed()));
        arms.push((2, (0..VALS.len(), 1u64..2_000).prop_map(|(val, amount)| Op::Accrue { val, amount }).boxed()));
        arms.push((2, (0..USERS.len(), 0..VALS.len(), 1u64..300).prop_map(|(user, val, amount)| Op::UserDelegate { user, val, amount }).boxed()));
    }
    prop::strategy::Union::new_weighted(arms).boxed()
}

// ---- minimal protobuf reader for the two LSM messages -------------------------------------------

fn pb_fields(mut b: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    while !b.is_empty() {
        let tag = b[0];
        assert_eq!(tag & 7, 2, "only length-delimited fields expected");
        let mut len = 0usize;
        let mut shift = 0;
        let mut i = 1;
        loop {
            let byte = b[i];
            i += 1;
            len |= ((byte & 0x7f) as usize) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        out.push((tag >> 3, b[i..i + len].to_vec()));
        b = &b[i + len..];
    }
    out
}

fn pb_coin(b: &[u8]) -> Coin {
    let f = pb_fields(b);
    let denom = String::from_utf8(f.iter().find(|(n, _)| *n == 1).unwrap().1.clone()).unwrap();
    let amount: u128 = String::from_utf8(f.iter().find(|(n, _)| *n == 2).unwrap().1.clone())
        .unwrap()
        .parse()
        .unwrap();
    coin(amount, denom)
}

fn tokenize_response(c: &Coin) -> MsgResponse {
    let mut coin_bytes = vec![0x0a, c.denom.len() as u8];
    coin_bytes.extend_from_slice(c.denom.as_bytes());
    let a = c.amount.to_string();
    coin_bytes.push(0x12);
    coin_bytes.push(a.len() as u8);
    coin_bytes.extend_from_slice(a.as_bytes());
    let mut value = vec![0x0a, coin_bytes.len() as u8];
    value.extend_from_slice(&coin_bytes);
    MsgResponse {
        type_url: "/gaia.liquid.v1beta1.MsgTokenizeSharesResponse".to_string(),
        value: Binary::from(value),
    }
}

// ---- the chain model ---------------------------------------------------------------------------------

/// Everything a transaction can change on the chain side (the contract's own storage is separate).
#[derive(Clone, Default)]
struct Chain {
    /// validator -> (tokens, delegator shares x SCALE). Slashing cuts tokens, never shares.
    vals: BTreeMap<String, (u128, u128)>,
    /// (delegator, validator) -> shares x SCALE. Tokenized-share records delegate as `rec/<id>`.
    dels: BTreeMap<(String, String), u128>,
    /// record id -> (validator, owner)
    records: BTreeMap<u64, (String, String)>,
    next_record: u64,
    /// share token balances: (holder, denom) -> amount
    tok: BTreeMap<(String, String), u128>,
    /// validator -> time until which the contract has a redelegation in progress into it
    redel_until: BTreeMap<String, u64>,
    cap_blocked: BTreeSet<String>,
    treasury_blocked: bool,
    /// the contract's pending rewards per validator
    rewards: BTreeMap<String, u128>,
    /// (matures at, validator, amount)
    unbonding: Vec<(u64, String, u128)>,
    atom: BTreeMap<Addr, u128>,
    li: BTreeMap<Addr, u128>,
    /// every tokenization: (delegator, validator, asked, tokens taken out, share token amount)
    tokenized: Vec<(String, String, u128, u128, u128)>,
    /// every redemption of a share token: (delegator, validator, tokens delegated)
    redeemed: Vec<(String, String, u128)>,
}

impl Chain {
    /// `Validator.SharesFromTokens`: truncates (`LegacyDec.QuoInt`).
    fn shares_from_tokens(&self, val: &str, amt: u128) -> u128 {
        let (t, s) = self.vals.get(val).copied().unwrap_or((0, 0));
        if s == 0 {
            amt * SCALE
        } else {
            muldiv(s, amt, t)
        }
    }

    /// `Validator.TokensFromShares(..).TruncateInt()`: the division is rounded half-even at 18 decimals
    /// first (`LegacyDec.Quo`), then truncated to an integer.
    fn tokens_from_shares(&self, val: &str, shares: u128) -> u128 {
        let (t, s) = self.vals.get(val).copied().unwrap_or((0, 0));
        if s == 0 {
            return 0;
        }
        let num = Uint256::from(shares) * Uint256::from(t) * Uint256::from(SCALE);
        let den = Uint256::from(s);
        let q = num / den;
        let r = num - q * den;
        let twice = r * Uint256::from(2u8);
        let q = if twice > den || (twice == den && (q % Uint256::from(2u8)) == Uint256::one()) {
            q + Uint256::one()
        } else {
            q
        };
        Uint128::try_from(q / Uint256::from(SCALE)).expect("fits").u128()
    }

    fn delegation_tokens(&self, del: &str, val: &str) -> u128 {
        let sh = self.dels.get(&(del.to_string(), val.to_string())).copied().unwrap_or(0);
        self.tokens_from_shares(val, sh)
    }

    fn delegate(&mut self, del: &str, val: &str, amt: u128) {
        let issued = self.shares_from_tokens(val, amt);
        let e = self.vals.entry(val.to_string()).or_default();
        e.0 += amt;
        e.1 += issued;
        *self.dels.entry((del.to_string(), val.to_string())).or_default() += issued;
    }

    fn validate_unbond(&self, del: &str, val: &str, amt: u128) -> Result<u128, String> {
        let d = *self
            .dels
            .get(&(del.to_string(), val.to_string()))
            .ok_or("no delegation")?;
        let (t, s) = *self.vals.get(val).ok_or("no validator")?;
        if t == 0 {
            return Err("empty validator".into());
        }
        let shares = muldiv(s, amt, t);
        let truncated = shares;
        if truncated > d {
            return Err("invalid shares amount".into());
        }
        Ok(shares.min(d))
    }

    fn unbond(&mut self, del: &str, val: &str, shares: u128) -> Result<u128, String> {
        let key = (del.to_string(), val.to_string());
        let d = *self.dels.get(&key).ok_or("no delegation")?;
        if d < shares {
            return Err("not enough delegation shares".into());
        }
        let (t, s) = *self.vals.get(val).ok_or("no validator")?;
        // the last delegator's shares take every remaining token
        let out = if s == shares { t } else { self.tokens_from_shares(val, shares) };
        let e = self.vals.get_mut(val).unwrap();
        e.0 -= out;
        e.1 -= shares;
        if d == shares {
            self.dels.remove(&key);
        } else {
            self.dels.insert(key, d - shares);
        }
        Ok(out)
    }

    /// `MsgTokenizeShares` of gaia's x/liquid, step by step. Returns the share token.
    fn tokenize(
        &mut self,
        now: u64,
        del: &str,
        del_is_contract: bool,
        val: &str,
        amount: u128,
        owner: &str,
    ) -> Result<Coin, String> {
        let shares = self.validate_unbond(del, val, amount)?;
        if shares == 0 {
            return Err("cannot tokenize zero shares".into());
        }
        if del_is_contract && self.redel_until.get(val).is_some_and(|until| *until > now) {
            return Err("redelegation in progress".into());
        }
        if self.cap_blocked.contains(val) {
            return Err("liquid staking cap exceeded".into());
        }
        let old = self.vals[val];
        let out = self.unbond(del, val, shares)?;
        // the share token maps 1:1 to shares, computed with the validator as it was before the unbond
        let new_shares = if old.1 == 0 { out * SCALE } else { muldiv(old.1, out, old.0) };
        let token_amount = new_shares / SCALE;
        if token_amount == 0 {
            return Err("invalid coins: zero share token".into());
        }
        self.next_record += 1;
        let id = self.next_record;
        self.records.insert(id, (val.to_string(), owner.to_string()));
        self.delegate(&format!("rec/{id}"), val, out);
        let denom = format!("{val}/{id}");
        *self.tok.entry((del.to_string(), denom.clone())).or_default() += token_amount;
        self.tokenized.push((del.to_string(), val.to_string(), amount, out, token_amount));
        Ok(coin(token_amount, denom))
    }

    /// `MsgRedeemTokensForShares`. Returns the tokens that were delegated to the redeemer.
    fn redeem(&mut self, del: &str, token: &Coin) -> Result<u128, String> {
        let key = (del.to_string(), token.denom.clone());
        let have = self.tok.get(&key).copied().unwrap_or(0);
        if have < token.amount.u128() {
            return Err("not enough balance".into());
        }
        let id: u64 = token
            .denom
            .split_once('/')
            .and_then(|(_, id)| id.parse().ok())
            .ok_or("not a share token")?;
        let (val, _) = self.records.get(&id).cloned().ok_or("no such record")?;
        let rec = format!("rec/{id}");
        let delshares = *self.dels.get(&(rec.clone(), val.clone())).ok_or("no delegation")?;
        let mut shares = token.amount.u128() * SCALE;
        if token.amount.u128() == delshares / SCALE {
            shares = delshares;
        }
        if self.tokens_from_shares(&val, shares) == 0 {
            return Err("tiny redemption".into());
        }
        let out = self.unbond(&rec, &val, shares)?;
        if !self.dels.contains_key(&(rec, val.clone())) {
            self.records.remove(&id);
        }
        *self.tok.get_mut(&key).unwrap() -= token.amount.u128();
        self.delegate(del, &val, out);
        self.redeemed.push((del.to_string(), val.clone(), out));
        Ok(out)
    }

    fn atom_of(&self, a: &Addr) -> u128 {
        *self.atom.get(a).unwrap_or(&0)
    }
    fn add_atom(&mut self, a: &Addr, n: u128) {
        *self.atom.entry(a.clone()).or_default() += n;
    }
    fn sub_atom(&mut self, a: &Addr, n: u128) {
        let b = self.atom.entry(a.clone()).or_default();
        *b = b.checked_sub(n).unwrap_or_else(|| panic!("{a} cannot pay {n} uatom (has {b})"));
    }
}

struct Snapshot {
    storage: Vec<(Vec<u8>, Vec<u8>)>,
    c: Chain,
    rf: refmodel::Ref,
}

struct Sim {
    deps: Deps,
    env: Env,
    contract: Addr,
    admin: Addr,
    treasury: Addr,
    users: Vec<Addr>,
    liatom: String,

    c: Chain,
    /// Total ATOM that must exist in the system.
    expected_total: u128,
    slashed: bool,
    /// ATOM paid out to claimants per batch.
    paid: BTreeMap<u64, u128>,
    accrued: u128,
    // ---- sonnet12 ----
    /// Realistic arrival: matured unbonding entries are paid out at the END of the first block whose time is at
    /// or after the completion time (after that block's transactions), not at its start.
    eb: bool,
    /// 0: separate admin / keeper / users; 1: admin = keeper = users[0]; 2: additionally treasury = users[0].
    roles: u8,
    hold_height: bool,
    /// a block was just entered through `advance`: its transactions run before its EndBlock
    fresh: bool,
    donated_li: u128,
    /// the configured unbonding period has differed from the chain's at some point (a documented limit: ATOM may be late)
    mismatch: bool,
    last_err: Option<String>,
    audit_err: Option<String>,
    rf: refmodel::Ref,
    dep_obs: Option<(String, u128, u128)>,
    trace: Vec<String>,
}

impl Sim {
    #[allow(dead_code)]
    #[allow(dead_code)]
    fn new() -> Self {
        Self::new_with(false)
    }

    /// `whale`: other delegators already hold a lot at every validator, as on the real Hub, so the
    /// pool is a small part of each validator and share/token rounding is negligible.
    fn new_with(whale: bool) -> Self {
        Self::new_cfg(whale, false, 0)
    }

    fn new_cfg(whale: bool, eb: bool, roles: u8) -> Self {
        let deps = mock_dependencies();
        let env = mock_env();
        let contract = env.contract.address.clone();
        let users: Vec<Addr> = USERS.iter().map(|u| deps.api.addr_make(u)).collect();
        let admin = if roles >= 1 { users[0].clone() } else { deps.api.addr_make("admin") };
        let treasury = if roles >= 2 { users[0].clone() } else { deps.api.addr_make("treasury") };
        let liatom = format!("factory/{}/liatom", contract);

        let mut c = Chain::default();
        c.atom.insert(admin.clone(), 100_000_000_000u128);
        for u in &users {
            c.atom.insert(u.clone(), 1_000_000_000_000u128);
        }
        c.atom.insert(contract.clone(), 0);
        c.atom.entry(treasury.clone()).or_insert(0);
        let mut expected_total: u128 = c.atom.values().sum();
        if whale {
            for v in VALS {
                c.delegate("whale", v, 1_000_000_000_000_000);
            }
            expected_total += 3_000_000_000_000_000;
        }

        let mut sim = Sim {
            deps,
            env,
            contract: contract.clone(),
            admin: admin.clone(),
            treasury: treasury.clone(),
            users,
            liatom,
            c,
            expected_total,
            slashed: false,
            paid: BTreeMap::new(),
            accrued: 0,
            eb,
            roles,
            hold_height: false,
            fresh: false,
            donated_li: 0,
            mismatch: false,
            last_err: None,
            audit_err: None,
            rf: refmodel::Ref::new(
                vec![("val1".into(), 5000), ("val2".into(), 3000), ("val3".into(), 2000)],
                1000, EPOCH_SECS, UNBONDING_SECS, 1_000_000, u128::MAX,
            ),
            dep_obs: None,
            trace: vec![],
        };
        sim.sync();

        let msg = InstantiateMsg {
            admin: admin.to_string(),
            treasury: treasury.to_string(),
            fee_bps: 1000,
            epoch_secs: EPOCH_SECS,
            unbonding_secs: UNBONDING_SECS,
            min_deposit: Uint128::new(1_000_000),
            max_tvl: Uint128::MAX,
            denom_creation_fee: Uint128::new(DENOM_FEE),
            validators: vec![
                ValidatorWeight { validator: "val1".into(), weight_bps: 5000 },
                ValidatorWeight { validator: "val2".into(), weight_bps: 3000 },
                ValidatorWeight { validator: "val3".into(), weight_bps: 2000 },
            ],
        };
        let funds = coin(SEED + DENOM_FEE, ATOM);
        sim.c.sub_atom(&admin, funds.amount.u128());
        sim.c.add_atom(&contract, funds.amount.u128());
        sim.sync();
        let res = instantiate(
            sim.deps.as_mut(),
            sim.env.clone(),
            message_info(&admin, &[funds]),
            msg,
        )
        .expect("instantiate");
        sim.exec_msgs(res.messages).expect("instantiate messages");
        sim.sync();
        sim.check_invariants("instantiate").expect("initial invariants");
        sim
    }

    // ---- balances -------------------------------------------------------------------------

    fn atom_of(&self, a: &Addr) -> u128 {
        self.c.atom_of(a)
    }
    fn li_of(&self, a: &Addr) -> u128 {
        *self.c.li.get(a).unwrap_or(&0)
    }
    fn add_li(&mut self, a: &Addr, n: u128) {
        *self.c.li.entry(a.clone()).or_default() += n;
    }
    fn sub_li(&mut self, a: &Addr, n: u128) {
        let b = self.c.li.entry(a.clone()).or_default();
        *b = b.checked_sub(n).unwrap_or_else(|| panic!("{a} cannot pay {n} liATOM (has {b})"));
    }
    fn contract_delegation(&self, val: &str) -> u128 {
        self.c.delegation_tokens(self.contract.as_str(), val)
    }
    fn contract_tokens_total(&self) -> u128 {
        VALS.iter().map(|v| self.contract_delegation(v)).sum()
    }
    fn now(&self) -> u64 {
        self.env.block.time.seconds()
    }

    // ---- model -> mock querier ---------------------------------------------------------------

    fn sync(&mut self) {
        let validators: Vec<Validator> = VALS
            .iter()
            .map(|v| {
                Validator::create(
                    v.to_string(),
                    Decimal::percent(5),
                    Decimal::percent(20),
                    Decimal::percent(1),
                )
            })
            .collect();
        let delegations: Vec<FullDelegation> = VALS
            .iter()
            .filter(|v| self.c.dels.contains_key(&(self.contract.to_string(), v.to_string())))
            .map(|v| {
                let a = self.contract_delegation(v);
                FullDelegation::create(
                    self.contract.clone(),
                    v.to_string(),
                    coin(a, ATOM),
                    coin(a, ATOM),
                    vec![],
                )
            })
            .collect();
        self.deps.querier.staking.update(ATOM, &validators, &delegations);

        let delegator = self.contract.to_string();
        for v in VALS {
            let amount = *self.c.rewards.get(v).unwrap_or(&0);
            let coins = if amount == 0 {
                vec![]
            } else {
                vec![DecCoin::new(Decimal256::from_atomics(amount, 0).unwrap(), ATOM)]
            };
            self.deps.querier.distribution.set_rewards(v, delegator.clone(), coins);
        }
        self.deps.querier.distribution.set_validators(delegator, VALS);

        let mut holders: Vec<Addr> = self.c.atom.keys().cloned().collect();
        for a in self.c.li.keys() {
            if !holders.contains(a) {
                holders.push(a.clone());
            }
        }
        for h in holders {
            let mut coins: Vec<Coin> = vec![];
            let a = self.atom_of(&h);
            let l = self.li_of(&h);
            if a > 0 {
                coins.push(coin(a, ATOM));
            }
            if l > 0 {
                coins.push(coin(l, self.liatom.clone()));
            }
            self.deps.querier.bank.update_balance(h.to_string(), coins);
        }
    }

    // ---- contract messages -> model ------------------------------------------------------------

    /// The distribution hook that runs whenever the contract's delegation changes.
    fn hook(&mut self, del: &str, val: &str) {
        if del == self.contract.as_str() {
            self.withdraw_rewards(val);
        }
    }

    fn withdraw_rewards(&mut self, val: &str) {
        let r = self.c.rewards.remove(val).unwrap_or(0);
        let c = self.contract.clone();
        self.c.add_atom(&c, r);
    }

    /// Runs messages the way wasmd runs a response: in order, depth first, replies included.
    fn exec_msgs(&mut self, msgs: Vec<SubMsg<TokenFactoryMsg>>) -> Result<(), String> {
        for m in msgs {
            self.exec_one(m)?;
        }
        Ok(())
    }

    fn exec_one(&mut self, m: SubMsg<TokenFactoryMsg>) -> Result<(), String> {
        let catches = matches!(m.reply_on, ReplyOn::Error | ReplyOn::Always);
        let snap = catches.then(|| self.c.clone());
        let outcome = self.dispatch(m.msg.clone());
        let result = match (outcome, m.reply_on.clone()) {
            (Ok(resp), ReplyOn::Success | ReplyOn::Always) => {
                SubMsgResult::Ok(SubMsgResponse {
                    events: vec![],
                    #[allow(deprecated)]
                    data: None,
                    msg_responses: resp,
                })
            }
            (Ok(_), _) => return Ok(()),
            (Err(e), ReplyOn::Error | ReplyOn::Always) => {
                self.c = snap.expect("snapshot");
                SubMsgResult::Err(e)
            }
            (Err(e), _) => return Err(e),
        };
        self.sync();
        let res = reply(
            self.deps.as_mut(),
            self.env.clone(),
            Reply { id: m.id, payload: m.payload.clone(), gas_used: 0, result },
        )
        .map_err(|e| format!("reply {} failed: {e}", m.id))?;
        self.exec_msgs(res.messages)?;
        self.sync();
        Ok(())
    }

    fn dispatch(&mut self, msg: CosmosMsg<TokenFactoryMsg>) -> Result<Vec<MsgResponse>, String> {
        let contract = self.contract.clone();
        let cstr = contract.to_string();
        let now = self.now();
        match msg {
            CosmosMsg::Custom(TokenFactoryMsg::CreateDenom { .. }) => {
                self.c.sub_atom(&contract, DENOM_FEE);
                self.expected_total -= DENOM_FEE;
            }
            // Bank metadata only, no balances change.
            CosmosMsg::Custom(TokenFactoryMsg::SetMetadata { .. }) => {}
            CosmosMsg::Custom(TokenFactoryMsg::MintTokens { amount, mint_to_address, .. }) => {
                self.add_li(&Addr::unchecked(mint_to_address), amount.u128());
            }
            CosmosMsg::Custom(TokenFactoryMsg::BurnTokens { amount, .. }) => {
                self.sub_li(&contract, amount.u128());
            }
            CosmosMsg::Staking(cosmwasm_std::StakingMsg::Delegate { validator, amount }) => {
                self.hook(&cstr, &validator);
                if self.atom_of(&contract) < amount.amount.u128() {
                    return Err("insufficient funds for delegation".into());
                }
                self.c.sub_atom(&contract, amount.amount.u128());
                self.c.delegate(&cstr, &validator, amount.amount.u128());
            }
            CosmosMsg::Staking(cosmwasm_std::StakingMsg::Redelegate {
                src_validator,
                dst_validator,
                amount,
            }) => {
                self.hook(&cstr, &src_validator);
                self.hook(&cstr, &dst_validator);
                let shares = self.c.validate_unbond(&cstr, &src_validator, amount.amount.u128())?;
                let out = self.c.unbond(&cstr, &src_validator, shares)?;
                self.c.delegate(&cstr, &dst_validator, out);
                self.c.redel_until.insert(dst_validator, now + UNBONDING_SECS);
            }
            CosmosMsg::Staking(cosmwasm_std::StakingMsg::Undelegate { validator, amount }) => {
                self.hook(&cstr, &validator);
                // Cosmos Hub allows at most 7 unbonding entries per delegator and validator.
                let live = self.c.unbonding.iter().filter(|(_, v, _)| v == &validator).count();
                if self.eb && live >= 7 {
                    // with a configured period shorter than the chain's, a halt can leave an eighth entry: the chain refuses it
                    return Err("too many unbonding entries".into());
                }
                assert!(live < 7, "7 unbonding entries already live on {validator}");
                let shares = self.c.validate_unbond(&cstr, &validator, amount.amount.u128())?;
                let out = self.c.unbond(&cstr, &validator, shares)?;
                let at = now + UNBONDING_SECS;
                self.c.unbonding.push((at, validator, out));
            }
            CosmosMsg::Distribution(cosmwasm_std::DistributionMsg::WithdrawDelegatorReward {
                validator,
            }) => self.withdraw_rewards(&validator),
            CosmosMsg::Bank(cosmwasm_std::BankMsg::Send { to_address, amount }) => {
                for c in amount {
                    if c.denom == ATOM {
                        if to_address == self.treasury.as_str() && self.c.treasury_blocked && self.roles < 2 {
                            return Err("treasury cannot receive funds".into());
                        }
                        if self.atom_of(&contract) < c.amount.u128() {
                            return Err("insufficient funds".into());
                        }
                        self.c.sub_atom(&contract, c.amount.u128());
                        self.c.add_atom(&Addr::unchecked(to_address.clone()), c.amount.u128());
                    } else {
                        let have = self.c.tok.get(&(cstr.clone(), c.denom.clone())).copied().unwrap_or(0);
                        if have < c.amount.u128() {
                            return Err(format!("insufficient {} balance", c.denom));
                        }
                        *self.c.tok.get_mut(&(cstr.clone(), c.denom.clone())).unwrap() -= c.amount.u128();
                        *self.c.tok.entry((to_address.clone(), c.denom.clone())).or_default() += c.amount.u128();
                    }
                }
            }
            CosmosMsg::Any(AnyMsg { type_url, value }) => {
                let f = pb_fields(value.as_slice());
                let get = |n: u8| f.iter().find(|(k, _)| *k == n).map(|(_, v)| v.clone()).unwrap();
                match type_url.as_str() {
                    "/gaia.liquid.v1beta1.MsgTokenizeShares" => {
                        let delegator = String::from_utf8(get(1)).unwrap();
                        let validator = String::from_utf8(get(2)).unwrap();
                        let amount = pb_coin(&get(3));
                        let owner = String::from_utf8(get(4)).unwrap();
                        assert_eq!(delegator, cstr, "the contract tokenizes only its own delegations");
                        assert_eq!(amount.denom, ATOM);
                        self.hook(&cstr, &validator);
                        let token = self.c.tokenize(now, &delegator, true, &validator, amount.amount.u128(), &owner)?;
                        return Ok(vec![tokenize_response(&token)]);
                    }
                    "/gaia.liquid.v1beta1.MsgRedeemTokensForShares" => {
                        let delegator = String::from_utf8(get(1)).unwrap();
                        let token = pb_coin(&get(2));
                        assert_eq!(delegator, cstr);
                        let val = token
                            .denom
                            .split_once('/')
                            .map(|(_, id)| id.parse::<u64>().ok())
                            .flatten()
                            .and_then(|id| self.c.records.get(&id).map(|(v, _)| v.clone()))
                            .ok_or("unknown share token")?;
                        // the delegation is created after the unbond, the hook runs before its shares change
                        self.hook(&cstr, &val);
                        let dep_before = self.c.delegation_tokens(&cstr, &val);
                        self.c.redeem(&delegator, &token)?;
                        let dep_after = self.c.delegation_tokens(&cstr, &val);
                        self.dep_obs = Some((val.clone(), dep_before, dep_after));
                    }
                    other => panic!("unexpected Any message {other}"),
                }
            }
            other => panic!("unexpected message {other:?}"),
        }
        Ok(vec![])
    }

    // ---- transactions ----------------------------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            storage: self.deps.storage.range(None, None, Order::Ascending).collect(),
            c: self.c.clone(),
            rf: self.rf.clone(),
        }
    }

    /// A failed transaction reverts everything, including contract storage writes.
    fn restore(&mut self, s: Snapshot) {
        let mut storage = MockStorage::default();
        for (k, v) in s.storage {
            storage.set(&k, &v);
        }
        self.deps.storage = storage;
        self.c = s.c;
        self.rf = s.rf;
    }

    fn tx_inner(&mut self, sender: &Addr, msg: ExecuteMsg, funds: Option<Coin>) -> bool {
        let snap = self.snapshot();
        let contract = self.contract.clone();
        if let Some(c) = &funds {
            if c.denom == ATOM {
                self.c.sub_atom(sender, c.amount.u128());
                self.c.add_atom(&contract, c.amount.u128());
            } else if c.denom == self.liatom {
                self.sub_li(sender, c.amount.u128());
                self.add_li(&contract, c.amount.u128());
            } else {
                let k = (sender.to_string(), c.denom.clone());
                *self.c.tok.get_mut(&k).expect("sender holds the share token") -= c.amount.u128();
                *self.c.tok.entry((contract.to_string(), c.denom.clone())).or_default() += c.amount.u128();
            }
        }
        self.sync();
        let info = message_info(sender, funds.as_slice());
        let label = format!("{msg:?}").split(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_string();
        self.last_err = None;
        let atom_before = self.c.atom.clone();
        match execute(self.deps.as_mut(), self.env.clone(), info, msg) {
            Ok(res) => match self.exec_msgs(res.messages) {
                Ok(()) => {
                    self.sync();
                    // sonnet12 outflow audit: ATOM leaves the contract only to the claimant (Claim) and the treasury
                    let t2 = self.deps.api.addr_make("treasury2");
                    for (a, bal) in self.c.atom.iter() {
                        if *a == contract {
                            continue;
                        }
                        let b0 = atom_before.get(a).copied().unwrap_or(0);
                        let ok = if *a == self.treasury || *a == t2 {
                            *bal >= b0
                        } else if a == sender && label == "Claim" {
                            *bal >= b0
                        } else {
                            *bal == b0
                        };
                        if !ok && self.audit_err.is_none() {
                            self.audit_err = Some(format!("ATOM of {a} went {b0} -> {bal} in a {label} tx by {sender}"));
                        }
                    }
                    true
                }
                Err(e) => {
                    note_fail(&label, &e);
                    self.last_err = Some(e);
                    self.restore(snap);
                    self.sync();
                    false
                }
            },
            Err(e) => {
                note_fail(&label, &e.to_string());
                self.last_err = Some(e.to_string());
                self.restore(snap);
                self.sync();
                false
            }
        }
    }

    fn keeper(&self) -> Addr { self.users[0].clone() }

    /// pct 1..=100: that percent of the user's liATOM; pct > 100: the raw amount pct - 100 (capped at the balance).
    fn amt_of(&self, u: &Addr, pct: u64) -> u128 {
        if pct > 100 { (pct as u128 - 100).min(self.li_of(u)) } else { self.li_of(u) * pct as u128 / 100 }
    }

    fn rate(&self) -> Decimal {
        let r: ExchangeRateResponse = cosmwasm_std::from_json(
            query(self.deps.as_ref(), self.env.clone(), QueryMsg::ExchangeRate {}).unwrap(),
        )
        .unwrap();
        r.rate
    }

    fn pool(&self) -> PoolResponse {
        cosmwasm_std::from_json(
            query(self.deps.as_ref(), self.env.clone(), QueryMsg::Pool {}).unwrap(),
        )
        .unwrap()
    }

    /// (pool value, effective supply): liATOM is worth `pool / supply` ATOM each.
    fn price(&self) -> (u128, u128) {
        let p = self.pool();
        (p.pool_value.u128(), (p.liatom_supply + p.pending_liatom).u128())
    }

    /// Everything `who` holds, in uatom, with liATOM at the given price.
    fn value_of(&self, who: &Addr, price: (u128, u128)) -> u128 {
        let w = who.to_string();
        let mut v = self.atom_of(who);
        for val in VALS {
            v += self.c.delegation_tokens(&w, val);
        }
        for ((holder, denom), amt) in &self.c.tok {
            if holder == &w && *amt > 0 {
                let id: u64 = denom.split_once('/').unwrap().1.parse().unwrap();
                if let Some((val, _)) = self.c.records.get(&id) {
                    // the SDK can issue the record a few shares fewer than the token amount: those cannot be redeemed
                    let rec = self.c.dels.get(&(format!("rec/{id}"), val.clone())).copied().unwrap_or(0);
                    v += self.c.tokens_from_shares(val, (amt * SCALE).min(rec));
                }
            }
        }
        v + muldiv(self.li_of(who), price.0, price.1)
    }

    fn batch(&self, id: u64) -> Option<Batch> {
        query(self.deps.as_ref(), self.env.clone(), QueryMsg::Batch { batch_id: id })
            .ok()
            .map(|b| cosmwasm_std::from_json(b).unwrap())
    }

    fn claim_of(&self, id: u64, user: &Addr) -> u128 {
        let q = QueryMsg::Claim { batch_id: id, address: user.to_string() };
        let b = query(self.deps.as_ref(), self.env.clone(), q).unwrap();
        cosmwasm_std::from_json::<Uint128>(b).unwrap().u128()
    }

    /// EndBlock of the current block: entries whose completion time has been reached are paid out.
    fn end_block(&mut self) {
        let now = self.env.block.time.seconds();
        let contract = self.contract.clone();
        let (matured, pending): (Vec<_>, Vec<_>) =
            self.c.unbonding.drain(..).partition(|(at, _, _)| *at <= now);
        self.c.unbonding = pending;
        for (_, _, amount) in matured {
            self.c.add_atom(&contract, amount);
        }
    }

    /// A new block: the previous one has ended (matured entries were paid out), unless time just jumped into this one.
    fn next_block(&mut self) {
        self.env.block.height += 1;
        if self.eb && !self.fresh {
            self.end_block();
            self.sync();
        }
        self.fresh = false;
    }

    /// Time passes with a block every 6 seconds (no transactions in between): everything that completed more than
    /// one block ago has been paid out, what completed in the last block is paid out after this block's transactions.
    fn advance(&mut self, secs: u64) {
        if self.eb {
            self.end_block();
            let target = self.now() + secs;
            let pre = target.saturating_sub(6).max(self.now());
            self.env.block.time = cosmwasm_std::Timestamp::from_seconds(pre);
            self.end_block();
            self.env.block.time = cosmwasm_std::Timestamp::from_seconds(target);
            self.fresh = true;
            self.sync();
        } else {
            self.advance_gap(secs);
        }
    }

    /// A gap without blocks (a chain halt): the first block after it executes its transactions before its EndBlock.
    fn advance_gap(&mut self, secs: u64) {
        if self.eb {
            self.fresh = true;
            // the block we leave ends: what matured by now is paid out after its transactions
            self.end_block();
            self.env.block.time = self.env.block.time.plus_seconds(secs);
        } else {
            self.env.block.time = self.env.block.time.plus_seconds(secs);
            self.end_block();
        }
        self.sync();
    }

    fn slash(&mut self, val: usize, bps: u64) {
        let v = VALS[val].to_string();
        let mut burned = 0u128;
        if let Some(e) = self.c.vals.get_mut(&v) {
            let cut = e.0 * bps as u128 / 10_000;
            e.0 -= cut;
            burned += cut;
        }
        let now = self.now();
        for (at, uv, amount) in self.c.unbonding.iter_mut() {
            // an entry that has completed is not slashed any more, even if its payout is still due at EndBlock
            if *uv == v && (!self.eb || *at > now) {
                let cut = *amount * bps as u128 / 10_000;
                *amount -= cut;
                burned += cut;
            }
        }
        if burned > 0 {
            self.slashed = true;
            self.expected_total -= burned;
        }
        self.sync();
    }

    /// The share tokens `user` holds, each capped at what the record's delegation can redeem (the SDK can
    /// mint a token or two more than the record has shares; the excess is stranded).
    fn user_tokens(&self, user: &Addr) -> Vec<(String, u128)> {
        self.c
            .tok
            .iter()
            .filter(|((h, _), a)| h == user.as_str() && **a > 0)
            .filter_map(|((_, d), a)| {
                let id: u64 = d.split_once('/')?.1.parse().ok()?;
                let (val, _) = self.c.records.get(&id)?;
                let rec = self.c.dels.get(&(format!("rec/{id}"), val.clone())).copied().unwrap_or(0);
                let cap = rec / SCALE;
                // redeeming exactly trunc(shares) takes the whole delegation, so cap at that
                let amt = (*a).min(cap);
                (amt > 0).then(|| (d.clone(), amt))
            })
            .collect()
    }

    // ---- operations ---------------------------------------------------------------------------------

    fn cfg(&self) -> Config {
        cosmwasm_std::from_json(query(self.deps.as_ref(), self.env.clone(), QueryMsg::Config {}).unwrap()).unwrap()
    }

    fn supply_total(&self) -> u128 {
        self.c.li.values().sum()
    }

    fn is_treasury_user(&self, u: &Addr) -> bool {
        self.roles >= 2 && *u == self.treasury
    }

    fn err_has(&self, needle: &str) -> bool {
        self.last_err.as_deref().is_some_and(|e| e.contains(needle))
    }

    fn run(&mut self, op: &Op) -> Result<(), TestCaseError> {
        if self.hold_height {
            self.hold_height = false;
        } else {
            self.next_block();
        }
        let rate_before = self.rate();
        let had_slash = self.slashed;
        let sup_before = self.supply_total();
        let pre_cfg = self.cfg();
        if pre_cfg.unbonding_secs != UNBONDING_SECS {
            self.mismatch = true;
        }
        let pre_pool = self.pool();
        let pre_price = self.price();
        // what the op did, for the post-conditions (sonnet12)
        let mut op_ok: Option<bool> = None;
        let mut op_amount: u128 = 0;
        let mut redeemed_before = self.c.redeemed.len();
        let keeper = self.users[0].clone();

        match op {
            Op::Deposit { user, amount } => {
                let u = self.users[*user].clone();
                let price = self.price();
                let before = self.value_of(&u, price);
                let ok = self.tx(&u, ExecuteMsg::Deposit {}, Some(coin(*amount as u128, ATOM)));
                op_ok = Some(ok);
                op_amount = *amount as u128;
                if pre_cfg.pause.is_some() {
                    prop_assert!(!ok && self.err_has("paused"), "Deposit under pause: ok={} err={:?}", ok, self.last_err);
                }
                if ok && !self.is_treasury_user(&u) {
                    let after = self.value_of(&u, price);
                    prop_assert!(after <= before + DUST_PER_PART, "deposit gained {} uatom", after - before);
                }
            }
            Op::Redeem { user, pct } => {
                let u = self.users[*user].clone();
                let amount = self.amt_of(&u, *pct);
                if amount > 0 {
                    let ok = self.tx(&u, ExecuteMsg::Redeem {}, Some(coin(amount, self.liatom.clone())));
                    op_ok = Some(ok);
                    op_amount = amount;
                    // property 5/8: Redeem is refused by the full pause and by nothing else
                    if pre_cfg.pause == Some(PauseMode::Full) {
                        prop_assert!(!ok && self.err_has("paused"), "Redeem under full pause: ok={} err={:?}", ok, self.last_err);
                    } else {
                        prop_assert!(ok, "Redeem failed without a full pause: {:?}", self.last_err);
                    }
                }
            }
            Op::Harvest => {
                let ok = self.tx(&keeper, ExecuteMsg::Harvest {}, None);
                prop_assert!(ok || !self.err_has("paused"), "Harvest refused as paused");
            }
            Op::Submit => {
                let ok = self.tx(&keeper, ExecuteMsg::SubmitUnbonding {}, None);
                prop_assert!(ok || !self.err_has("paused"), "Submit refused as paused");
                if ok {
                    // pricing: the batch is owed floor(burned * pool_value / effective supply) at the moment of the call
                    let id = self.last_submitted();
                    if let Some(b) = id.and_then(|i| self.batch(i)) {
                        let want = muldiv(b.liatom_burned.u128(), pre_price.0, pre_price.1);
                        prop_assert_eq!(b.atom_expected.unwrap().u128(), want, "batch priced wrong at submit");
                    }
                }
            }
            Op::Finalize { id } => {
                let b = self.batch(*id);
                let now = self.now();
                let arrived = self.c.unbonding.iter().all(|(at, _, _)| *at > now);
                let ok = self.tx(&keeper, ExecuteMsg::FinalizeBatch { batch_id: *id }, None);
                prop_assert!(ok || !self.err_has("paused"), "Finalize refused as paused");
                // liveness without slashing: once everything has arrived, a matured batch can always be finalized
                if let Some(b) = b {
                    if !self.slashed
                        && !self.mismatch
                        && arrived
                        && b.status == BatchStatus::Unbonding
                        && now >= b.mature_at.unwrap().seconds() + 60
                    {
                        prop_assert!(ok, "Finalize of batch {} failed with all ATOM arrived and no slash: {:?}", id, self.last_err);
                    }
                }
            }
            Op::Claim { user, id } => {
                let u = self.users[*user].clone();
                let before = self.atom_of(&u);
                let expected = self.batch(*id).and_then(|b| {
                    (b.status == BatchStatus::Matured).then(|| {
                        let share = self.claim_of(*id, &u);
                        b.atom_received.unwrap().u128() * share / b.liatom_burned.u128()
                    })
                });
                let ok = self.tx(&u, ExecuteMsg::Claim { batch_id: *id }, None);
                let delta = self.atom_of(&u) - before;
                *self.paid.entry(*id).or_default() += delta;
                if let Some(b) = self.batch(*id) {
                    let had_claim = !ok && b.status == BatchStatus::Matured && self.claim_of(*id, &u) > 0;
                    prop_assert!(!had_claim, "Claim of a matured batch with a claim failed: {:?}", self.last_err);
                }
                prop_assert!(ok || !self.err_has("paused"), "Claim refused as paused");
                // The payout is exactly the floored pro-rata share, never a uatom more.
                if let (true, Some(expected)) = (ok, expected) {
                    prop_assert_eq!(delta, expected, "claim in batch {} paid the wrong amount", id);
                }
            }
            Op::Accrue { val, amount } => {
                // rewards only exist for a delegation that is there
                if self.c.dels.contains_key(&(self.contract.to_string(), VALS[*val].to_string())) {
                    *self.c.rewards.entry(VALS[*val].to_string()).or_default() += *amount as u128;
                    self.accrued += *amount as u128;
                    self.expected_total += *amount as u128;
                    self.sync();
                }
            }
            Op::Advance { secs } => self.advance(*secs),
            Op::Slash { val, bps } => self.slash(*val, *bps),
            Op::Reweight { v } => {
                let sum: u64 = v.iter().sum();
                let mut bps: Vec<u16> = v.iter().map(|x| (x * 10_000 / sum) as u16).collect();
                let assigned: u16 = bps.iter().sum();
                bps[0] += 10_000 - assigned;
                let validators: Vec<ValidatorWeight> = VALS
                    .iter()
                    .zip(bps)
                    .map(|(name, weight_bps)| ValidatorWeight {
                        validator: name.to_string(),
                        weight_bps,
                    })
                    .collect();
                let admin = self.admin.clone();
                let targets: Vec<u128> = validators
                    .iter()
                    .map(|w| w.weight_bps as u128)
                    .collect();
                let ok = self.tx(&admin, ExecuteMsg::UpdateValidators { validators }, None);
                let before = self.deviation(&targets);
                let total_before = self.contract_tokens_total();
                let moved = self.tx(
                    &admin,
                    ExecuteMsg::Rebalance { min_move: Some(Uint128::one()) },
                    None,
                );
                // A rebalance must bring delegations closer to the weights, never further away.
                if ok && moved {
                    let after = self.deviation(&targets);
                    // The same call also re-delegates the rewards the redelegation paid out, which
                    // grows the total and can move every target by that amount.
                    let grown = self.contract_tokens_total().saturating_sub(total_before);
                    prop_assert!(
                        after <= before + 3 + 2 * grown,
                        "rebalance increased the deviation from {before} to {after}"
                    );
                }
            }
            Op::SetEpoch { secs } => {
                let admin = self.admin.clone();
                let msg = ExecuteMsg::UpdateConfig {
                    treasury: None,
                    fee_bps: None,
                    epoch_secs: Some(*secs),
                    unbonding_secs: None,
                    min_deposit: None,
                    max_tvl: None,
                };
                self.tx(&admin, msg, None);
            }
            Op::SetFee { bps } => {
                let admin = self.admin.clone();
                let msg = ExecuteMsg::UpdateConfig {
                    treasury: None,
                    fee_bps: Some(*bps),
                    epoch_secs: None,
                    unbonding_secs: None,
                    min_deposit: None,
                    max_tvl: None,
                };
                let ok = self.tx(&admin, msg, None);
                // a fee change harvests at the old fee first: the pool value and the price do not move
                let p1 = self.pool();
                prop_assert!(
                    p1.pool_value + Uint128::new(1) >= pre_pool.pool_value && p1.pool_value <= pre_pool.pool_value + Uint128::new(3),
                    "SetFee (ok={}) moved the pool value {} -> {}", ok, pre_pool.pool_value, p1.pool_value
                );
            }

            // ---- liquid staking module paths ----
            Op::RedeemShares { user, pct } => {
                let u = self.users[*user].clone();
                let amount = self.amt_of(&u, *pct);
                if amount > 0 {
                    let price = self.price();
                    let before = self.value_of(&u, price);
                    let stakes: Vec<u128> = VALS.iter().map(|v| self.contract_delegation(v)).collect();
                    let log_before = self.c.tokenized.len();
                    let ok = self.tx(
                        &u,
                        ExecuteMsg::RedeemToShares {},
                        Some(coin(amount, self.liatom.clone())),
                    );
                    TRY_EXIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    op_ok = Some(ok);
                    op_amount = amount;
                    if pre_cfg.pause == Some(PauseMode::Full) {
                        prop_assert!(!ok && self.err_has("paused"), "RedeemToShares under full pause: ok={} err={:?}", ok, self.last_err);
                    } else if !ok {
                        prop_assert!(!self.err_has("paused"), "RedeemToShares refused as paused without full pause");
                    }
                    if ok {
                        OK_EXIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if self.c.tokenized.len() - log_before > 1 { MULTI_PART_EXIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                        let after = self.value_of(&u, price);
                        let parts: Vec<(String, u128, u128)> = self.c.tokenized[log_before..]
                            .iter()
                            .map(|(_, v, asked, out, _)| (v.clone(), *asked, *out))
                            .collect();
                        let worth = muldiv(amount, price.0, price.1);
                        prop_assert!(
                            self.is_treasury_user(&u) || after <= before + DUST_PER_PART * parts.len() as u128,
                            "leaving as shares gained {} uatom (parts {:?})",
                            after.saturating_sub(before),
                            parts
                        );
                        // 7. pro rata over all validators
                        let total: u128 = stakes.iter().sum();
                        let asked: u128 = parts.iter().map(|(_, a, _)| *a).sum();
                        prop_assert_eq!(asked, worth, "the parts do not add up to the worth of the burned liATOM");
                        for (i, v) in VALS.iter().enumerate() {
                            let got: u128 = parts.iter().filter(|(pv, _, _)| pv == v).map(|(_, a, _)| *a).sum();
                            let fair = muldiv(worth, stakes[i], total);
                            prop_assert!(
                                got.abs_diff(fair) <= VALS.len() as u128 + 1 + 9 * VALS.len() as u128, // + folded dust parts (< 10 uatom each)
                                "{} gave {} but its pro rata part is {} (stakes {:?}, worth {})",
                                v, got, fair, stakes, worth
                            );
                        }
                    }
                }
            }
            Op::DepositShares { user, pick } => {
                let u = self.users[*user].clone();
                let held = self.user_tokens(&u);
                if !held.is_empty() {
                    let (denom, amt) = held[pick % held.len()].clone();
                    let price = self.price();
                    let before = self.value_of(&u, price);
                    let ok = self.tx(&u, ExecuteMsg::DepositTokenizedShares {}, Some(coin(amt, denom)));
                    TRY_ENTRY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    op_ok = Some(ok);
                    if pre_cfg.pause.is_some() {
                        prop_assert!(!ok && self.err_has("paused"), "DepositTokenizedShares under pause: ok={} err={:?}", ok, self.last_err);
                    }
                    if ok {
                        OK_ENTRY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let after = self.value_of(&u, price);
                        prop_assert!(
                            self.is_treasury_user(&u) || after <= before + 2 * DUST_PER_PART,
                            "tokenized deposit gained {} uatom",
                            after.saturating_sub(before)
                        );
                    }
                }
            }
            Op::UserDelegate { user, val, amount } => {
                let u = self.users[*user].clone();
                self.c.sub_atom(&u, *amount as u128);
                self.c.delegate(u.as_str(), VALS[*val], *amount as u128);
                self.sync();
            }
            Op::UserTokenize { user, val, pct } => {
                let u = self.users[*user].clone();
                let d = self.c.delegation_tokens(u.as_str(), VALS[*val]);
                let amt = d * *pct as u128 / 100;
                if amt > 0 {
                    let snap = self.c.clone();
                    let now = self.now();
                    if self.c.tokenize(now, u.as_str(), false, VALS[*val], amt, u.as_str()).is_err() {
                        self.c = snap;
                    }
                    self.sync();
                }
            }
            Op::UserRedeem { user, pick } => {
                let u = self.users[*user].clone();
                let held = self.user_tokens(&u);
                if !held.is_empty() {
                    let (denom, amt) = held[pick % held.len()].clone();
                    let snap = self.c.clone();
                    if self.c.redeem(u.as_str(), &coin(amt, denom)).is_err() {
                        self.c = snap;
                    }
                    self.sync();
                }
            }
            Op::DropValidator { val, rebalance } => {
                let kept: Vec<&str> = VALS.iter().enumerate().filter(|(i, _)| i != val).map(|(_, v)| *v).collect();
                let validators = vec![
                    ValidatorWeight { validator: kept[0].to_string(), weight_bps: 5000 },
                    ValidatorWeight { validator: kept[1].to_string(), weight_bps: 5000 },
                ];
                let admin = self.admin.clone();
                self.tx(&admin, ExecuteMsg::UpdateValidators { validators }, None);
                if let Some(min) = rebalance {
                    self.tx(&admin, ExecuteMsg::Rebalance { min_move: Some(Uint128::new(*min as u128)) }, None);
                }
            }
            Op::Pause { mode } => {
                let admin = self.admin.clone();
                let msg = match mode {
                    0 => ExecuteMsg::Unpause {},
                    1 => ExecuteMsg::Pause { mode: PauseMode::DepositsOnly },
                    _ => ExecuteMsg::Pause { mode: PauseMode::Full },
                };
                self.tx(&admin, msg, None);
            }
            Op::SetTvl { pct } => {
                let admin = self.admin.clone();
                let max = if *pct >= 300 {
                    Uint128::MAX
                } else {
                    Uint128::new(self.pool().pool_value.u128() * *pct as u128 / 100)
                };
                let msg = ExecuteMsg::UpdateConfig {
                    treasury: None,
                    fee_bps: None,
                    epoch_secs: None,
                    unbonding_secs: None,
                    min_deposit: None,
                    max_tvl: Some(max),
                };
                self.tx(&admin, msg, None);
                prop_assert_eq!(self.pool().pool_value, pre_pool.pool_value, "SetTvl moved the pool value");
            }
            Op::TreasuryBlocked { blocked } => self.c.treasury_blocked = *blocked,
            Op::Jump { secs } => {
                self.advance_gap(*secs);
                self.env.block.height += 5;
            }
            Op::SetUnbonding { secs } => {
                let admin = self.admin.clone();
                let msg = ExecuteMsg::UpdateConfig {
                    treasury: None, fee_bps: None, epoch_secs: None,
                    unbonding_secs: Some(*secs), min_deposit: None, max_tvl: None,
                };
                self.tx(&admin, msg, None);
            }
            Op::SetMinDeposit { v } => {
                let admin = self.admin.clone();
                let msg = ExecuteMsg::UpdateConfig {
                    treasury: None, fee_bps: None, epoch_secs: None,
                    unbonding_secs: None, min_deposit: Some(Uint128::new(*v as u128)), max_tvl: None,
                };
                self.tx(&admin, msg, None);
            }
            Op::SetTreasury { who } => {
                let admin = self.admin.clone();
                let t = if *who == 0 { self.treasury.clone() } else { self.deps.api.addr_make("treasury2") };
                let msg = ExecuteMsg::UpdateConfig {
                    treasury: Some(t.to_string()), fee_bps: None, epoch_secs: None,
                    unbonding_secs: None, min_deposit: None, max_tvl: None,
                };
                self.tx(&admin, msg, None);
            }
            Op::SettleAll => {
                let keeper = self.keeper();
                for _round in 0..2 {
                    for id in 1..=MAX_BATCH_PROBE {
                        if self.batch(id).is_some_and(|b| b.status == BatchStatus::Unbonding) {
                            self.tx(&keeper, ExecuteMsg::FinalizeBatch { batch_id: id }, None);
                        }
                    }
                    self.next_block();
                }
            }
            Op::ClaimAll { user } => {
                let u = self.users[*user].clone();
                for id in 1..=MAX_BATCH_PROBE {
                    let before = self.atom_of(&u);
                    let ok = self.tx(&u, ExecuteMsg::Claim { batch_id: id }, None);
                    if ok {
                        *self.paid.entry(id).or_default() += self.atom_of(&u) - before;
                    }
                }
            }
            Op::CapBlocked { val, blocked } => {
                if *blocked {
                    self.c.cap_blocked.insert(VALS[*val].to_string());
                } else {
                    self.c.cap_blocked.remove(VALS[*val]);
                }
            }

            // ---- sonnet12 ----
            Op::SameBlock => {
                self.hold_height = true;
            }
            Op::AdvEpoch => self.advance(1_001),
            Op::AdvUnbond => self.advance(UNBONDING_SECS + 461),
            Op::DonateAtom { user, amount } => {
                let u = self.users[*user].clone();
                let contract = self.contract.clone();
                let a = (*amount as u128).min(self.atom_of(&u));
                self.c.sub_atom(&u, a);
                self.c.add_atom(&contract, a);
                self.sync();
                let p1 = self.pool();
                prop_assert_eq!(p1.pool_value, pre_pool.pool_value, "an ATOM donation moved the pool value");
                prop_assert_eq!(p1.liatom_supply, pre_pool.liatom_supply);
                prop_assert_eq!(self.rate(), rate_before, "an ATOM donation moved the rate");
            }
            Op::DonateLi { user, pct } => {
                let u = self.users[*user].clone();
                let contract = self.contract.clone();
                let a = self.amt_of(&u, *pct);
                self.sub_li(&u, a);
                self.add_li(&contract, a);
                self.donated_li += a;
                self.sync();
                let p1 = self.pool();
                prop_assert_eq!(p1.pool_value, pre_pool.pool_value, "a liATOM donation moved the pool value");
                prop_assert_eq!(p1.liatom_supply, pre_pool.liatom_supply);
                prop_assert_eq!(self.rate(), rate_before, "a liATOM donation moved the rate");
            }
            Op::Combo { user, amount } => {
                let ids = |s: &Sim| -> Vec<u64> {
                    (1..=MAX_BATCH_PROBE).filter(|i| s.batch(*i).is_some_and(|b| b.status == BatchStatus::Unbonding)).collect()
                };
                self.hold_height = true;
                self.run(&Op::Deposit { user: *user, amount: *amount })?;
                self.hold_height = true;
                self.run(&Op::Redeem { user: *user, pct: 100 })?;
                self.hold_height = true;
                self.run(&Op::Submit)?;
                for id in ids(self) {
                    self.hold_height = true;
                    self.run(&Op::Finalize { id })?;
                }
                for id in 1..=MAX_BATCH_PROBE {
                    if self.batch(id).is_some_and(|b| b.status == BatchStatus::Matured) {
                        self.hold_height = true;
                        self.run(&Op::Claim { user: *user, id })?;
                    }
                }
                return Ok(());
            }
            Op::LsmThenRedeem { user, pick } => {
                self.hold_height = true;
                self.run(&Op::DepositShares { user: *user, pick: *pick })?;
                self.hold_height = true;
                self.run(&Op::Redeem { user: *user, pct: 100 })?;
                self.hold_height = true;
                self.run(&Op::Submit)?;
                return Ok(());
            }
            Op::Loop { user, pct } => {
                let u = self.users[*user].clone();
                let price = self.price();
                let before = self.value_of(&u, price);
                self.hold_height = true;
                self.run(&Op::RedeemShares { user: *user, pct: *pct })?;
                for pick in 0..3 {
                    self.hold_height = true;
                    self.run(&Op::UserRedeem { user: *user, pick })?;
                }
                for val in 0..VALS.len() {
                    self.hold_height = true;
                    self.run(&Op::UserTokenize { user: *user, val, pct: 100 })?;
                }
                for pick in 0..3 {
                    self.hold_height = true;
                    self.run(&Op::DepositShares { user: *user, pick })?;
                }
                let after = self.value_of(&u, price);
                prop_assert!(
                    self.is_treasury_user(&u) || after <= before + 12 * DUST_PER_PART,
                    "wash loop gained {} uatom",
                    after.saturating_sub(before)
                );
                return Ok(());
            }
            Op::OrderCheck => {
                let ids: Vec<u64> = (1..=MAX_BATCH_PROBE)
                    .filter(|i| self.batch(*i).is_some_and(|b| b.status == BatchStatus::Unbonding))
                    .collect();
                if !ids.is_empty() {
                    let h0 = self.env.block.height;
                    let fresh0 = self.fresh;
                    let mut outcomes: Vec<Vec<(u64, BatchStatus, Option<u128>)>> = vec![];
                    for variant in 0..3usize {
                        let snap = self.snapshot();
                        self.env.block.height = h0;
                        self.fresh = fresh0;
                        let caller = if variant == 1 { self.users[2].clone() } else { self.keeper() };
                        let mut order = ids.clone();
                        match variant {
                            1 => order.reverse(),
                            2 => order.rotate_left(ids.len() / 2),
                            _ => {}
                        }
                        for _round in 0..3 {
                            self.next_block();
                            for id in &order {
                                self.tx(&caller, ExecuteMsg::FinalizeBatch { batch_id: *id }, None);
                            }
                        }
                        let out: Vec<_> = ids
                            .iter()
                            .map(|i| {
                                let b = self.batch(*i).unwrap();
                                (*i, b.status, b.atom_received.map(|r| r.u128()))
                            })
                            .collect();
                        outcomes.push(out);
                        self.restore(snap);
                        self.sync();
                    }
                    self.env.block.height = h0;
                    self.fresh = fresh0;
                    {
                        use std::sync::atomic::Ordering::Relaxed;
                        static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                        static MULTI: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                        static SETTLED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                        static SHORT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                        let n = RUNS.fetch_add(1, Relaxed) + 1;
                        if ids.len() > 1 { MULTI.fetch_add(1, Relaxed); }
                        if outcomes[0].iter().any(|(_, st, _)| *st == BatchStatus::Matured) { SETTLED.fetch_add(1, Relaxed); }
                        // a short settlement: some settled batch received less than the others' ratio implies; approximate by slashed
                        if self.slashed && outcomes[0].iter().any(|(_, st, _)| *st == BatchStatus::Matured) { SHORT.fetch_add(1, Relaxed); }
                        if n % 500 == 0 {
                            eprintln!("COVERAGE order-check runs {} multi-batch {} settled {} settled-after-slash {}", n, MULTI.load(Relaxed), SETTLED.load(Relaxed), SHORT.load(Relaxed));
                        }
                    }
                    prop_assert!(
                        outcomes[0] == outcomes[1] && outcomes[0] == outcomes[2],
                        "finalize outcome depends on order/caller: {:?}",
                        outcomes
                    );
                }
                return Ok(());
            }
        }

        if let Some(e) = self.audit_err.take() {
            return Err(TestCaseError::fail(e));
        }
        self.check_invariants(&format!("{op:?}"))?;

        // supply audit: liATOM supply changes only through Deposit (mint at the current rate), DepositTokenizedShares
        // (mint at the current rate for the delegation the redemption added), Redeem and RedeemToShares (burn exactly the attached amount).
        let sup_after = self.supply_total();
        match (op, op_ok) {
            (Op::Deposit { .. }, Some(true)) => {
                prop_assert_eq!(
                    sup_after - sup_before,
                    muldiv(op_amount, pre_price.1, pre_price.0),
                    "Deposit minted other than floor(amount * supply / pool_value)"
                );
            }
            (Op::DepositShares { .. }, Some(true)) => {
                let out: u128 = self.c.redeemed[redeemed_before..]
                    .iter()
                    .filter(|(d, _, _)| d == self.contract.as_str())
                    .map(|(_, _, o)| *o)
                    .sum();
                let minted = sup_after - sup_before;
                // never more than the delegation the redemption added is worth at the price before it (1 uatom of rounding)
                prop_assert!(
                    minted * pre_price.0 <= (out + 1) * pre_price.1,
                    "DepositTokenizedShares minted {} for {} uatom redeemed at {:?}",
                    minted, out, pre_price
                );
                prop_assert!(minted > 0);
            }
            (Op::Redeem { .. }, Some(true)) | (Op::RedeemShares { .. }, Some(true)) => {
                prop_assert_eq!(sup_before - sup_after, op_amount, "burn is not the attached amount");
            }
            (Op::DonateAtom { .. }, _) | (Op::DonateLi { .. }, _) => {
                prop_assert_eq!(sup_after, sup_before, "a donation changed the supply");
            }
            _ => {
                prop_assert_eq!(sup_after, sup_before, "liATOM supply changed in {:?}", op);
            }
        }
        // property 8: the cap holds for every successful deposit
        if matches!(op, Op::Deposit { .. } | Op::DepositShares { .. }) && op_ok == Some(true) {
            let p1 = self.pool();
            prop_assert!(
                p1.pool_value <= pre_cfg.max_tvl,
                "deposit left the pool at {} over the cap {}",
                p1.pool_value,
                pre_cfg.max_tvl
            );
        }
        redeemed_before = self.c.redeemed.len();
        let _ = redeemed_before;

        // 3. The rate never decreases without slashing.
        let after = self.rate();
        if !had_slash && !self.slashed {
            prop_assert!(
                after >= rate_before,
                "rate fell from {rate_before} to {after} after {op:?}"
            );
        } else if !matches!(op, Op::Slash { .. }) {
            // after a slash the rate must still never fall, except for chain rounding (a few uatom per validator touched)
            let (_, eff) = self.price();
            let tol = Decimal::from_ratio(12u128, eff);
            prop_assert!(
                after + tol >= rate_before,
                "rate fell from {rate_before} to {after} (tolerance {tol}) after {op:?} with an old slash"
            );
        }
        // 4. The seed (and any liATOM donated to the contract) stays with the contract.
        let contract = self.contract.clone();
        prop_assert_eq!(self.li_of(&contract), SEED + self.donated_li);
        Ok(())
    }

    fn last_submitted(&self) -> Option<u64> {
        CURRENT_BATCH.load(&self.deps.storage).ok().and_then(|c| c.checked_sub(1))
    }

    /// Sum of |delegation - target| over all validators, targets given as weights in bps.
    fn deviation(&self, weights_bps: &[u128]) -> u128 {
        let total = self.contract_tokens_total();
        VALS.iter()
            .enumerate()
            .map(|(i, v)| {
                let target = total * weights_bps[i] / 10_000;
                self.contract_delegation(v).abs_diff(target)
            })
            .sum()
    }

    fn check_invariants(&self, after: &str) -> Result<(), TestCaseError> {
        // 1. Conservation.
        let held: u128 = self.c.atom.values().sum();
        let delegated: u128 = self.c.vals.values().map(|(t, _)| *t).sum();
        let rewards: u128 = self.c.rewards.values().sum();
        let unbonding: u128 = self.c.unbonding.iter().map(|(_, _, a)| *a).sum();
        prop_assert_eq!(
            held + delegated + rewards + unbonding,
            self.expected_total,
            "value not conserved after {}",
            after
        );

        // fee: the treasuries together never hold more than the maximum fee (20%) of all rewards ever accrued
        if self.roles < 2 {
            let t2 = self.deps.api.addr_make("treasury2");
            let got = self.atom_of(&self.treasury) + self.atom_of(&t2);
            prop_assert!(got <= self.accrued * 2_000 / 10_000 + 2, "treasury holds {} of {} accrued rewards after {}", got, self.accrued, after);
        }
        // sonnet12 accounting invariants on the contract's own storage
        {
            let pool = self.pool();
            let cur = CURRENT_BATCH.load(&self.deps.storage).unwrap();
            let open = self.batch(cur).unwrap();
            prop_assert_eq!(open.status, BatchStatus::Open, "current batch is not open after {}", after);
            prop_assert_eq!(pool.pending_liatom, open.liatom_burned, "pending_liatom != liATOM of the open batch after {}", after);
            let c_supply: u128 = self.c.li.values().sum();
            prop_assert_eq!(pool.liatom_supply.u128(), c_supply, "supply query != sum of balances");
            // claims of batches that are not matured add up to what the batch burned
            let mut unb_ids: Vec<u64> = vec![];
            let mut owed_expect: i128 = 0;
            for id in 1..=cur {
                let b = self.batch(id).unwrap();
                match b.status {
                    BatchStatus::Open | BatchStatus::Unbonding => {
                        let sum: u128 = self.users.iter().map(|u| self.claim_of(id, u)).sum();
                        prop_assert_eq!(sum, b.liatom_burned.u128(), "claims of batch {} != burned after {}", id, after);
                        if b.status == BatchStatus::Unbonding {
                            unb_ids.push(id);
                        }
                    }
                    BatchStatus::Matured => {
                        owed_expect += b.atom_received.unwrap().u128() as i128 - *self.paid.get(&id).unwrap_or(&0) as i128;
                    }
                }
            }
            let mut stored = UNBONDING.load(&self.deps.storage).unwrap();
            stored.sort_unstable();
            prop_assert_eq!(stored, unb_ids, "UNBONDING list != batches in Unbonding status after {}", after);
            let owed = OWED_TO_REDEEMERS.load(&self.deps.storage).unwrap().u128() as i128;
            prop_assert_eq!(owed, owed_expect, "OWED_TO_REDEEMERS != sum(received - paid) of matured batches after {}", after);
            prop_assert!(
                self.atom_of(&self.contract) as i128 >= owed,
                "balance {} below owed {} after {}", self.atom_of(&self.contract), owed, after
            );
        }
        // 8. The chain model itself is consistent.
        for (v, (_, shares)) in &self.c.vals {
            let sum: u128 = self.c.dels.iter().filter(|((_, dv), _)| dv == v).map(|(_, s)| *s).sum();
            prop_assert_eq!(sum, *shares, "delegations do not add up to the shares of {}", v);
        }
        for (id, (v, _)) in &self.c.records {
            prop_assert!(
                self.c.dels.contains_key(&(format!("rec/{id}"), v.clone())),
                "record {} has no delegation after {}",
                id,
                after
            );
        }

        // 2 + 5. Solvency and claim bounds on finalized batches.
        let mut outstanding = 0u128;
        for id in 1..=MAX_BATCH_PROBE {
            let Some(b) = self.batch(id) else { continue };
            if b.status != BatchStatus::Matured {
                continue;
            }
            let received = b.atom_received.expect("matured batch has atom_received").u128();
            if !self.slashed {
                // tolerance: chain rounding is nil here (no slash => 1 token per share)
                prop_assert_eq!(
                    received,
                    b.atom_expected.unwrap().u128(),
                    "batch {} underpaid without any slash after {}", id, after
                );
            }
            let mut batch_claims = 0u128;
            for u in &self.users {
                let c = self.claim_of(id, u);
                batch_claims += c;
                outstanding += received * c / b.liatom_burned.u128().max(1);
            }
            let paid = *self.paid.get(&id).unwrap_or(&0);
            prop_assert!(
                paid <= received,
                "batch {} paid {} but received only {} after {}",
                id,
                paid,
                received,
                after
            );
            prop_assert!(
                batch_claims <= b.liatom_burned.u128(),
                "claims {} exceed burned {} in batch {} after {}",
                batch_claims,
                b.liatom_burned,
                id,
                after
            );
        }
        let balance = self.atom_of(&self.contract);
        prop_assert!(
            balance >= outstanding,
            "insolvent after {}: balance {} < owed {}",
            after,
            balance,
            outstanding
        );

        // The pool never reports more owed than the contract could pay once everything matured.
        let pool = self.pool();
        prop_assert!(pool.pool_value.u128() > 0, "empty pool after {}", after);
        Ok(())
    }
}


// ---- differential testing glue: the reference model (tests/refmodel/mod.rs) watches every contract transaction ----
impl Sim {
    fn ref_env(&self) -> refmodel::Env {
        let t2 = self.deps.api.addr_make("treasury2");
        let cur_treasury_is_first = self.cfg().treasury == self.treasury;
        refmodel::Env {
            now: self.now(),
            height: self.env.block.height,
            stake: VALS.iter().map(|v| (v.to_string(), self.contract_delegation(v))).collect(),
            rewards: VALS.iter().map(|v| (v.to_string(), *self.c.rewards.get(*v).unwrap_or(&0))).collect(),
            supply: self.c.li.values().sum(),
            balance: self.atom_of(&self.contract),
            treasury: self.atom_of(&self.treasury) + self.atom_of(&t2),
            treasury_blocked: self.c.treasury_blocked && self.roles < 2 && cur_treasury_is_first,
            skip_treasury: self.roles >= 2,
            slashed: self.slashed,
        }
    }

    fn ref_view(&self) -> refmodel::View {
        let p = self.pool();
        let cur = CURRENT_BATCH.load(&self.deps.storage).unwrap();
        let mut batches = BTreeMap::new();
        let mut claims = BTreeMap::new();
        for id in 1..=cur {
            let b = self.batch(id).unwrap();
            batches.insert(id, refmodel::BView {
                burned: b.liatom_burned.u128(),
                status: match b.status { BatchStatus::Open => 0, BatchStatus::Unbonding => 1, BatchStatus::Matured => 2 },
                expected: b.atom_expected.map(|x| x.u128()),
                received: b.atom_received.map(|x| x.u128()),
                mature_at: b.mature_at.map(|t| t.seconds()),
                undeleg: b.undelegations,
            });
            for u in &self.users {
                let c = self.claim_of(id, u);
                if c > 0 { claims.insert((id, u.to_string()), c); }
            }
        }
        refmodel::View {
            pending: p.pending_liatom.u128(),
            owed: OWED_TO_REDEEMERS.load(&self.deps.storage).unwrap().u128(),
            delegated: p.delegated.u128(),
            unclaimed: p.unclaimed_rewards.u128(),
            accrued_fee: p.accrued_fee.u128(),
            pool_value: p.pool_value.u128(),
            supply: p.liatom_supply.u128(),
            rate18: self.rate().atomics().u128(),
            cur,
            batches,
            claims,
        }
    }

    fn to_call(&self, sender: &Addr, msg: &ExecuteMsg, funds: &Option<Coin>) -> refmodel::Call {
        use refmodel::Call;
        let who = sender.to_string();
        let amt = funds.as_ref().map(|c| c.amount.u128()).unwrap_or(0);
        match msg {
            ExecuteMsg::Deposit {} => Call::Deposit { who, amount: amt },
            ExecuteMsg::Redeem {} => Call::Redeem { who, amount: amt },
            ExecuteMsg::RedeemToShares {} => Call::RedeemToShares { who, amount: amt },
            ExecuteMsg::DepositTokenizedShares {} => Call::DepositShares {
                who,
                validator: funds.as_ref().and_then(|c| c.denom.split('/').next()).unwrap_or("").to_string(),
            },
            ExecuteMsg::Claim { batch_id } => Call::Claim { who, id: *batch_id },
            ExecuteMsg::Harvest {} => Call::Harvest,
            ExecuteMsg::SubmitUnbonding {} => Call::Submit,
            ExecuteMsg::FinalizeBatch { batch_id } => Call::Finalize { id: *batch_id },
            ExecuteMsg::Pause { mode } => Call::Pause(match mode { PauseMode::DepositsOnly => 1, PauseMode::Full => 2 }),
            ExecuteMsg::Unpause {} => Call::Pause(0),
            ExecuteMsg::UpdateValidators { validators } => Call::UpdateValidators(
                validators.iter().map(|w| (w.validator.clone(), w.weight_bps as u128)).collect(),
            ),
            ExecuteMsg::UpdateConfig { treasury, fee_bps, epoch_secs, unbonding_secs, min_deposit, max_tvl } => {
                let n = treasury.is_some() as u8 + fee_bps.is_some() as u8 + epoch_secs.is_some() as u8
                    + unbonding_secs.is_some() as u8 + min_deposit.is_some() as u8 + max_tvl.is_some() as u8;
                if n != 1 { return Call::Other; }
                if let Some(f) = fee_bps { Call::SetFee(*f as u128) }
                else if let Some(e) = epoch_secs { Call::SetEpoch(*e) }
                else if let Some(u) = unbonding_secs { Call::SetUnbonding(*u) }
                else if let Some(m) = min_deposit { Call::SetMinDeposit(m.u128()) }
                else if let Some(m) = max_tvl { Call::SetMaxTvl(m.u128()) }
                else { Call::Other }
            }
            _ => Call::Other,
        }
    }

    fn report_diffs(&mut self, call: &refmodel::Call, diffs: Vec<String>) {
        if diffs.is_empty() { return; }
        use std::io::Write;
        let mut text = format!("DISAGREEMENT after {call:?}:\n");
        for d in &diffs { text.push_str(&format!("  - {d}\n")); }
        text.push_str("  recent transactions:\n");
        for t in self.trace.iter().rev().take(25).rev() { text.push_str(&format!("    {t}\n")); }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            std::env::var("DIFF_LOG").unwrap_or_else(|_| "/tmp/diff_log.txt".into()),
        ) {
            let _ = writeln!(f, "{text}");
        }
        if std::env::var("DIFF_STRICT").map(|v| v != "0").unwrap_or(true) {
            panic!("{text}");
        }
    }

    /// Every transaction goes through the reference model: prediction before, comparison after.
    fn tx(&mut self, sender: &Addr, msg: ExecuteMsg, funds: Option<Coin>) -> bool {
        let call = self.to_call(sender, &msg, &funds);
        self.sync();
        // the contract's view of the world must still equal the model's before anything happens
        let pre = self.ref_env();
        let view0 = self.ref_view();
        let d0 = self.rf.compare_view(&pre, &view0);
        self.report_diffs(&call, d0.into_iter().map(|s| format!("(before the call) {s}")).collect());

        let sender_before = self.atom_of(sender) as i128;
        let unb_len = self.c.unbonding.len();
        let tok_len = self.c.tokenized.len();
        self.dep_obs = None;
        let ok = self.tx_inner(sender, msg, funds);
        let post = self.ref_env();
        let obs = refmodel::Obs {
            ok,
            err: self.last_err.clone().unwrap_or_default(),
            new_unbonding: if ok { self.c.unbonding[unb_len.min(self.c.unbonding.len())..].iter().map(|(_, v, a)| (v.clone(), *a)).collect() } else { vec![] },
            tokenized: if ok { self.c.tokenized[tok_len.min(self.c.tokenized.len())..].iter().map(|(_, v, asked, _, _)| (v.clone(), *asked)).collect() } else { vec![] },
            dep: self.dep_obs.clone(),
            sender_delta: self.atom_of(sender) as i128 - sender_before,
        };
        self.trace.push(format!("{call:?} -> ok={ok} err={:?} t={} h={}", obs.err.chars().take(60).collect::<String>(), pre.now, pre.height));
        let view = self.ref_view();
        let diffs = self.rf.step(&call, &pre, &post, &obs, &view);
        self.report_diffs(&call, diffs);
        ok
    }
}

fn run_sequence(ops: Vec<Op>) -> Result<(), TestCaseError> {
    run_sequence_with(ops, false)
}

fn run_sequence_with(ops: Vec<Op>, whale: bool) -> Result<(), TestCaseError> {
    let mut sim = Sim::new_with(whale);
    for op in &ops {
        sim.run(op)?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(64), max_shrink_iters: 2000, ..ProptestConfig::default() })]

    /// Without slashing the rate is monotonic and value is conserved.
    #[test]
    fn rate_is_monotonic_and_value_is_conserved(ops in prop::collection::vec(op_strategy(false, false), 1..60)) {
        run_sequence(ops)?;
    }

    /// With slashing, value is still conserved and the contract stays solvent.
    #[test]
    fn conservation_and_solvency_hold_with_slashing(ops in prop::collection::vec(op_strategy(true, false), 1..60)) {
        run_sequence(ops)?;
    }

    /// Both liquid staking paths, several validators, share/token drift, caps, pauses, a treasury that refuses
    /// funds, validators dropped from the weight list: no slashing, so the rate must be monotonic.
    #[test]
    fn lsm_paths_keep_the_rate_monotonic(ops in prop::collection::vec(op_strategy(false, true), 1..80)) {
        run_sequence(ops)?;
    }

    /// The same with other delegators holding most of every validator (realistic sizes).
    #[test]
    fn lsm_paths_with_large_validators(ops in prop::collection::vec(op_strategy(true, true), 1..80)) {
        run_sequence_with(ops, true)?;
    }

    /// The same with slashing (shares and tokens per share drift apart): value is conserved, the pool solvent,
    /// nobody gains from the operations.
    #[test]
    fn lsm_paths_conserve_value_with_slashing(ops in prop::collection::vec(op_strategy(true, true), 1..80)) {
        run_sequence(ops)?;
    }
}

#[test]
fn zz_diff_report() {
    eprintln!("==== reference model histograms (value -> count) ====");
    for (k, h) in refmodel::HIST.lock().unwrap().iter() {
        eprintln!("{k}: {h:?}");
    }
    eprintln!("==== reference model info/ambiguity counters ====");
    for (k, (n, ex)) in refmodel::INFO.lock().unwrap().iter() {
        eprintln!("{n:7}  {k}   e.g. {ex}");
    }
}

#[test]
fn zz_coverage_report() {
    // run after the proptests when invoked with --test-threads=1
    use std::sync::atomic::Ordering::Relaxed;
    for (k, n) in FAILS.lock().unwrap().iter() {
        eprintln!("{n:6}  {k}");
    }
    eprintln!("exit ok/try {}/{} (multi-part {}), entry ok/try {}/{}", OK_EXIT.load(Relaxed), TRY_EXIT.load(Relaxed), MULTI_PART_EXIT.load(Relaxed), OK_ENTRY.load(Relaxed), TRY_ENTRY.load(Relaxed));
}

/// A fixed regression sequence that touches every operation, runs without proptest shrinking.
#[test]
fn smoke_sequence() {
    let ops = vec![
        Op::Deposit { user: 0, amount: 3_000_000_000 },
        Op::Deposit { user: 1, amount: 1_234_567_890 },
        Op::Accrue { val: 0, amount: 100_000_000 },
        Op::Harvest,
        Op::Redeem { user: 0, pct: 50 },
        Op::Redeem { user: 1, pct: 100 },
        Op::Submit,
        // the contract finalizes 60 s after maturity (the chain releases the ATOM at the end of that block)
        Op::Advance { secs: UNBONDING_SECS + 61 },
        Op::Finalize { id: 1 },
        Op::Claim { user: 0, id: 1 },
        Op::Claim { user: 1, id: 1 },
        Op::Claim { user: 1, id: 1 },
    ];
    SEQ_FAILS.with(|c| c.set(0));
    run_sequence(ops).unwrap();
    let failed = SEQ_FAILS.with(|c| c.get());
    assert_eq!(failed, SMOKE_EXPECTED_FAILS, "the fixed sequence must run as written");
}

/// Both liquid staking paths with three validators, slashing and rewards, in a fixed order.
#[test]
fn lsm_smoke_sequence() {
    let ops = vec![
        Op::Deposit { user: 0, amount: 3_000_000_000 },
        Op::Deposit { user: 1, amount: 1_234_567_890 },
        Op::Deposit { user: 2, amount: 2_000_000_000 },
        Op::Slash { val: 1, bps: 777 },
        Op::Accrue { val: 0, amount: 100_000_000 },
        Op::Accrue { val: 2, amount: 3_000_001 },
        Op::UserDelegate { user: 2, val: 1, amount: 2_000_000_000 },
        Op::UserTokenize { user: 2, val: 1, pct: 60 },
        Op::DepositShares { user: 2, pick: 0 },
        Op::RedeemShares { user: 0, pct: 40 },
        Op::UserRedeem { user: 0, pick: 0 },
        Op::UserRedeem { user: 0, pick: 1 },
        Op::UserRedeem { user: 0, pick: 2 },
        Op::Redeem { user: 1, pct: 50 },
        Op::Submit,
        Op::RedeemShares { user: 1, pct: 100 },
        Op::Advance { secs: UNBONDING_SECS + 61 },
        Op::Finalize { id: 1 },
        Op::Claim { user: 1, id: 1 },
    ];
    SEQ_FAILS.with(|c| c.set(0));
    run_sequence(ops).unwrap();
    let failed = SEQ_FAILS.with(|c| c.get());
    assert_eq!(failed, LSM_SMOKE_EXPECTED_FAILS, "the fixed sequence must run as written");
}

proptest! {
    /// Settling matured batches never pays more than expected or more than is available, and the
    /// share of each batch does not depend on the order the batches are listed in.
    #[test]
    fn settle_matured_is_bounded_and_order_independent(
        expected in proptest::collection::vec(0u128..10u128.pow(15), 1..8),
        available in 0u128..8 * 10u128.pow(15),
        rot in 0usize..8,
    ) {
        use liatom_hub::rate::settle_matured;
        let exp: Vec<Uint128> = expected.iter().map(|e| Uint128::new(*e)).collect();
        let (shares, shortfall) = settle_matured(Uint128::new(available), &exp);

        let total: u128 = expected.iter().sum();
        prop_assert_eq!(shortfall, available < total);
        let paid: u128 = shares.iter().map(|s| s.u128()).sum();
        prop_assert!(paid <= available.min(total));
        for (s, e) in shares.iter().zip(&expected) {
            prop_assert!(s.u128() <= *e);
        }
        if !shortfall {
            prop_assert_eq!(&shares, &exp);
        }

        // Same batches in another order: each batch gets the same amount.
        let mut rotated = exp.clone();
        let n = rotated.len();
        rotated.rotate_left(rot % n);
        let (rshares, _) = settle_matured(Uint128::new(available), &rotated);
        let mut back = rshares.clone();
        back.rotate_right(rot % n);
        prop_assert_eq!(back, shares);
    }
}

// ======================= audit extension (sonnet11) =======================

impl Sim {
    /// From whatever state the sequence ended in: with the admin acting honestly (unpause, working treasury),
    /// the keeper calling its three messages, every redeemer must be able to get ATOM out.
    fn drain_check(&mut self) -> Result<(), TestCaseError> {
        let admin = self.admin.clone();
        let keeper = self.keeper();
        self.c.treasury_blocked = false;
        self.tx(&admin, ExecuteMsg::Unpause {}, None);
        let t = self.treasury.clone();
        self.tx(&admin, ExecuteMsg::UpdateConfig {
            treasury: Some(t.to_string()), fee_bps: None, epoch_secs: None, unbonding_secs: None,
            min_deposit: None, max_tvl: None }, None);
        for _round in 0..8 {
            self.next_block();
            self.tx(&keeper, ExecuteMsg::Harvest {}, None);
            self.next_block();
            self.advance(EPOCH_SECS.max(1_001));
            self.tx(&keeper, ExecuteMsg::SubmitUnbonding {}, None);
            self.next_block();
            self.advance(UNBONDING_SECS + 604_800 + 1_000);
            for _ in 0..3 {
                self.next_block();
                for id in 1..=MAX_BATCH_PROBE {
                    if self.batch(id).is_some_and(|b| b.status == BatchStatus::Unbonding) {
                        self.tx(&keeper, ExecuteMsg::FinalizeBatch { batch_id: id }, None);
                    }
                }
            }
        }
        let users = self.users.clone();
        for u in users {
            for id in 1..=MAX_BATCH_PROBE {
                let before = self.atom_of(&u);
                if self.tx(&u, ExecuteMsg::Claim { batch_id: id }, None) {
                    *self.paid.entry(id).or_default() += self.atom_of(&u) - before;
                }
            }
        }
        // nothing may be stuck
        let p = self.pool();
        prop_assert!(p.pending_liatom.is_zero(), "liATOM still pending in an open batch after the drain: {}", p.pending_liatom);
        for id in 1..=MAX_BATCH_PROBE {
            let Some(b) = self.batch(id) else { continue };
            prop_assert!(b.status != BatchStatus::Unbonding, "batch {} still unbonding after the drain", id);
            for u in &self.users {
                if b.status == BatchStatus::Matured {
                    prop_assert!(self.claim_of(id, u) == 0, "unclaimed claim in batch {} for {}", id, u);
                }
            }
        }
        self.check_invariants("drain")?;
        static SHORT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        static DRAINS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let d = DRAINS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let mut any_short = false;
        for id in 1..=MAX_BATCH_PROBE {
            if let Some(b) = self.batch(id) {
                if b.status == BatchStatus::Matured && b.atom_received.unwrap() < b.atom_expected.unwrap() { any_short = true; }
            }
        }
        if any_short { SHORT.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
        if d % 100 == 0 { eprintln!("COVERAGE drains {} with a short-settled batch {}", d, SHORT.load(std::sync::atomic::Ordering::Relaxed)); }
        Ok(())
    }
}

fn run_sequence_ext(ops: Vec<Op>, whale: bool, drain: bool) -> Result<(), TestCaseError> {
    let mut sim = Sim::new_with(whale);
    for op in &ops {
        sim.run(op)?;
    }
    if drain {
        sim.drain_check()?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(64), max_shrink_iters: 3000, ..ProptestConfig::default() })]

    #[test]
    fn x_noslash_ext(ops in prop::collection::vec(op_strategy_x(false, true, true), 1..90)) {
        run_sequence_ext(ops, false, true)?;
    }
    #[test]
    fn x_slash_ext(ops in prop::collection::vec(op_strategy_x(true, true, true), 1..90)) {
        run_sequence_ext(ops, false, true)?;
    }
    #[test]
    fn x_slash_ext_whale(ops in prop::collection::vec(op_strategy_x(true, true, true), 1..90)) {
        run_sequence_ext(ops, true, true)?;
    }
}


// ======================= audit extension (sonnet12) =======================

fn s12_new_ops() -> BoxedStrategy<Op> {
    let arms: Vec<(u32, BoxedStrategy<Op>)> = vec![
        (4, Just(Op::SameBlock).boxed()),
        (2, (0..USERS.len(), 1u64..3_000_000).prop_map(|(user, amount)| Op::DonateAtom { user, amount }).boxed()),
        (2, (0..USERS.len(), 1u64..=100).prop_map(|(user, pct)| Op::DonateLi { user, pct }).boxed()),
        (3, (0..USERS.len(), 1_000_000u64..2_000_000_000).prop_map(|(user, amount)| Op::Combo { user, amount }).boxed()),
        (3, (0..USERS.len(), 0usize..4).prop_map(|(user, pick)| Op::LsmThenRedeem { user, pick }).boxed()),
        (3, (0..USERS.len(), 1u64..=100).prop_map(|(user, pct)| Op::Loop { user, pct }).boxed()),
        (3, Just(Op::OrderCheck).boxed()),
        (6, Just(Op::AdvEpoch).boxed()),
        (6, Just(Op::AdvUnbond).boxed()),
        (5, (0..USERS.len(), 1_000_000u64..2_000_000_000).prop_map(|(user, amount)| Op::Deposit { user, amount }).boxed()),
        (5, (0..USERS.len(), 1u64..=100).prop_map(|(user, pct)| Op::Redeem { user, pct }).boxed()),
        (4, Just(Op::Submit).boxed()),
        (4, (1u64..8).prop_map(|id| Op::Finalize { id }).boxed()),
        (3, (0..VALS.len(), 1u64..=500_000).prop_map(|(val, amount)| Op::Accrue { val, amount }).boxed()),
    ];
    prop::strategy::Union::new_weighted(arms).boxed()
}

fn op_strategy_s12(with_slash: bool) -> BoxedStrategy<Op> {
    prop::strategy::Union::new_weighted(vec![
        (5, op_strategy_x(with_slash, true, true)),
        (4, s12_new_ops()),
    ])
    .boxed()
}

fn run_sequence_s12(ops: Vec<Op>, whale: bool, eb: bool, roles: u8, drain: bool) -> Result<(), TestCaseError> {
    let mut sim = Sim::new_cfg(whale, eb, roles);
    for op in &ops {
        sim.run(op)?;
    }
    if drain {
        sim.drain_check()?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(64), max_shrink_iters: 3000, ..ProptestConfig::default() })]

    /// realistic EndBlock arrival, no slashing
    #[test]
    fn s12_eb_noslash(ops in prop::collection::vec(op_strategy_s12(false), 1..90)) {
        run_sequence_s12(ops, false, true, 0, true)?;
    }
    /// realistic EndBlock arrival, with slashing, large validators
    #[test]
    fn s12_eb_slash_whale(ops in prop::collection::vec(op_strategy_s12(true), 1..90)) {
        run_sequence_s12(ops, true, true, 0, true)?;
    }
    /// realistic EndBlock arrival, with slashing, small validators
    #[test]
    fn s12_eb_slash_small(ops in prop::collection::vec(op_strategy_s12(true), 1..90)) {
        run_sequence_s12(ops, false, true, 0, true)?;
    }
    /// admin = keeper = depositor = redeemer
    #[test]
    fn s12_same_actor(ops in prop::collection::vec(op_strategy_s12(true), 1..90)) {
        run_sequence_s12(ops, false, true, 1, true)?;
    }
    /// treasury = admin = keeper = depositor = redeemer
    #[test]
    fn s12_same_actor_treasury(ops in prop::collection::vec(op_strategy_s12(true), 1..90)) {
        run_sequence_s12(ops, true, true, 2, true)?;
    }
    /// the old (arrival at the start of the block) timing model with the new operations and checks
    #[test]
    fn s12_old_timing(ops in prop::collection::vec(op_strategy_s12(true), 1..90)) {
        run_sequence_s12(ops, false, false, 0, true)?;
    }
}
