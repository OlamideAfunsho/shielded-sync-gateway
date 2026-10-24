# Step 3: connect verification to wallet sync

Goal from the plan:

- release only complete verified blocks
- derive or authenticate the tree metadata the wallet needs
- save state atomically
- recover after a restart
- roll back on a reorg
- document which RPCs are authenticated

Pass condition: failures never advance the verified height, and a short
reorg rolls back and rescans correctly.

Result: **pass**, 10 of 10 checks in
[results/step3/checks.json](../results/step3/checks.json). The failure half
of the pass condition is shown in step 4: in every tampered case the
verified height stayed one below the altered block.

## How the gateway is put together

`ssg-gateway serve` runs two things.

**The follower** (`crates/gateway/src/follower.rs`) loops every 500 ms:

1. Ask the trusted zebrad for its tip.
2. If the trusted chain no longer contains the stored tip, walk back to the
   last height where the stored hash equals the trusted hash. Delete
   everything above it in one SQLite transaction. A reorg below the
   checkpoint stops the follower.
3. For up to 200 heights above the stored tip, fetch:
   - from the trusted node: the header and tree state;
   - from the indexer: the compact blocks, all four pool types;
   - from the producer: the record, requested by height and trusted hash.
4. For each block in order:
   - require the trusted header's previous hash to equal the stored tip;
   - run `verify_block`;
   - check that the tree sizes after the block equal the trusted
     `z_gettreestate` sizes;
   - store the rebuilt compact block, with `chainMetadata` from those
     sizes, and its txids, in one SQLite transaction.
5. On the first failure, record the height and the reason, and stop.
   Nothing past that height is stored. The next pass tries again.

**The server** (`crates/gateway/src/server.rs`) implements
`CompactTxStreamer` with tonic.

- Block requests wait up to 30 s for the height to be verified.
- If verification has failed at or below the height, the request returns
  `DATA_LOSS` with the reason.
- `GetTreeState` serves the trusted `z_gettreestate`, but only at verified
  heights, and only if its hash and sizes match the verified chain.
- The rest is in [trust-boundaries.md](trust-boundaries.md).

### Transparent data had to be authenticated too

After a reorg, or when new blocks arrive during a session, Zingo asks for
`GetBlockRange` with the TRANSPARENT pool type. The first version of the
gateway refused those requests. The reorg test caught it: the wallet
failed with `Operation is not implemented`.

The record therefore carries the ZIP 244 sequence digest instead of the
whole transparent digest. The verifier rebuilds the prevouts and outputs
digests from the compact `vin` and `vout`, re-adding the coinbase null
input that compact blocks leave out. That way transparent compact fields
are authenticated by the same txid check.

### Checkpoint

`ssg-gateway checkpoint --height H` records:

- the network and height;
- the block hash;
- each pool's frontier, root and size from the trusted node.

On start, `serve` rebuilds the same thing from the trusted node and
refuses to run if anything differs. The state database stores the
checkpoint it was built from, and refuses a different one.

The scripts pin height 0, so wallets with birthday 1 work. Height 112
(non-empty Ironwood tree) was tried by hand with a wallet whose birthday
is 113: the gateway verified from 113 and the wallet received both
payments. That run is not part of a script.

## The checks

From `scripts/step3.sh`. Each "agree" check compares the wallet's own output
(balance, height, notes, transactions, value transfers), normalized as
described in `tools/normalize.py`.

| check | what happened in the recorded run |
|---|---|
| receive: gateway wallet matches direct | both 2.5 ZEC Ironwood + 1.25 ZEC Sapling at height 116 |
| restart: resumes from stored height | stopped at 116, mined 5, restarted; verified exactly 5 new blocks |
| restart: log shows resume | `resumed_at=116` |
| crash: state is a contiguous verified prefix | `kill -9` while verifying 300 new blocks; state held heights 1..147, txids only for stored blocks |
| crash: restarted gateway reaches tip | resumed at 147, reached 421 |
| crash: wallets agree afterwards | height 421 |
| reorg: wallets agree before | spend mined at 422 |
| reorg: gateway rolled back and re-verified | block 422 replaced on the trusted node; gateway rolled back 425→421 (4 blocks), verified to 427 |
| reorg: every stored hash is on the trusted best chain | 0 mismatches over every stored height |
| reorg: wallets agree after | both show the spend as failed (it was not re-mined) and the funds unspent |

The exact heights in `results/step3/` come from the last run. The crash
point changes from run to run (147 here; 129 and 205 in earlier runs).

## A comparison detail

For a transaction it marks failed or pending, zingo-cli prints the local
clock time at which that wallet copy noticed it. In the reorg check the two
copies synced 3 seconds apart, and that timestamp was the only difference.
`tools/normalize.py` blanks the time only for entries that are not
confirmed. Everything else is compared byte for byte.
