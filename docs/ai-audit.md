# Auditing liatom-hub with AI models

This repository is meant to be audited in public, by anyone, with any model. There is no paid third-party
audit planned for the first launch. Instead the contract is small, fully tested, deployed with a hard
deposit cap, and open to review. This page tells you (or your model) how to do a useful review.

**Read this first.** A model saying "no issues found" is not evidence of safety. A finding is only worth
reporting if it comes with a **failing test or a concrete step-by-step scenario**. Please prefer reproducible
exploits over opinions.

## What is being audited

`contracts/liatom-hub`: a single CosmWasm contract on Cosmos Hub (gaia v29.1.0, wasmd 0.60.10) that issues a
liquid-staking token `liATOM` (tokenfactory denom `factory/<contract>/liatom`).

| File | Lines | Role |
|---|---|---|
| `src/contract.rs` | ~1350 | entry points, every message handler, pool valuation (`compute_pool`) |
| `src/rate.rs` | ~490 | pure arithmetic: exchange rate, mint, fee split, rebalance, matured-batch settlement |
| `src/lsm.rs` | ~190 | LSM share tokens: denom parsing, the hand-written protobuf of `MsgTokenizeShares` / `MsgRedeemTokensForShares` and of the tokenize response |
| `src/state.rs`, `msg.rs`, `error.rs`, `tokenfactory.rs` | ~500 | storage, messages, errors, tokenfactory custom messages |

Out of scope: `src/tests.rs`, `tests/invariants.rs`, `scripts/`, `web/`. The keeper bot (`scripts/keeper/`) holds
no power over funds: it only calls permissionless messages. The web UI is a static page that sends
`MsgExecuteContract` to one address from `config.json`.

Launch parameters: one validator, `max_tvl` of 100 ATOM (raised to 300 ATOM after launch), fee 10%. The contract is live
on Cosmos Hub, see the status in the [README](../README.md).

## Where the money is

- User ATOM is delegated by the contract itself (it is the delegator). Only the contract can undelegate.
- liATOM is minted by the contract in `Deposit` and `DepositTokenizedShares` only (plus the one-time seed at `instantiate`).
- ATOM leaves the contract only through `Claim` (to the claimant), fee transfers to the treasury (made by every
  call that collects rewards: `Harvest`, `Deposit`, `DepositTokenizedShares`, `RedeemToShares`, `SubmitUnbonding`,
  `Rebalance`, a fee change) and staking messages. `RedeemToShares` also hands out LSM share tokens of the
  contract's own delegations, for liATOM it burned. Anything else leaving the contract is a bug.

## Roles and trust

| Who | Can do |
|---|---|
| Anyone | `Deposit`, `DepositTokenizedShares`, `Redeem`, `RedeemToShares`, `Claim`, and the permissionless maintenance calls `Harvest`, `SubmitUnbonding`, `FinalizeBatch` |
| Admin | `UpdateConfig`, `UpdateValidators`, `Rebalance`, `Pause`/`Unpause`, `ProposeAdmin`/`AcceptAdmin` |
| Wasm admin | `MsgMigrateContract`: can replace the code. Users fully trust this key. There is no timelock. |

