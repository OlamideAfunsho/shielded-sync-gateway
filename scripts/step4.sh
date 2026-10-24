#!/usr/bin/env bash
# Step 4: demonstrate protection.
#
# One fresh chain. The receiver gets an Ironwood and a Sapling payment at
# PAY_HEIGHT and later spends from another copy of its wallet at
# SPEND_HEIGHT. Then, for each mutation, three runs from identical wallet
# starting state (same seed, same birthday, empty data directory):
#
#   tampered direct   wallet -> tamper proxy -> zainod
#   gateway           wallet -> ssg-gateway -> tamper proxy -> zainod
#                                           -> ssg-producer -> zebrad
#                                           -> zebrad (trusted headers)
#   recovery          rules cleared; the gateway's wallet syncs again
#
# and an honest baseline for both paths. Mutations keep note commitments
# intact. A last case makes the producer lie instead of the indexer.
#
# Writes results/step4/{summary.json,summary.md,...}. Exits non-zero if any
# pass condition fails.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
OUT=${OUT:-$ROOT/results/step4}
need_bins
rm -rf "$OUT"
mkdir -p "$OUT"

trap 'stop_bg gateway; stop_bg tamperproxy; stop_bg producer; "$REGTEST" down' EXIT

say "chain"
build_chain
# Another copy of the receiver spends 1 ZEC; whoever only scans compact
# blocks learns about this spend from the nullifier.
restore spender $ZAINO "$RECEIVER_SEED" 1
wallet spender $ZAINO --nosync -- quicksend "$FAUCET_UA" 100000000 "spend" > "$OUT/spend.json"
SPEND_HEIGHT=$("$REGTEST" mine 1)
"$REGTEST" mine 3 > /dev/null
TIP=$(rpc getblockcount | jq -r .result)
echo "payment at $PAY_HEIGHT, spend at $SPEND_HEIGHT, tip $TIP"
"$ROOT/scripts/chain-census.sh" > "$OUT/chain-census.json"

RULES=$OUT/indexer-rules.txt
PRULES=$OUT/producer-rules.txt
: > "$RULES"
: > "$PRULES"
start_producer "$PRULES"
start_tamper_proxy "$RULES" "$OUT/indexer-calls.jsonl"

say "honest baseline"
snapshot honest-direct $PROXY "$OUT/honest/direct" "$RECEIVER_SEED" 1
start_gateway "$OUT/honest/gw"
wait_gateway "$OUT/honest/gw" tip > /dev/null
snapshot honest-gateway $GW "$OUT/honest/gateway" "$RECEIVER_SEED" 1
stop_bg gateway
HONEST=$OUT/honest/direct
read -r HI HS < <(balances "$HONEST")
echo "honest balance: ironwood $HI, sapling $HS"
same_state "$HONEST" "$OUT/honest/gateway" && HONEST_SAME=true || HONEST_SAME=false
echo "gateway matches direct: $HONEST_SAME"

results=()

