#!/usr/bin/env bash
# Overhead of syncing through the verifying gateway versus directly.
#
# Chain: the step 1 shape, then grown to ~2,000 blocks with a shielded
# payment every 400 blocks. Everything runs on one machine, so these
# numbers are CPU and protocol overhead with no WAN latency.
#
# For each path, RUNS fresh wallet restores (same seed, birthday 1):
#   direct    wallet -> recorder -> zainod
#   warm      wallet -> recorder -> gateway (already verified to tip)
#   cold      wallet -> recorder -> gateway started from an empty state
# The recorder (ssg-gateway proxy) counts bytes between wallet and server
# identically for every path. Wallet wall time and peak RSS come from
# /usr/bin/time. Gateway-side costs (verification time, bytes pulled from
# indexer, producer and trusted node, peak RSS) come from its status file.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
OUT=${OUT:-$ROOT/results/bench}
RUNS=${RUNS:-3}
TARGET=${TARGET:-2000}
need_bins
[ -x /usr/bin/time ] || { echo "needs /usr/bin/time (apt install time)" >&2; exit 1; }
rm -rf "$OUT"
mkdir -p "$OUT"
trap 'stop_bg gateway; stop_bg recorder; stop_bg producer; "$REGTEST" down' EXIT

say "chain"
build_chain
n=0
while [ "$(rpc getblockcount | jq -r .result)" -lt "$TARGET" ]; do
  "$REGTEST" mine 100 > /dev/null
  n=$((n + 1))
  if [ $((n % 4)) = 0 ]; then
    wallet faucet $ZAINO --waitsync -- balance > /dev/null
    wallet faucet $ZAINO --nosync -- quicksend "$RECV_UA" 10000000 "bench payment $n" > /dev/null
    "$REGTEST" mine 1 > /dev/null
  fi
done
TIP=$(rpc getblockcount | jq -r .result)
"$ROOT/scripts/chain-census.sh" > "$OUT/chain-census.json"
FULL_BYTES=$(for h in $(seq 1 "$TIP"); do rpc getblock "[\"$h\", 1]" | jq .result.size; done | awk '{s+=$1} END{print s}')
echo "tip $TIP, $(jq .with_shielded_parts "$OUT/chain-census.json") shielded txs, full blocks $FULL_BYTES bytes"

start_producer

# timed_sync LABEL SERVER RUN: fresh restore through SERVER; appends a row.
timed_sync() {
  local label=$1 server=$2 i=$3 name="bench-$1-$3"
  rm -rf "${W:?}/$name"
  (cd "$W" && ZINGO_SEED="$RECEIVER_SEED" /usr/bin/time -f "%e %M" -o "$OUT/$name.time" \
    "$BIN/zingo-cli" --chain regtest --server "$server" --data-dir "$W/$name" --birthday 1 --waitsync balance \
    > "$OUT/$name.balance" 2> "$OUT/$name.stderr") || true
  read -r secs rss < "$OUT/$name.time"
  echo "$label run $i: ${secs}s, wallet peak RSS ${rss} kB, balance $(awk -F': ' '/total_ironwood_balance/{print $2}' "$OUT/$name.balance")"
  echo "$label $i $secs $rss" >> "$OUT/runs.txt"
}

# recorder LOG UPSTREAM
recorder() {
  start_bg recorder "$GATEWAY" proxy --listen 127.0.0.1:9171 --upstream "$2" --record "$1"
  wait_port 127.0.0.1:9171
}

wallet_bytes() { # call log -> "requests responses"
  jq -s '[map(.request_bytes) | add, map(.response_bytes) | add] | @tsv' -r "$1"
}

say "direct"
for i in $(seq 1 "$RUNS"); do
  recorder "$OUT/direct-$i.jsonl" "http://$ZAINO"
  timed_sync direct 127.0.0.1:9171 "$i"
  stop_bg recorder
done

say "gateway, cold (verification included)"
for i in $(seq 1 "$RUNS"); do
  rm -rf "$OUT/gw-cold-$i"
  start_gateway "$OUT/gw-cold-$i" "http://$ZAINO"
  recorder "$OUT/cold-$i.jsonl" "http://$GW"
  timed_sync cold 127.0.0.1:9171 "$i"
  wait_gateway "$OUT/gw-cold-$i" tip > /dev/null
  stop_bg recorder
  stop_bg gateway
done

say "gateway, warm"
start_gateway "$OUT/gw-warm" "http://$ZAINO"
t0=$(date +%s.%N)
wait_gateway "$OUT/gw-warm" tip > /dev/null
t1=$(date +%s.%N)
CATCHUP=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.2f", b-a}')
echo "gateway verified $TIP blocks from the checkpoint in ${CATCHUP}s (wall, incl. startup)"
for i in $(seq 1 "$RUNS"); do
  recorder "$OUT/warm-$i.jsonl" "http://$GW"
  timed_sync warm 127.0.0.1:9171 "$i"
  stop_bg recorder
