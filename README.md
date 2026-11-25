# liATOM

Liquid staking for ATOM on Cosmos Hub, as a single CosmWasm contract (`contracts/liatom-hub`).
Deposit ATOM (or already staked ATOM, through LSM share tokens), receive `liATOM` (a tokenfactory denom) whose exchange rate grows with staking rewards (10% fee).
Redemptions run in 4-day epoch batches through the chain's 21-day unbonding. No instant redeem, no voting from the contract.

**Status: pre-launch.** No mainnet contract yet. The first launch is capped at 100 ATOM.
The code is meant to be reviewed in public with any tools or models: start with [docs/ai-audit.md](docs/ai-audit.md),
results are tracked in [docs/audit-log.md](docs/audit-log.md), reporting rules are in [SECURITY.md](SECURITY.md).

```bash
cargo test --workspace          # unit tests and invariant tests (chain simulator)
bash scripts/build-wasm.sh      # wasm build + cosmwasm-check against Hub capabilities
bash scripts/dev/mutation-check.sh
```

Design reference: [Stride](https://github.com/Stride-Labs/stride). License: Apache-2.0.

## Documentation

[Architecture](docs/architecture.md), [Hub capabilities](docs/hub-capabilities.md),
[review preparation and known limits](docs/audit-prep.md), [AI review guide](docs/ai-audit.md),
[audit log](docs/audit-log.md), [development environment](docs/dev-environment.md), [keeper bot](docs/keeper.md),
[web interface](docs/web.md).
