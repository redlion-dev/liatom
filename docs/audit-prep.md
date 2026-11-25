# liatom-hub review preparation

A document for reviewers. Decision for the first launch: no paid independent audit for the
first launch. Instead the contract is reviewed by several AI models and the repository is public, so anyone can
review it with their own models (see [ai-audit.md](ai-audit.md); results in [audit-log.md](audit-log.md)). The
risk is bounded by `max_tvl` = 100 ATOM. The contract holds users' ATOM and stakes it in its own name.

## 1. Scope

| File | Lines | Content |
|---|---|---|
| `contracts/liatom-hub/src/contract.rs` | ~850 | entry points, every message, pool valuation |
| `contracts/liatom-hub/src/rate.rs` | ~460 | pure arithmetic: rate, mint, fee, rebalance, timing checks, matured-batch settlement |
| `contracts/liatom-hub/src/state.rs`, `msg.rs`, `error.rs`, `tokenfactory.rs` | ~400 | state, messages, errors, tokenfactory custom messages |

Out of scope: tests (`src/tests.rs`, `tests/invariants.rs`), the `scripts/` folder, the keeper bot
`scripts/keeper/` (its key has no power over funds, see keeper.md).
Version under review: branch `main`; state the commit in your report. Dependencies are pinned in `Cargo.lock`
(`cosmwasm-std 2.2`, `cw-storage-plus 2.0`).

Network: Cosmos Hub, gaia v29.1.0, CosmWasm (wasmd 0.60.10). What is available to the contract is described in
[hub-capabilities.md](hub-capabilities.md); the whole model is in [architecture.md](architecture.md).

## 2. How to reproduce the checks

```bash
cargo test --workspace                       # 151 unit tests and 18 simulator tests
PROPTEST_CASES=2000 cargo test --test invariants   # deep simulator run (about 40 s)
bash scripts/build-wasm.sh                   # nightly build and cosmwasm-check with Hub capabilities
bash scripts/dev/mutation-check.sh           # 44 mutations, each must break the tests
bash scripts/dev/smoke-test.sh artifacts/liatom_hub.wasm        # full cycle on a local gaiad, about 6 minutes
bash scripts/dev/smoke-rebalance.sh artifacts/liatom_hub.wasm   # Rebalance with a real Redelegate
```

The environment (dev VM, gaiad, local chain) is described in [dev-environment.md](dev-environment.md).
The public Hub testnet (`provider`) cannot take the contract: CosmWasm is disabled on it.

## 3. Trust model

