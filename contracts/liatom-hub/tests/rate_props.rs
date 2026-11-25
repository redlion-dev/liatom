//! Property tests and extreme-input tests for the pure functions of `src/rate.rs` (review of e896e13).
//!
//! `finding_*` tests are expected to FAIL on the current code (a panic or a violated documented property).
//! Each one notes whether the contract can reach the input on the chain; see the review report.

use std::collections::BTreeMap;

use cosmwasm_std::{Decimal, Uint128, Uint256};
use liatom_hub::error::ContractError;
use liatom_hub::rate::{
    atom_for_liatom, exchange_rate, mint_amount, pick_validator, plan_rebalance, settle_matured,
    split_by_stake, split_fee, validate_timing, validate_weights, MAX_UNBONDING_EPOCH_RATIO,
};
use liatom_hub::state::{ValidatorWeight, MAX_EPOCH_SECS, MAX_UNBONDING_SECS, MAX_VALIDATORS};
use proptest::prelude::*;

const REALISTIC_MAX: u128 = 1_000_000_000_000_000; // 1e15 uatom (1e9 ATOM), far above the real supply

fn w(v: &str, bps: u16) -> ValidatorWeight {
    ValidatorWeight { validator: v.to_string(), weight_bps: bps }
}
fn d(v: &str, a: u128) -> (String, Uint128) {
    (v.to_string(), Uint128::new(a))
}
fn u(x: u128) -> Uint128 {
    Uint128::new(x)
}

/// Realistic amounts (uatom, liATOM) up to 1e9 ATOM.
fn realistic() -> impl Strategy<Value = u128> {
    0u128..=REALISTIC_MAX
}

/// Realistic amounts mixed with the extremes a pure function must survive.
fn any_amount() -> impl Strategy<Value = u128> {
    prop_oneof![
        1 => Just(0u128),
        1 => Just(1u128),
        1 => Just(2u128),
        1 => Just(u128::MAX),
        1 => Just(u128::MAX / 2),
        1 => Just(u128::MAX / 2 + 1),
        1 => Just(1u128 << 63),
        4 => realistic(),
        2 => any::<u128>(),
    ]
}

/// A weight list that the contract would accept: `n` unique names, each weight at least 1, summing to 10000.
fn valid_weights(n: usize, raw: &[u32]) -> Vec<ValidatorWeight> {
    let sum: u64 = raw[..n].iter().map(|x| *x as u64).sum();
    let mut bps: Vec<u64> = raw[..n].iter().map(|x| (*x as u64) * 10_000 / sum).collect();
    for b in bps.iter_mut() {
        if *b == 0 {
            *b = 1;
        }
    }
    let total: u64 = bps.iter().sum();
    if total > 10_000 {
        // Shave the largest entry; the validity check below catches any remaining mismatch.
        let i = (0..n).max_by_key(|i| bps[*i]).unwrap();
        bps[i] -= total - 10_000;
    } else {
        let i = (0..n).max_by_key(|i| bps[*i]).unwrap();
        bps[i] += 10_000 - total;
    }
    (0..n).map(|i| w(&format!("val{i}"), bps[i] as u16)).collect()
}

// ---------- mint_amount ----------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// No panic for any inputs. A minted amount is exactly floor(deposit * supply / pool): never more than
    /// the exact share, and never a unit less than it.
    #[test]
    fn mint_is_floor_of_the_exact_share(dep in any_amount(), sup in any_amount(), pool in any_amount()) {
        match mint_amount(u(dep), u(sup), u(pool)) {
            Ok(m) => {
                if sup == 0 {
                    prop_assert_eq!(m, u(dep));
                } else {
                    prop_assert!(!m.is_zero());
                    let exact_num = Uint256::from(dep) * Uint256::from(sup);
                    let p = Uint256::from(pool);
                    prop_assert!(Uint256::from(m) * p <= exact_num, "minted more than the exact share");
                    prop_assert!((Uint256::from(m) + Uint256::from(1u128)) * p > exact_num, "rounded down by more than one unit");
                }
            }
            Err(ContractError::ZeroMint {}) => {}
            Err(ContractError::EmptyPool {}) => prop_assert!(sup != 0 && pool == 0),
            Err(other) => prop_assert!(false, "unexpected error {other:?}"),
        }
    }

    /// More ATOM never mints less liATOM (same supply and pool).
    #[test]
    fn mint_is_monotone_in_the_deposit(a in realistic(), b in realistic(), sup in 1u128..=REALISTIC_MAX, pool in 1u128..=REALISTIC_MAX) {
        let (lo, hi) = (a.min(b), a.max(b));
        if let (Ok(x), Ok(y)) = (mint_amount(u(lo), u(sup), u(pool)), mint_amount(u(hi), u(sup), u(pool))) {
            prop_assert!(x <= y);
        }
    }

    /// Deposit, then leave at the price after the deposit: the holder never gets back more than was put in.
    #[test]
    fn deposit_then_redeem_never_gains(dep in realistic(), sup in 1u128..=REALISTIC_MAX, pool in 1u128..=REALISTIC_MAX) {
        if let Ok(m) = mint_amount(u(dep), u(sup), u(pool)) {
            let back = atom_for_liatom(m, u(pool + dep), u(sup + m.u128()));
            prop_assert!(back <= u(dep));
        }
    }
}

