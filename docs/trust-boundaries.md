# Trust boundaries

## Components

```
                      trusted                                  untrusted
                ┌──────────────────┐            ┌──────────────────────────────────┐
                │ zebrad (validator)│            │ indexer (zainod)   producer       │
                │ headers, tip,     │            │ compact blocks     records from   │
                │ tree states,      │            │                    full blocks    │
                │ subtree roots     │            │                                   │
                └─────────┬────────┘            └───────┬───────────────┬──────────┘
                          │ JSON-RPC                     │ gRPC          │ HTTP
                          ▼                              ▼               ▼
                ┌───────────────────────────────────────────────────────────────────┐
                │ ssg-gateway: checkpoint, verifier, SQLite verified chain           │
                └───────────────────────────────┬───────────────────────────────────┘
                                                │ gRPC (CompactTxStreamer)
                                                ▼
                                          light wallet
```

- **Trusted:** the zebrad named by `--trusted`, the pinned checkpoint, and
  the gateway process.
- **Untrusted:** the indexer, the producer, and the producer's own source
  node.

In the scripts all three nodes run on one regtest zebrad, reached over
separate connections. That keeps the demos small. In a deployment, the
trusted validator must be a different node from anything the indexer or
producer controls, and it should be run by the wallet operator. Pointing
`--trusted` and the producer's `--source` at different nodes is only a
configuration change.

## The claim

> Every compact block, tree state, subtree root and chain tip the gateway
> serves is the one the trusted validator's best chain contains, from the
> checkpoint onward.

The check is in `docs/proof-spec.md`. It holds as long as BLAKE2b and
SHA-256d resist collisions and the trusted node is honest.

## Per RPC

| RPC | what the wallet gets | authenticated? |
|---|---|---|
| `GetBlockRange`, `GetBlock` | compact blocks rebuilt from verified fields: shielded data, transparent `vin`/`vout`, `chainMetadata` | yes |
| `GetBlockRangeNullifiers`, `GetBlockNullifiers` | same blocks, nullifiers only | yes |
| `GetLatestBlock` | trusted tip | from the trusted node |
| `GetTreeState`, `GetLatestTreeState` | trusted `z_gettreestate`, served only at verified heights, cross-checked against the verified chain's hash and tree sizes | yes |
| `GetSubtreeRoots` | trusted `z_getsubtreesbyindex`, limited to subtrees completed at or below the verified height | from the trusted node |
| `GetTransaction` | indexer bytes, accepted only if they hash to the requested txid and that txid is in a verified block; height replaced with the verified height | yes for mined transactions; mempool transactions (height 0) pass through |
| `GetLightdInfo` | indexer response; chain name must match the trusted node; heights replaced with the trusted tip | partly |
| `SendTransaction` | forwarded | not applicable |
| `GetTaddressTxids`, `GetTaddressTransactions`, `GetTaddressBalance*`, `GetAddressUtxos*` | forwarded | **no** (transparent address history is outside the MVP) |
| `GetMempoolTx`, `GetMempoolStream` | forwarded | **no** (unconfirmed data) |
| `Ping` | forwarded | not applicable |

## Failure behaviour

- A block that fails verification is never stored. The verified height
  stays below it. Every request that needs that block or anything after
  it gets gRPC `DATA_LOSS` with the reason, for example
  `block 114 failed verification; refusing to serve it: transaction 1:
  compact block claims txid …, fields hash to …`.
- The follower retries every 500 ms. Once the indexer or producer serves
  correct data, the block verifies and the error clears. No restart is
  needed.
- If the trusted chain no longer contains the stored tip, the gateway
  rolls back to the last common block before doing anything else, in one
  SQLite transaction, and verifies forward again. A reorg below the
  checkpoint stops the follower: the checkpoint is pinned.
- Every block is stored in its own SQLite transaction. After a crash the
  state file holds a contiguous verified prefix, and the gateway resumes
  from it.

## What this MVP does not do

- **Header authentication without a trusted node.** A phone wallet can't
  run zebrad. The production path needs a header route light clients can
  check themselves: FlyClient, or checkpointed PoW header chains. That is
  out of scope here, as the plan says.
- **Mobile wallets.** Only Zingo CLI has been tested.
- **Privacy.** The gateway sees what the wallet asks for. Run it locally
  or with an operator you trust with that.
- **`fee` in compact transactions.** It is not committed by the txid and
  is set to 0.
