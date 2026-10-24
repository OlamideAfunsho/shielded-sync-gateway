#!/usr/bin/env bash
# Clone and build zebrad, zainod and zingo-cli at the versions in
# versions.env. Binaries land in .deps/bin. Takes a while on a laptop
# (roughly an hour on 2 cores, most of it zebrad).
set -euo pipefail
cd "$(dirname "$0")/.."
. ./versions.env
for v in ZEBRA_REPO ZAINO_REPO ZINGOLIB_REPO; do
  if [ -z "${!v:-}" ]; then
    echo "set $v to the git URL (or local path) of that upstream repository; see versions.env" >&2
    exit 1
  fi
done

DEPS=${DEPS:-$PWD/.deps}
mkdir -p "$DEPS/src" "$DEPS/bin"

fetch() { # repo tag dir
  if [ ! -d "$DEPS/src/$3/.git" ]; then
    git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$2" "$1" "$DEPS/src/$3"
  fi
  git -C "$DEPS/src/$3" -c advice.detachedHead=false checkout --quiet "$2"
  echo "$3 $(git -C "$DEPS/src/$3" rev-parse HEAD)"
}

fetch "$ZEBRA_REPO" "$ZEBRA_TAG" zebra
fetch "$ZAINO_REPO" "$ZAINO_TAG" zaino
fetch "$ZINGOLIB_REPO" "$ZINGOLIB_TAG" zingolib

# Each upstream pins its own toolchain in rust-toolchain.toml. Set
# RUSTUP_TOOLCHAIN=stable to override when those can't be downloaded; we
# built with 1.97.0 stable without changes to any of the three.
( cd "$DEPS/src/zebra" && cargo build --release --locked --bin zebrad )
cp "$DEPS/src/zebra/target/release/zebrad" "$DEPS/bin/"

( cd "$DEPS/src/zaino" && cargo build --release --locked -p zainod --features "$ZAINO_FEATURES" )
cp "$DEPS/src/zaino/target/release/zainod" "$DEPS/bin/"

# zingolib's build script fetches the Sapling parameters from
# download.z.cash. Where that host is blocked, the same bytes ship on
# crates.io as wagyu-zcash-parameters-*, which zebrad's build has already
# pulled into the cargo cache. Reassemble and check the published hashes.
params=$HOME/.zcash-params
if [ ! -s "$params/sapling-spend.params" ]; then
  reg=$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/ 2>/dev/null | head -1)
  if [ -n "$reg" ] && [ -d "$reg/wagyu-zcash-parameters-6-0.2.0" ]; then
    mkdir -p "$params"
    cat "$reg"/wagyu-zcash-parameters-{1,2,3,4,5}-0.2.0/src/sapling-spend-*.params > "$params/sapling-spend.params"
    cp "$reg/wagyu-zcash-parameters-6-0.2.0/src/sapling-output-1.params" "$params/sapling-output.params"
    ( cd "$params" && b2sum -c - ) <<'SUMS'
8270785a1a0d0bc77196f000ee6d221c9c9894f55307bd9357c3f0105d31ca63991ab91324160d8f53e2bbd3c2633a6eb8bdf5205d822e7f3f73edac51b2b70c  sapling-spend.params
657e3d38dbb5cb5e7dd2970e8b03d69b4787dd907285b5a7f0790dcc8072f60bf593b32cc2d1c030e00ff5ae64bf84c5c3beb84ddc841d48264b4a171744d028  sapling-output.params
SUMS
  fi
fi

( cd "$DEPS/src/zingolib" && cargo build --release --locked -p zingo-cli --no-default-features --features "$ZINGO_CLI_FEATURES" )
cp "$DEPS/src/zingolib/target/release/zingo-cli" "$DEPS/bin/"

"$DEPS/bin/zebrad" --version
"$DEPS/bin/zainod" --version || true
"$DEPS/bin/zingo-cli" --version
