#!/usr/bin/env bash
# Count every transaction on the local chain by version and shielded
# content, straight from zebrad. Used to check what the compact blocks the
# wallet received did and did not include.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
RPC=${ZEBRA_RPC:-http://127.0.0.1:18232}

rpc() {
  curl -sf -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}" "$RPC"
}

tip=$(rpc getblockcount '[]' | jq -r .result)
for h in $(seq 1 "$tip"); do
  rpc getblock "[\"$h\", 2]" | jq -c --argjson h "$h" '.result.tx | to_entries[] | {
      height: $h,
      index: .key,
      txid: .value.txid,
      version: .value.version,
      group: .value.versiongroupid,
      coinbase: (.key == 0),
      sapling_spends: (.value.vShieldedSpend | length),
      sapling_outputs: (.value.vShieldedOutput | length),
      orchard_actions: ((.value.orchard.actions // []) | length),
      ironwood_actions: ((.value.ironwood.actions // []) | length)
    }'
done | jq -s '{
    tip: (map(.height) | max),
    transactions: length,
    coinbase: (map(select(.coinbase)) | length),
    with_shielded_parts: (map(select(.sapling_spends + .sapling_outputs + .orchard_actions + .ironwood_actions > 0)) | length),
    by_version: (group_by(.version) | map({version: .[0].version, group: .[0].group, count: length})),
    shielded: (map(select(.sapling_spends + .sapling_outputs + .orchard_actions + .ironwood_actions > 0))
               | map({height, txid, version, sapling_spends, sapling_outputs, orchard_actions, ironwood_actions}))
  }'
