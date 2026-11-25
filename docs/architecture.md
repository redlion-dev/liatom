# liATOM architecture

Status: v0.1, implemented. Builds on [hub-capabilities.md](hub-capabilities.md).
Parameter values in section 11 are the launch defaults chosen by the project owner.

## 1. Goal and scope

A user sends ATOM to the contract and receives liATOM. liATOM is a native denom
`factory/<contract>/liatom` (tokenfactory), usable over IBC and on DEXes. The contract stakes ATOM directly
on Cosmos Hub, without LSM and without IBC. The liATOM/ATOM rate grows as rewards are reinvested.
Redeeming liATOM for ATOM takes the Hub unbonding period (21 days).

Out of scope for v1: instant redemption from liquidity, cross-chain deployment, ICA, a dedicated chain.

## 2. Components

One CosmWasm contract, `liatom-hub` (Rust, `cosmwasm-std` 2.x), plus an external keeper bot that calls the
public `Harvest`, `SubmitUnbonding` and `FinalizeBatch` messages on a schedule (the contract cannot run itself on a timer).

```
user   --Deposit(ATOM)--------> liatom-hub --Delegate--> Hub validators
user   --Redeem(liATOM)-------> liatom-hub (burn, recorded in a batch)
keeper --Harvest--------------> WithdrawReward -> fee -> Delegate (reinvest)
keeper --SubmitUnbonding------> Undelegate per validator (once per epoch)
keeper --FinalizeBatch--------> records what the matured batches received
user   --Claim(batch)---------> Bank send ATOM (after 21 days)
```

## 3. State

| Key | Content |
|---|---|
| `Config` | admin, pending_admin, treasury, fee_bps, epoch_secs, unbonding_secs, min_deposit, max_tvl, pause (None / deposits_only / full), denoms |
| `Validators` | validator -> weight (bps); weights sum to 10000 |
| `Batches` | id -> { status: Open/Unbonding/Matured, liatom_burned, atom_expected, atom_received, mature_at } |
| `Claims` | (batch_id, address) -> liatom_burned |
| `CurrentBatch`, `LastSubmit` | id of the current Open batch, time of the last submit |
| `OwedToRedeemers` | ATOM already unbonded and finalized, waiting to be claimed |
| `PendingLiatom` | liATOM burned into the open batch whose ATOM is still in the pool |
| `Unbonding` | ids of batches that are unbonding or matured but not yet finalized |

State size is bounded: at most 50 validators and one batch per epoch.

## 4. Exchange rate

```
unclaimed     = sum(DelegationTotalRewards)      // a query (cosmwasm_1_4), floored per validator exactly as the chain pays it out
fee           = floor(unclaimed * fee_bps / 10000)
pool_value    = sum(delegations)                 // StakingQuery::AllDelegations, already net of slashing
              + unclaimed - fee
supply_eff    = supply(liATOM) + pending_liatom  // pending = liATOM burned into the open batch, not yet submitted
rate          = pool_value / supply_eff          // ATOM per 1 liATOM, starts at 1.0
```

- **The contract's bank balance is never part of any price.** The rate, what a deposit mints, what a batch is owed and what a
  harvest does depend only on the delegations and the unclaimed rewards. Everything else on the balance is either unbonded ATOM
  that belongs to redeemers (whenever it arrives) or a donation, which belongs to nobody and changes nothing. The balance is read in
  `FinalizeBatch` only: `available = balance - owed_to_redeemers`. The contract used to try to tell donations from arrived
  unbonded ATOM by looking at the balance, and every pricing bug found in review rounds 2-8 came from that ambiguity.
- Rewards reach the balance not only through `WithdrawDelegatorReward`: any delegation change (Delegate, Undelegate) makes
  distribution pay the validator's rewards out to the balance. So every call that changes a delegation (`Deposit`,
  `SubmitUnbonding`, `Rebalance`) collects the rewards of the touched validators in the same transaction (`sweep_rewards`):
  the fee goes to the treasury and the rest is delegated again. Those ATOM were already priced as unclaimed rewards, so the price
  does not move. In `SubmitUnbonding` the fee transfer is a submessage whose failure is ignored (`reply`): a treasury that
  refuses funds must not block users' exits. The `reply` delegates the unsent fee to the validator named in its payload (an
  unreadable payload is ignored because this `reply` is on the exit path and must never fail).