done
cp "$OUT/gw-warm/status.json" "$OUT/gateway-status.json"
stop_bg gateway

say "summary"
python3 - "$OUT" "$TIP" "$FULL_BYTES" "$CATCHUP" <<'PY'
import json, statistics, sys, glob, os
out, tip, full_bytes, catchup = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), float(sys.argv[4])
rows = [l.split() for l in open(f"{out}/runs.txt")]
def calls(path):
    req = resp = n = 0
    for l in open(path):
        r = json.loads(l); req += r["request_bytes"]; resp += r["response_bytes"]; n += 1
    return n, req, resp
res = {"tip": tip, "full_block_bytes": full_bytes, "gateway_catchup_seconds": catchup, "paths": {}}
for label in ("direct", "cold", "warm"):
    rs = [r for r in rows if r[0] == label]
    secs = [float(r[2]) for r in rs]; rss = [int(r[3]) for r in rs]
    cs = [calls(f"{out}/{label}-{r[1]}.jsonl") for r in rs]
    res["paths"][label] = {
        "runs": len(rs),
        "sync_seconds": secs, "sync_seconds_median": statistics.median(secs),
        "wallet_peak_rss_kb_median": statistics.median(rss),
        "calls_median": statistics.median(c[0] for c in cs),
        "wallet_bytes_sent_median": statistics.median(c[1] for c in cs),
        "wallet_bytes_received_median": statistics.median(c[2] for c in cs),
    }
st = json.load(open(f"{out}/gateway-status.json"))
res["gateway"] = {
    "blocks_verified": st["blocks_verified"],
    "verify_cpu_seconds": round(st["verify_seconds"], 3),
    "verify_ms_per_block": round(1000 * st["verify_seconds"] / max(st["blocks_verified"], 1), 4),
    "compact_bytes_in": st["compact_bytes_in"],
    "record_bytes_in": st["record_bytes_in"],
    "trusted_bytes_in": st["trusted_bytes_in"],
    "peak_rss_kb": st["peak_rss_kb"],
}
json.dump(res, open(f"{out}/summary.json", "w"), indent=2)
d, c, w = (res["paths"][k] for k in ("direct", "cold", "warm"))
g = res["gateway"]
md = [
    f"Chain: {tip} blocks. Full blocks total {full_bytes:,} bytes. {d['runs']} runs per path, medians shown.",
    "",
    "| | direct | gateway (warm) | gateway (cold) |",
    "|---|---:|---:|---:|",
    f"| wallet sync time (s) | {d['sync_seconds_median']:.2f} | {w['sync_seconds_median']:.2f} | {c['sync_seconds_median']:.2f} |",
    f"| bytes wallet received | {d['wallet_bytes_received_median']:,.0f} | {w['wallet_bytes_received_median']:,.0f} | {c['wallet_bytes_received_median']:,.0f} |",
    f"| bytes wallet sent | {d['wallet_bytes_sent_median']:,.0f} | {w['wallet_bytes_sent_median']:,.0f} | {c['wallet_bytes_sent_median']:,.0f} |",
    f"| RPC calls | {d['calls_median']:.0f} | {w['calls_median']:.0f} | {c['calls_median']:.0f} |",
    f"| wallet peak RSS (kB) | {d['wallet_peak_rss_kb_median']:,.0f} | {w['wallet_peak_rss_kb_median']:,.0f} | {c['wallet_peak_rss_kb_median']:,.0f} |",
    "",
    "Gateway side, one verification pass from the checkpoint to the tip:",
    "",
    "| | |",
    "|---|---:|",
    f"| blocks verified | {g['blocks_verified']:,} |",
    f"| wall time to reach tip (incl. fetching) | {catchup:.2f} s |",
    f"| verification CPU time | {g['verify_cpu_seconds']} s ({g['verify_ms_per_block']} ms/block) |",
    f"| compact blocks pulled from indexer | {g['compact_bytes_in']:,} bytes |",
    f"| records pulled from producer | {g['record_bytes_in']:,} bytes ({100*g['record_bytes_in']/full_bytes:.1f}% of full blocks) |",
    f"| JSON-RPC from trusted node | {g['trusted_bytes_in']:,} bytes |",
    f"| gateway peak RSS | {g['peak_rss_kb']:,} kB |",
]
open(f"{out}/summary.md", "w").write("\n".join(md) + "\n")
print("\n".join(md))
PY
