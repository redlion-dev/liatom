#!/usr/bin/env bash
# Adds a second validator to the running local testchain (see init-testchain.sh).
# The new validator has its own consensus key but no node behind it: it never signs. That is fine
# for staking tests (the jailing window is thousands of blocks) and avoids setting up p2p.
# Its self-bond is kept far below 1/3 of the voting power so the real validator alone keeps the
# chain live. Idempotent: does nothing when the second validator already exists.
set -euo pipefail
export PATH=$PATH:$HOME/src/gaia/build
H=$HOME/liatom-dev/node
CHAIN=liatom-local
Q="gaiad --home $H"
G="gaiad --home $H --keyring-backend test"
TX="--chain-id $CHAIN --gas auto --gas-adjustment 1.6 --gas-prices 2uatom -y -o json --broadcast-mode sync"
SELF_BOND=${SELF_BOND:-100000000000} # 100k ATOM, vs 500k of the first validator

if ! $G keys show val2 >/dev/null 2>&1; then $G keys add val2 >/dev/null 2>&1; fi
VAL2=$($G keys show val2 -a)
VALOPER2=$($G keys show val2 --bech val -a)

if $Q q staking validator "$VALOPER2" -o json >/dev/null 2>&1; then
  echo "validator 2 already exists: $VALOPER2"
  exit 0
fi

# fund val2 from the validator account
out=$($G tx bank send validator "$VAL2" $((SELF_BOND + 5000000000))uatom --from validator $TX 2>&1 | grep "^{")
[ "$(echo "$out" | jq -r .code)" = "0" ] || { echo "fund failed: $out"; exit 1; }
sleep 6

# throwaway consensus key
TMP=$(mktemp -d)
gaiad init val2 --chain-id $CHAIN --home "$TMP" >/dev/null 2>&1
PUBKEY=$(gaiad tendermint show-validator --home "$TMP")
rm -rf "$TMP"

JSON=$(mktemp)
jq -n --argjson pk "$PUBKEY" --arg amt "${SELF_BOND}uatom" '{
  pubkey:$pk, amount:$amt, moniker:"val2", identity:"", website:"", security:"", details:"",
  "commission-rate":"0.1", "commission-max-rate":"0.2", "commission-max-change-rate":"0.01",
  "min-self-delegation":"1"}' > "$JSON"

out=$($G tx staking create-validator "$JSON" --from val2 $TX 2>&1 | grep "^{")
[ "$(echo "$out" | jq -r .code)" = "0" ] || { echo "create-validator failed: $out"; exit 1; }
sleep 6
$Q q staking validator "$VALOPER2" -o json | jq -c '{operator_address, status, tokens}'
echo "validator 2: $VALOPER2"