- The fee is deducted from the rate immediately (as rewards accrue), so a depositor does not overpay for rewards part of which goes to
  the treasury.
- `pending_liatom` exists because `Redeem` burns liATOM at once while the ATOM leave the pool only at `SubmitUnbonding`.
  Without it the rate would be overstated for everyone between the two calls.
- Mint on deposit: `mint = floor(amount / rate)`, rounding in the protocol's favour.
- Defence against the first-depositor inflation attack: the seed deposit at instantiate (at least `min_deposit`) is minted to the
  contract itself and never withdrawn.
- The `cosmwasm_1_4` capability is available on the Hub (checked against wasmd v0.60.10, see hub-capabilities.md), so unclaimed
  rewards are read with a query.
- `FinalizeBatch` is accepted only 60 seconds after `mature_at` (`FINALIZE_DELAY_SECS`): the chain returns the ATOM at the end of the
  block of `mature_at`, and at least one block has passed by then. What is paid depends only on the batch's amount (`atom_expected`) and
  on what actually arrived (`available`), never on the price.

## 5. Messages

All messages below are implemented and covered by unit tests, invariant tests and smoke tests on a local chain.

**User**
- `Deposit {}` with attached `uatom`: computes the rate, mints liATOM and sends `StakingMsg::Delegate` to the
  validator with the largest deficit against its target weight (on a tie the last of the tied validators in weight-list order). One Delegate per deposit, no buffer.
  Rejected if the pool would exceed `max_tvl` (`TvlCapExceeded`).
- `DepositTokenizedShares {}` with one attached LSM share token (`<validator>/<record id>`, made by the user with `MsgTokenizeShares` from an existing delegation): redeems it with `MsgRedeemTokensForShares` (sent as `CosmosMsg::Any` in a submessage), after which the chain moves the delegation to the contract at once, without unbonding. The reply measures how much the contract's delegation to that validator grew by and mints liATOM for exactly that amount at the same rate, minimum and TVL cap as `Deposit` (the price is taken before the redemption, so the two paths agree to the unit). Only validators in the weight list are accepted, so a depositor cannot pick the validator whose slashing risk the pool takes over, and the deposit may not leave its validator more than 20% (relative) above its target weight (`ValidatorOverweight`): the instant LSM paths are a slash-free way to move stake between validators, and without the cap a deposit and exit loop could concentrate the pool on a validator that is about to be slashed. The rewards the redemption pays out are collected in the same transaction (`sweep_rewards`). Paused with deposits.
- `RedeemToShares {}` with attached liATOM: leave as ordinary staked ATOM at once, without a batch or the 21-day wait. The liATOM is burned at the current rate (the price a deposit pays and a batch is fixed at, rounded down; at least `min_deposit` worth) and the contract tokenizes that much of its delegations with `MsgTokenizeShares` (as `CosmosMsg::Any`, one submessage per validator), taking from every validator in proportion to its stake like a batch (the rounding remainder of the proportional split goes to the largest stake first; parts below 10 uatom are folded into a larger one that has room for them, and if none has, the few uatom are dropped (the leaver gets up to 9 uatom less per part than the burned worth, the pool gains it), because the chain can turn such a part into 0 on a slashed validator and refuse it), so the leaver carries the same risk as the holders who stay and cannot pick a healthy validator to leave the loss of a risky one behind. The caller is named the owner of the share records. Each reply checks that its share token belongs to that validator and that the delegation shrank by no more than the part taken plus 2 uatom (chain rounding), then sends the token to the caller, who redeems it into a delegation of their own (or sells it). The rewards the tokenizations pay out are collected in the same transaction; the fee transfer may fail without stopping the exit (its reply delegates the unsent fee). Blocked by the full pause only. It can fail when the chain refuses to tokenize (the global LSM cap of 25% of the bonded stake, a redelegation in progress to one of the validators); the whole transaction then fails and nothing is burned, and `Redeem` always remains.
- `Redeem {}` with attached liATOM: burns it, records it in the Open batch and increases `pending_liatom`.
  No ATOM is paid at once.