# run_case NAME HEIGHT KIND TARGET(indexer|producer)
run_case() {
  local name=$1 height=$2 kind=$3 target=$4 d=$OUT/$1
  say "$name: $kind at height $height ($target)"
  mkdir -p "$d"
  if [ "$target" = indexer ]; then echo "$height $kind" > "$RULES"; else echo "$height $kind" > "$PRULES"; fi

  # Tampered direct sync (only meaningful when the indexer lies).
  local direct_json=null
  if [ "$target" = indexer ]; then
    rm -rf "$W/$name-direct"
    snapshot "$name-direct" $PROXY "$d/direct" "$RECEIVER_SEED" 1
    read -r i s < <(balances "$d/direct")
    local same=false
    same_state "$HONEST" "$d/direct" && same=true
    direct_json=$(jq -cn --argjson i "$i" --argjson s "$s" --argjson same $same \
      --arg errors "$(cat "$d/direct/sync_errors.txt")" --arg height "$(jq -r .height "$d/direct/height.json")" \
      '{ironwood: $i, sapling: $s, matches_honest: $same, sync_errors: $errors, wallet_height: ($height|tonumber? // null)}')
    echo "direct: ironwood $i sapling $s, matches honest: $same"
  fi

  # Through the gateway, verifying from scratch.
  rm -rf "$d/gw"
  start_gateway "$d/gw"
  wait_gateway "$d/gw" > /dev/null
  local v1 v2 reason
  v1=$(gw_status "$d/gw" verified_height)
  rm -rf "$W/$name-gw"
  snapshot "$name-gw" $GW "$d/gateway" "$RECEIVER_SEED" 1
  sleep 3
  v2=$(gw_status "$d/gw" verified_height)
  reason=$(jq -r '.failure.reason // ""' "$d/gw/status.json")
  read -r gi gs < <(balances "$d/gateway")
  echo "gateway: verified height $v1 -> $v2 (failure: ${reason:-none})"
  echo "gateway wallet: ironwood $gi sapling $gs"
  sed 's/^/  wallet: /' "$d/gateway/sync_errors.txt"

  # Correct the source; the same wallet syncs again through the gateway.
  : > "$RULES"
  : > "$PRULES"
  wait_gateway "$d/gw" tip > /dev/null
  snapshot "$name-gw" $GW "$d/recovered"
  local recovered=false
  same_state "$HONEST" "$d/recovered" && recovered=true
  echo "after correcting the source: matches honest: $recovered"
  cp "$RUN/gateway.log" "$d/gateway.log"
  stop_bg gateway

  local held=false
  [ "$v1" = $((height - 1)) ] && [ "$v2" = $((height - 1)) ] && held=true
  results+=("$(jq -cn --arg name "$name" --arg kind "$kind" --arg target "$target" --argjson height "$height" \
    --argjson direct "$direct_json" --argjson v1 "$v1" --argjson v2 "$v2" --argjson held $held \
    --arg reason "$reason" --arg werr "$(cat "$d/gateway/sync_errors.txt")" \
    --argjson gi "$gi" --argjson gs "$gs" --argjson recovered $recovered \
    '{name: $name, mutation: $kind, target: $target, height: $height, direct: $direct,
      gateway: {verified_height_before_wallet: $v1, verified_height_after_wallet: $v2, held_below_mutation: $held,
                failure: $reason, wallet_sync_errors: $werr, wallet_ironwood: $gi, wallet_sapling: $gs},
      recovered_matches_honest: $recovered}')")
}

run_case ciphertext "$PAY_HEIGHT" ciphertext indexer
run_case ephemeral-key "$PAY_HEIGHT" epk indexer
run_case nullifier "$SPEND_HEIGHT" nullifier indexer
run_case omitted-tx "$PAY_HEIGHT" omit-tx indexer
run_case lying-producer "$PAY_HEIGHT" memo-digest producer

printf '%s\n' "${results[@]}" | jq -s \
  --argjson pay "$PAY_HEIGHT" --argjson spend "$SPEND_HEIGHT" --argjson tip "$TIP" \
  --argjson hi "$HI" --argjson hs "$HS" --argjson hsame $HONEST_SAME \
  '{payment_height: $pay, spend_height: $spend, tip: $tip,
    honest: {ironwood: $hi, sapling: $hs, gateway_matches_direct: $hsame}, cases: .}' > "$OUT/summary.json"

# Markdown table.
{
  echo "| mutation | height | direct wallet (tampered indexer) | gateway: verified height held | gateway: wallet sees | after correcting source |"
  echo "|---|---:|---|---|---|---|"
  jq -r --argjson hi "$HI" --argjson hs "$HS" '.cases[] |
    "| \(.mutation) (\(.target)) | \(.height) | " +
    (if .direct == null then "n/a" else
      (if .direct.matches_honest then "same as honest" else
        "ironwood \(.direct.ironwood), sapling \(.direct.sapling) (honest: \($hi), \($hs))" +
        (if .direct.sync_errors != "" then "; error: \(.direct.sync_errors | split("\n") | last)" else "; no error reported" end)
      end) end) +
    " | \(.gateway.verified_height_after_wallet) (\(if .gateway.held_below_mutation then "yes" else "NO" end)) | " +
    (.gateway.wallet_sync_errors | split("\n") | last | .[0:160]) +
    " | \(if .recovered_matches_honest then "matches honest" else "DIFFERS" end) |"' "$OUT/summary.json"
} > "$OUT/summary.md"
cat "$OUT/summary.md"

ok=$(jq '[.honest.gateway_matches_direct] + [.cases[] | .gateway.held_below_mutation, .recovered_matches_honest, (.gateway.wallet_sync_errors | test("failed verification"))] | all' "$OUT/summary.json")
if [ "$ok" = true ]; then
  echo "PASS: honest sync matches; every mutation blocked at its height; every case recovers" | tee "$OUT/RESULT"
else
  echo "FAIL: see $OUT/summary.json" | tee "$OUT/RESULT"
  exit 1
fi