| Who | Can | Cannot |
|---|---|---|
| Any user | `Deposit`, `Redeem`, `Claim`, and the public `Harvest`, `SubmitUnbonding`, `FinalizeBatch` | take other people's ATOM, bypass the pro-rata payout |
| Admin | `UpdateValidators`, `Rebalance`, `UpdateConfig` (fee up to 20%, treasury, timings, min deposit, TVL cap), `Pause`, hand over the admin | withdraw ATOM, mint liATOM, change the rate directly |
| Treasury | receives the reward fee | anything beyond it |
| Wasm code admin (the contract's `admin` field in CosmWasm) | **migrate the contract to any code** | see section 6, item 1 |
| Validators in the set | cause slashing if they misbehave | |

Money leaves the contract by only three paths: the fee to the treasury (`Harvest`), the `Claim` payout in
proportion to the share in a batch, and staking messages (`Delegate` / `Redelegate` / `Undelegate` to validators).

## 4. What was checked

Invariants checked by the simulator in `tests/invariants.rs` over random sequences of operations (deposit,
redeem, harvest, submit unbonding, finalize, claim, reward accrual, time passing, slashing, weight changes, fee
and epoch length changes):

1. Conservation of value: the ATOM of users, the contract, the treasury and the chain (unbonding) add up, net of slashing.
2. Solvency: the contract balance covers everything finalized batches still owe.
3. The rate does not fall without slashing.
4. The seed deposit stays with the contract.
5. A `Claim` payout is at most what the batch received and equals the exact rounded-down share.
6. At most 7 live unbonding entries per validator (the simulator panics on the eighth, like the chain).
7. `Rebalance` does not increase the deviation of delegations from the target weights.

Mutation checking (44 cases) shows that the tests catch: ignoring `pending_liatom`, rounding a payout or a mint
up, a fee that bypasses the pool, ignoring slashing, the redeemers' reserve in the rate, a fee change without a
harvest, weak timing validation, rebalancing the wrong way, accepting a shortfall without the grace period, a
shortfall assigned entirely to the first batch instead of shared pro rata, a deposit ignoring the TVL cap,
missing denom metadata, pricing a deposit while matured funds are in transit, finalizing in the arrival block, unbounded timing parameters, pricing while a matured batch is unfinalized, early-arriving unbonded ATOM being harvestable, a deposit that does not collect the payout of its own delegation, rounding dust treated as slashing, the shortfall grace counted from the latest batch, a batch that matured moments ago joining the settlement set, settling all batches under one grace period, a lowerable `unbonding_secs`, and `SubmitUnbonding` depending on the fee transfer.

Verified on a real `gaiad`: the full cycle with unbonding, `UpdateConfig`, `Rebalance` with a real `Redelegate`
(50/50 move, draining a removed validator), refusals for a non-admin and unsafe timings, pause modes, the TVL cap,
denom metadata, and the keeper bot end to end.

**Not verified:** real slashing on a network (only in the model and on a mock querier), `Rebalance` under the
chain limits (7 entries per pair, the transitive ban), load and gas with 30 validators, behaviour with several
simultaneous unbonding batches on a real network with the real 21-day period.

## 5. Results of the 13th review series

The contract went through thirteen series of independent AI review passes (several models per series, each in a
private copy of the code, each asked to reproduce a finding with a failing test or a numeric trace). Every
finding of the earlier series was fixed, covered by a regression test and by a deliberate breakage that the tests
must catch (`scripts/dev/mutation-check.sh`); the reports of those series are not kept in this repository. The
results of the latest (13th) series, on commit `e896e13` before the fixes listed here:

- **Differential testing (sonnet).** An independent reference model of the protocol's accounting was written from
  the documentation only (`tests/refmodel/mod.rs`) and attached to every transaction of the chain-model simulator.
  It compares minted and burned liATOM, every batch's `atom_expected`, every claim, the rate, the pool value, the
  treasury fees and which operations are refused. 5,000 cases on all 19 simulator tests: no disagreement that is a
  contract bug; four deliberate breakages each fail with a one-uatom trace. Documentation ambiguities that the model
  exposed are clarified in `architecture.md`.
- **Build, keeper, web and supply chain (opus).** The wasm build was not reproducible by a third party (unpinned
  nightly, builder paths inside the binary, a stale `Cargo.lock`): fixed (`rust-toolchain.toml` pins a dated
  nightly, `--locked`, `--remap-path-prefix`, `artifacts/BUILD-INFO.txt`; two builds from different checkout
  directories give the same sha256 and the binary contains no builder path). The documented keeper service could not
  sign under systemd with a `file` keyring and its default gas price was 400 times the network floor: fixed. The web
  page now shows the contract address it talks to. A validator that is jailed is accepted by `UpdateValidators`
  and deposits keep flowing to it (documented, section 6 item 7). No XSS sinks, the dev-mnemonic path is stripped
  from production builds, the keeper key has no power over funds.
- **Parsing and fuzzing (haiku).** Every message the web, the keeper and the scripts send deserialises with the
  contract's real types. Unknown JSON fields were silently ignored (a misspelled admin field skipped a setting):
  fixed (`deny_unknown_fields`). `parse_tokenize_response` rejected unknown protobuf fields, which would make
  every `RedeemToShares` fail if the chain adds a field: fixed. Unreachable panics and wraps in the pure functions
  for inputs far above any real supply are documented (section 6 item 22) and kept as ignored tests.

Test totals at this point: 151 unit tests, 19 simulator tests (with the reference model), 46 wire-format, fuzz and
property tests, 44 deliberate breakages (all caught).

## 6. Known limits and questions for reviewers

1. **Wasm code admin.** If the contract is created with `--admin <address>`, that address can run `MsgMigrateContract` to arbitrary code, that is, take all funds. The contract has no timelock and no `migrate` entry point of its own: new code must bring its own. Users trust that address fully; the wasm admin can also be cleared (`clear-contract-admin`), after which migration is impossible. `ProposeAdmin` / `AcceptAdmin` change only the contract's own admin, not the wasm admin. Reviewers should also assess the migration path.
2. **Pause modes.** `deposits_only` blocks `Deposit` and `DepositTokenizedShares`, `full` also blocks `Redeem` and `RedeemToShares`. With `full` the admin can
   lock users' exit (`Claim` and `FinalizeBatch` always work), so this is a trust point. The block
   applies only to joining the queue, not to payouts that have already matured.
3. **Slashing during unbonding.** The shortfall is shared pro rata between the matured batches that are settled together (`settle_matured`); see item 10 for who carries it. Donations and rewards on the balance are not part of any price (item 6).
4. **The redemption rate is fixed at `SubmitUnbonding`**, not at `Redeem`: for up to 4 days the user bears slashing
   risk and earns no rewards.
5. **The ATOM-arrival block.** Closed: no price reads the balance, so arrival timing cannot move a price. `FinalizeBatch` waits 60 seconds after `mature_at`.
6. **Donations.** ATOM sent straight to the contract is ownerless: it is not part of any price, cannot be withdrawn and does not change a rate, a mint or a batch (the balance is read only in `FinalizeBatch`, as `balance - owed_to_redeemers`). It can only cover a batch's shortfall or sit idle. The seed deposit protects against the first-depositor inflation attack; `ZeroMint` rejects deposits that round to zero.
7. **Validators.** The launch set is a single validator : the whole stake is exposed to slashing and
   downtime of one validator, `Rebalance` is not needed, and if the validator is jailed or becomes unavailable,
   `Deposit` keeps delegating to it (the chain accepts a delegation to a jailed validator, which earns nothing) until the admin replaces the set with `UpdateValidators` (exit keeps working: unbonding
   comes from the same validator). `UpdateValidators` does not check that a validator is bonded and not jailed: the operator checks it. The contract does not watch for jailed or tombstoned validators: the admin must
   remove them from the weights and call `Rebalance`. An unavailable validator in the set breaks `Deposit` until the
   admin removes it.
8. **Chain unbonding period.** `unbonding_secs` in the config is set at instantiate (it can later be lowered only to 7/8 of that value) and the
   contract does not read the chain parameter (the Hub accepts only one gRPC query from contracts, and the unbonding entries
   are not exposed to them). Safety does not depend on the two matching, with one bounded exception:
   - **Config longer than the chain's** (including after governance shortens the chain's period): the ATOM arrives early and
     waits, reserved, until `mature_at`; nobody can harvest it. Redemptions are only slower.
   - **Config shorter than the chain's by less than 7 days:** a batch whose ATOM is late waits for it for 7 days counted from
     its own `mature_at`.
   - **Config shorter than the chain's by more than 7 days** (for example after governance lengthens the chain's period and
     nobody raises `unbonding_secs`): the late ATOM is mistaken for slashing after the grace period, redeemers are underpaid
     and the ATOM that arrives later goes to the holders. The keeper bot logs an error as soon as the two differ; the admin
     must then raise `unbonding_secs` at once. If it is longer than the chain's, nothing is lost (the early ATOM waits until `mature_at`); the keeper bot reports this as an error and the admin lowers it, down to the floor.