The admin has no message that mints liATOM, withdraws staked ATOM or rewrites a claim. Treat any path that gives it
one as a finding. Its indirect powers are known: fee up to 20% and the treasury address, the validator set (stake
can be pointed at a bad validator), pause `full` (stops new redeems), and misconfigured timings (for example an
`unbonding_secs` shorter than the chain's makes `FinalizeBatch` treat late funds as slashing). Report anything
beyond these. The wasm admin is outside the threat model by design (it can replace the code), but say so if
migration could be made safer.

## Properties that should always hold

1. **Conservation.** Total value (delegations, unbonding, balance, rewards) is accounted for; nothing is created
   or lost except through slashing and the protocol fee.
2. **Solvency.** The balance covers everything finalized batches still owe.
3. **No free minting.** liATOM supply grows only by `Deposit` at the current rate against attached ATOM, or by `DepositTokenizedShares` at the same rate against the growth of the contract's delegation that the redemption of the attached share token caused.
4. **Rate does not fall without slashing.** `pool_value / (supply + pending_liatom)` is non-decreasing.
5. **Exit is never blocked by `deposits_only`;** `Claim`, `FinalizeBatch`, `Harvest`, `SubmitUnbonding` work
   in every pause mode. Only `full` pause stops new `Redeem`.
6. **A claim pays at most its pro-rata share of what its batch received**; shortfalls are shared pro rata and
   the outcome does not depend on who calls `FinalizeBatch` or in which order.
7. **At most 7 unbonding entries per validator** (a chain limit), enforced by the `unbonding_secs <= 6 * epoch_secs` guard.
8. The `max_tvl` cap holds for every deposit; lowering it never blocks exits.
9. The seed liATOM held by the contract is never released.

## How to run everything

```bash
cargo test --workspace                        # 151 unit tests, 19 simulator tests (with a reference model, tests/refmodel) and 46 wire-format, fuzz and property tests
PROPTEST_CASES=2000 cargo test --test invariants
bash scripts/build-wasm.sh                    # needs rust nightly + rust-src; runs cosmwasm-check
bash scripts/dev/mutation-check.sh            # 44 mutations; each must break the tests
```

`tests/invariants.rs` contains a chain simulator (delegations, rewards, slashing, unbonding with the
real 7-entry limit, EndBlock-time arrival of matured funds). Add your exploit as a test there or in
`src/tests.rs` (the unit-test helpers `arrival_window()` and `two_matured_batches()` in `src/tests.rs` are ready-made scenarios).

On-chain scripts (`scripts/dev/*.sh`) need a local `gaiad` v29.1.0; see `docs/dev-environment.md`.
Most findings do not need them.

## Chain facts the contract relies on (verify these too)

- Unbonded ATOM returns to the balance at EndBlock of the first block with time >= completion, i.e. after
  the transactions of that block. Hence the 7-day `SHORTFALL_GRACE_SECS` (per batch) and the 60-second `FINALIZE_DELAY_SECS`. The contract cannot read the chain's real unbonding entries, only its own `unbonding_secs`:
  no price reads the balance at all (it is read only in `FinalizeBatch`), and the rewards that delegation
  changes pay out are collected in the same transaction (`sweep_rewards`).
- Distribution pays pending rewards to the delegator whenever a delegation changes.
- Tokenfactory charges a 10 ATOM creation fee to the contract balance; the custom message format is the Hub
  binding (`create_denom`, `set_metadata`, `mint_tokens`, `burn_tokens`).
- `Redelegate` has its own limits (7 entries, no transitive redelegation). These were not tested against a
  real network.

## Known limits (do not report as new)

See `docs/audit-prep.md` section 6 and `docs/audit-log.md`. In short: slashing is covered first by
free rewards on the balance; the contract trusts the configured `unbonding_secs` and does not read the chain
parameter (a batch is underpaid only if the chain's period exceeds the configured one by more than 7 days); the
wasm admin can migrate the contract; real slashing was not tested on a live network.

## Suggested review procedure

Run several independent passes with different models, and do not show a model the findings of another pass
until it has produced its own.

1. **Map.** Have the model summarise every message handler and the storage it touches.
2. **Properties.** Ask it to try to break each property above, one at a time.
3. **Adversarial passes**, one per attacker: a depositor, a redeemer, a front-runner around epoch and
   maturity boundaries, a donor sending ATOM to the contract, the keeper caller, a malicious or
   compromised admin, a slashed validator.
4. **Arithmetic.** Rounding direction at every division, overflow, zero amounts, tiny amounts, first and last depositor.
5. **Chain interaction.** Message order inside one transaction, state read vs state after the messages run,
   what happens in the block where unbonding completes.
6. **Reproduce.** Every claimed bug must become a failing test. Run the test; paste its output.

### Prompt you can start from

```text
You are auditing a CosmWasm liquid staking contract for ATOM (Cosmos Hub). Repository root contains docs/ai-audit.md
and docs/architecture.md. Read docs/ai-audit.md first. Audit contracts/liatom-hub/src/{contract,rate,state,msg,error,tokenfactory}.rs.
Goal: find any way to (a) take ATOM that belongs to others, (b) mint liATOM without matching ATOM, (c) block
users from exiting, (d) make the exchange rate fall without slashing, (e) give the admin power over user funds.
For every issue give: severity, exact file and function, a numbered exploit scenario with concrete numbers,
and a Rust test that fails on the current code (use the helpers in src/tests.rs). If you cannot produce a
failing test or a concrete scenario, list it separately as "suspicion", not as a finding. Do not report the
known limits in docs/ai-audit.md. Say which properties you tried to break and why you think each holds.
```

## Reporting

Open an issue (or send the maintainers a report) using this shape:

```text
Model and version:
Commit:
Severity (critical / high / medium / low / info):
Location (file, function):
Scenario (numbered steps with concrete numbers):
Failing test (code) and its output:
Suggested fix:
```

Each reviewed commit and its outcome is tracked in [audit-log.md](audit-log.md). If you ran a pass and found
nothing, please say so too, with the model, commit and scope: negative results are useful to see.
