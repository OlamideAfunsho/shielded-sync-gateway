# Verification record and checking rules

This is what the gateway checks before it gives a compact block to a wallet.
The code is in `crates/verifier`. The record format is
`crates/lwproto/proto/ssg/record.proto`.

## Inputs per block

| input | source | trusted? |
|---|---|---|
| header: height, hash, previous hash, transaction Merkle root, time | zebrad `getblockheader` | yes |
| tree sizes after the block (Sapling, Orchard, Ironwood) | zebrad `z_gettreestate` | yes |
| compact block, all four pool types | indexer `GetBlockRange` | no |
| verification record | producer `/v1/record/<height>?hash=` | no |

## The record

A `BlockRecord` lists every transaction in the block in block order. Each
entry is one of two forms.

**`raw`** is the full serialized transaction. v1 to v4 transactions must use
this form, because their txid is SHA-256d of the whole encoding. Any version
may use it. The verifier parses it with `zcash_primitives` 0.30.0, rejects
trailing bytes, takes the txid from the parse, and checks the compact fields
field by field against the parsed transaction.

**`pruned`** is for v5 and v6 only. It carries every part of the ZIP 244
txid digest tree that the compact block does not:

| record field | digest node it feeds |
|---|---|
| `header`, `version_group_id`, `consensus_branch_id`, `lock_time`, `expiry_height` | T.1 header digest |
| `sequence_digest` (empty = no transparent bundle) | T.2b |
| Sapling `spends_noncompact_digest` | T.3a non-compact part (cv, [anchor in v5], rk) |
| Sapling `outputs_memos_digest`, `outputs_noncompact_digest` | T.3b memo and non-compact parts |
| Sapling `value_balance` | T.3 |
| Orchard / Ironwood `memos_digest`, `noncompact_digest`, `flags`, `value_balance` | T.4 |
| Orchard `anchor` (v5 only) | T.4 |

The compact block supplies the rest:

| compact field | digest node |
|---|---|
| `vin` prevouts (coinbase null input re-added at position 0), `vout` value + script | T.2a prevouts, T.2c outputs |
| Sapling spend `nf` | T.3a compact part |
| Sapling output `cmu`, `ephemeralKey`, `ciphertext[0..52]` | T.3b compact part |
| Orchard / Ironwood action `nullifier`, `cmx`, `ephemeralKey`, `ciphertext[0..52]` | T.4 compact part |

Personalization strings follow `zcash_primitives` 0.30.0 and `orchard`
0.15.3:

- v6 Sapling non-compact spends: `ZTxIdSSpendNH_v6`, anchor dropped.
- v6 Orchard: `ZTxIdOrchardH_v6`, anchor dropped. The action-level strings
  are the same as v5.
- Ironwood: `ZTxIdIronwd_H_v6`, `ZTxIdIrnActCH_v6`, `ZTxIdIrnActMH_v6`,
  `ZTxIdIrnActNH_v6`.
- The v6 txid hashes a fifth node, the Ironwood digest. An absent bundle
  uses the protocol's empty digest for its pool.

## Checks, in order

1. The record's height and block hash equal the trusted header's.
2. The compact block's height, hash and previous hash equal the trusted
   header's. A mismatch usually means the indexer is on another fork.
3. Compact transaction indices are strictly increasing and inside the
   record's transaction count.
4. For each record entry `i`:
   - `pruned` with a Sapling, Orchard or Ironwood bundle, but no compact
     transaction at index `i`: **reject** (omitted transaction).
   - The field counts in the compact transaction must equal the counts in
     the record. Field lengths must be exact: 32 bytes for keys and
     nullifiers, 52 for ciphertext.
   - Recompute the txid. If the compact transaction claims a different
     txid: **reject**.
5. No txid may appear twice.
6. Rebuild the Merkle root from all txids. Any level with two equal nodes
   forming a pair is **rejected**. This is the CVE-2012-2459 ambiguity,
   where `[a,b,c]` and `[a,b,c,c]` share a root.
7. The root must equal the trusted header's `hashMerkleRoot`.
8. Tree sizes: the previous verified sizes plus this block's Sapling
   outputs, Orchard actions and Ironwood actions must equal the sizes in
   the trusted tree state at this height.

Only then is the block stored. The stored compact block is rebuilt from
values that were checked: header fields from the trusted header, fields
from the compact block that hashed correctly, and `chainMetadata` from the
running tree sizes. The `fee` field is not authenticated and is set to 0.

## Why this is enough

Step 7 makes the list of txids exactly the block's list. A collision in
SHA-256d would break that. Each txid is a BLAKE2b tree over every
consensus-relevant field of its transaction. So each compact field that
feeds a txid is fixed by the header, unless BLAKE2b breaks. A changed
ciphertext, ephemeral key, commitment, nullifier, transparent input or
output changes the txid. A missing, extra, duplicated or reordered
transaction changes the root.

A producer cannot help an indexer lie either. The record supplies hash
inputs, never results. A wrong record gives a wrong txid and a wrong root,
the same as a wrong compact block.

## What the check does not cover

- Memo plaintexts and outgoing data. These come through `GetTransaction`,
  which the gateway authenticates separately: it parses the returned
  transaction, recomputes its txid, and requires it to equal the requested
  txid and to sit in a verified block.
- The `fee` field.
- Whether the header itself is honest. That is the trusted node's job in
  this MVP. Moving it to light-client header verification is the next
  phase.

## Fixtures

`crates/verifier/tests/fixtures.rs` checks every reconstructed txid against
`Transaction::txid()` from `zcash_primitives`, and every root against a
real header:

| fixture | transactions | covers |
|---|---|---|
| 15 mainnet blocks from Zebra's test vectors | v1 ×1, v3 ×1, v4 ×39 (raw); v5 ×33 (pruned) | 32 Sapling outputs, 19 Sapling spends, 6 Orchard actions in 3 v5 transactions |
| 122-block regtest chain from zebrad 6.3.0 after NU6.3 | v6 ×126 (pruned) | 8 Ironwood actions; 1 v6 transaction with 2 Sapling outputs; headers checked against `getblockheader` |
| ZIP 244 test vectors (zcash-test-vectors via `zcash_primitives`) | v5 | Sapling spends, Sapling outputs and Orchard in various combinations |

Gaps in the fixtures:

- No v6 transaction with Sapling *spends*.
- No v6 transaction with an Orchard bundle (as opposed to Ironwood).

The code paths are the same as for v5, plus the v6 personalizations listed
above. They are not yet corroborated by a real v6 transaction.
