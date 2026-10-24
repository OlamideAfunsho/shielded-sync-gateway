# Step 1: wallet connection

Goal from the plan: pin compatible Zingo CLI, Zebra and Zaino versions,
create a disposable local chain, receive a shielded payment, sync through a
minimal gateway, and identify every call the wallet needs.

Pass condition: payment, balance and sync height match direct sync.

Result: **pass**, five runs out of five on fresh chains
([results/step1/repeats.md](../results/step1/repeats.md)). The gateway
copy and the direct copy of the wallet also agreed after a spend and after a
send, which goes beyond the pass condition.

## Setup

| component | version | source |
|---|---|---|
| zebrad | 6.3.0 | `ZcashFoundation/zebra` tag `v6.3.0` |
| zainod | 0.10.1 | `zingolabs/zaino` tag `zainod-0.10.1`, feature `no_tls_use_unencrypted_traffic` |
| zingo-cli | from zingolib 7.0.0 | `zingolabs/zingolib` tag `zingolib_v7.0.0`, `--no-default-features --features nakednet-test-mode` |
| ssg-gateway | 0.1.0 | this repo |

The three upstream versions are the set zingolib 7.0.0 itself tests against
(its `.env.testing-artifacts`, pinned 2026-09-28). All three built
unmodified with Rust 1.97.0 stable.

```
zingo-cli ──h2c──▶ ssg-gateway :9168 ──h2c──▶ zainod :8137 ──JSON-RPC──▶ zebrad :18232 (regtest, no peers)
     └──────────────────────── direct ─────────▶ zainod :8137
```

### Decisions and why

**zingo-cli without the mixnet.** zingo-cli 7 sends transactions over the
Nym mixnet and will not go online at all without the `nym-proxy` binary.
A regtest chain on localhost is not reachable through the mixnet. The
`nakednet-test-mode` build is what zingolabs uses for its own sync
benchmarks (`makers sync-bench --nakednet`); it lets the session go online
directly. Sync traffic is plain gRPC to the configured indexer in both
builds, so this does not change where the gateway sits or what it sees.
Only `SendTransaction` would take a different route in the default build.

**Activation heights.** zingo-cli has no option for regtest activation
heights; it assumes everything through NU6.3 is active at height 1 and NU7
is off. zebrad is configured to match. With NU6.1 at height 1, zebrad
rejects block 1 unless the deferred funding stream also starts at height 1,
because the NU6.1 lockbox disbursement would otherwise draw from an empty
pool (`Deferred(Constraint { value: -1 })`). See
[regtest/zebrad.toml.in](../regtest/zebrad.toml.in).

**Transparent coinbase, then shield.** Coinbase goes to the faucet's
transparent address and the faucet shields it, which is also zingolabs'
default. We tried mining straight to the faucet's unified address first.
After NU6.3, zebrad puts that coinbase in the Ironwood pool, and zingo-cli
7.0.0 then fails to scan with `invalid memo bytes` / `Invalid UTF-8`. Notes
that zingolib builds itself scan fine. We have not traced which side is
wrong. It looks like a zebrad/zingolib disagreement over the Ironwood
coinbase memo, and it should go upstream as an issue rather than be worked
around here.

**Sapling parameters.** zingolib's build script downloads the Sapling
proving parameters from download.z.cash. If that host is unreachable,
`build-deps.sh` can't do it for you. The same files are published on
crates.io as the `wagyu-zcash-parameters-*` crates (already in the cargo
cache after building zebrad); concatenated, they match the canonical
BLAKE2b hashes. Put them in `~/.zcash-params/`.

## What the script does

[scripts/step1.sh](../scripts/step1.sh), in order:

1. Fresh chain: zebrad mines 110 blocks to the faucet, zainod starts and
   catches up.
2. Faucet ("abandon … art" test seed) restores, syncs, shields its mature
   coinbase. Mine 3 (zingolib spends only after 3 confirmations).
3. Faucet pays the receiver ("hospital museum …" test seed) in one
   transaction with two outputs: 2.5 ZEC to its default unified address and
   1.25 ZEC to a Sapling-only unified address. Mine 3.
4. Start the gateway. Restore the receiver twice, same seed, same birthday
   (1), into two empty data directories: `receiver-direct` syncs from
   zainod, `receiver-gateway` from the gateway. Snapshot both.
5. `receiver-direct` sends 1 ZEC back to the faucet. Mine 3. Both copies
   sync; snapshot both. The gateway copy only learns about this spend by
   scanning, so this checks nullifier detection through the gateway.
6. `receiver-gateway` sends 0.5 ZEC to the faucet through the gateway.
   Mine 3. Both copies sync; snapshot both.
7. Count every transaction on the chain from zebrad, summarise the gateway
   log, compare snapshots.

A snapshot is the wallet's own output for `balance`, `height`, `notes all`,
`transactions` and `value_transfers`. The comparison is byte-for-byte,
except `notes all`. In two of the five runs the spend phase listed them in
the same order and the comparison was byte-for-byte too. That command lists notes in the wallet's internal order,
which depends on whether the wallet built the note itself (change from its
own send) or found it while scanning. In those cases the script compares the
sorted note lists and reports "same notes, listed in a different order".

## Results

From [results/step1/comparison.md](../results/step1/comparison.md):

