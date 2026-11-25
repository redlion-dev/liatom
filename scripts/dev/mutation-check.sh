#!/usr/bin/env bash
# Mutation check: each mutation must make the test suite (unit and invariant tests) fail.
. "$HOME/.cargo/env"
cd "$(dirname "$0")/../.."
C=contracts/liatom-hub/src/contract.rs
R=contracts/liatom-hub/src/rate.rs
cp $C /tmp/contract.orig; cp $R /tmp/rate.orig
trap 'cp /tmp/contract.orig $C; cp /tmp/rate.orig $R' EXIT

run() {
  name=$1
  out=$(cargo test 2>&1)
  if echo "$out" | grep -q "could not compile"; then
    echo "BUILD ERROR $name (mutation invalid)"
  elif ! echo "$out" | grep -q "test result:"; then
    echo "ERROR    $name (cargo did not run the tests)"
  elif echo "$out" | grep -q "test result: FAILED"; then
    echo "KILLED   $name: $(echo "$out" | grep -m1 -E 'rate fell|not conserved|insolvent|paid|panicked at' | cut -c1-160)"
  else
    echo "SURVIVED $name"
  fi
  cp /tmp/contract.orig $C; cp /tmp/rate.orig $R
}

# 1. ignore burned-but-unsubmitted liATOM when pricing
sed -i 's/Ok(self.liatom_supply.checked_add(self.pending_liatom)?)/Ok(self.liatom_supply)/' $C
run "supply ignores pending_liatom"

# 2. pay claims rounding up
sed -i "s/let payout = received.multiply_ratio(liatom, batch.liatom_burned);/let payout = received.multiply_ratio(liatom, batch.liatom_burned) + Uint128::one();/" $C
run "claim payout rounds up (+1)"

# 3. mint rounds up instead of down
sed -i 's|let minted = Uint256::from(deposit) \* Uint256::from(supply) / Uint256::from(pool_value);|let minted = (Uint256::from(deposit) * Uint256::from(supply) + Uint256::from(pool_value) - Uint256::one()) / Uint256::from(pool_value);|' $R
run "mint rounds up"

# 4. pool ignores the protocol fee on rewards
sed -i 's/let pool_value = delegated.checked_add(net_rewards)?;/let pool_value = delegated.checked_add(unclaimed_rewards)?;/' $C
run "pool counts gross rewards (no fee)"

# 5. finalize pays the full expected amount even if short
sed -i 's/if available >= total {/if true {/' $R
run "finalize ignores slashing shortfall"

# 7. fee change does not harvest under the old fee first
sed -i 's/res = res.add_messages(plan.msgs);/res = res.add_messages(Vec::<CosmosMsg<TokenFactoryMsg>>::new());/' $C
run "fee change skips the harvest at the old fee"

# 8. timing validation lets epochs that overflow the 7-entry limit through
sed -i 's/epoch_secs.saturating_mul(MAX_UNBONDING_EPOCH_RATIO)/epoch_secs.saturating_mul(1000)/' $R
run "timing validation too lax"

# 9. rebalance moves stake the wrong way round
sed -i 's/moves.push((surplus\[i\].0.clone(), deficit\[j\].0.clone(), amount));/moves.push((deficit[j].0.clone(), surplus[i].0.clone(), amount));/' $R
run "rebalance redelegates in the wrong direction"

# 10. a shortfall is accepted right after maturity (funds may still be in transit)
perl -0pi -e 's/if !matured\.iter\(\)\.any\(\|b\| b\.id == batch_id && grace_over\(b\)\) \{/if false {/' $C
run "finalize accepts a shortfall without the grace period"

# 11. the shortfall goes to the earliest batch instead of being shared pro rata
sed -i 's/\.map(|e| e\.multiply_ratio(available, total))/.map(|e| (*e).min(available))/' $R
run "shortfall not shared pro rata"

# 12. deposits ignore the TVL cap
sed -i 's/if after > cfg.max_tvl {/if false {/' $C
run "deposit ignores the tvl cap"

# 13. the denom metadata message is dropped (wallets would show a raw factory denom)
sed -i '/.add_message(tokenfactory::set_metadata(&liatom_denom))/d' $C
run "instantiate does not set denom metadata"

# 15. FinalizeBatch is accepted in the very block where the unbonded ATOM has not arrived yet
sed -i 's/let finalizable_at = mature_at.plus_seconds(FINALIZE_DELAY_SECS);/let finalizable_at = mature_at;/' $C
run "finalize allowed in the arrival block"

