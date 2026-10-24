# shielded-sync-gateway

A local gateway that sits between a Zcash light wallet and its indexer. It
refuses to give the wallet any shielded scanning data it cannot
authenticate.

## The problem

Light wallets scan compact blocks. For each output and action they get:

- the note commitment
- the ephemeral key
- the first 52 bytes of the note ciphertext
- the nullifiers

The wallet can check commitments against the note commitment tree it
builds. Nothing lets it check the ciphertext, ephemeral key or nullifier
next to a commitment. An indexer that changes those fields can hide an
incoming payment, or hide a spend, while every commitment stays correct.

## What the gateway does

It rebuilds each transaction's ZIP 244 txid from:

- the compact fields the indexer sent, and
- a small verification record from a separate producer.

It then rebuilds the block's transaction Merkle root from every txid and
compares it with the root in a header from a trusted Zebra node. A block
reaches the wallet only if the roots match.

## Status

Every step of the MVP plan is implemented and has been run against Zingo
CLI on a local chain.

| step | result | details |
|---|---|---|
| 1. Wallet connection | gateway sync = direct sync; full RPC inventory | [docs/step1-wallet-connection.md](docs/step1-wallet-connection.md), [results/step1](results/step1) |
| 2. Producer + verifier | rebuilt txids and roots match 15 mainnet blocks (v1–v5), 122 v6/Ironwood regtest blocks and the ZIP 244 vectors | [docs/proof-spec.md](docs/proof-spec.md), `crates/verifier` |
| 3. Verifying gateway | restart resume, crash safety, reorg rollback: 10/10 checks | [docs/step3-gateway.md](docs/step3-gateway.md), [results/step3](results/step3) |
| 4. Protection + overhead | 5/5 mutations blocked at their block; all recover; overhead measured | [docs/step4-protection.md](docs/step4-protection.md), [results/step4](results/step4), [results/bench](results/bench) |

A demo video is in [docs/media/mvp-demo.mp4](docs/media/mvp-demo.mp4). The
step 1 video is [docs/media/step1-demo.mp4](docs/media/step1-demo.mp4).

## The main result

Setup for each case:

- **Same start:** a fresh wallet restored from the same seed and birthday.
- **Same chain:** the receiver got 2.5 ZEC (Ironwood) and 1.25 ZEC
  (Sapling) at block 114, and another copy of the wallet spent 1 ZEC at
  block 117.
- **Only one field kind changed:** the indexer altered one kind of field
  in one block. Commitments were never touched.

| what the indexer altered | wallet syncing directly | wallet through the gateway |
|---|---|---|
| ciphertexts, block 114 | **1.4999 ZEC instead of 2.7499 ZEC**; the block-114 payment never appears; no error | sync error, block 114 refused, verified height stays 113 |
| ephemeral keys, block 114 | **1.4999 ZEC instead of 2.7499 ZEC**; no error | same, refused at 114 |
| nullifiers, block 117 | **3.75 ZEC instead of 2.7499 ZEC**; the spend is never seen; no error | refused at 117 |
| omitted transactions, block 114 | balance 0 and a "RESCAN TRIGGERED" error (Zingo noticed the tree size didn't add up) | refused at 114 |
| producer lies (memo digest), block 114 | (not applicable) | refused at 114 |

In every case, once the source served correct data again, the gateway
verified the block without a restart, and the same wallet ended up
identical to the honest one. The wallet's error message reads, verbatim:
`ssg-gateway: block 114 failed verification; refusing to serve it:
transaction 1: compact block claims txid 24f3…, fields hash to c7a8…`.

## What it costs

Measured on a 1,018-block regtest chain, all on one machine
([results/bench](results/bench/summary.md)):

| | direct | through gateway |
|---|---:|---:|
| bytes the wallet received | 131,020 | 131,047 (+27: the gateway's vendor string) |
| RPC calls per sync | 51 | 51 |
| wallet peak memory | 26.0 MB | 26.2 MB |

On the gateway side:

- Verifying all 1,018 blocks from the checkpoint took 1.3 s wall time and
  0.4 ms of CPU per block, with 12.6 MB peak memory.
- Records were 5.8% of the size of full blocks.
- Wallet sync time shows no difference beyond zingo-cli's whole-second
  polling (3.02 s direct, 2.02 s through a warm gateway). We don't claim
  the gateway is faster.

## Trust model

| | |
|---|---|
| **Trusted** | one Zebra node: headers, chain tip, note-tree state; and a pinned checkpoint |
| **Untrusted** | the indexer, the producer, and whatever node the producer reads |
| **Claim** | compact blocks (shielded and transparent fields), tree states, subtree roots and the tip are what the trusted node's best chain contains, from the checkpoint onward |
| **Not covered** | transparent address history and the mempool (passed through); header authentication for phones without a trusted node (the next phase) |

The per-RPC table is in [docs/trust-boundaries.md](docs/trust-boundaries.md).

## Layout

```
crates/verifier    ZIP 244 digests (v5, v6, Ironwood), Merkle root with
                   CVE-2012-2459 check, block verification, record producer
crates/producer    ssg-producer: full blocks from a node -> records over HTTP
crates/gateway     ssg-gateway: serve (verifying gateway), checkpoint,
                   proxy (recorder / tamper proxy), inspect, summarize
crates/lwproto     lightwallet protocol + record protobufs, tonic stubs
regtest/           zebrad and zainod config templates
scripts/           build-deps, regtest, step1, step3, step4, bench, chain-census
tools/             output normalizer, demo-video recorder and renderer
results/           output of the last run of each script
docs/              per-step write-ups, proof spec, trust boundaries, videos
```

## Running it

You need Rust, `jq`, `curl`, `protoc`, clang and Python 3. `bench.sh` also
needs `/usr/bin/time`.

```sh
# ZEBRA_REPO, ZAINO_REPO, ZINGOLIB_REPO: git URLs or local clones of the upstreams
scripts/build-deps.sh   # zebrad 6.3.0, zainod 0.10.1, zingo-cli (zingolib 7.0.0) -> .deps/bin
cargo test --release    # 28 tests: fixtures, tamper cases, pass-through proxy
scripts/step1.sh        # wallet connection
scripts/step3.sh        # restart, crash, reorg
scripts/step4.sh        # tampering demonstration
scripts/bench.sh        # overhead
```

If rustup can't fetch the toolchains the upstreams pin, run
`RUSTUP_TOOLCHAIN=stable scripts/build-deps.sh`. It was tested with 1.97.0
and builds all three unmodified. Each script starts its own disposable
regtest chain, writes to `results/` and exits non-zero on failure.

Running the pieces yourself:

```sh
ssg-producer serve --source http://127.0.0.1:18232 --listen 127.0.0.1:9180
ssg-gateway checkpoint --trusted http://127.0.0.1:18232 --height 0 --out checkpoint.json
ssg-gateway serve --trusted http://127.0.0.1:18232 --indexer http://127.0.0.1:8137 \
    --producer http://127.0.0.1:9180 --checkpoint checkpoint.json --state state.db
zingo-cli --chain regtest --server 127.0.0.1:9168 ...
```

## License

MIT. The `.proto` files in `crates/lwproto/proto` are MIT, copyright The
Zcash developers. The mainnet blocks in `crates/verifier/tests/fixtures`
come from Zebra's test vectors.