// ---------- atom_for_liatom / exchange_rate --------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// For liATOM within the supply, the payout is exactly floor(liatom * pool / supply) and never above the pool.
    #[test]
    fn atom_for_liatom_is_floor_and_within_the_pool(l in any_amount(), p in any_amount(), s in any_amount()) {
        let r = atom_for_liatom(u(l), u(p), u(s));
        if s == 0 {
            prop_assert!(r.is_zero());
        } else if l <= s {
            prop_assert!(r <= u(p));
            prop_assert_eq!(Uint256::from(r), Uint256::from(l) * Uint256::from(p) / Uint256::from(s));
        }
    }

    /// Realistic inputs never make `Decimal::from_ratio` panic.
    #[test]
    fn exchange_rate_is_total_on_realistic_inputs(p in realistic(), s in realistic()) {
        let r = exchange_rate(u(p), u(s));
        if s == 0 {
            prop_assert_eq!(r, Decimal::one());
        } else {
            // r = atomics / 1e18, rounded down, so s * r <= p
            let scale = Uint256::from(10u128.pow(18));
            prop_assert!(Uint256::from(s) * Uint256::from(r.atomics().u128()) <= Uint256::from(p) * scale);
        }
    }
}

// ---------- pick_validator -------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// The returned validator has the largest deficit (target minus current, after the deposit).
    #[test]
    fn pick_validator_takes_the_largest_deficit(
        n in 1usize..=50,
        raw in prop::collection::vec(1u32..=100, 50),
        stakes in prop::collection::vec(realistic(), 0..=50),
        dep in realistic(),
    ) {
        let weights = valid_weights(n, &raw);
        let delegations: Vec<(String, Uint128)> = stakes.iter().enumerate()
            .map(|(i, s)| (format!("val{i}"), u(*s))).collect();
        let total: i128 = stakes.iter().sum::<u128>() as i128 + dep as i128;
        let deficit = |name: &str| -> i128 {
            let target = total * weights.iter().find(|x| x.validator == name).map(|x| x.weight_bps as i128).unwrap_or(0) / 10_000;
            let current: i128 = delegations.iter().filter(|(v, _)| v == name).map(|(_, a)| a.u128() as i128).sum();
            target - current
        };
        let picked = pick_validator(&weights, &delegations, u(dep)).expect("non-empty weights");
        let best = weights.iter().map(|x| deficit(&x.validator)).max().unwrap();
        prop_assert_eq!(deficit(&picked), best);
    }
}

#[test]
fn pick_validator_on_empty_weights_is_none() {
    assert_eq!(pick_validator(&[], &[d("a", 5)], u(5)), None);
}

/// Deposits are below 2^127 on the chain (the supply is ~5e14 uatom), so `as i128` never wraps in practice.
/// Still, with `deposit = u128::MAX` the cast gives -1 and the function silently picks "b" (target 0 for both)
/// instead of "a" (the 60% target). Severity Info: unreachable on chain, and no funds move.
#[test]
#[ignore = "documented: unreachable with a real supply (about 5e14 uatom); see audit-prep limit 22"]
fn finding_pick_validator_casts_a_huge_deposit_to_negative() {
    let ws = [w("a", 6000), w("b", 4000)];
    assert_eq!(pick_validator(&ws, &[], u(u128::MAX)).as_deref(), Some("a"));
}

/// `total * weight_bps` overflows i128 once the total passes ~1.7e34 uatom. Panics in debug and in the release
/// profile (overflow-checks = true). Unreachable on chain. Severity Info.
#[test]
#[ignore = "documented: unreachable with a real supply (about 5e14 uatom); see audit-prep limit 22"]
fn finding_pick_validator_panics_on_a_huge_total() {
    let ws = [w("a", 6000), w("b", 4000)];
    assert!(pick_validator(&ws, &[], u(u128::MAX / 2)).is_some());
}

// ---------- split_fee ------------------------------------------------------------------------