9. **Gaia specifics:** the 10 ATOM tokenfactory fee is taken from the contract balance at instantiate; the format
   of tokenfactory custom messages depends on the Hub bindings; `Redelegate` is limited by the chain.
10. **Slashing is socialised across batches, and the split depends on the moment of finalization (design, haiku, opus2).**
    A batch finalized alone before a later batch has matured gets what the balance holds for it; batches finalized together
    share a shortfall pro rata to what they expected. A slash hits only the entries that were unbonding at the infraction, but
    the contract cannot tell whose ATOM is whose (ATOM is fungible on the balance), so it shares the loss pro rata: in either
    direction (a slash on the earlier batch is partly carried by a later one, and the reverse). With a configured period
    longer than the chain's, a later batch's early ATOM can also cover an earlier batch's shortfall. These are transfers
    between redeemers only; total payouts never exceed the balance and holders are not affected. A candidate fix, subtracting
    the expected amount of not-yet-matured batches from the available balance in `FinalizeBatch`, was rejected: in the normal
    flow their ATOM has not arrived yet, so every finalization would look short and wait the grace period.
    Consequence (sonnet4, opus4): after a real slash that hit one batch, the old batches (past their own grace) are paid in full
    out of the ATOM that already arrived for the younger ones, and each younger batch in turn is short when its own grace is over,
    so the whole loss lands on the youngest batches and an unslashed batch can lose more than the slash itself. This is a transfer
    between redeemers only and the only alternative (writing off a late batch together with the old one) would leak redeemers'
    ATOM to the holders, which is worse; the loss is settled batch after batch, one grace period each.
