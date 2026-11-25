//! Pure math: exchange rate and validator selection. No chain access, easy to fuzz.

use cosmwasm_std::{Decimal, Uint128, Uint256};

use crate::error::ContractError;
use crate::state::{ValidatorWeight, MAX_BPS, MAX_EPOCH_SECS, MAX_UNBONDING_SECS, MAX_VALIDATORS};

/// liATOM minted for `deposit` ATOM.
/// `pool_value` and `supply` are measured BEFORE the deposit. Rounds down (in favour of the protocol).
pub fn mint_amount(
    deposit: Uint128,
    supply: Uint128,
    pool_value: Uint128,
) -> Result<Uint128, ContractError> {
    if supply.is_zero() {
        return Ok(deposit);
    }
    if pool_value.is_zero() {
        return Err(ContractError::EmptyPool {});
    }
    let minted = Uint256::from(deposit) * Uint256::from(supply) / Uint256::from(pool_value);
    let minted = Uint128::try_from(minted).map_err(|_| ContractError::ZeroMint {})?;
    if minted.is_zero() {
        return Err(ContractError::ZeroMint {});
    }
    Ok(minted)
}

/// ATOM per liATOM. 1.0 while nothing is minted.
pub fn exchange_rate(pool_value: Uint128, supply: Uint128) -> Decimal {
    if supply.is_zero() {
        Decimal::one()
    } else {
        Decimal::from_ratio(pool_value, supply)
    }
}

/// ATOM paid out for `liatom` at the given rate. Rounds down.
pub fn atom_for_liatom(liatom: Uint128, pool_value: Uint128, supply: Uint128) -> Uint128 {
    if supply.is_zero() {
        return Uint128::zero();
    }
    let v = Uint256::from(liatom) * Uint256::from(pool_value) / Uint256::from(supply);
    Uint128::try_from(v).unwrap_or(Uint128::MAX)
}

/// Validator whose stake is furthest below its target after the deposit.
pub fn pick_validator(
    weights: &[ValidatorWeight],
    delegations: &[(String, Uint128)],
    deposit: Uint128,
) -> Option<String> {
    let current = |v: &str| -> i128 {
        delegations
            .iter()
            .filter(|(addr, _)| addr == v)
            .map(|(_, a)| a.u128() as i128)
            .sum()
    };
    let total: i128 = delegations.iter().map(|(_, a)| a.u128() as i128).sum::<i128>()
        + deposit.u128() as i128;

    weights
        .iter()
        .map(|w| {
            let target = total * w.weight_bps as i128 / MAX_BPS as i128;
            (w.validator.clone(), target - current(&w.validator))
        })
        .max_by_key(|(_, deficit)| *deficit)
        .map(|(v, _)| v)
}

/// Protocol fee on gross rewards. Returns `(fee, net)`; fee rounds down.
pub fn split_fee(gross: Uint128, fee_bps: u16) -> (Uint128, Uint128) {
    let fee = gross.multiply_ratio(fee_bps as u128, MAX_BPS as u128);
    (fee, gross - fee)
}

/// Splits `amount` across validators proportionally to their stake. Each validator gets at most
/// its stake and the parts sum to exactly `amount`. Zero parts are dropped.
/// Returns `None` if `amount` exceeds the total stake.
pub fn split_by_stake(
    amount: Uint128,
    stakes: &[(String, Uint128)],
) -> Option<Vec<(String, Uint128)>> {
    let total: Uint128 = stakes.iter().map(|(_, a)| *a).sum();
    if amount > total {
        return None;
    }
    if total.is_zero() {
        return Some(vec![]);
    }
    let mut parts: Vec<(String, Uint128)> = stakes
        .iter()
        .map(|(v, s)| (v.clone(), amount.multiply_ratio(*s, total)))
        .collect();
    // Hand the rounding remainder to validators with room left, largest stake first.
    let mut remainder = amount - parts.iter().map(|(_, a)| *a).sum::<Uint128>();
    let mut order: Vec<usize> = (0..stakes.len()).collect();
    order.sort_by_key(|i| std::cmp::Reverse(stakes[*i].1));
    for i in order {
        if remainder.is_zero() {
            break;
        }
        let room = stakes[i].1 - parts[i].1;
        let add = room.min(remainder);
        parts[i].1 += add;
        remainder -= add;
    }
    parts.retain(|(_, a)| !a.is_zero());
    Some(parts)
}

/// A validator may hold at most 7 unbonding entries on Cosmos Hub. `SubmitUnbonding` adds one
/// entry per validator per epoch and each lives `unbonding_secs`, so at most
/// `unbonding_secs / epoch_secs + 1` are live at once. Keeping the ratio at or below 6 leaves room.
pub const MAX_UNBONDING_EPOCH_RATIO: u64 = 6;