proptest! {
    #[test]
    fn split_fee_adds_up_and_rounds_down(gross in any_amount(), bps in 0u16..=2_000) {
        let (fee, net) = split_fee(u(gross), bps);
        prop_assert_eq!(fee + net, u(gross));
        let exact = Uint256::from(gross) * Uint256::from(bps as u128) / Uint256::from(10_000u128);
        prop_assert_eq!(Uint256::from(fee.u128()), exact);
    }
}

/// `fee_bps` above 10000 makes the fee larger than the gross and `gross - fee` panics. The contract caps fee_bps
/// at 2000 in both instantiate and update_config, so this is a precondition, not a reachable path. Severity Info.
#[test]
#[ignore = "documented: unreachable with a real supply (about 5e14 uatom); see audit-prep limit 22"]
fn finding_split_fee_panics_above_100_percent() {
    let (fee, net) = split_fee(u(100), 20_000);
    assert!(fee <= u(100) && net <= u(100));
}

// ---------- split_by_stake -------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// Parts sum to exactly the amount, none exceeds its stake, none is zero, and an amount above the total is refused.
    #[test]
    fn split_by_stake_sums_to_amount_within_stakes(stakes in prop::collection::vec(realistic(), 1..=50), frac in 0u128..=1000) {
        let total: u128 = stakes.iter().sum();
        let named: Vec<(String, Uint128)> = stakes.iter().enumerate().map(|(i, s)| (format!("v{i}"), u(*s))).collect();
        let amount = total * frac / 1000;
        let parts = split_by_stake(u(amount), &named).expect("amount is at most the total");
        let sum: u128 = parts.iter().map(|(_, a)| a.u128()).sum();
        prop_assert_eq!(sum, amount);
        for (v, a) in &parts {
            prop_assert!(!a.is_zero());
            let cap = named.iter().find(|(n, _)| n == v).unwrap().1;
            prop_assert!(*a <= cap);
        }
    }

    #[test]
    fn split_by_stake_refuses_more_than_the_total(stakes in prop::collection::vec(realistic(), 1..=50), extra in 1u128..=1000) {
        let total: u128 = stakes.iter().sum();
        let named: Vec<(String, Uint128)> = stakes.iter().enumerate().map(|(i, s)| (format!("v{i}"), u(*s))).collect();
        prop_assert!(split_by_stake(u(total + extra), &named).is_none());
    }
}

#[test]
fn split_by_stake_extremes() {
    let half = u128::MAX / 2;
    // two stakes that together fit in u128
    let parts = split_by_stake(u(half + half - 1), &[d("a", half), d("b", half)]).unwrap();
    assert_eq!(parts.iter().map(|(_, a)| a.u128()).sum::<u128>(), half + half - 1);
    assert!(split_by_stake(u(u128::MAX), &[d("a", half), d("b", half)]).is_none());
    assert_eq!(split_by_stake(u(0), &[d("a", 3)]).unwrap(), vec![]);
    assert_eq!(split_by_stake(u(0), &[]).unwrap(), vec![]);
    // duplicate names are returned as separate parts; the sum still holds (callers must not rely on uniqueness)
    let dup = split_by_stake(u(10), &[d("a", 5), d("a", 5)]).unwrap();
    assert_eq!(dup.iter().map(|(_, a)| a.u128()).sum::<u128>(), 10);
}

/// Three stakes of u128::MAX/2 do not fit in a Uint128 total. `Sum` panics on the overflow in debug and release.
/// Not reachable on chain (the total stake is ~5e14 uatom). Severity Info.
#[test]
#[ignore = "documented: unreachable with a real supply (about 5e14 uatom); see audit-prep limit 22"]
fn finding_split_by_stake_panics_when_the_total_overflows() {
    let half = u128::MAX / 2;
    let r = split_by_stake(u(1), &[d("a", half), d("b", half), d("c", half)]);
    assert!(r.is_some());
}

// ---------- settle_matured -------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// Full payout when the balance covers the batches. Otherwise each batch gets floor(expected * available / total):
    /// the shares sum to at most `available` and to more than `available - n`, and a larger expectation never gets less.
    #[test]
    fn settle_matured_is_pro_rata_and_never_pays_more_than_available(
        avail in realistic(),
        expected in prop::collection::vec(realistic(), 1..=8),
    ) {
        let total: u128 = expected.iter().sum();
        let exp: Vec<Uint128> = expected.iter().map(|e| u(*e)).collect();
        let (shares, short) = settle_matured(u(avail), &exp);
        if avail >= total {
            prop_assert!(!short);
            prop_assert_eq!(shares, exp);
        } else {
            prop_assert!(short);
            let s: u128 = shares.iter().map(|x| x.u128()).sum();
            prop_assert!(s <= avail, "paid {s} from {avail}");
            prop_assert!(s + expected.len() as u128 > avail, "rounding lost more than one unit per batch");
            for i in 0..exp.len() {
                prop_assert!(shares[i] <= exp[i]);
                for j in 0..exp.len() {
                    if exp[i] <= exp[j] {
                        prop_assert!(shares[i] <= shares[j]);
                    }
                }
            }
        }
    }
}

