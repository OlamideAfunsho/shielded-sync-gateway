# Step 4: demonstrate protection and measure overhead

Goal from the plan:

- Compare honest, tampered and gateway runs, each starting from the same
  wallet state.
- Change ciphertexts or ephemeral keys while keeping commitments intact.
  Also test altered nullifiers and omitted transactions.
- Record what the wallet actually does.
- Measure network bytes, sync time and memory against direct sync.

Pass condition:

- honest payments and spends are recognised;
- targeted mutations are blocked before release;
- correcting the source lets the wallet recover.

Result: **pass**. See [results/step4/summary.json](../results/step4/summary.json)
and [results/bench/summary.md](../results/bench/summary.md).

## Setup

`scripts/step4.sh` builds one fresh chain:

- **Block 114:** the faucet pays the receiver 2.5 ZEC to its default
  unified address (lands in Ironwood) and 1.25 ZEC to a Sapling-only
  address, in one v6 transaction.
- **Block 117:** another copy of the receiver's wallet sends 1 ZEC back.
  Any other copy can only learn about this spend from the nullifier.
- **Tip:** 120.

Topology:

```
direct:   wallet -> tamper proxy -> zainod -> zebrad
gateway:  wallet -> ssg-gateway -> tamper proxy -> zainod -> zebrad   (compact blocks)
                               -> ssg-producer -> zebrad              (records)
                               -> zebrad                              (trusted headers, tree state)
```

The tamper proxy is `ssg-gateway proxy --tamper-file`. It decodes each
compact block, applies the rules, and re-encodes it. Rules are re-read on
every request, so emptying the file corrects the source.

Each run uses a new data directory restored from the same seed with
birthday 1. Each gateway run starts from an empty state database, so it has
to verify the altered block itself.

## What happened

Honest baseline: 1.4999 ZEC Ironwood + 1.25 ZEC Sapling. The gateway wallet
matched the direct wallet exactly.

### Ciphertext, block 114

The proxy flipped one bit in the 52-byte compact ciphertext of every output
and action in the block.

- **Direct:** balance 1.4999 ZEC Ironwood and 0 Sapling. The block-114
  payment is missing from the transaction list. The 1.4999 is still there
  because it is change from the block-117 spend, which the proxy left
  alone. **No error was reported.**
- **Gateway:** the wallet's sync failed with
  `DATA_LOSS: ssg-gateway: block 114 failed verification; refusing to serve
  it: transaction 1: compact block claims txid 24f3…, fields hash to
  c7a8…`. Verified height stayed 113 before and after the wallet's
  attempt.
- **Recovery:** after the rules were cleared, the follower verified 114 on
  its next pass. The same wallet then synced to a state identical to
  honest.

### Ephemeral key, block 114

Same outcome as the ciphertext case: 1.4999 ZEC instead of 2.7499 ZEC, no
error. The gateway refused block 114 and recovered.

### Nullifier, block 117

- **Direct:** 2.5 ZEC Ironwood + 1.25 ZEC Sapling. The spend at 117 never
  appears, so the wallet reports **1 ZEC more than it has**, with no error.
- **Gateway:** refused at 117, verified height held at 116, recovered.

### Omitted transactions, block 114

The proxy removed every compact transaction from the block, but left
`chainMetadata` alone.

- **Direct:** Zingo noticed the tree size didn't add up:
  `RESCAN TRIGGERED … at height 114 the wallet's history accounted for a
  Sapling commitment tree of 0 where the chain reports 2`. It ended the
  session with balance 0.
- **Gateway:** refused with `transaction 1 has shielded data but the
  compact block omits it`, and recovered.

So Zingo 7 already catches this one case, through the tree sizes the same
indexer reports. An indexer that also lowered `chainMetadata` would not
trip that check. The gateway's sizes come from verified blocks and the
trusted tree state.

### Lying producer, block 114

The producer flipped a bit in a memo digest in its record. This has no
direct-path equivalent. The gateway refused block 114, because the
rebuilt txid no longer matched, and recovered once the producer told the
truth.

## Failures never advance the verified height

In each case, `verified_height` was read from the gateway's status file
twice:

1. once the follower settled on the failure;
2. 3 seconds after the wallet's failed sync.

Both readings were one below the altered block (113 or 116). The store
only appends a block whose parent is the stored tip and which passed
verification. So nothing after a failed block can be stored either.

## Overhead

`scripts/bench.sh`: a 1,018-block regtest chain with 4 shielded
transactions, 3 fresh wallet syncs per path, medians. A recording proxy
sits in front of both the direct and gateway paths, so bytes are counted
the same way. Everything runs on one 2-core machine. There is no network
latency in these numbers, only CPU and protocol overhead.

| | direct | gateway, warm | gateway, cold |
|---|---:|---:|---:|
| wallet sync time (s) | 3.02 | 2.02 | 3.02 |
| bytes wallet received | 131,020 | 131,047 | 131,047 |
| bytes wallet sent | 1,453 | 1,453 | 1,453 |
| RPC calls | 51 | 51 | 51 |
| wallet peak RSS (kB) | 26,648 | 26,836 | 26,832 |

- **Bytes:** the only difference is `GetLightdInfo`, 27 bytes longer
  because the gateway names itself in `vendor`. Per method, the compact
  blocks the gateway builds came to exactly the same byte count as
  zainod's (90,687).
- **Sync time:** zingo-cli waits on whole-second polling, so the times are
  quantized. "Cold" includes the gateway verifying the whole chain while
  the wallet syncs. The 1-second gap between warm and direct is polling
  alignment, not something the gateway does better. The honest reading is
  that the gateway added no visible sync time at this chain size.

Gateway side, one pass from the checkpoint to the tip:

| | |
|---|---:|
| blocks verified | 1,018 |
| wall time to reach the tip | 1.29 s |
| verification CPU time | 0.40 s (0.39 ms/block) |
| compact blocks from the indexer (all pools) | 157,856 bytes |
| records from the producer | 98,013 bytes, 5.8% of full blocks (1,700,503 bytes) |
| JSON-RPC from the trusted node | 4,571,437 bytes |
| gateway peak RSS | 12,888 kB |

**Where the cost is:** most of it is the trusted-node traffic.
`getblockheader` in verbose JSON carries the Equihash solution as hex
(about 2.7 kB per block), and `z_gettreestate` is fetched for every block.
Fetching raw 1,487-byte headers and checking tree sizes only at batch
ends would cut most of this. We haven't done it yet.

Regtest blocks are tiny. Mainnet blocks carry far more shielded data, so
the record and verification numbers will grow with the number of
transactions. Measuring that needs a mainnet-synced setup, which is part
of the next phase.