| phase | balance | height | notes | transactions | value transfers |
|---|---|---|---|---|---|
| receive | identical | identical (116) | identical | identical | identical |
| spend by direct copy | identical | identical (119) | same set, different order | identical | identical |
| send through gateway | identical | identical (122) | same set, different order | identical | identical |

Final balance on both copies: 0.9998 ZEC Ironwood, 1.25 ZEC Sapling.

Sync times are equal to the millisecond (about 2.03 s both ways) because
they are set by zingo-cli's sync polling, not by the network. They say
nothing about gateway overhead. Overhead is measured properly in step 4.

## RPC inventory

From [results/step1/rpc-inventory.md](../results/step1/rpc-inventory.md),
92 calls through the gateway: three sync sessions plus one send.

| call | per sync session | notes |
|---|---|---|
| `GetLightdInfo` | 1 | first call; chain name `test`, branch id `37a5165b` (NU6.3), protocol `v0.5.0` |
| `GetLatestBlock` | 1 | tip height and hash |
| `GetMempoolStream` | 1 | opened at start, held open, closed by the wallet at exit |
| `GetTreeState` | 1 | **at the tip**, not at the birthday; carries Sapling, Orchard and Ironwood frontiers |
| `GetTaddressTxids` | 21 | one per transparent address the wallet derives, over the unscanned range |
| `GetSubtreeRoots` | 3 | Sapling, Orchard, Ironwood, from index 0; none exist yet on a 122-block chain |
| `GetBlockRange` | 1 to 2 | pool types `[SAPLING, ORCHARD, IRONWOOD]`; first sync split as 1..=10 then 11..=116 |
| `GetTransaction` | 0 to 1 | full transaction by txid, for each transaction the scan found relevant |
| `SendTransaction` | per send | raw v6 transaction, 9,166 bytes; zainod answers with the txid in `errorMessage` and code 0 |

Not seen: `GetBlock`, `GetBlockNullifiers`, `GetBlockRangeNullifiers`,
`GetLatestTreeState`, `GetTaddressBalance`, `GetAddressUtxos`,
`GetMempoolTx`, `Ping`.

Fields the wallet received across 122 compact blocks:

| field | times present |
|---|---:|
| `block.hash`, `block.prev_hash`, `block.time`, `block.chain_metadata` | every block |
| `block.header` | never |
| `tx.txid` | 4 |
| `sapling_output.{cmu, ephemeral_key, ciphertext}` | 2 each |
| `ironwood_action.{nullifier, cmx, ephemeral_key, ciphertext}` | 8 each |
| `orchard_action.*`, `sapling_spend.nf` | never |

## What this means for steps 2 to 4

**Ironwood and v6 are the baseline.** All 126 transactions on the chain,
coinbase included, are v6 (version group `0xd884b698`). Payments to a
unified address and all change went to Ironwood. A gateway that verifies
only Sapling and Orchard would pass the wallet's real payments through
unverified. The verifier needs Ironwood actions (`CompactTx` field 9) and
the v6 txid digest from day one. The payment at height 114 has two Sapling
outputs inside a v6 transaction. That is a real, non-empty v6 Sapling case
to use as a step 2 fixture.

**Compact blocks don't show the whole block.** The chain census
([results/step1/chain-census.json](../results/step1/chain-census.json))
has 126 transactions. The wallet got 4 compact transactions: the ones with
shielded parts. Coinbase and transparent-only transactions are left out by
design. So the gateway can't check "every transaction is represented" by
counting compact transactions. The producer's record must list every txid
in block order, and the gateway must rebuild the block's transaction
commitment from that full list.

**Block hashes in compact blocks are claims.** `header` is empty, so the
wallet never sees anything it could hash to check `hash` or `prev_hash`.
That supports the MVP's trust split: headers and the tip come from a
separately trusted Zebra, and scanning data from the untrusted producer.

**Tree state needs the same treatment as blocks.** The wallet seeds its
note trees from `GetTreeState` at the tip and checks its scan against
`chain_metadata` sizes and subtree roots. An indexer that lies there breaks
the wallet's witnesses even if every block is honest. Step 3 has to derive
or authenticate these from verified blocks, not forward them.

**`GetTransaction` is easy to authenticate.** The wallet asks by txid and
gets the full transaction, which is where memos and outgoing data come
from. Recomputing the txid of the returned bytes (ZIP 244) and comparing
it with the request closes this path. It uses the same code as step 2.

**Out of scope for the MVP, written down so it isn't forgotten:**
`GetTaddressTxids` (21 calls per session, transparent history) and
`GetMempoolStream` both pass through unauthenticated. Transparent
addresses aren't part of the shielded-scanning claim, and mempool data is
unconfirmed by nature.

## zingo-cli behaviour to know about

These cost time during setup and should be reported upstream:

- A wallet created with `--nosync` and synced in a later session stalls:
  it fetches subtree roots, then polls the tip every 10 s and never scans.
  Creating and syncing in the same session (`--birthday N --waitsync
  balance`) works.
- `--waitsync` doesn't return while the wallet has its own transaction in
  the mempool. On a chain mined by hand that means never. Sends are made
  with `--nosync` from a wallet synced in the previous command.
- The Ironwood coinbase scan failure described above.

## Reproducing

```sh
scripts/build-deps.sh     # or BIN=/dir/with/binaries
scripts/step1.sh
```

Needs ports 8137, 9168, 18080, 18230, 18232 and 18344 free on localhost.
About four minutes per run on a 2-core machine once everything is built.
