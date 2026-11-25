#!/usr/bin/env bash
# On-chain check of DepositTokenizedShares: a user tokenizes an existing delegation (LSM), sends the
# share token to the hub, the hub redeems it and mints liATOM at the normal rate. About 1 minute.
# Usage: smoke-lsm.sh /path/to/liatom_hub.wasm
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
fail() { echo "FAIL: $*"; exit 1; }

CODE=$(send $G tx wasm store "$WASM" --from deployer $TX | jq -r '.events[] | select(.type=="store_code") | .attributes[] | select(.key=="code_id") | .value')
INIT=$(jq -nc --arg a "$(addr deployer)" --arg v "$V1" '{admin:$a, treasury:$a, fee_bps:1000, epoch_secs:60,
  unbonding_secs:300, min_deposit:"1000000", max_tvl:"340282366920938463463374607431768211455", denom_creation_fee:"10000000", validators:[{validator:$v, weight_bps:10000}]}')
send $G tx wasm instantiate "$CODE" "$INIT" --label "liatom-lsm-$RANDOM" --admin "$(addr deployer)" \
  --amount 20000000uatom --from deployer $TX >/dev/null
CONTRACT=$($Q q wasm list-contract-by-code "$CODE" -o json | jq -r '.contracts[-1]')
DENOM="factory/$CONTRACT/liatom"
sq() { $Q q wasm contract-state smart "$CONTRACT" "$1" -o json | jq -c .data; }
bal() { $Q q bank balances "$1" -o json | jq -r "[.balances[] | select(.denom==\"$2\") | .amount][0] // \"0\""; }
delegated() { $Q q staking delegation "$CONTRACT" "$V1" -o json | jq -r '.delegation_response.balance.amount'; }

send $G tx wasm execute "$CONTRACT" '{"deposit":{}}' --amount 5000000uatom --from alice $TX >/dev/null
echo "waiting for rewards to accrue"
sleep 20

BOB=$(addr bob)
send $G tx staking delegate "$V1" 30000000uatom --from bob $TX >/dev/null
send $G tx liquid tokenize-share "$V1" 20000000uatom "$BOB" --from bob $TX >/dev/null
SHARE=$($Q q bank balances "$BOB" -o json | jq -r '[.balances[] | select(.denom | startswith("cosmosvaloper"))][0].denom')
SHARES=$(bal "$BOB" "$SHARE")
echo "share token: $SHARE ($SHARES)"
[ -n "$SHARE" ] && [ "$SHARES" -gt 0 ] || fail "no share token"

RATE0=$(sq '{"exchange_rate":{}}' | jq -r .rate)
POOL0=$(sq '{"pool":{}}' | jq -r .pool_value)
D0=$(delegated)
echo "before: rate=$RATE0 pool_value=$POOL0 delegated=$D0 supply=$(sq '{"pool":{}}' | jq -r .liatom_supply)"

echo "== a plain uatom is refused"
if send $G tx wasm execute "$CONTRACT" '{"deposit_tokenized_shares":{}}' --amount 2000000uatom --from bob $TX >/dev/null 2>&1; then fail "uatom was accepted"; fi