- `Claim { batch_id }`: once the batch is `Matured`, sends `atom_received x user's share`, rounded down.
  A claim cannot be taken twice (the record is removed).

**Public (the bot calls them, anyone can)**
- `Harvest {}`: takes `gross` (the unclaimed rewards), sends `fee` to the treasury
  (`BankMsg::Send`) and delegates the rest to the validator with the largest deficit. No `reply`: amounts are
  computed by queries before the messages are sent, and `WithdrawDelegatorReward` goes first.
- `SubmitUnbonding {}`: if `epoch_secs` have passed since the last submit and the batch is not empty, computes
  `atom_expected = burned x pool_value / supply_eff` and sends `Undelegate` to validators in proportion to stake
  (at most one per validator). The batch becomes `Unbonding`, `mature_at = now + unbonding_secs`, and a new batch
  opens. If `atom_expected` exceeds total delegations the call is rejected (`InsufficientDelegations`).
- `FinalizeBatch { batch_id }`: 60 seconds after that batch's `mature_at`, finalizes *all* batches that matured at least 60 seconds ago in one call (the
  order and the caller do not affect the result; a batch that matured moments ago waits for the next call), records `atom_received` (see section 8), moves them to `Matured`
  and increases `owed_to_redeemers`. A shortfall is written off only in a later block than the one that first sees it (the first call records the block and pays
  nothing, returning Ok; after a chain halt longer than the grace period the first block already counts as past it, while the
  unbonded ATOM is only released at its end). A deficit of up to 10 uatom per `Undelegate` message of the batches is chain rounding
  and is settled at once, pro rata from what is on the balance (the redeemers absorb that dust). A larger shortfall is settled only for the batches that are past *their own* 7-day grace period
  (counted from their own `mature_at`); younger batches stay unbonding, so a batch whose ATOM is merely late is not
  written off, and batches that mature later cannot push an older batch's deadline away.

**Admin**
- `UpdateValidators`: new target weights (sum 10000 bps, validators must exist and take at most 25% commission when they are added; a validator can raise it later, so the admin has to watch it). The list plus the validators the contract still holds stake at may not exceed 50 (`TooManyValidators`): every exit and every price iterates the delegations. A removed validator stays delegated until `Rebalance` drains it (`Rebalance { min_move: 1 }` also drains dust below the default 1 ATOM). When an exit collects rewards and none of the touched validators is in the weight list, they are re-delegated to the touched validator with the most stake, never to an untouched weight that may have vanished from the chain.
- `Rebalance { min_move? }`: `Redelegate` between validators so delegations match the weights. A validator removed
  from the weight list is drained completely. Moves below `min_move` (default 1 ATOM) are skipped.
- `UpdateConfig { treasury?, fee_bps?, epoch_secs?, unbonding_secs?, min_deposit?, max_tvl? }`: at least one field,
  fee at most 20%, `unbonding_secs <= 6 x epoch_secs`, `epoch_secs <= 30 days`, `unbonding_secs <= 42 days` (`TimingTooLong`; the ratio rule returns `InvalidTiming`).
  `unbonding_secs` can be lowered only down to 7/8 of its value at instantiate (`UnbondingBelowFloor`): a lower value would let a batch be
  settled before the chain has paid it, a higher one makes deposits overpay while the chain's unbonded ATOM arrives early. The validator set has at most 50 entries. The treasury cannot be the contract itself (`InvalidTreasury`). `max_tvl` caps the pool value: a `Deposit` that would push the
  pool over the cap is rejected (`TvlCapExceeded`, the error reports the room left). Lowering the cap below the
  current value only stops new deposits; exits (`Redeem`, `Claim`) are unaffected. `Uint128::MAX` means no cap.
  When `fee_bps` changes the contract first harvests at the old fee, so accrued rewards are not repriced
  retroactively and the rate does not jump.
- `Pause { mode }` / `Unpause`: `deposits_only` blocks `Deposit` and `DepositTokenizedShares` (exits through `Redeem` and `RedeemToShares` stay open); `full`
  blocks `Deposit` and `Redeem`. `Claim`, `FinalizeBatch`, `Harvest` and `SubmitUnbonding` always work. The admin
  picks the mode: `deposits_only` for validator or rate problems, `full` if a redemption bug is suspected.