pub fn validate_timing(epoch_secs: u64, unbonding_secs: u64) -> Result<(), ContractError> {
    if epoch_secs == 0
        || unbonding_secs == 0
        || unbonding_secs > epoch_secs.saturating_mul(MAX_UNBONDING_EPOCH_RATIO)
    {
        return Err(ContractError::InvalidTiming {
            max_ratio: MAX_UNBONDING_EPOCH_RATIO,
        });
    }
    if epoch_secs > MAX_EPOCH_SECS || unbonding_secs > MAX_UNBONDING_SECS {
        return Err(ContractError::TimingTooLong {
            max_epoch: MAX_EPOCH_SECS,
            max_unbonding: MAX_UNBONDING_SECS,
        });
    }
    Ok(())
}

/// Redelegations `(from, to, amount)` that bring delegations to the target weights.
///
/// Validators missing from `weights` have a target of zero and are drained. Moves smaller than
/// `min_move` are skipped (dust stays where it is) and at most `max_moves` are returned.
/// A validator is never both source and destination.
pub fn plan_rebalance(
    weights: &[ValidatorWeight],
    delegations: &[(String, Uint128)],
    min_move: Uint128,
    max_moves: usize,
) -> Vec<(String, String, Uint128)> {
    let total: Uint128 = delegations.iter().map(|(_, a)| *a).sum();

    let mut names: Vec<&String> = weights.iter().map(|w| &w.validator).collect();
    for (v, _) in delegations {
        if !names.contains(&v) {
            names.push(v);
        }
    }

    // Floor division leaves a few uatom unassigned; give them to the heaviest validator so that
    // validators dropped from the weight list drain completely.
    let mut targets: Vec<(String, Uint128)> = weights
        .iter()
        .map(|w| {
            (
                w.validator.clone(),
                total.multiply_ratio(w.weight_bps as u128, MAX_BPS as u128),
            )
        })
        .collect();
    let assigned: Uint128 = targets.iter().map(|(_, t)| *t).sum();
    if let Some(heaviest) = weights
        .iter()
        .max_by(|a, b| a.weight_bps.cmp(&b.weight_bps).then_with(|| b.validator.cmp(&a.validator)))
    {
        if let Some(t) = targets.iter_mut().find(|(v, _)| v == &heaviest.validator) {
            t.1 += total - assigned;
        }
    }

    let mut surplus: Vec<(String, Uint128)> = vec![];
    let mut deficit: Vec<(String, Uint128)> = vec![];
    for name in names {
        let target = targets
            .iter()
            .find(|(v, _)| v == name)
            .map(|(_, t)| *t)
            .unwrap_or_default();
        let current: Uint128 = delegations
            .iter()
            .filter(|(v, _)| v == name)
            .map(|(_, a)| *a)
            .sum();
        if current > target {
            surplus.push((name.clone(), current - target));
        } else if target > current {
            deficit.push((name.clone(), target - current));
        }
    }
    // Largest first, ties broken by name so the plan is deterministic.
    surplus.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    deficit.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut moves = vec![];
    let (mut i, mut j) = (0, 0);
    while i < surplus.len() && j < deficit.len() && moves.len() < max_moves {
        let amount = surplus[i].1.min(deficit[j].1);
        if amount < min_move {
            // The smaller side is dust, skip it.
            if surplus[i].1 <= deficit[j].1 {
                i += 1;
            } else {
                j += 1;
            }
            continue;
        }
        moves.push((surplus[i].0.clone(), deficit[j].0.clone(), amount));
        surplus[i].1 -= amount;
        deficit[j].1 -= amount;
        if surplus[i].1.is_zero() {
            i += 1;
        }
        if deficit[j].1.is_zero() {
            j += 1;
        }
    }
    moves
}

