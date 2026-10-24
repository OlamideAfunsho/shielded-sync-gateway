#!/usr/bin/env bash
# Step 1: prove the wallet connection.
#
# Builds a fresh regtest chain, pays a test wallet in Ironwood and Sapling,
# then restores that wallet twice from the same seed and birthday: once
# syncing straight from zainod, once through the pass-through gateway.
# The two copies must end up with the same balance, notes, transaction
# history and sync height. Then the direct copy spends, and the gateway
# copy has to notice the spend (nullifier detection through the gateway).
# Finally the gateway copy sends, so transaction submission goes through the
# gateway too, and the direct copy has to agree with the result.
#
# Every gateway call is logged to results/step1/calls.jsonl and reduced
# to results/step1/rpc-inventory.md.
#
#   BIN=/path/to/bins scripts/step1.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
export BIN=${BIN:-$ROOT/.deps/bin}
export RUN=${RUN:-$ROOT/.run}
OUT=${OUT:-$ROOT/results/step1}
GATEWAY=$ROOT/target/release/ssg-gateway
REGTEST="$ROOT/scripts/regtest.sh"

ZAINO=127.0.0.1:8137
GW=127.0.0.1:9168

# Fixture seeds from zingolabs/infrastructure (zingo_test_vectors). Test
# keys only; anything sent to them on a real network is public.
FAUCET_SEED="abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art"
RECEIVER_SEED="hospital museum valve antique skate museum unfold vocal weird milk scale social vessel identify crowd hospital control album rib bulb path oven civil tank"

PAY_IRONWOOD=250000000   # 2.5 ZEC to the receiver's default UA (lands in Ironwood)
PAY_SAPLING=125000000    # 1.25 ZEC to a Sapling-only receiver
SPEND_BACK=100000000       # 1 ZEC the direct copy sends back to the faucet
SEND_VIA_GATEWAY=50000000  # 0.5 ZEC the gateway copy sends to the faucet

say() { printf '\n== %s\n' "$*"; }

[ -x "$GATEWAY" ] || (cd "$ROOT" && cargo build --release -q)
for b in zebrad zainod zingo-cli; do
  [ -x "$BIN/$b" ] || { echo "missing $BIN/$b; run scripts/build-deps.sh" >&2; exit 1; }
done

rm -rf "$OUT" "$RUN/wallets"
mkdir -p "$OUT" "$RUN/wallets"
W=$RUN/wallets

# zingo-cli prints its session chatter on stdout next to command output;
# keep only the command's own output.
clean() {
  grep -vE '^(WARNING: this build carries|Launching (sync|save) task|Save task shutdown|Zingo CLI quit|Sync (task|complete))' || true
}

# wallet NAME SERVER [session options...] -- command...
wallet() {
  local name=$1 server=$2; shift 2
  local opts=()
  while [ "$1" != "--" ]; do opts+=("$1"); shift; done
  shift
  ( cd "$W" && timeout "${WALLET_TIMEOUT:-300}" "$BIN/zingo-cli" --chain regtest --server "$server" --data-dir "$W/$name" "${opts[@]}" "$@" 2>>"$W/$name.stderr" ) | clean
}

# Restoring and syncing must happen in the same session. A wallet created
# with --nosync and synced in a later session stalls after fetching subtree
# roots and never scans (zingo-cli 7.0.0; see docs/step1-wallet-connection.md).
restore() { # name server seed birthday
  ZINGO_SEED=$3 wallet "$1" "$2" --birthday "$4" --waitsync -- balance > /dev/null
}