echo "== deposit 12 ATOM of the 20 ATOM share token"
send $G tx wasm execute "$CONTRACT" '{"deposit_tokenized_shares":{}}' --amount "12000000$SHARE" --from bob $TX >/dev/null
MINTED=$(bal "$BOB" "$DENOM")
D1=$(delegated)
POOL1=$(sq '{"pool":{}}' | jq -r .pool_value)
RATE1=$(sq '{"exchange_rate":{}}' | jq -r .rate)
CB=$(bal "$CONTRACT" uatom)
echo "after: minted=$MINTED rate=$RATE1 pool_value=$POOL1 delegated=$D1 contract balance=$CB"
GREW=$((D1 - D0))
[ "$GREW" -ge 11999990 ] && [ "$GREW" -le 12500000 ] || fail "delegation grew by $GREW (expected about 12000000 plus swept rewards)"
[ "$(bal "$BOB" "$SHARE")" = "$((SHARES - 12000000))" ] || fail "share token was not taken"
# minted = deposited / rate (rate is a decimal, compare with awk)
awk -v m="$MINTED" -v r="$RATE0" 'BEGIN{ e=12000000/r; if (m<e*0.9999||m>e*1.0001) {print "minted "m" expected "e; exit 1} }' || fail "minted amount is off"
# the rate must not move (a deposit is rate-neutral), apart from rewards that accrued meanwhile
awk -v a="$RATE0" -v b="$RATE1" 'BEGIN{ d=(b-a)/a; if (d<-0.000001||d>0.01) {print "rate "a" -> "b; exit 1} }' || fail "the rate moved"
# nothing may be left on the balance: the rewards paid out by the redemption were collected
[ "$CB" -le 5 ] || fail "ATOM left on the contract balance: $CB"

echo "== the rest of the share token redeems too"
send $G tx wasm execute "$CONTRACT" '{"deposit_tokenized_shares":{}}' --amount "8000000$SHARE" --from bob $TX >/dev/null
[ "$(bal "$BOB" "$SHARE")" = "0" ] || fail "share token left"
$Q q liquid tokenize-share-records-owned "$BOB" -o json | jq -e '(.records // []) | length == 0' >/dev/null || fail "the record was not deleted"
echo "pool: $(sq '{"pool":{}}')"
echo "== leave as staked ATOM: burn 10 liATOM, receive a share token"
LB0=$(bal "$BOB" "$DENOM")
D2=$(delegated)
RATE2=$(sq '{"exchange_rate":{}}' | jq -r .rate)
send $G tx wasm execute "$CONTRACT" '{"redeem_to_shares":{}}' --amount "10000000$DENOM" --from bob $TX >/dev/null
LB1=$(bal "$BOB" "$DENOM")
[ "$((LB0 - LB1))" = "10000000" ] || fail "liATOM was not burned: $LB0 -> $LB1"
TOK=$($Q q bank balances "$BOB" -o json | jq -r '[.balances[] | select(.denom | startswith("cosmosvaloper"))][0].denom')
TOKAMT=$(bal "$BOB" "$TOK")
echo "share token: $TOK ($TOKAMT)"
D3=$(delegated)
awk -v t="$TOKAMT" -v r="$RATE2" 'BEGIN{ e=10000000*r; if (t<e*0.9999||t>e*1.0001) {print "token "t" expected "e; exit 1} }' || fail "share token amount is off"
DROP=$((D2 - D3))
[ "$DROP" -ge 9999990 ] && [ "$DROP" -le 10500000 ] || fail "delegation dropped by $DROP"
RATE3=$(sq '{"exchange_rate":{}}' | jq -r .rate)
awk -v a="$RATE2" -v b="$RATE3" 'BEGIN{ d=(b-a)/a; if (d<-0.000001||d>0.01) {print a" -> "b; exit 1} }' || fail "the rate moved on exit"
[ "$(bal "$CONTRACT" uatom)" -le 5 ] || fail "ATOM left on the contract balance after the exit"
$Q q liquid tokenize-share-records-owned "$BOB" -o json | jq -e '(.records | length) >= 1' >/dev/null || fail "bob does not own the record"

echo "== bob redeems the token into his own delegation"
BD0=$($Q q staking delegation "$BOB" "$V1" -o json 2>/dev/null | jq -r '.delegation_response.balance.amount // "0"')
send $G tx liquid redeem-tokens "${TOKAMT}${TOK}" --from bob $TX >/dev/null
BD1=$($Q q staking delegation "$BOB" "$V1" -o json | jq -r '.delegation_response.balance.amount')
echo "bob delegation: $BD0 -> $BD1"
[ "$((BD1 - BD0))" -ge 9999990 ] || fail "bob did not get the delegation"
echo "LSM OK"
