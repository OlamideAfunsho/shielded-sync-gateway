#!/usr/bin/env bash
# Step 3: the verifying gateway in front of a real wallet.
#
#   1. receive     wallet via gateway == wallet direct
#   2. restart     stop the gateway, mine, restart on the same state file:
#                  it resumes from the stored height without re-verifying
#   3. crash       kill -9 in the middle of verifying 300 blocks; the state
#                  file holds a contiguous verified prefix and the restarted
#                  gateway continues from it
#   4. reorg       a spend is mined, both wallets see it, the block is
#                  invalidated on the trusted node and a longer chain mined;
#                  the gateway rolls back and re-verifies, the wallets agree
#
# Writes results/step3/. Exits non-zero if a check fails.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
OUT=${OUT:-$ROOT/results/step3}
need_bins
rm -rf "$OUT"
mkdir -p "$OUT"
trap 'stop_bg gateway; stop_bg producer; "$REGTEST" down' EXIT

GWDIR=$OUT/gw
CHECKS=()
check() { # name condition-result detail
  CHECKS+=("$(jq -cn --arg n "$1" --argjson ok "$2" --arg d "$3" '{check: $n, pass: $ok, detail: $d}')")
  printf '  [%s] %s: %s\n' "$([ "$2" = true ] && echo pass || echo FAIL)" "$1" "$3"
}
db_summary() { # prints "count min max contiguous"
  python3 - "$GWDIR/state.db" <<'PY'
import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
hs = [r[0] for r in c.execute("SELECT height FROM blocks ORDER BY height")]
txids = c.execute("SELECT COUNT(*) FROM txids").fetchone()[0]
orphan = c.execute("SELECT COUNT(*) FROM txids WHERE height NOT IN (SELECT height FROM blocks)").fetchone()[0]
contiguous = hs == list(range(1, len(hs) + 1))
print(len(hs), hs[0] if hs else 0, hs[-1] if hs else 0, str(contiguous and orphan == 0).lower(), txids)
PY
}
hashes_match_trusted() { # all stored hashes equal zebrad's best chain
  python3 - "$GWDIR/state.db" "$ZEBRA_RPC" <<'PY'
import sqlite3, sys, json, urllib.request
c = sqlite3.connect(sys.argv[1])
bad = 0
for h, hb in c.execute("SELECT height, hash FROM blocks"):
    req = urllib.request.Request(sys.argv[2], data=json.dumps({"jsonrpc":"2.0","id":1,"method":"getblockhash","params":[h]}).encode(), headers={"content-type":"application/json"})
    want = json.load(urllib.request.urlopen(req))["result"]
    if hb[::-1].hex() != want:
        bad += 1
print("true" if bad == 0 else "false", bad)
PY
}

say "chain"
build_chain
start_producer
start_gateway "$GWDIR" "http://$ZAINO"
wait_gateway "$GWDIR" tip > /dev/null

say "1. receive"
snapshot r-direct $ZAINO "$OUT/receive/direct" "$RECEIVER_SEED" 1
snapshot r-gateway $GW "$OUT/receive/gateway" "$RECEIVER_SEED" 1
same_state "$OUT/receive/direct" "$OUT/receive/gateway" && ok=true || ok=false
check "receive: gateway wallet matches direct" $ok "balance $(balances "$OUT/receive/gateway"), height $(jq .height "$OUT/receive/gateway/height.json")"

say "2. restart"
before=$(gw_status "$GWDIR" verified_height)
stop_bg gateway
"$REGTEST" mine 5 > /dev/null
start_gateway "$GWDIR" "http://$ZAINO"
after=$(wait_gateway "$GWDIR" tip)
newly=$(gw_status "$GWDIR" blocks_verified)
cp "$RUN/gateway.log" "$OUT/restart-gateway.log"
check "restart: resumes from stored height" "$( [ "$newly" = 5 ] && echo true || echo false )" \
  "stopped at $before, restarted, verified $newly new blocks, now at $after"
grep -q "resuming after restart" "$OUT/restart-gateway.log" && ok=true || ok=false
check "restart: log shows resume" $ok "$(grep -o 'resumed_at=[0-9]*' "$OUT/restart-gateway.log" | head -1)"