- `ProposeAdmin` / `AcceptAdmin`.
- liATOM metadata (name, symbol, 6 decimals) is written in `instantiate` with `SetMetadata` right after `CreateDenom`.
- Not implemented: migrations with a timelock.

## 6. The max_entries = 7 limit

On Hub one delegator/validator pair holds at most 7 unfinished unbonding entries. `SubmitUnbonding` does at most
one `Undelegate` per validator per epoch, so live entries per validator number at most
`unbonding_secs / epoch_secs + 1`. The contract does not leave this to the admin: `instantiate` and `UpdateConfig`
reject settings with `unbonding_secs > 6 x epoch_secs` (`InvalidTiming`). With `epoch_secs = 4 days` and 21 days of
unbonding there are at most 6 live entries. The simulator in the invariant tests models the chain limit (it panics
on an eighth entry) and changes `epoch_secs` at random through `UpdateConfig`.

`Rebalance` uses `Redelegate`, which has its own chain limits: up to 7 redelegation entries per (source, destination)
pair, and no redelegating away from a validator that just received a redelegation until it matures. If violated the
chain rejects the message and the whole call reverts. Only the admin calls `Rebalance`, rarely, with at most 8 moves per call.

## 7. Rewards and fee

- `fee_bps` is taken from rewards, not from deposits or redemptions. Default 1000 (10%), as at Stride.
- Harvest is public with no reward to the caller in v1: the bot belongs to the project.
- Rewards are paid to the contract itself (it is its own withdraw address), so they land on its balance.

## 8. Slashing and validators

- Slashing reduces delegations automatically and therefore the rate. The loss is shared equally by all liATOM
  holders, including those in the open batch until it is submitted.
- After `SubmitUnbonding` the batch amount (`atom_expected`) is fixed. `FinalizeBatch` finalizes all matured
  batches in one call: if the balance (net of what finalized batches still owe) covers their `atom_expected`, each
  gets exactly that; otherwise the shortfall is shared between them pro rata to `atom_expected`, rounded down, so the
  result does not depend on the order of finalization or on the caller. v1 limit: free rewards on the balance
  cover a shortfall first, so the loss falls on the remaining holders rather than on the batches; with no free ATOM
  the batches bear it.
- A jailed or disabled validator is removed by the admin through `UpdateValidators`; its stake is moved by
  `Redelegate` or goes to the redemption queue.

## 9. Governance

The delegation belongs to the contract, not to users. Decision for v1: the contract does not vote. Alternatives
(the admin voting through `Gov::Vote`; handing votes to liATOM holders) are possible later projects.

## 10. Security

- The admin has a two-step handover; the tokenfactory denom admin is handed to the contract itself after creation.
- Two pause modes (`deposits_only`, `full`); `Claim` and `FinalizeBatch` are never blocked.
- Integer arithmetic only (`Uint256` / `Decimal256`), rounding in favour of the protocol, invariant tests:
  `supply x rate ~ pool_value`, `sum of claims <= atom_received`, the rate does not fall without slashing.
- Migrations are performed by the wasm admin. The contract has no `migrate` entry point and no
  timelock: new code must bring its own `migrate`. The wasm admin can be cleared (`clear-contract-admin`).
- No paid third-party audit is planned for the first launch. Instead: reviews by several AI models, a public
  repository open to anyone's review (see [ai-audit.md](ai-audit.md)), and a hard TVL cap at launch.

## 11. Parameters (launch defaults)

| Parameter | Value |
|---|---|
| `fee_bps` | 1000 |
| `epoch_secs` | 345 600 (4 days) |
| `unbonding_secs` | 1 814 400 (21 days, must equal the chain's unbonding time, see audit-prep.md section 6 item 8) |
| `min_deposit` | 1 ATOM |
| `max_tvl` | 100 ATOM (`100000000` uatom) for the first launch |
| Validators in the set | one at launch (weight 10000 bps); later 15-30, at most 10% weight each (a recommendation: the contract checks only that weights are unique, non-zero, sum to 10000 and number at most 50) |
| Ticker / decimals | `liATOM` / 6 |