11. **Unclaimed rewards can delay a near-full `SubmitUnbonding` until `Harvest` (haiku, haiku3).** The pool value counts unclaimed rewards, but undelegation draws only from delegations, so a batch near 100% of the supply can fail with `InsufficientDelegations` until someone calls the permissionless `Harvest`.
12. **The TVL cap bounds the pool value** (delegated stake plus net free rewards). ATOM that is unbonding for
    redeemers is not counted, so ATOM committed at the same time can exceed `max_tvl` (haiku).
13. **Rounding tolerance can settle a late batch short by dust (opus8).** A deficit of up to 10 uatom per `Undelegate` is settled at once, pro rata from what is there (so rounding dust never freezes a batch for the grace period); if the ATOM is in fact late (a chain halt across maturity, a chain period up to 7 days longer than configured) the batch is finalized short by at most that amount (about 500 uatom for 50 validators) and the late ATOM becomes ownerless. Bounded and not exploitable.
14. **The treasury must be an ordinary account (sonnet3).** If it cannot receive funds (a module account, a typo), `Deposit`,
    `Harvest` and fee changes fail until the admin corrects it with a treasury-only `UpdateConfig`; exits keep working
    (`SubmitUnbonding` ignores a failed fee transfer, `Redeem`, `FinalizeBatch` and `Claim` do not send fees). Check the address
    with a small test transfer before launch.
