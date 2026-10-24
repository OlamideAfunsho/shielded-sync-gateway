#!/usr/bin/env bash
# Dump a running regtest zebrad's chain as a verifier fixture: one JSON
# object per block with the raw block and what zebrad reports for its
# header. Used to pin v6 / Ironwood / v6-Sapling behaviour in tests.
set -euo pipefail
RPC=${ZEBRA_RPC:-http://127.0.0.1:18232}
rpc() { curl -sf -H 'content-type: application/json' --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}" "$RPC"; }
tip=$(rpc getblockcount '[]' | jq -r .result)
for h in $(seq 1 "$tip"); do
  raw=$(rpc getblock "[\"$h\", 0]" | jq -r .result)
  rpc getblockheader "[\"$h\", true]" | jq -c --arg raw "$raw" '.result | {height, hash, previousblockhash, merkleroot, time, raw: $raw}'
done
