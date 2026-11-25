# Cosmos Hub capabilities for liATOM

Checked on 2026-11-22 against the Gaia v29.1.0 source (tag `v29.1.0`) and the public `cosmoshub-4` mainnet REST.
Mainnet runs `gaiad v29.1.0`, so the source version matches the live network.

## Stack (Gaia v29.1.0 go.mod)

| Component | Version |
|---|---|
| wasmd | 0.60.10 |
| wasmvm | v2.3.6 (CosmWasm 2.x) |
| cosmos-sdk | 0.53.8 |
| ibc-go | v10.7.0 |
| tokenfactory | github.com/cosmos/tokenfactory v0.53.5 |
| Wasm size limit | 1_677_722 bytes (1.6 MiB) |

## Conclusions for the architecture

1. **Deployment is open.** `code_upload_access = Everybody`, `instantiate_default_permission = Everybody`.
   No whitelist or governance vote is needed to upload code.
2. **Tokenfactory is wired to wasm.** `tokenfactorybindings` are registered in `keepers.go`, so a contract can
   create, mint and burn a native denom `factory/<contract>/liatom`. No cw20 is needed: liATOM is a native token
   (it works with IBC and DEXes without wrappers). Parameters: creating a denom costs 10 ATOM (`10000000 uatom`)
   plus `denom_creation_gas_consume = 2_000_000`. The capabilities `EnableSetMetadata` and
   `EnableCommunityPoolFeeFunding` are enabled.
3. **A contract can stake directly**, without LSM: `StakingMsg::Delegate/Undelegate/Redelegate` and
   `DistributionMsg::WithdrawDelegatorReward`. Such a delegation does not count towards the LSM cap.
4. **`CosmosMsg::Any` is available**, so `MsgTokenizeShares` from `x/liquid` could be sent if ever needed.
5. **Voting from a contract** (`Gov::Vote`, `VoteWeighted`) passes through a Gaia ante decorator that requires the
   contract to hold stake. A contract with stake could vote; this is a policy decision (the v1 contract does not vote).
6. **gRPC queries from a contract are limited by an accept list.** `keepers.go` allows only
   `/cosmos.staking.v1beta1.Query/Validator`. Everything else must be read through the built-in `StakingQuery`
   (BondedDenom, AllValidators, Validator, AllDelegations, Delegation) and `BankQuery`.

## Mainnet parameters at the time of checking

| Parameter | Value |
|---|---|
| Unbonding time | 1 814 400 s (21 days) |
| `max_entries` | 7 (unbonding entries per delegator/validator pair) |
| `max_validators` | 180 |
| `min_commission_rate` | 5% |
| bond denom | `uatom` |
| `global_liquid_staking_cap` (LSM) | 0.25 on mainnet when this was checked; the gaia default is 1.0, so query it live before launch |
| `validator_liquid_staking_cap` (LSM) | 1.0 |
| Total staked | 345 591 002 865 819 uatom (~345.6M ATOM) |
| Staked through LSM | 2 376 416 852 010 uatom (~2.38M ATOM, ~0.69% of stake) |
| Inflation | 10% |

## Risks and open questions

- **The `max_entries = 7` limit.** Daily unbonding on a single contract/validator pair would hit 7 entries within
  21 days. A batch scheme is needed: one unbonding entry per validator per epoch, an epoch of at least 3 days, or a
  queue spreading withdrawals over several validators.
- **Rewards.** The contract must collect and reinvest rewards itself (like Stride's reinvest), or the liATOM/ATOM
  rate does not grow. A call from outside (the keeper bot) is needed: a contract has no automatic `EndBlock`.
  Who makes the call and whether they are rewarded is a design question.
- **Slashing.** A validator's losses must show up in the rate, not fall on the last people to exit. The rate is
  computed as `total_staked_value / supply(liATOM)`.
- **Checked on 2026-11-22 against the wasmd v0.60.10 source** (`x/wasm/keeper/capabilities.go`):
  `BuiltInCapabilities` = `iterator`, `staking`, `stargate`, `cosmwasm_1_1` ... `cosmwasm_1_4`, `cosmwasm_2_0`,
  `cosmwasm_2_1`, `cosmwasm_2_2`. There is nothing above `cosmwasm_2_2`, so the contract is built with
  `cosmwasm-std` no newer than 2.2 and checked with `cosmwasm-check --available-capabilities`.
  `DistributionQuery::DelegationTotalRewards` and `DelegatorWithdrawAddress` are implemented in `query_plugins.go`,
  so the rate scheme in architecture.md (section 4) works without workarounds.
- **Verified later on a local chain** (see dev-environment.md): `Redelegate` from a contract account between two
  validators, and the tokenfactory custom messages. **Not verified on a live network:** the `Redelegate` limits
  (7 entries per pair, the transitive ban) and real slashing.
- **Keys and governance.** The contract admin (fee, validator set) and the wasm admin (migrations) are separate roles; see audit-prep.md section 6.

## What we take from Stride (ideas, not code)

- The exchange rate and `total_staked` / `supply` accounting.
- A protocol fee on rewards.
- An unbonding queue and redemption records.
- Distributing stake over validators by weights, and rebalancing.

Stride's ICA, ICQ, epochs and icacallbacks modules are not needed: staking happens on the same chain.
