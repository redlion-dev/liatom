#!/usr/bin/env bash
# On-chain test of Rebalance / UpdateValidators with real Redelegate messages.
# Needs the local testchain with two validators (init-testchain.sh, then add-second-validator.sh).
# Usage: smoke-rebalance.sh /path/to/liatom_hub.wasm
set -euo pipefail
export PATH=$PATH:$HOME/src/gaia/build
WASM=${1:?path to liatom_hub.wasm}
H=$HOME/liatom-dev/node
CHAIN=liatom-local
Q="gaiad --home $H"
G="gaiad --home $H --keyring-backend test"
TX="--chain-id $CHAIN --gas auto --gas-adjustment 1.6 --gas-prices 2uatom -y -o json --broadcast-mode sync"

send() {
  local out hash res code bcast
  out=$("$@" 2>&1) || { echo "send failed: $out" >&2; return 1; }
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

V1=$($G keys show validator --bech val -a)
V2=$($G keys show val2 --bech val -a)
$Q q staking validator "$V2" -o json >/dev/null || { echo "run add-second-validator.sh first"; exit 1; }

CODE=$(send $G tx wasm store "$WASM" --from deployer $TX | jq -r '.events[] | select(.type=="store_code") | .attributes[] | select(.key=="code_id") | .value')
INIT=$(jq -nc --arg a "$(addr deployer)" --arg v "$V1" '{
  admin:$a, treasury:$a, fee_bps:1000, epoch_secs:60, unbonding_secs:300, min_deposit:"1000000", max_tvl:"340282366920938463463374607431768211455",
  denom_creation_fee:"10000000", validators:[{validator:$v, weight_bps:10000}]}')
send $G tx wasm instantiate "$CODE" "$INIT" --label "liatom-rebalance-$RANDOM" --admin "$(addr deployer)" \
  --amount 20000000uatom --from deployer $TX >/dev/null
CONTRACT=$($Q q wasm list-contract-by-code "$CODE" -o json | jq -r '.contracts[-1]')
echo "contract=$CONTRACT"

sq() { $Q q wasm contract-state smart "$CONTRACT" "$1" -o json | jq -c .data; }
exec_as() { local who=$1 msg=$2; shift 2; send $G tx wasm execute "$CONTRACT" "$msg" --from "$who" "$@" $TX; }
# delegation of the contract to one validator, in uatom (0 when none)
deleg() { $Q q staking delegation "$CONTRACT" "$1" -o json 2>/dev/null | jq -r '.delegation_response.balance.amount // "0"' || echo 0; }
show() { echo "  v1=$(deleg "$V1") v2=$(deleg "$V2")"; }
expect_near() { # name actual expected tolerance
  local d=$(( $2 > $3 ? $2 - $3 : $3 - $2 ))
  if [ "$d" -gt "$4" ]; then echo "FAIL $1: got $2, expected $3 (+-$4)"; exit 1; fi
  echo "  ok $1: $2"
}

echo "== deposit 100 ATOM, everything goes to v1"
exec_as alice '{"deposit":{}}' --amount 100000000uatom >/dev/null
show

echo "== rebalance with unchanged weights: nothing to move"
if exec_as deployer '{"rebalance":{}}' >/dev/null 2>&1; then echo "FAIL: rebalance without changes"; exit 1; fi
echo "  refused"

echo "== weights 50/50, then Rebalance"
exec_as deployer "$(jq -nc --arg a "$V1" --arg b "$V2" '{update_validators:{validators:[{validator:$a,weight_bps:5000},{validator:$b,weight_bps:5000}]}}')" >/dev/null
exec_as deployer '{"rebalance":{}}' >/dev/null
show
TOTAL=$(( $(deleg "$V1") + $(deleg "$V2") ))
# Rebalance also re-delegates the rewards its redelegations paid out (a few uatom), to one validator.
expect_near "v1 half" "$(deleg "$V1")" $((TOTAL / 2)) 40
expect_near "v2 half" "$(deleg "$V2")" $((TOTAL / 2)) 40
REDEL=$(curl -s "localhost:1317/cosmos/staking/v1beta1/delegators/$CONTRACT/redelegations?src_validator_addr=$V1&dst_validator_addr=$V2" | jq -c '[.redelegation_responses[] | {entries:(.entries|length)}]')
echo "  redelegations v1->v2: $REDEL"
echo "  pool: $(sq '{"pool":{}}')"

echo "== rebalance again: already balanced"
if exec_as deployer '{"rebalance":{}}' >/dev/null 2>&1; then echo "FAIL: second rebalance should find nothing"; exit 1; fi
echo "  refused"

echo "== drop v1 from the weights (v2 100%), Rebalance drains v1"
# v1 just received a redelegation as source; moving back from v2 is the allowed direction, v1 as
# source is allowed too because v1 is not a destination of a live redelegation.
exec_as deployer "$(jq -nc --arg b "$V2" '{update_validators:{validators:[{validator:$b,weight_bps:10000}]}}')" >/dev/null
exec_as deployer '{"rebalance":{}}' >/dev/null
show
expect_near "v1 drained" "$(deleg "$V1")" 0 0
echo "  pool: $(sq '{"pool":{}}')"
echo "  rate: $(sq '{"exchange_rate":{}}')"

echo "== deposit still works and goes to the target validator"
exec_as bob '{"deposit":{}}' --amount 10000000uatom >/dev/null
show
echo "== pause modes"
exec_as deployer '{"pause":{"mode":"deposits_only"}}' >/dev/null
if exec_as bob '{"deposit":{}}' --amount 2000000uatom >/dev/null 2>&1; then echo "FAIL: deposit while paused"; exit 1; fi
DENOM="factory/$CONTRACT/liatom"
exec_as bob "{\"redeem\":{}}" --amount "1000000$DENOM" >/dev/null
echo "  deposits_only: deposit refused, redeem works"
exec_as deployer '{"pause":{"mode":"full"}}' >/dev/null
if exec_as bob "{\"redeem\":{}}" --amount "1000000$DENOM" >/dev/null 2>&1; then echo "FAIL: redeem under full pause"; exit 1; fi
echo "  full: redeem refused"
exec_as deployer '{"unpause":{}}' >/dev/null
exec_as bob '{"deposit":{}}' --amount 2000000uatom >/dev/null
echo "  unpaused: deposit works"
echo "== tvl cap"
exec_as deployer '{"update_config":{"max_tvl":"1000000"}}' >/dev/null
if exec_as bob '{"deposit":{}}' --amount 2000000uatom >/dev/null 2>&1; then echo "FAIL: deposit over the tvl cap"; exit 1; fi
echo "  over the cap: deposit refused"
exec_as deployer '{"update_config":{"max_tvl":"340282366920938463463374607431768211455"}}' >/dev/null
exec_as bob '{"deposit":{}}' --amount 2000000uatom >/dev/null
echo "  cap lifted: deposit works"
echo "== denom metadata"
LIDENOM="factory/$CONTRACT/liatom"
META=$($Q q bank denom-metadata-by-query-string --denom "$LIDENOM" -o json)
[ "$(echo "$META" | jq -r '.metadata.symbol')" = "liATOM" ] || { echo "metadata missing: $META"; exit 1; }
[ "$(echo "$META" | jq -r '.metadata.denom_units[1].exponent')" = "6" ] || { echo "bad exponent"; exit 1; }
echo "  liATOM, 6 decimals"
echo "REBALANCE OK"
