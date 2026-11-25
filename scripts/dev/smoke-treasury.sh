#!/usr/bin/env bash
# On-chain check that a treasury which cannot receive funds (a blocked module account) does not stop
# users from exiting: SubmitUnbonding still goes through (its fee transfer is allowed to fail), while
# a Deposit, whose fee transfer is strict, is refused. About 1 minute.
# Usage: smoke-treasury.sh /path/to/liatom_hub.wasm
set -euo pipefail
export PATH=$PATH:$HOME/src/gaia/build
WASM=${1:?path to liatom_hub.wasm}
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
send $G tx wasm instantiate "$CODE" "$INIT" --label "liatom-treasury-$RANDOM" --admin "$(addr deployer)" \
  --amount 20000000uatom --from deployer $TX >/dev/null
CONTRACT=$($Q q wasm list-contract-by-code "$CODE" -o json | jq -r '.contracts[-1]')
DENOM="factory/$CONTRACT/liatom"
sq() { $Q q wasm contract-state smart "$CONTRACT" "$1" -o json | jq -c .data; }

send $G tx wasm execute "$CONTRACT" '{"deposit":{}}' --amount 5000000uatom --from alice $TX >/dev/null
send $G tx wasm execute "$CONTRACT" '{"redeem":{}}' --amount "2000000$DENOM" --from alice $TX >/dev/null

# The bonded-tokens pool is a module account: the bank module refuses to send to it.
BAD=$($Q q auth module-account bonded_tokens_pool -o json | jq -r '.account.value.address // .account.base_account.address')
echo "blocked treasury: $BAD"
send $G tx wasm execute "$CONTRACT" "{\"update_config\":{\"treasury\":\"$BAD\"}}" --from deployer $TX >/dev/null

echo "waiting for the epoch (60 s) and for rewards to accrue"
sleep 65
echo "pool before: $(sq '{"pool":{}}')"

# A deposit must be refused (its fee transfer is strict), because there are rewards to collect.
if send $G tx wasm execute "$CONTRACT" '{"deposit":{}}' --amount 2000000uatom --from bob $TX >/dev/null 2>/tmp/dep.err; then
  echo "NOTE: the deposit went through (no reward to collect at that moment)"
else
  echo "deposit refused as expected: $(tail -c 200 /tmp/dep.err)"
fi

# The exit must go through.
OUT=$(send $G tx wasm execute "$CONTRACT" '{"submit_unbonding":{}}' --from keeper $TX)
echo "$OUT" | jq -r '.events[] | select(.type=="wasm") | .attributes[] | select(.key=="action") | .value' | sort | uniq -c
STATUS=$(sq '{"batch":{"batch_id":1}}' | jq -r .status)
echo "batch 1 status: $STATUS"
[ "$STATUS" = "unbonding" ] || { echo "FAIL: the exit was blocked by the treasury"; exit 1; }
# The fee the treasury could not take must have been delegated back (not left on the balance, where
# the unbonding reserve would hide it from the exchange rate).
BAL=$($Q q bank balances "$CONTRACT" -o json | jq -r "[.balances[] | select(.denom==\"uatom\") | .amount][0] // \"0\"")
echo "contract uatom balance after the submit: $BAL"
[ "$BAL" -le 5 ] || { echo "FAIL: the unsent fee was left on the balance"; exit 1; }
echo "TREASURY OK"