/// The saturating sum of the expectations hides an overflow: with [MAX, MAX] the shortfall is computed against
/// MAX, and each batch then gets MAX*(MAX-1)/MAX = MAX-1, so 2*(MAX-1) is paid from MAX-1. Not reachable on chain
/// (the batches hold ~5e14 uatom in total). Severity Info.
#[test]
#[ignore = "documented: unreachable with a real supply (about 5e14 uatom); see audit-prep limit 22"]
fn finding_settle_matured_overpays_when_expectations_overflow() {
    let (shares, _) = settle_matured(u(u128::MAX - 1), &[u(u128::MAX), u(u128::MAX)]);
    let paid: Uint256 = shares.iter().map(|x| Uint256::from(x.u128())).sum();
    assert!(paid <= Uint256::from(u128::MAX - 1), "paid {paid}");
}

// ---------- plan_rebalance -------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// For weights the contract accepts, with delegations to any validators (some not in the list):
    /// stake is conserved, every move is positive, between different validators and at least `min_move`,
    /// the move count is capped, and the end state is the target: each weighted validator holds its floor
    /// (one of them the rounding remainder too) and every removed validator is drained.
    #[test]
    fn rebalance_conserves_stake_and_reaches_the_targets(
        n in 1usize..=50,
        raw in prop::collection::vec(1u32..=100, 50),
        stakes in prop::collection::vec(realistic(), 1..=50),
        extra in prop::collection::vec(realistic(), 0..=3),
        min_move in prop_oneof![Just(1u128), Just(1_000_000u128), realistic()],
        cap in prop_oneof![Just(10_000usize), 0usize..=12],
    ) {
        let weights = valid_weights(n, &raw);
        let mut delegations: Vec<(String, Uint128)> = stakes.iter().take(n).enumerate()
            .map(|(i, s)| (format!("val{i}"), u(*s))).collect();
        for (j, e) in extra.iter().enumerate() {
            delegations.push((format!("gone{j}"), u(*e)));
        }
        let total: u128 = delegations.iter().map(|(_, a)| a.u128()).sum();
        let plan = plan_rebalance(&weights, &delegations, u(min_move), cap);
        prop_assert!(plan.len() <= cap);

        let mut after: BTreeMap<String, u128> = delegations.iter().map(|(v, a)| (v.clone(), a.u128())).collect();
        for (from, to, amt) in &plan {
            prop_assert!(from != to);
            prop_assert!(amt.u128() >= min_move);
            let f = after.get_mut(from).unwrap();
            prop_assert!(*f >= amt.u128());
            *f -= amt.u128();
            *after.entry(to.clone()).or_default() += amt.u128();
        }
        prop_assert_eq!(after.values().sum::<u128>(), total);

        if cap == 10_000 && min_move == 1 {
            // Full plan: the end state is the target
            let mut mismatched = 0;
            for wt in &weights {
                let floor = total * wt.weight_bps as u128 / 10_000;
                let rem = total - weights.iter().map(|x| total * x.weight_bps as u128 / 10_000).sum::<u128>();
                let got = after.get(&wt.validator).copied().unwrap_or(0);
                if got != floor {
                    mismatched += 1;
                    prop_assert_eq!(got, floor + rem);
                }
            }
            prop_assert!(mismatched <= 1);
            for (v, a) in &after {
                if !weights.iter().any(|x| &x.validator == v) {
                    prop_assert_eq!(*a, 0, "{} not drained", v);
                }
            }
        }
    }
}

#[test]
fn rebalance_extremes_on_the_chain_scale() {
    let ws = [w("a", 3000), w("b", 3000), w("c", 4000)];
    let big = u128::MAX / 4;
    let plan = plan_rebalance(&ws, &[d("a", big), d("b", 0), d("c", 0), d("x", 7)], u(1), 100);
    let moved: u128 = plan.iter().map(|(_, _, a)| a.u128()).sum();
    assert!(moved <= big + 7);
    assert!(plan.iter().all(|(f, t, _)| f != t));
    assert!(plan_rebalance(&ws, &[], u(1), 8).is_empty());
}