say "3. crash during verification"
stop_bg gateway
"$REGTEST" mine 300 > /dev/null
TIP=$(rpc getblockcount | jq -r .result)
start_gateway "$GWDIR" "http://$ZAINO"
# Kill as soon as the follower has made progress but before it is done.
for _ in $(seq 1 200); do
  v=$(jq -r .verified_height "$GWDIR/status.json" 2>/dev/null || echo 0)
  [ "$v" -gt "$after" ] && break
  sleep 0.02
done
stop_bg gateway KILL
read -r n lo hi contiguous ntx < <(db_summary)
check "crash: state is a contiguous verified prefix" $contiguous "killed with $hi of $TIP verified; $n blocks, heights $lo..$hi, $ntx txids, no orphans"
start_gateway "$GWDIR" "http://$ZAINO"
final=$(wait_gateway "$GWDIR" tip)
check "crash: restarted gateway reaches tip" "$( [ "$final" = "$TIP" ] && echo true || echo false )" "resumed at $hi, reached $final"
snapshot r-direct $ZAINO "$OUT/crash/direct"
snapshot r-gateway $GW "$OUT/crash/gateway"
same_state "$OUT/crash/direct" "$OUT/crash/gateway" && ok=true || ok=false
check "crash: wallets agree afterwards" $ok "height $(jq .height "$OUT/crash/gateway/height.json")"

say "4. reorg"
wallet r-direct $ZAINO --nosync -- quicksend "$FAUCET_UA" 50000000 "spend before reorg" > "$OUT/reorg-spend.json"
SPEND=$("$REGTEST" mine 1)
"$REGTEST" mine 3 > /dev/null
wait_gateway "$GWDIR" tip > /dev/null
snapshot r-direct $ZAINO "$OUT/reorg/before-direct"
snapshot r-gateway $GW "$OUT/reorg/before-gateway"
same_state "$OUT/reorg/before-direct" "$OUT/reorg/before-gateway" && ok=true || ok=false
check "reorg: wallets agree before" $ok "spend mined at $SPEND"
old_hash=$(rpc getblockhash "[$SPEND]" | jq -r .result)
rb_before=$(gw_status "$GWDIR" rollbacks)
rpc invalidateblock "[\"$old_hash\"]" > /dev/null
echo "invalidated $SPEND ($old_hash); zebrad now at $(rpc getblockcount | jq -r .result)"
"$REGTEST" mine 6 > /dev/null
NEWTIP=$(rpc getblockcount | jq -r .result)
new_hash=$(rpc getblockhash "[$SPEND]" | jq -r .result)
reached=$(wait_gateway "$GWDIR" tip)
rb_after=$(gw_status "$GWDIR" rollbacks)
rolled=$(gw_status "$GWDIR" rolled_back_blocks)
cp "$RUN/gateway.log" "$OUT/reorg-gateway.log"
check "reorg: gateway rolled back and re-verified" "$( [ "$rb_after" -gt "$rb_before" ] && [ "$reached" = "$NEWTIP" ] && echo true || echo false )" \
  "height $SPEND changed $old_hash -> $new_hash; rollbacks $rb_before -> $rb_after ($rolled blocks total); verified to $reached"
read -r okh bad < <(hashes_match_trusted)
check "reorg: every stored hash is on the trusted best chain" $okh "$bad mismatches"
snapshot r-direct $ZAINO "$OUT/reorg/after-direct"
snapshot r-gateway $GW "$OUT/reorg/after-gateway"
same_state "$OUT/reorg/after-direct" "$OUT/reorg/after-gateway" && ok=true || ok=false
check "reorg: wallets agree after" $ok "balance $(balances "$OUT/reorg/after-gateway"), height $(jq .height "$OUT/reorg/after-gateway/height.json")"
grep -h "rolled back" "$OUT/reorg-gateway.log" | sed 's/^/  log: /'

cp "$GWDIR/status.json" "$OUT/final-status.json"
printf '%s\n' "${CHECKS[@]}" | jq -s . > "$OUT/checks.json"
if jq -e 'all(.pass)' "$OUT/checks.json" > /dev/null; then
  echo "PASS: $(jq length "$OUT/checks.json") checks" | tee "$OUT/RESULT"
else
  echo "FAIL: see $OUT/checks.json" | tee "$OUT/RESULT"
  exit 1
fi