pub fn validate_weights(weights: &[ValidatorWeight]) -> Result<(), ContractError> {
    if weights.is_empty() || weights.len() > MAX_VALIDATORS {
        return Err(ContractError::InvalidWeights {});
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut sum: u32 = 0;
    for w in weights {
        if w.weight_bps == 0 || !seen.insert(&w.validator) {
            return Err(ContractError::InvalidWeights {});
        }
        sum += w.weight_bps as u32;
    }
    if sum != MAX_BPS as u32 {
        return Err(ContractError::InvalidWeights {});
    }
    Ok(())
}

/// Splits `available` ATOM between matured batches that expected `expected[i]` each.
///
/// Returns the payout of each batch and whether there was a shortfall. Without a shortfall every
/// batch gets exactly what it expected. With one, the shortfall is shared pro rata
/// (`expected[i] * available / total`, rounded down), so the result depends only on the set of
/// batches and on `available`, never on which of them is finalized first or by whom.
pub fn settle_matured(available: Uint128, expected: &[Uint128]) -> (Vec<Uint128>, bool) {
    let total = expected.iter().fold(Uint128::zero(), |a, e| a.saturating_add(*e));
    if available >= total {
        return (expected.to_vec(), false);
    }
    let shares = expected
        .iter()
        .map(|e| e.multiply_ratio(available, total))
        .collect();
    (shares, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(v: &str, bps: u16) -> ValidatorWeight {
        ValidatorWeight {
            validator: v.to_string(),
            weight_bps: bps,
        }
    }

    #[test]
    fn first_deposit_is_one_to_one() {
        assert_eq!(
            mint_amount(Uint128::new(5_000_000), Uint128::zero(), Uint128::zero()).unwrap(),
            Uint128::new(5_000_000)
        );
    }

    #[test]
    fn mint_follows_rate_and_rounds_down() {
        // rate 1.25: 1_000_000 ATOM backs 800_000 liATOM
        let minted = mint_amount(
            Uint128::new(1_000_000),
            Uint128::new(800_000),
            Uint128::new(1_000_000),
        )
        .unwrap();
        assert_eq!(minted, Uint128::new(800_000));
        // deposit 3 at rate 1.25 -> 2.4 -> 2
        let minted = mint_amount(Uint128::new(3), Uint128::new(800_000), Uint128::new(1_000_000))
            .unwrap();
        assert_eq!(minted, Uint128::new(2));
    }

    #[test]
    fn dust_deposit_is_rejected() {
        let err = mint_amount(Uint128::new(1), Uint128::new(1), Uint128::new(10)).unwrap_err();
        assert_eq!(err, ContractError::ZeroMint {});
    }

    #[test]
    fn empty_pool_with_supply_is_rejected() {
        let err = mint_amount(Uint128::new(10), Uint128::new(5), Uint128::zero()).unwrap_err();
        assert_eq!(err, ContractError::EmptyPool {});
    }

    #[test]
    fn round_trip_never_gains() {
        let (pool, supply) = (Uint128::new(1_234_567), Uint128::new(1_000_003));
        let dep = Uint128::new(777_777);
        let minted = mint_amount(dep, supply, pool).unwrap();
        let back = atom_for_liatom(minted, pool + dep, supply + minted);
        assert!(back <= dep);
    }

    #[test]
    fn rate_is_one_without_supply() {
        assert_eq!(exchange_rate(Uint128::zero(), Uint128::zero()), Decimal::one());
        assert_eq!(
            exchange_rate(Uint128::new(150), Uint128::new(100)),
            Decimal::percent(150)
        );
    }

    #[test]
    fn picks_largest_deficit() {
        let ws = [w("a", 5000), w("b", 3000), w("c", 2000)];
        let del = [
            ("a".to_string(), Uint128::new(500)),
            ("b".to_string(), Uint128::new(100)),
            ("c".to_string(), Uint128::new(400)),
        ];
        // total 1000 + 100 deposit = 1100, targets a=550 b=330 c=220 -> deficits 50, 230, -180
        assert_eq!(pick_validator(&ws, &del, Uint128::new(100)).unwrap(), "b");
    }

    #[test]
    fn picks_first_validator_on_empty_state() {
        let ws = [w("a", 6000), w("b", 4000)];
        assert_eq!(pick_validator(&ws, &[], Uint128::new(10)).unwrap(), "a");
    }

    #[test]
    fn fee_split_rounds_down() {
        assert_eq!(
            split_fee(Uint128::new(1_000_000), 1000),
            (Uint128::new(100_000), Uint128::new(900_000))
        );
        assert_eq!(
            split_fee(Uint128::new(9), 1000),
            (Uint128::zero(), Uint128::new(9))
        );
    }

    #[test]
    fn split_by_stake_sums_exactly() {
        let stakes = [
            ("a".to_string(), Uint128::new(3)),
            ("b".to_string(), Uint128::new(3)),
            ("c".to_string(), Uint128::new(4)),
        ];
        let parts = split_by_stake(Uint128::new(5), &stakes).unwrap();
        let sum: Uint128 = parts.iter().map(|(_, a)| *a).sum();
        assert_eq!(sum, Uint128::new(5));
        for (v, a) in &parts {
            let cap = stakes.iter().find(|(s, _)| s == v).unwrap().1;
            assert!(*a <= cap);
        }
    }

    #[test]
    fn split_by_stake_rejects_more_than_total() {
        let stakes = [("a".to_string(), Uint128::new(3))];
        assert!(split_by_stake(Uint128::new(4), &stakes).is_none());
        assert_eq!(
            split_by_stake(Uint128::new(3), &stakes).unwrap(),
            vec![("a".to_string(), Uint128::new(3))]
        );
    }

    fn d(v: &str, a: u128) -> (String, Uint128) {
        (v.to_string(), Uint128::new(a))
    }

    #[test]
    fn rebalance_is_empty_when_already_on_target() {
        let ws = [w("a", 6000), w("b", 4000)];
        let del = [d("a", 600), d("b", 400)];
        assert!(plan_rebalance(&ws, &del, Uint128::one(), 8).is_empty());
    }

    #[test]
    fn rebalance_moves_surplus_to_deficit() {
        let ws = [w("a", 5000), w("b", 5000)];
        let del = [d("a", 800), d("b", 200)];
        let plan = plan_rebalance(&ws, &del, Uint128::one(), 8);
        assert_eq!(plan, vec![("a".to_string(), "b".to_string(), Uint128::new(300))]);
    }

    #[test]
    fn rebalance_drains_validators_missing_from_weights() {
        let ws = [w("a", 5000), w("b", 5000)];
        let del = [d("a", 500), d("b", 300), d("gone", 200)];
        let plan = plan_rebalance(&ws, &del, Uint128::one(), 8);
        let drained: Uint128 = plan
            .iter()
            .filter(|(from, _, _)| from == "gone")
            .map(|(_, _, a)| *a)
            .sum();
        assert_eq!(drained, Uint128::new(200));
        assert!(plan.iter().all(|(from, to, _)| from != to));
    }

    #[test]
    fn rebalance_skips_dust_and_respects_move_cap() {
        let ws = [w("a", 5000), w("b", 5000)];
        let del = [d("a", 505), d("b", 495)];
        // surplus of 5 is below min_move
        assert!(plan_rebalance(&ws, &del, Uint128::new(10), 8).is_empty());

        let ws = [w("a", 2500), w("b", 2500), w("c", 2500), w("d", 2500)];
        let del = [d("a", 1000), d("b", 0), d("c", 0), d("d", 0)];
        assert_eq!(plan_rebalance(&ws, &del, Uint128::one(), 2).len(), 2);
    }

    #[test]
    fn rebalance_never_overshoots() {
        let ws = [w("a", 3000), w("b", 3000), w("c", 4000)];
        let del = [d("a", 900), d("b", 100), d("c", 1000), d("x", 123)];
        let plan = plan_rebalance(&ws, &del, Uint128::one(), 8);
        let mut after: std::collections::BTreeMap<String, i128> =
            del.iter().map(|(v, a)| (v.clone(), a.u128() as i128)).collect();
        for (from, to, amount) in &plan {
            *after.get_mut(from).unwrap() -= amount.u128() as i128;
            *after.entry(to.clone()).or_default() += amount.u128() as i128;
        }
        let total: i128 = after.values().sum();
        assert_eq!(total, 2123);
        for w in &ws {
            let target = total * w.weight_bps as i128 / 10_000;
            assert!(
                after[&w.validator] <= target + 3,
                "{} overshoots",
                w.validator
            );
        }
        assert_eq!(after["x"], 0);
    }

    #[test]
    fn timing_validation() {
        assert!(validate_timing(345_600, 1_814_400).is_ok());
        assert!(validate_timing(1_814_400 / 6, 1_814_400).is_ok());
        assert!(validate_timing(345_600 / 2, 1_814_400).is_err());
        assert!(validate_timing(0, 100).is_err());
        assert!(validate_timing(100, 0).is_err());
        // upper bounds: no astronomically long epochs or unbonding periods
        assert!(validate_timing(MAX_EPOCH_SECS, MAX_UNBONDING_SECS).is_ok());
        assert!(validate_timing(MAX_EPOCH_SECS + 1, MAX_UNBONDING_SECS).is_err());
        assert!(validate_timing(MAX_EPOCH_SECS, MAX_UNBONDING_SECS + 1).is_err());
        assert!(validate_timing(u64::MAX / 2, u64::MAX / 2).is_err());
    }

    #[test]
    fn validator_count_is_capped() {
        let make = |n: usize| -> Vec<ValidatorWeight> {
            let mut v: Vec<ValidatorWeight> = (0..n)
                .map(|i| ValidatorWeight { validator: format!("val{i}"), weight_bps: 1 })
                .collect();
            let assigned = n as u16;
            v[0].weight_bps += 10_000 - assigned;
            v
        };
        assert!(validate_weights(&make(MAX_VALIDATORS)).is_ok());
        assert!(validate_weights(&make(MAX_VALIDATORS + 1)).is_err());
    }

    #[test]
    fn weight_validation() {
        assert!(validate_weights(&[w("a", 6000), w("b", 4000)]).is_ok());
        assert!(validate_weights(&[]).is_err());
        assert!(validate_weights(&[w("a", 6000), w("b", 3000)]).is_err());
        assert!(validate_weights(&[w("a", 5000), w("a", 5000)]).is_err());
        assert!(validate_weights(&[w("a", 0), w("b", 10000)]).is_err());
    }
}