/// Weights above 10000 bps are refused by `validate_weights` before they are stored, so `plan_rebalance` is never
/// given them on chain. If it were, `total - assigned` underflows and the call panics. Severity Info (precondition).
#[test]
#[ignore = "documented: unreachable with a real supply (about 5e14 uatom); see audit-prep limit 22"]
fn finding_rebalance_panics_when_weights_exceed_10000() {
    let ws = [w("a", 10_000), w("b", 10_000)];
    let plan = plan_rebalance(&ws, &[d("a", 100), d("b", 100)], u(1), 8);
    assert!(plan.is_empty());
}

// ---------- validate_weights / validate_timing ----------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// Matches the spec exactly: 1..=50 entries, unique names, non-zero weights, sum 10000.
    #[test]
    fn validate_weights_matches_the_spec(
        entries in prop::collection::vec((any::<u16>(), 0u8..=60), 0..=60),
    ) {
        let ws: Vec<ValidatorWeight> = entries.iter().map(|(b, i)| w(&format!("v{i}"), *b)).collect();
        let mut names = std::collections::BTreeSet::new();
        let unique = ws.iter().all(|x| names.insert(x.validator.clone()));
        let spec = !ws.is_empty() && ws.len() <= MAX_VALIDATORS && unique
            && ws.iter().all(|x| x.weight_bps != 0)
            && ws.iter().map(|x| x.weight_bps as u64).sum::<u64>() == 10_000;
        prop_assert_eq!(validate_weights(&ws).is_ok(), spec);
    }

    /// Every valid split of 10000 over 1..=50 validators is accepted; adding one bp to any of them is refused.
    #[test]
    fn valid_splits_are_accepted_and_perturbed_ones_refused(n in 1usize..=50, raw in prop::collection::vec(1u32..=100, 50), k in 0usize..50) {
        let ws = valid_weights(n, &raw);
        prop_assert!(validate_weights(&ws).is_ok());
        let mut bad = ws.clone();
        bad[k % n].weight_bps += 1;
        prop_assert!(validate_weights(&bad).is_err());
    }
}

#[test]
fn fifty_one_validators_are_refused() {
    let make = |n: usize| -> Vec<ValidatorWeight> {
        let mut v: Vec<ValidatorWeight> = (0..n).map(|i| w(&format!("val{i}"), 1)).collect();
        v[0].weight_bps += (10_000 - n) as u16;
        v
    };
    assert!(validate_weights(&make(MAX_VALIDATORS)).is_ok());
    assert!(validate_weights(&make(MAX_VALIDATORS + 1)).is_err());
    // 1000 entries of u16::MAX do not overflow the u32 sum check and are refused
    let huge: Vec<ValidatorWeight> = (0..1000).map(|i| w(&format!("v{i}"), u16::MAX)).collect();
    assert!(validate_weights(&huge).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// Matches the spec: positive epoch and unbonding, unbonding <= 6 epochs (InvalidTiming first), then the
    /// upper bounds (TimingTooLong).
    #[test]
    fn validate_timing_matches_the_spec(
        e in prop_oneof![Just(0u64), Just(1u64), Just(u64::MAX), Just(MAX_EPOCH_SECS), any::<u64>(), 0u64..=MAX_EPOCH_SECS * 2],
        un in prop_oneof![Just(0u64), Just(1u64), Just(u64::MAX), Just(MAX_UNBONDING_SECS), any::<u64>(), 0u64..=MAX_UNBONDING_SECS * 2],
    ) {
        let ratio_ok = e != 0 && un != 0 && (un as u128) <= 6 * (e as u128);
        let range_ok = e <= MAX_EPOCH_SECS && un <= MAX_UNBONDING_SECS;
        let expected = if !ratio_ok {
            Err(ContractError::InvalidTiming { max_ratio: MAX_UNBONDING_EPOCH_RATIO })
        } else if !range_ok {
            Err(ContractError::TimingTooLong { max_epoch: MAX_EPOCH_SECS, max_unbonding: MAX_UNBONDING_SECS })
        } else {
            Ok(())
        };
        prop_assert_eq!(validate_timing(e, un), expected);
    }
}

#[test]
fn timing_extremes_do_not_overflow() {
    assert_eq!(
        validate_timing(u64::MAX, u64::MAX),
        Err(ContractError::TimingTooLong { max_epoch: MAX_EPOCH_SECS, max_unbonding: MAX_UNBONDING_SECS })
    );
    assert!(validate_timing(1, u64::MAX).is_err());
    assert!(validate_timing(u64::MAX / 6, u64::MAX).is_err());
}
