# Shared helpers for scripts/step3.sh, step4.sh and bench.sh. Source it.
#
# Ports: zebrad 18232, zainod 8137, tamper proxy 9170, producer 9180,
# gateway 9168, recording proxies 9171/9172.

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
export BIN=${BIN:-$ROOT/.deps/bin}
export RUN=${RUN:-$ROOT/.run}
REGTEST="$ROOT/scripts/regtest.sh"
GATEWAY="$ROOT/target/release/ssg-gateway"
PRODUCER="$ROOT/target/release/ssg-producer"

ZAINO=127.0.0.1:8137
PROXY=127.0.0.1:9170
GW=127.0.0.1:9168
ZEBRA_RPC=http://127.0.0.1:18232

# Fixture seeds from zingolabs/infrastructure (zingo_test_vectors).
FAUCET_SEED="abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art"
RECEIVER_SEED="hospital museum valve antique skate museum unfold vocal weird milk scale social vessel identify crowd hospital control album rib bulb path oven civil tank"

W=$RUN/wallets

say() { printf '\n== %s\n' "$*"; }

need_bins() {
  for b in zebrad zainod zingo-cli; do
    [ -x "$BIN/$b" ] || { echo "missing $BIN/$b; run scripts/build-deps.sh" >&2; exit 1; }
  done
  [ -x "$GATEWAY" ] && [ -x "$PRODUCER" ] || (cd "$ROOT" && cargo build --release -q)
}

rpc() {
  curl -sf -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" "$ZEBRA_RPC"
}

clean() {
  grep -vE '^(WARNING: this build carries|Launching (sync|save) task|Save task shutdown|Zingo CLI quit|Sync (task|complete))' || true
}

# wallet NAME SERVER [session options...] -- command...
# zingo-cli's stderr goes to $W/NAME.stderr; its exit status is kept in
# $W/NAME.rc so callers can tell a sync error from success.
wallet() {
  local name=$1 server=$2; shift 2
  local opts=()
  while [ "$1" != "--" ]; do opts+=("$1"); shift; done
  shift
  mkdir -p "$W"
  (
    cd "$W" || exit 1
    timeout "${WALLET_TIMEOUT:-180}" "$BIN/zingo-cli" --chain regtest --server "$server" \
      --data-dir "$W/$name" "${opts[@]}" "$@" 2>>"$W/$name.stderr"
    echo $? > "$W/$name.rc"
  ) | clean
}

# Restore from seed and sync in one session (a wallet created with
# --nosync and synced later stalls in zingo-cli 7.0.0).
restore() { # name server seed birthday
  ZINGO_SEED=$3 wallet "$1" "$2" --birthday "$4" --waitsync -- balance > /dev/null
}

# Errors zingo-cli printed in the last session of NAME, one per line.
wallet_errors() {
  { grep -E '^(error|caused by)' "$W/$1.stderr" 2>/dev/null || true; } | tail -6
}

