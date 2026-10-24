#!/usr/bin/env bash
# Disposable local Zcash chain: zebrad (regtest) + zainod in front of it.
#
#   scripts/regtest.sh up          fresh chain, mine to maturity, start zainod
#   scripts/regtest.sh mine N      mine N blocks and wait for zainod to see them
#   scripts/regtest.sh tip         zebrad height and zainod height
#   scripts/regtest.sh rpc M [P]   raw zebrad JSON-RPC call
#   scripts/regtest.sh down        stop everything (state stays in $RUN)
#
# Nothing here touches mainnet or testnet; zebrad has no peers.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)

BIN=${BIN:-$ROOT/.deps/bin}
RUN=${RUN:-$ROOT/.run}
GATEWAY=${GATEWAY:-$ROOT/target/release/ssg-gateway}
ZEBRA_RPC=http://127.0.0.1:18232
ZAINO_GRPC=http://127.0.0.1:8137

# Coinbase goes to the "abandon ... art" test wallet (the faucet). This is
# the Orchard-receiver unified address zingolabs' fixtures use; after NU6.3
# zebrad pays a UA's shielded coinbase into the Ironwood pool.
FAUCET_UA=uregtest1zkuzfv5m3yhv2j4fmvq5rjurkxenxyq8r7h4daun2zkznrjaa8ra8asgdm8wwgwjvlwwrxx7347r8w0ee6dqyw4rufw4wg9djwcr6frzkezmdw6dud3wsm99eany5r8wgsctlxquu009nzd6hsme2tcsk0v3sgjvxa70er7h27z5epr67p5q767s2z5gt88paru56mxpm6pwz0cu35m
FAUCET_TADDR=tmBsTi2xWTjUdEXnuTceL7fecEQKeWaPDJd
MINER=${MINER:-$FAUCET_TADDR}

log() { printf '[regtest] %s\n' "$*" >&2; }

rpc() {
  local method=$1 params=${2:-[]}
  curl -sf -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$method\",\"params\":$params}" \
    "$ZEBRA_RPC"
}

zebra_height() { rpc getblockcount | jq -r .result; }
zaino_height() { "$GATEWAY" tip "$ZAINO_GRPC" 2>/dev/null | cut -d' ' -f1; }

wait_for() { # description timeout_s command...
  local what=$1 limit=$2; shift 2
  local i=0
  until "$@" >/dev/null 2>&1; do
    i=$((i + 1))
    if [ "$i" -ge "$((limit * 4))" ]; then
      log "timed out waiting for $what"
      return 1
    fi
    sleep 0.25
  done
}

zaino_at() { [ "$(zaino_height)" = "$1" ]; }

render() { # template out
  sed -e "s|@RUN@|$RUN|g" -e "s|@MINER@|$MINER|g" "$1" > "$2"
}

start_bg() { # name cmd...
  local name=$1; shift
  nohup "$@" > "$RUN/$name.log" 2>&1 &
  echo $! > "$RUN/$name.pid"
}

stop() {
  for name in zainod zebrad; do
    if [ -f "$RUN/$name.pid" ]; then
      kill "$(cat "$RUN/$name.pid")" 2>/dev/null || true
      rm -f "$RUN/$name.pid"
    fi
    # Catch anything an interrupted run left behind without a pid file.
    pkill -f "$RUN/$name.toml" 2>/dev/null || true
  done
  # Give RocksDB a moment to close cleanly before anyone reuses $RUN.
  sleep 1
}

mine() {
  local n=$1
  local target=$(( $(zebra_height) + n ))
  rpc generate "[$n]" > /dev/null
  if [ -f "$RUN/zainod.pid" ]; then
    wait_for "zainod to reach $target" 120 zaino_at "$target"
  fi
  echo "$target"
}

up() {
  local initial=${1:-110}
  stop
  rm -rf "$RUN/zebra" "$RUN/zaino"
  mkdir -p "$RUN/zebra" "$RUN/zaino"
  render "$ROOT/regtest/zebrad.toml.in" "$RUN/zebrad.toml"
  render "$ROOT/regtest/zainod.toml.in" "$RUN/zainod.toml"

  log "starting $("$BIN/zebrad" --version 2>/dev/null | head -1)"
  start_bg zebrad "$BIN/zebrad" -c "$RUN/zebrad.toml" start
  wait_for "zebrad rpc" 60 rpc getblockcount

  # Height 1 first, on its own: zebrad's regtest wants the chain started
  # before NU5 so the first coinbase can be built without an Orchard anchor.
  rpc generate '[1]' > /dev/null
  rpc generate "[$((initial - 1))]" > /dev/null
  log "zebrad at height $(zebra_height)"

  log "starting zainod"
  start_bg zainod "$BIN/zainod" start -c "$RUN/zainod.toml"
  wait_for "zainod grpc" 120 zaino_at "$(zebra_height)"
  log "zainod serving height $(zaino_height) on $ZAINO_GRPC"
}

case "${1:-}" in
  up) up "${2:-110}" ;;
  down) stop ;;
  mine) mine "${2:-1}" ;;
  tip) echo "zebrad $(zebra_height)  zainod $(zaino_height)" ;;
  rpc) rpc "$2" "${3:-[]}" | jq . ;;
  *) sed -n '2,10p' "$0"; exit 2 ;;
esac