# 16. timing parameters have no upper bound
sed -i 's/if epoch_secs > MAX_EPOCH_SECS || unbonding_secs > MAX_UNBONDING_SECS {/if false {/' $R
run "timing parameters unbounded"

# 19. a deposit leaves the rewards its delegation pays out on the balance
perl -0pi -e 's/let sweep = sweep_rewards\(deps\.as_ref\(\), &cfg, &pool, &\[validator\.clone\(\)\], &after, false\)\?;/let sweep: Vec<SubMsg<TokenFactoryMsg>> = vec![]; let _ = after;/' $C
run "deposit does not collect the payout of its own delegation"

# 20. rounding dust of an Undelegate counts as slashing (batches wait for the grace period)
perl -0pi -e 's/if total_expected\.saturating_sub\(available\) > tolerance_of\(&matured\) \{/if total_expected > available {/' $C
run "rounding dust treated as slashing"

# 22. a batch that matured moments ago joins the set (its ATOM may still be in transit)
sed -i 's/\&\& b.mature_at.is_some_and(|m| env.block.time >= m.plus_seconds(FINALIZE_DELAY_SECS))/\&\& b.mature_at.is_some_and(|m| env.block.time >= m)/' $C
run "just-matured batch joins the settlement set"

# 23. each batch no longer has its own grace period: one deadline for the whole set again
perl -0pi -e 's/matured\.retain\(\|b\| grace_over\(b\)\);/let _ = grace_over;/' $C
run "settled together under one grace period"

# 24. unbonding_secs can be lowered by the admin
perl -0pi -e 's/if u < cfg\.min_unbonding_secs \{/if false {/' $C
run "unbonding_secs can be lowered without a floor"

# 25. SubmitUnbonding depends on the fee transfer succeeding
perl -0pi -e 's/touched, &after, true\)\?\);/touched, &after, false)?);/' $C
run "submit fails when the treasury cannot receive"

# 26. a failed fee transfer in SubmitUnbonding leaves the fee on the balance (hidden by the reserve)
sed -i "s/if !unsent.amount.is_zero() {/if false {/" $C
run "unsent fee is not delegated back"

# 27. a shortfall is written off in the very block that first sees it (a chain halt spanning maturity)
perl -0pi -e 's/matured\.retain\(\|b\| b\.shortfall_noted_at\.is_some_and\(\|h\| h < height\)\);/let _ = height;/' $C
run "shortfall written off in the block that first sees it"

# 30. the contract's bank balance becomes part of the price (a donation moves every price)
perl -0pi -e 's/let pool_value = delegated\.checked_add\(net_rewards\)\?;/let pool_value = delegated.checked_add(net_rewards)?.checked_add(deps.querier.query_balance(&env.contract.address, &cfg.bond_denom)?.amount)?;/' $C
run "a donation changes the price"

# 31. messages that take no funds accept them
perl -0pi -e 's/if !info\.funds\.is_empty\(\)\s+&& !matches!/if false \&\& !matches!/' $C
run "funds attached to a no-funds message are accepted"

# 32. a tokenized deposit is priced on the whole delegation instead of what it grew by
perl -0pi -e 's/let deposited = after\.saturating_sub\(p\.delegation_before\);/let deposited = after;/' $C
run "tokenized deposit priced on the whole delegation"

# 33. a tokenized deposit does not collect the rewards its redemption pays out
perl -0pi -e 's/let sweep = sweep_rewards\(deps\.as_ref\(\), &cfg, &pool, &\[validator\.clone\(\)\], &pool\.delegations, false\)\?;\n\n    Ok\(Response::new\(\)\n        \.add_submessage\(\n            SubMsg::reply_on_success/let sweep: Vec<SubMsg<TokenFactoryMsg>> = vec![];\n\n    Ok(Response::new()\n        .add_submessage(\n            SubMsg::reply_on_success/' $C
run "tokenized deposit leaves the redemption's rewards on the balance"

# 34. a tokenized deposit accepts a validator outside the set
perl -0pi -e 's/\.ok_or_else\(\|\| ContractError::ValidatorNotInSet \{ validator: validator\.clone\(\) \}\)\?;/.unwrap_or(10_000);/' $C
run "tokenized deposit accepts any validator"

