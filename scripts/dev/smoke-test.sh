#!/usr/bin/env bash
# On-chain smoke test of liatom-hub against the local testchain (see init-testchain.sh).
# Usage: smoke-test.sh /path/to/liatom_hub.wasm
set -euo pipefail
export PATH=$PATH:$HOME/src/gaia/build
WASM=${1:?path to liatom_hub.wasm}
H=$HOME/liatom-dev/node
CHAIN=liatom-local
Q="gaiad --home $H"
G="gaiad --home $H --keyring-backend test"
TX="--chain-id $CHAIN --gas auto --gas-adjustment 1.6 --gas-prices 2uatom -y -o json --broadcast-mode sync"

# Sends a tx, waits for inclusion, fails loudly on a non-zero code, prints the tx JSON.
send() {
  local out hash res code
  out=$("$@" 2>&1) || { echo "send failed: $out" >&2; return 1; }
  local bcast
  bcast=$(echo "$out" | grep "^{")
  if [ "$(echo "$bcast" | jq -r '.code')" != "0" ]; then
    echo "rejected at broadcast: $(echo "$bcast" | jq -r '.raw_log')" >&2
    return 1
  fi
  hash=$(echo "$bcast" | jq -r ".txhash")
  for _ in $(seq 1 15); do
    sleep 1
    res=$($Q q tx "$hash" -o json 2>/dev/null) && break || true
  done
  code=$(echo "$res" | jq -r '.code')
  if [ "$code" != "0" ]; then echo "tx $hash failed: $(echo "$res" | jq -r '.raw_log')" >&2; return 1; fi
  echo "$res"
}

addr() { $G keys show "$1" -a; }
bal()  { $Q q bank balances "$1" -o json | jq -c '[.balances[] | {(.denom): .amount}] | add // {}'; }

VALOPER=$($G keys show validator --bech val -a)
echo "== store"
CODE=$(send $G tx wasm store "$WASM" --from deployer $TX | jq -r '.events[] | select(.type=="store_code") | .attributes[] | select(.key=="code_id") | .value')
echo "code_id=$CODE"

echo "== instantiate"
INIT=$(jq -nc --arg a "$(addr deployer)" --arg t "$(addr deployer)" --arg v "$VALOPER" '{
  admin:$a, treasury:$t, fee_bps:1000, epoch_secs:60, unbonding_secs:300, min_deposit:"1000000", max_tvl:"340282366920938463463374607431768211455",
  denom_creation_fee:"10000000", validators:[{validator:$v, weight_bps:10000}]}')
send $G tx wasm instantiate "$CODE" "$INIT" --label liatom --admin "$(addr deployer)" \
  --amount 20000000uatom --from deployer $TX >/dev/null
CONTRACT=$($Q q wasm list-contract-by-code "$CODE" -o json | jq -r '.contracts[-1]')
DENOM="factory/$CONTRACT/liatom"
echo "contract=$CONTRACT"
echo "denom=$DENOM"
echo "delegations: $($Q q staking delegations "$CONTRACT" -o json | jq -c '[.delegation_responses[] | .balance]')"
echo "contract balances: $(bal "$CONTRACT")"

echo "== deposit 5 ATOM from alice"
send $G tx wasm execute "$CONTRACT" '{"deposit":{}}' --amount 5000000uatom --from alice $TX >/dev/null
echo "alice: $(bal "$(addr alice)")"
echo "pool: $($Q q wasm contract-state smart "$CONTRACT" '{"pool":{}}' -o json | jq -c .data)"
echo "rate: $($Q q wasm contract-state smart "$CONTRACT" '{"exchange_rate":{}}' -o json | jq -c .data)"

echo "== redeem 2 liATOM from alice"
send $G tx wasm execute "$CONTRACT" '{"redeem":{}}' --amount "2000000$DENOM" --from alice $TX >/dev/null
echo "alice: $(bal "$(addr alice)")"
echo "batch 1: $($Q q wasm contract-state smart "$CONTRACT" '{"batch":{"batch_id":1}}' -o json | jq -c .data)"
echo "claim:   $($Q q wasm contract-state smart "$CONTRACT" "{\"claim\":{\"batch_id\":1,\"address\":\"$(addr alice)\"}}" -o json | jq -c .data)"
echo "supply:  $($Q q bank total-supply-of "$DENOM" -o json | jq -c .amount)"

sq() { $Q q wasm contract-state smart "$CONTRACT" "$1" -o json | jq -c .data; }
exec_as() { local who=$1 msg=$2; shift 2; send $G tx wasm execute "$CONTRACT" "$msg" --from "$who" "$@" $TX; }

echo "== harvest (keeper)"
sleep 6
echo "pool before: $(sq '{"pool":{}}')"
exec_as keeper '{"harvest":{}}' >/dev/null
echo "pool after:  $(sq '{"pool":{}}')"
echo "rate:        $(sq '{"exchange_rate":{}}')"
echo "treasury (deployer) balance: $(bal "$(addr deployer)")"

echo "== submit unbonding (keeper)"
sleep 10
RATE_BEFORE=$(sq '{"exchange_rate":{}}' | jq -r .rate)
exec_as keeper '{"submit_unbonding":{}}' >/dev/null
echo "batch 1: $(sq '{"batch":{"batch_id":1}}')"
echo "delegations: $($Q q staking delegations "$CONTRACT" -o json | jq -c '[.delegation_responses[] | .balance.amount]')"
echo "rate before=$RATE_BEFORE after=$(sq '{"exchange_rate":{}}' | jq -r .rate)"

echo "== waiting for unbonding (300 s)"
sleep 310

echo "== finalize and claim"
exec_as keeper '{"finalize_batch":{"batch_id":1}}' >/dev/null
echo "batch 1: $(sq '{"batch":{"batch_id":1}}')"
ALICE_BEFORE=$($Q q bank balance "$(addr alice)" uatom -o json | jq -r .balance.amount)
exec_as alice '{"claim":{"batch_id":1}}' >/dev/null
ALICE_AFTER=$($Q q bank balance "$(addr alice)" uatom -o json | jq -r .balance.amount)
echo "alice uatom: $ALICE_BEFORE -> $ALICE_AFTER (fees paid in the claim tx are deducted)"
echo "pool: $(sq '{"pool":{}}')"

echo "== admin: update_config and rebalance"
cfg() { sq '{"config":{}}' | jq -c '{fee_bps,epoch_secs,unbonding_secs,min_deposit,treasury}'; }
echo "before: $(cfg)"
exec_as deployer '{"update_config":{"fee_bps":500,"min_deposit":"2000000"}}' >/dev/null
echo "after:  $(cfg)"
# keeper is not the admin
if exec_as keeper '{"update_config":{"fee_bps":100}}' >/dev/null 2>&1; then echo "FAIL: non-admin changed config"; exit 1; fi
# epoch 10 s with a 300 s unbonding would allow more than 7 entries per validator
if exec_as deployer '{"update_config":{"epoch_secs":10}}' >/dev/null 2>&1; then echo "FAIL: unsafe timing accepted"; exit 1; fi
echo "unsafe timing and non-admin update rejected"
# one local validator already holds everything, so there is nothing to move
if exec_as deployer '{"rebalance":{}}' >/dev/null 2>&1; then echo "FAIL: rebalance with a single validator"; exit 1; fi
echo "rebalance correctly refused (single validator)"
echo "SMOKE OK"
