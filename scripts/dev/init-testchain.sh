#!/usr/bin/env bash
# Local single-validator Cosmos Hub (gaiad v29.1.0) test chain for liATOM development.
# Throwaway test keys only. Never reuse these keys on a real network.
set -euo pipefail
export PATH=$PATH:/usr/local/go/bin:$HOME/go/bin:$HOME/src/gaia/build
G=gaiad
HOME_DIR=$HOME/liatom-dev/node
CHAIN=liatom-local
KR="--keyring-backend test --home $HOME_DIR"

rm -rf "$HOME_DIR"
mkdir -p "$HOME_DIR"
$G init liatom-dev --chain-id $CHAIN --home "$HOME_DIR" >/dev/null 2>&1

# gaiad init defaults every denom to "stake"; Hub uses uatom.
sed -i 's/"stake"/"uatom"/g' "$HOME_DIR/config/genesis.json"

for k in validator deployer alice bob keeper; do
  $G keys add $k $KR >/dev/null 2>&1
done

addr() { $G keys show "$1" -a $KR; }
$G genesis add-genesis-account "$(addr validator)" 1000000000000uatom --home "$HOME_DIR"
for k in deployer alice bob keeper; do
  $G genesis add-genesis-account "$(addr $k)" 100000000000uatom --home "$HOME_DIR"
done
$G genesis gentx validator 500000000000uatom --chain-id $CHAIN $KR >/dev/null 2>&1
$G genesis collect-gentxs --home "$HOME_DIR" >/dev/null 2>&1

# Genesis tweaks: short unbonding so claim flow can be tested in minutes, fast gov.
G_JSON=$HOME_DIR/config/genesis.json
jq '.app_state.staking.params.unbonding_time = "300s"
  | .app_state.staking.params.max_entries = 7
  | .app_state.gov.params.voting_period = "60s"
  | .app_state.gov.params.expedited_voting_period = "30s"
  | .app_state.liquid.params.global_liquid_staking_cap = "0.25"' "$G_JSON" > /tmp/genesis.tmp && mv /tmp/genesis.tmp "$G_JSON"

sed -i 's/^minimum-gas-prices = .*/minimum-gas-prices = "0uatom"/' "$HOME_DIR/config/app.toml"
sed -i '/^\[api\]/,/^\[/ s/^enable = false/enable = true/' "$HOME_DIR/config/app.toml"
sed -i 's/^timeout_commit = .*/timeout_commit = "2s"/' "$HOME_DIR/config/config.toml"

$G genesis validate --home "$HOME_DIR"
echo "validator: $(addr validator)"
echo "deployer:  $(addr deployer)"
