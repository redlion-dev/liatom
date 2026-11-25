#!/usr/bin/env bash
tmux kill-session -t chain 2>/dev/null || true
tmux new-session -d -s chain "$HOME/src/gaia/build/gaiad start --home $HOME/liatom-dev/node 2>&1 | tee $HOME/liatom-dev/node.log"
sleep 25
echo "height: $(curl -s localhost:26657/status | jq -r '.result.sync_info.latest_block_height')"
B=localhost:1317
echo "wasm:"; curl -s $B/cosmwasm/wasm/v1/codes/params; echo
echo "liquid:"; curl -s $B/gaia/liquid/v1beta1/params; echo
echo "tokenfactory:"; curl -s $B/osmosis/tokenfactory/v1beta1/params; echo
echo "staking:"; curl -s $B/cosmos/staking/v1beta1/params | jq -c .params
tail -3 $HOME/liatom-dev/node.log | cut -c1-200