# 35. a tokenized deposit ignores the TVL cap
perl -0pi -e 's/if total > cfg\.max_tvl \{/if false {/' $C
run "tokenized deposit ignores the tvl cap"

# 36. a tokenized deposit ignores the minimum deposit
perl -0pi -e 's/if deposited < cfg\.min_deposit \{/if false {/' $C
run "tokenized deposit ignores the minimum"

# 37. a tokenized deposit works while deposits are paused
perl -0pi -e 's/(fn execute_deposit_tokenized[\s\S]*?)if cfg\.pause\.is_some\(\) \{/$1if false {/' $C
run "tokenized deposit ignores the pause"

# 38. leaving as shares is priced 1:1 instead of at the rate
perl -0pi -e 's/let amount = rate::atom_for_liatom\(funds\.amount, pool\.pool_value, pool\.effective_supply\(\)\?\);/let amount = funds.amount;/' $C
run "exit as shares ignores the rate"

# 39. leaving as shares does not collect the rewards the tokenization pays out
perl -0pi -e 's/let sweep = sweep_rewards\(deps\.as_ref\(\), &cfg, &pool, &touched, &after, true\)\?;\n\n    Ok\(res/let sweep: Vec<SubMsg<TokenFactoryMsg>> = vec![];\n\n    Ok(res/' $C
run "exit as shares leaves the payout on the balance"

# 40. the reply does not check how much the pool gave up
perl -0pi -e 's/if taken > p\.amount\.checked_add\(Uint128::new\(2\)\)\? \{/if false {/' $C
run "exit reply does not check the delegation taken"

# 41. the reply sends a share token of any validator
perl -0pi -e 's/lsm::share_token_validator\(&share\.denom\) != Some\(p\.validator\.as_str\(\)\) \|\| share\.amount\.is_zero\(\)/share.amount.is_zero()/' $C
run "exit reply accepts any share token"

# 42. a full pause does not stop leaving as shares
perl -0pi -e 's/(fn execute_redeem_to_shares[\s\S]*?)if cfg\.pause == Some\(PauseMode::Full\) \{/$1if false {/' $C
run "exit as shares ignores the full pause"

# 43. leaving as shares has no minimum
perl -0pi -e 's/if amount < cfg\.min_deposit \{\n        return Err\(ContractError::ExitTooSmall/if false {\n        return Err(ContractError::ExitTooSmall/' $C
run "exit as shares has no minimum"

# 44. leaving as shares takes everything from the first validator instead of pro rata
perl -0pi -e 's/rate::split_by_stake\(amount, &pool\.delegations\)\.ok_or\(/Some(vec![(pool.delegations[0].0.clone(), amount)]).ok_or(/' $C
run "exit as shares lets one validator carry the whole exit"

# 45. the fee transfer on the exit path must not stop it (best effort)
perl -0pi -e 's/&touched, &after, true\)\?;\n\n    Ok\(res/&touched, &after, false)?;\n\n    Ok(res/' $C
run "exit as shares is blocked by a treasury that refuses the fee"

# 46. a tokenized deposit may leave its validator at any weight
perl -0pi -e 's/if after > max_stake \{/if false {/' $C
run "tokenized deposit ignores the validator weight cap"

# 47. a dust part of the exit is sent to the chain as it is
perl -0pi -e 's/if \*p < floor \{/if false {/' $C
run "exit as shares keeps dust parts"

# 48. validator addresses are stored as the admin typed them (not lowercased like the chain's)
perl -0pi -e 's/validator: w\.validator\.to_lowercase\(\)/validator: w.validator.clone()/' $C
run "validator addresses are not canonicalised"

# 49. an exit re-delegates the swept rewards to an untouched weight instead of a validator it holds stake at
perl -0pi -e 's/Some\(v\) => \(\(\*v\)\.clone\(\), true\),\n            None => \(/Some(_) => (rate::pick_validator(&weights, delegations_after, net).ok_or(ContractError::InvalidWeights {})?, false),\n            None => (/' $C
run "exit re-delegates to an untouched weight"

# 50. the validator list may push the delegations over the limit
perl -0pi -e 's/if names\.len\(\) > MAX_VALIDATORS \{/if false {/' $C
run "validator list ignores the delegation limit"

# 51. a validator with a high commission may join the set
perl -0pi -e 's/if v\.commission > max \{/if false {/' $C
run "validator commission is not checked"