15. **Absent:** governance voting, instant redemption, keeper bot alerts (the bot itself exists, see keeper.md).
16. **A chain period much longer than the configured one throttles exits (sonnet8).** If governance lengthens the chain's unbonding period far beyond `unbonding_secs` (for epoch 4 d and 21 d configured: 28 d or more), the chain's 7-entry limit makes `SubmitUnbonding` revert until older entries complete: exits slow down, nothing is lost. The keeper bot reports the mismatch; the admin raises `unbonding_secs`.
17. **The entry through LSM can dodge a known slash when there are several validators (opus9, opus10).** A delegator at a validator that is about to be slashed (downtime is visible in advance, a double-sign proof can arrive late) tokenizes and deposits: the contract mints at the pre-slash value, the slash then hits the pool's stake at that validator pro rata, and the depositor's exposure to it drops from 100% to their share of the pool. The loss moves to the other holders. A deposit and exit loop is bounded by the weight cap: an LSM deposit may leave its validator at most 20% (relative) above its target weight, so the pool cannot be concentrated on the risky validator beyond that; the single-shot dodge remains, bounded by the room under the cap. Zero with the one-validator launch set. The exit half is closed: `RedeemToShares` is pro rata over all validators. Further mitigations if needed: pause deposits while a validator is jailed or about to be slashed, a cooldown on liATOM minted through the LSM path.
18. **`RedeemToShares` can be blocked while LSM caps or a redelegation are in the way (opus9, opus10, sonnet10).** It tokenizes from every validator, so one validator with a recent incoming redelegation (21 days after a `Rebalance` into it, `HasReceivingRedelegation`), or at its per-validator or the global LSM cap (an attacker with stake can fill a small validator's cap), blocks the whole exit. Nothing is burned when it fails and `Redeem` always works. A wallet that redeems the whole balance of a share token may be a few shares short (the record can hold slightly fewer shares than tokens were issued); redeem `trunc(shares)`. The gas of a 50-validator exit was not measured.
19. **A redelegation slash can hit batches in flight harder than the slash itself (opus11).** After an admin `Rebalance` A -> B, a double-sign by A from before the move that is proven later is slashed as `slashFactor x InitialBalance` taken first from unbonding entries at B created after the infraction (up to 100% of each entry, so a batch can lose its whole part at B), and then the full `slashFactor x shares` is still unbonded from the delegation, so more than the slash amount can burn; the unbond also pays B's pending rewards outside any contract transaction (they become ownerless balance and the pool value drops by them). Needs a `Rebalance` followed by a late proof of an old infraction; zero with the one-validator launch set. Mitigations if it matters: avoid `Rebalance` while evidence against the source validator may still arrive, or have `SubmitUnbonding` skip validators with a fresh incoming redelegation.
20. **What the admin can still do without a migration (opus12).** (a) `Pause { full }` blocks `Redeem` and `RedeemToShares` for good (limit 2); a batch already submitted always finalizes and pays. (b) The set can name a validator with up to 25% commission when it is added, and the validator can raise it later up to its own max rate; the chosen validators also hold the governance votes of the pool's stake. (c) `UpdateConfig { min_deposit }` set very high disables deposits and `RedeemToShares` (`Redeem` stays open). (d) `ProposeAdmin` / `AcceptAdmin` change only `Config.admin`, not the wasm admin. (e) After `clear-contract-admin` the contract's admin keeps (a)-(c) with no migration left to recover from them. (f) A migration can mint unlimited liATOM (the denom is the contract's) and dump it on exchanges, lending markets and IBC holders; it cannot burn or move other people's liATOM (the Hub enables only `EnableSetMetadata` and `EnableCommunityPoolFeeFunding`). (g) The earlier admin-only freezes were closed (an exit re-delegates only to a validator it holds stake at, the number of delegations is bounded).
21. **Gas is estimated, not measured (opus12).** SDK-gas estimates (about +-50%): `compute_pool` costs about 26k gas per delegation (1.3M at 50), `Deposit` about 1.6M, `SubmitUnbonding` about 7M, `Harvest` about 4M, `Rebalance` about 3.7M, `RedeemToShares` 17-22M at 50 validators (60-75% of a 30M block; one tokenize plus one reply per validator) and it leaves the user one share token per validator to redeem. The `Pool` and `ExchangeRate` smart queries fail near 115 delegations against the usual 3M node query gas limit. Check the live block gas limit (`gaiad q feemarket params`) and measure a 50-validator exit on the local chain before the set is widened beyond a few validators; the launch set has one.
22. **Pure functions assume a real supply (haiku13).** Several helpers in `rate.rs` panic or wrap for inputs far above any real supply (a deposit or total near `u128::MAX/2`, a fee above 100%, weights summing over 10000, stakes whose total overflows): the contract caps the fee at 20%, validates the weights and the real supply is about 5e14 uatom, so none is reachable; `overflow-checks` is on in the release profile, so such an input traps and fails the transaction instead of wrapping. The tests that show it are kept as ignored tests in `tests/rate_props.rs`. Also: `Redeem` has no minimum (a dust burn at a rate below 1 can be worth 0 ATOM, the burner's loss only).

Questions that need independent opinions:
- Correctness of the rate formula with simultaneous implicit reward payouts (distribution hooks on any delegation
  change) and the fee charged on unclaimed rewards.
- Whether `ZeroMint` and the seed suffice against rate manipulation through donations and rounding.
- Safety of the message order in `Harvest` (`WithdrawDelegatorReward`, then `BankMsg::Send`, then `Delegate`) when
  amounts are computed before the messages are sent, without `reply`.
- Getting stuck in a state with no exit: a batch in `Unbonding` that cannot be finalized, `SubmitUnbonding` with
  `InsufficientDelegations`, `UNBONDING` overflow.
- Behaviour when a validator is slashed while holding a delegation that was just redelegated.