# Snapshot what a user would look at. Identical chain + identical keys
# must give byte-identical output.
# With a seed and birthday, the wallet is restored from scratch and the
# timed sync is the full first scan.
snapshot() { # name server label [seed birthday]
  local d=$OUT/$3
  mkdir -p "$d"
  local t0 t1
  t0=$(date +%s.%N)
  if [ $# -ge 5 ]; then
    ZINGO_SEED=$4 wallet "$1" "$2" --birthday "$5" --waitsync -- balance > "$d/balance.txt"
  else
    wallet "$1" "$2" --waitsync -- balance > "$d/balance.txt"
  fi
  t1=$(date +%s.%N)
  echo "$t0 $t1" | awk '{printf "%.2f\n", $2-$1}' > "$d/sync_seconds.txt"
  wallet "$1" "$2" --nosync -- height > "$d/height.json"
  wallet "$1" "$2" --nosync -- notes all > "$d/notes.json"
  wallet "$1" "$2" --nosync -- transactions > "$d/transactions.txt"
  wallet "$1" "$2" --nosync -- value_transfers > "$d/value_transfers.txt"
}

# notes.json lists notes in the wallet's internal map order, which depends
# on how a note was first learned about (built locally as change vs found
# while scanning). The set is what matters, so notes are compared sorted.
normalize() { # file
  case "$1" in
    *notes.json) jq -S 'map_values(.note_summaries |= sort_by(.txid, .output_index))' "$1" ;;
    *) cat "$1" ;;
  esac
}

compare() { # labelA labelB report
  local a=$OUT/$1 b=$OUT/$2 ok=0
  {
    echo "### $1 vs $2"
    echo
    for f in balance.txt height.json notes.json transactions.txt value_transfers.txt; do
      if cmp -s "$a/$f" "$b/$f"; then
        echo "- \`$f\`: identical ($(wc -c < "$a/$f") bytes)"
      elif cmp -s <(normalize "$a/$f") <(normalize "$b/$f"); then
        echo "- \`$f\`: same notes, listed in a different order"
      else
        echo "- \`$f\`: **DIFFERENT**"
        echo '```diff'
        diff -u <(normalize "$a/$f") <(normalize "$b/$f") | head -40
        echo '```'
        ok=1
      fi
    done
    echo "- sync time: $(cat "$a/sync_seconds.txt")s direct, $(cat "$b/sync_seconds.txt")s through gateway"
    echo
  } >> "$3"
  return $ok
}

say "versions"
{
  "$BIN/zebrad" --version | head -1
  "$BIN/zainod" --version | head -1
  "$BIN/zingo-cli" --version | head -1
  "$GATEWAY" --version
} | tee "$OUT/versions.txt"

say "fresh regtest chain"
"$REGTEST" up 110

say "faucet: restore, sync, shield mature coinbase"
# Sends are always made from a freshly synced wallet with --nosync. With
# --waitsync, zingo-cli keeps the session open until its own mempool
# transaction is mined, which on a chain we mine by hand is never.
restore faucet $ZAINO "$FAUCET_SEED" 1
wallet faucet $ZAINO --nosync -- quickshield | tee "$OUT/faucet_shield.json"
# zingolib spends notes only after 3 confirmations.
"$REGTEST" mine 3 > /dev/null

say "receiver addresses"
# A throwaway copy of the receiver, used only to derive addresses.
restore receiver-keys $ZAINO "$RECEIVER_SEED" 1
RECV_UA=$(wallet receiver-keys $ZAINO --nosync -- addresses | jq -r '.[0].encoded_address')
RECV_SAPLING=$(wallet receiver-keys $ZAINO --nosync -- new_address z | jq -r 'if type == "array" then .[0].encoded_address else .encoded_address end')
FAUCET_UA=$(wallet faucet $ZAINO --nosync -- addresses | jq -r '.[0].encoded_address')
echo "receiver ua:      $RECV_UA"
echo "receiver sapling: $RECV_SAPLING"
printf 'receiver_ua=%s\nreceiver_sapling_ua=%s\nfaucet_ua=%s\n' "$RECV_UA" "$RECV_SAPLING" "$FAUCET_UA" > "$OUT/addresses.txt"