snapshot() { # name server dir [seed birthday]
  local d=$3
  mkdir -p "$d" "$W"
  : > "$W/$1.stderr"
  local t0 t1
  t0=$(date +%s.%N)
  if [ $# -ge 5 ]; then
    ZINGO_SEED=$4 wallet "$1" "$2" --birthday "$5" --waitsync -- balance > "$d/balance.txt"
  else
    wallet "$1" "$2" --waitsync -- balance > "$d/balance.txt"
  fi
  t1=$(date +%s.%N)
  cp "$W/$1.rc" "$d/sync_exit_code.txt"
  wallet_errors "$1" > "$d/sync_errors.txt"
  awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.2f\n", b-a}' > "$d/sync_seconds.txt"
  wallet "$1" "$2" --nosync -- height > "$d/height.json"
  wallet "$1" "$2" --nosync -- notes all > "$d/notes.json"
  wallet "$1" "$2" --nosync -- transactions > "$d/transactions.txt"
  wallet "$1" "$2" --nosync -- value_transfers > "$d/value_transfers.txt"
}

# See tools/normalize.py for the two things it is allowed to ignore.
normalize() { # file
  python3 "$ROOT/tools/normalize.py" "$1"
}

# same_state DIR_A DIR_B: 0 when balance, height, note set, transactions
# and value transfers all match.
same_state() {
  local f
  for f in balance.txt height.json notes.json transactions.txt value_transfers.txt; do
    cmp -s <(normalize "$1/$f") <(normalize "$2/$f") || return 1
  done
}

# Balances as "ironwood sapling" in zatoshis.
balances() {
  awk -F': ' '/total_ironwood_balance/{gsub("_","",$2); i=$2} /total_sapling_balance/{gsub("_","",$2); s=$2} END{print i+0, s+0}' "$1/balance.txt"
}

start_bg() { # name cmd...
  local name=$1; shift
  nohup "$@" > "$RUN/$name.log" 2>&1 &
  echo $! > "$RUN/$name.pid"
}

stop_bg() { # name [signal]
  local f=$RUN/$1.pid
  if [ -f "$f" ]; then
    kill "-${2:-TERM}" "$(cat "$f")" 2>/dev/null || true
    for _ in $(seq 1 40); do kill -0 "$(cat "$f")" 2>/dev/null || break; sleep 0.1; done
    rm -f "$f"
  fi
}

wait_port() { # host:port
  for _ in $(seq 1 100); do
    (exec 3<>"/dev/tcp/${1%:*}/${1#*:}") 2>/dev/null && return 0
    sleep 0.1
  done
  echo "nothing listening on $1" >&2
  return 1
}

start_producer() { # [tamper file]
  local extra=()
  [ -n "${1:-}" ] && extra=(--tamper-file "$1")
  start_bg producer "$PRODUCER" serve --source "$ZEBRA_RPC" --listen 127.0.0.1:9180 "${extra[@]}"
  wait_port 127.0.0.1:9180
}

start_tamper_proxy() { # rules file, call log
  start_bg tamperproxy "$GATEWAY" proxy --listen $PROXY --upstream "http://$ZAINO" --tamper-file "$1" --record "$2"
  wait_port $PROXY
}

# start_gateway STATE_DIR [indexer]
start_gateway() {
  local dir=$1 indexer=${2:-http://$PROXY}
  mkdir -p "$dir"
  [ -f "$dir/checkpoint.json" ] || "$GATEWAY" checkpoint --trusted "$ZEBRA_RPC" --height 0 --out "$dir/checkpoint.json" > /dev/null
  start_bg gateway "$GATEWAY" serve --listen $GW --indexer "$indexer" --producer http://127.0.0.1:9180 \
    --trusted "$ZEBRA_RPC" --checkpoint "$dir/checkpoint.json" --state "$dir/state.db" --status-file "$dir/status.json"
  wait_port $GW
}

gw_status() { # dir field
  jq -r ".$2" "$1/status.json"
}

# wait_gateway DIR [settle|tip]: wait (up to 120 s) until the follower
# reaches the trusted tip, or, in "settle" mode (default), until it either
# reaches the tip or reports a failure. Prints the verified height.
wait_gateway() {
  local mode=${2:-settle} tip v f
  tip=$(rpc getblockcount | jq -r .result)
  for _ in $(seq 1 600); do
    if [ -f "$1/status.json" ]; then
      v=$(jq -r .verified_height "$1/status.json")
      f=$(jq -r '.failure.height // empty' "$1/status.json")
      if [ "$v" = "$tip" ] && [ -z "$f" ]; then echo "$v"; return 0; fi
      if [ "$mode" = settle ] && [ -n "$f" ]; then echo "$v"; return 0; fi
    fi
    sleep 0.2
  done
  echo "gateway did not settle" >&2
  return 1
}

# Builds the step 1 chain shape: faucet shields coinbase, then pays the
# receiver 2.5 ZEC (Ironwood, default UA) and 1.25 ZEC (Sapling-only UA)
# in one transaction. Exports PAY_HEIGHT, RECV_UA, RECV_SAPLING, FAUCET_UA.
build_chain() {
  rm -rf "$W"
  mkdir -p "$W"
  "$REGTEST" up 110
  restore faucet $ZAINO "$FAUCET_SEED" 1
  wallet faucet $ZAINO --nosync -- quickshield > /dev/null
  "$REGTEST" mine 3 > /dev/null
  restore receiver-keys $ZAINO "$RECEIVER_SEED" 1
  RECV_UA=$(wallet receiver-keys $ZAINO --nosync -- addresses | jq -r '.[0].encoded_address')
  RECV_SAPLING=$(wallet receiver-keys $ZAINO --nosync -- new_address z | jq -r 'if type == "array" then .[0].encoded_address else .encoded_address end')
  FAUCET_UA=$(wallet faucet $ZAINO --nosync -- addresses | jq -r '.[0].encoded_address')
  local payments
  payments=$(jq -cn --arg a "$RECV_UA" --arg b "$RECV_SAPLING" \
    '[{address: $a, amount: 250000000, memo: "ironwood payment"}, {address: $b, amount: 125000000, memo: "sapling payment"}]')
  wallet faucet $ZAINO --waitsync -- balance > /dev/null
  wallet faucet $ZAINO --nosync -- quicksend "$payments" > /dev/null
  PAY_HEIGHT=$("$REGTEST" mine 1)
  "$REGTEST" mine 2 > /dev/null
  export PAY_HEIGHT RECV_UA RECV_SAPLING FAUCET_UA
}
