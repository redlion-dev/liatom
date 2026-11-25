#!/usr/bin/env bash
# End to end test of the keeper bot on the local testchain: a user redeems, the bot alone runs
# submit_unbonding, finalize_batch and harvest, and the user can claim. Takes about 7 minutes.
# Usage: keeper-e2e.sh /path/to/liatom_hub.wasm
set -euo pipefail
export PATH=$PATH:$HOME/src/gaia/build
WASM=${1:?path to liatom_hub.wasm}
HERE=$(cd "$(dirname "$0")" && pwd)
H=$HOME/liatom-dev/node
CHAIN=liatom-local
Q="gaiad --home $H"
G="gaiad --home $H --keyring-backend test"
TX="--chain-id $CHAIN --gas auto --gas-adjustment 1.6 --gas-prices 2uatom -y -o json --broadcast-mode sync"

send() {
  local out bcast hash res
  out=$("$@" 2>&1) || { echo "send failed: $out" >&2; return 1; }
  bcast=$(echo "$out" | grep "^{")
  [ "$(echo "$bcast" | jq -r '.code')" = "0" ] || { echo "rejected: $(echo "$bcast" | jq -r '.raw_log')" >&2; return 1; }
  hash=$(echo "$bcast" | jq -r .txhash)
  for _ in $(seq 1 15); do sleep 1; res=$($Q q tx "$hash" -o json 2>/dev/null) && break || true; done
  [ "$(echo "$res" | jq -r '.code')" = "0" ] || { echo "tx failed: $(echo "$res" | jq -r '.raw_log')" >&2; return 1; }
  echo "$res"
}
addr() { $G keys show "$1" -a; }
V1=$($G keys show validator --bech val -a)

CODE=$(send $G tx wasm store "$WASM" --from deployer $TX | jq -r '.events[] | select(.type=="store_code") | .attributes[] | select(.key=="code_id") | .value')
INIT=$(jq -nc --arg a "$(addr deployer)" --arg v "$V1" '{admin:$a, treasury:$a, fee_bps:1000, epoch_secs:60,
  unbonding_secs:300, min_deposit:"1000000", max_tvl:"340282366920938463463374607431768211455", denom_creation_fee:"10000000", validators:[{validator:$v, weight_bps:10000}]}')
send $G tx wasm instantiate "$CODE" "$INIT" --label "liatom-keeper-$RANDOM" --admin "$(addr deployer)" \
  --amount 20000000uatom --from deployer $TX >/dev/null
CONTRACT=$($Q q wasm list-contract-by-code "$CODE" -o json | jq -r '.contracts[-1]')
DENOM="factory/$CONTRACT/liatom"
echo "contract=$CONTRACT"
sq() { $Q q wasm contract-state smart "$CONTRACT" "$1" -o json | jq -c .data; }

send $G tx wasm execute "$CONTRACT" '{"deposit":{}}' --amount 5000000uatom --from alice $TX >/dev/null
send $G tx wasm execute "$CONTRACT" '{"redeem":{}}' --amount "2000000$DENOM" --from alice $TX >/dev/null
echo "redeemed, batch 1: $(sq '{"batch":{"batch_id":1}}')"

python3 -m unittest discover -s "$HERE/../keeper" 2>&1 | tail -3

LOG=$(mktemp)
python3 "$HERE/../keeper/keeper.py" --gas-prices 2uatom --contract "$CONTRACT" --node http://localhost:26657 --api http://localhost:1317 \
  --chain-id $CHAIN --from keeper --keyring-backend test --home "$H" --interval 15 \
  --min-harvest 1 --harvest-interval 120 > "$LOG" 2>&1 &
KPID=$!
trap 'kill $KPID 2>/dev/null || true' EXIT
echo "keeper pid $KPID, waiting 400 s (epoch 60, unbonding 300)"
sleep 400
kill $KPID 2>/dev/null || true
cat "$LOG"

STATUS=$(sq '{"batch":{"batch_id":1}}' | jq -r .status)
echo "batch 1 status: $STATUS"
[ "$STATUS" = "matured" ] || { echo "FAIL: batch not finalized by the keeper"; exit 1; }
grep -q '"submit_unbonding"' "$LOG" || { echo "FAIL: no submit in the log"; exit 1; }
grep -q '"finalize_batch"' "$LOG" || { echo "FAIL: no finalize in the log"; exit 1; }
grep -q '"harvest"' "$LOG" || { echo "FAIL: no harvest in the log"; exit 1; }
send $G tx wasm execute "$CONTRACT" '{"claim":{"batch_id":1}}' --from alice $TX >/dev/null
echo "claim ok, pool: $(sq '{"pool":{}}')"
echo "KEEPER OK"