say "payments (one transaction, two recipients)"
PAYMENTS=$(jq -cn --arg a "$RECV_UA" --arg b "$RECV_SAPLING" --argjson x $PAY_IRONWOOD --argjson y $PAY_SAPLING \
  '[{address: $a, amount: $x, memo: "step 1: ironwood payment"}, {address: $b, amount: $y, memo: "step 1: sapling payment"}]')
wallet faucet $ZAINO --waitsync -- balance > /dev/null
wallet faucet $ZAINO --nosync -- quicksend "$PAYMENTS" | tee "$OUT/payment.json"
PAY_HEIGHT=$("$REGTEST" mine 1)
"$REGTEST" mine 2 > /dev/null
echo "payment mined at height $PAY_HEIGHT"

say "gateway"
"$GATEWAY" proxy --listen $GW --upstream http://$ZAINO --record "$OUT/calls.jsonl" 2> "$OUT/gateway.log" &
GW_PID=$!
trap 'kill $GW_PID 2>/dev/null || true; "$REGTEST" down' EXIT
sleep 1

say "receive: direct vs gateway, same seed, same birthday"
snapshot receiver-direct $ZAINO receive-direct "$RECEIVER_SEED" 1
snapshot receiver-gateway $GW receive-gateway "$RECEIVER_SEED" 1
cat "$OUT/receive-direct/balance.txt"

REPORT=$OUT/comparison.md
echo "# Step 1 comparison" > "$REPORT"
echo >> "$REPORT"
RECV_OK=0
compare receive-direct receive-gateway "$REPORT" || RECV_OK=1

say "spend from the direct copy; the gateway copy must see it"
wallet receiver-direct $ZAINO --nosync -- quicksend "$FAUCET_UA" $SPEND_BACK "step 1: spend back" | tee "$OUT/spend.json"
"$REGTEST" mine 3 > /dev/null
snapshot receiver-direct $ZAINO spend-direct
snapshot receiver-gateway $GW spend-gateway
cat "$OUT/spend-direct/balance.txt"
SPEND_OK=0
compare spend-direct spend-gateway "$REPORT" || SPEND_OK=1

say "send through the gateway; the direct copy must see it"
# This puts SendTransaction (and the proposal-building calls) through the
# gateway, so the inventory covers sending as well as scanning.
wallet receiver-gateway $GW --nosync -- quicksend "$FAUCET_UA" $SEND_VIA_GATEWAY "step 1: sent through gateway" | tee "$OUT/send-via-gateway.json"
"$REGTEST" mine 3 > /dev/null
snapshot receiver-direct $ZAINO send-direct
snapshot receiver-gateway $GW send-gateway
cat "$OUT/send-gateway/balance.txt"
SEND_OK=0
compare send-direct send-gateway "$REPORT" || SEND_OK=1

kill $GW_PID 2>/dev/null || true
wait $GW_PID 2>/dev/null || true
trap '"$REGTEST" down' EXIT

say "chain census (from zebrad, every transaction)"
"$ROOT/scripts/chain-census.sh" > "$OUT/chain-census.json"
jq -c '{tip, transactions, coinbase, with_shielded_parts, by_version}' "$OUT/chain-census.json"

say "rpc inventory"
"$GATEWAY" summarize "$OUT/calls.jsonl" > "$OUT/rpc-inventory.md"
head -30 "$OUT/rpc-inventory.md"

# Rendered configs, with this machine's run directory shown as .run.
for f in zebrad.toml zainod.toml; do sed "s|$RUN|.run|g" "$RUN/$f" > "$OUT/$f"; done
"$REGTEST" tip > "$OUT/final_tip.txt"

say "result"
cat "$REPORT"
if [ $RECV_OK -eq 0 ] && [ $SPEND_OK -eq 0 ] && [ $SEND_OK -eq 0 ]; then
  echo "PASS: gateway sync matches direct sync (receive, spend detection, send)" | tee "$OUT/RESULT"
else
  echo "FAIL: see $REPORT" | tee "$OUT/RESULT"
  exit 1
fi
