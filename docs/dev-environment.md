# Development environment

The project was developed on a Linux VM (Ubuntu 26.04 LTS, 2 cores, 7.3 GB RAM, 38 GB disk, 8 GB swap) reached over ssh
(an ssh alias, `<dev-vm>` below). Nothing here is specific to that machine: anyone can reproduce the environment on any Linux
host with the same versions. No production validator is used for development.

## Installed

| What | Version | Where |
|---|---|---|
| Rust | 1.100.0 stable, target `wasm32-unknown-unknown`, clippy, rustfmt (plus nightly with `rust-src` for the wasm build) | `~/.cargo` |
| cosmwasm-check | 3.0.11 | `~/.cargo/bin` |
| binaryen (`wasm-opt`) | 120 | apt |
| Go | 1.26.10 | `/usr/local/go` |
| gaiad | v29.1.0 (built from source with `make build`) | `~/src/gaia/build/gaiad` |
| Other | build-essential, clang, cmake, pkg-config, libssl-dev, tmux, jq, git | apt |

The `gaiad` version matches the Hub mainnet (v29.1.0).

## Local test chain

A single-validator chain `liatom-local`, home folder `~/liatom-dev/node`, started in a tmux session `chain`.
Scripts: [scripts/dev/init-testchain.sh](../scripts/dev/init-testchain.sh) (recreates the chain from scratch),
[scripts/dev/start-and-check.sh](../scripts/dev/start-and-check.sh) (start and check parameters).

Genesis differences from mainnet: `unbonding_time = 300s`, `voting_period = 60s`, `min_commission_rate = 0`, a
block every 2 s. Everything else matches: wasm upload and instantiate for everyone, tokenfactory (denom creation
10 ATOM, gas 2_000_000), LSM cap 0.25, `max_entries = 7`.

Test accounts (keys only in the test keyring on the VM, test tokens only): `validator`, `deployer`, `alice`, `bob`,
`keeper`. Keys never enter the repository.

Ports (listening on the VM's localhost only): RPC 26657, REST 1317, gRPC 9090.

## Commands

```bash
tmux attach -t chain                      # chain log
bash ~/liatom-dev/start-and-check.sh      # restart and check
~/src/gaia/build/gaiad status             # status
```

## Building and checking the contract

On the dev VM (`~/work/liatom`, sources synchronised from the workstation with `tar | ssh`):

```bash
cargo test                                   # unit tests and tests on a mock querier
bash scripts/build-wasm.sh                   # wasm + cosmwasm-check with Hub capabilities
bash scripts/dev/smoke-test.sh artifacts/liatom_hub.wasm   # full cycle on the test chain, about 6 minutes (waits for the 300 s unbonding)
bash scripts/dev/add-second-validator.sh                   # a second validator on the same test chain (idempotent)
bash scripts/dev/smoke-lsm.sh artifacts/liatom_hub.wasm         # DepositTokenizedShares and RedeemToShares: tokenize a delegation, redeem it into liATOM, leave as a share token
bash scripts/dev/smoke-treasury.sh artifacts/liatom_hub.wasm    # a treasury that cannot receive funds must not stop SubmitUnbonding; about 1 minute
bash scripts/dev/smoke-rebalance.sh artifacts/liatom_hub.wasm   # UpdateValidators + Rebalance with a real Redelegate, pause modes, TVL cap, denom metadata; about 1 minute
```

The second validator is created with a `create-validator` transaction using a separate consensus key but without a
node: it does not sign blocks. That is enough for staking tests: the jail window is thousands of blocks and the
chain runs on the first validator (its share is above 2/3). This avoids running p2p between two nodes.
`smoke-rebalance.sh` checks on a real chain: a 50/50 move from one validator, a refusal when weights are already
balanced, fully draining a validator removed from the weights, and that a deposit after that goes to the remaining one.

The public Hub testnet (`provider`) does not fit the contract: CosmWasm is not enabled on it.

Things found while building and running:
- **Stable Rust >= 1.82 does not work for wasm:** the standard library contains bulk-memory instructions, and
  CosmWasm rejects such bytecode in static validation. So `build-wasm.sh` builds with nightly, `-Zbuild-std` and
  `-C target-cpu=mvp`. The linker needs `--allow-undefined` for host imports.
- **Hub uses feemarket:** the minimum gas price is `1 uatom`, and a transaction without a fee is rejected at
  broadcast. The scripts use `--gas-prices 2uatom`.
- **Creating a tokenfactory denom takes 10 ATOM from the contract balance**, so the funds attached at instantiate
  are the fee plus the seed deposit (`denom_creation_fee` in `InstantiateMsg`).
- The format of the tokenfactory custom messages (`create_denom`, `set_metadata`, `mint_tokens`, `burn_tokens`) is
  confirmed on a real `gaiad v29.1.0`: the smoke test passes end to end.

## Invariant tests

[contracts/liatom-hub/tests/invariants.rs](../contracts/liatom-hub/tests/invariants.rs): a network simulator runs
the contract's real `execute` / `query` over random sequences of operations (deposit, redeem, harvest, submit and
finalize a batch, claim, reward accrual, time passing, slashing), applies the contract's messages to a model of the
chain, and checks the invariants after every step:

1. **Conservation of value:** the ATOM in the system (balances, treasury, contract, delegations, unwithdrawn
   rewards, unbonding) changes only through reward accrual, slashing and the tokenfactory fee.
2. **Solvency:** the contract balance covers what finalized batches still owe.
3. **The rate does not fall** without slashing (rounding always favours the pool).
4. **The contract's seed liATOM** never leaves the contract.
5. **Batch payouts** do not exceed `atom_received`, and claims do not exceed what was burned.

```bash
cargo test --test invariants                       # 64 random sequences per property
PROPTEST_CASES=3000 cargo test --release --test invariants   # deep run, about 2.5 minutes
bash scripts/dev/mutation-check.sh                 # each deliberate breakage of the contract must fail the tests
```

`mutation-check.sh` replaces one line of logic at a time (a price without `pending_liatom`, rounding up on mint and
payout, a rate without the fee deducted, finalization ignoring slashing, the redemption reserve not subtracted from
the pool, a fee change without a harvest, weak timing validation, stake moved the wrong way, a shortfall accepted
without the grace period or not shared pro rata, a deposit ignoring the TVL cap, missing denom metadata, pricing in
the arrival window, finalizing in the arrival block, unbounded timings, pricing while a matured batch is unfinalized, early-arriving unbonded ATOM being harvestable, a deposit that does not collect its own payout, rounding dust treated as slashing, the shortfall grace counted from the latest batch, a just-matured batch joining the settlement set, one grace period for all batches, a lowerable `unbonding_secs`, a submit that depends on the fee transfer) and checks that the tests fail. All 44 mutations are killed. If the sources are synchronised with
`tar`, run `find . -name '*.rs' -not -path './target/*' -exec touch {} +` before running: otherwise cargo may miss
edits because of old mtimes.

A limit the tests do not count as a bug: with slashing during unbonding, free rewards on the balance can partly
cover a batch's loss at the expense of the remaining holders (see architecture.md, section 8).
