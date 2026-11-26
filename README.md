# liATOM

Liquid staking for ATOM on Cosmos Hub, as a single CosmWasm contract (`contracts/liatom-hub`).
Deposit ATOM (or already staked ATOM, through LSM share tokens), receive `liATOM` (a tokenfactory denom) whose exchange rate grows with staking rewards (10% fee).
Redemptions run in 4-day epoch batches through the chain's 21-day unbonding; holders can also leave at once as staked ATOM
(LSM share tokens), but there is no instant redeem into liquid ATOM. The contract does not vote.

## Status: live on Cosmos Hub, limited launch

| | |
|---|---|
| Contract | `cosmos13mqp6tqu0pu4eeexshkx3k20hplf6z6z6pfkec3dqhdl73hg45mqmc79z0` |
| Code ID | `849`, built from commit `afeb52e` of this repository |
| Code checksum (sha256) | `cf46886bc47a9a0767e22707375747d62ce7662ed59a37810be25c1ead68e1a5` |
| Token | `factory/cosmos13mqp6tqu0pu4eeexshkx3k20hplf6z6z6pfkec3dqhdl73hg45mqmc79z0/liatom` (liATOM, 6 decimals) |
| Deposit cap | 300 ATOM for the whole pool (raised step by step) |
| Validator set | one validator at launch: Red Lion |

The build is reproducible: check out `afeb52e`, run `bash scripts/build-wasm.sh` and compare
`sha256sum artifacts/liatom_hub.wasm` with the checksum the chain reports
(`gaiad q wasm code-info 849 -o json | jq -r .checksum`).

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
