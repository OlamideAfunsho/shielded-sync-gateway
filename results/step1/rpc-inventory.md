| method | calls | ok | cancelled by wallet | request bytes | response bytes | response msgs | time (ms) |
|---|---:|---:|---:|---:|---:|---:|---:|
| `GetBlockRange` | 4 | 4 | 0 | 72 | 11844 | 122 | 136.3 |
| `GetLatestBlock` | 3 | 3 | 0 | 15 | 123 | 3 | 0.9 |
| `GetLightdInfo` | 3 | 3 | 0 | 15 | 492 | 3 | 5.2 |
| `GetMempoolStream` | 3 | 0 | 3 | 15 | 0 | 0 | 3906.4 |
| `GetSubtreeRoots` | 9 | 9 | 0 | 57 | 0 | 0 | 131.6 |
| `GetTaddressTxids` | 63 | 63 | 0 | 3276 | 0 | 0 | 2713.9 |
| `GetTransaction` | 3 | 3 | 0 | 117 | 29496 | 3 | 102.2 |
| `GetTreeState` | 3 | 3 | 0 | 21 | 1729 | 3 | 147.3 |
| `SendTransaction` | 1 | 1 | 0 | 9176 | 71 | 1 | 186.8 |
| **total** | 92 | | | 12764 | 43755 | | |

Block requests: 1..=10 (10 blocks), 11..=116 (106 blocks), 117..=119 (3 blocks), 120..=122 (3 blocks)
Non-consecutive heights inside a range: 0

| compact field | times present |
|---|---:|
| `block.chain_metadata` | 122 |
| `block.hash` | 122 |
| `block.prev_hash` | 122 |
| `block.time` | 122 |
| `ironwood_action.ciphertext` | 8 |
| `ironwood_action.cmx` | 8 |
| `ironwood_action.ephemeral_key` | 8 |
| `ironwood_action.nullifier` | 8 |
| `sapling_output.ciphertext` | 2 |
| `sapling_output.cmu` | 2 |
| `sapling_output.ephemeral_key` | 2 |
| `tx.txid` | 4 |

Call sequence:

| # | method | request | response |
|---|---|---|---|
| 1 | `GetLightdInfo` | `{}` | `{"block_height":116,"chain_name":"test","consensus_branch_id":"37a5165b","estimated_height":6614963,"lightwallet_protocol_version":"v0.5.0","sapling_activation_height":1,"taddr_support":true,"upgrade_height":1,"upgrade_name":"NU6.3","vendor":"ZingoLabs ZainoD","version":"0.10.1"}` |
| 2 | `GetLatestBlock` | `{}` | `{"hash":"5a6735e631896cd06e9dd4f88d4adf6cdf46fb9d8a88ea967b5a5cf301aa90c3","height":116}` |
| 3 | `GetMempoolStream` | `{}` | 0 messages (stream closed by wallet) |
| 4 | `GetTreeState` | `{"block":116}` | `{"hash":"5a6735e631896cd06e9dd4f88d4adf6cdf46fb9d8a88ea967b5a5cf301aa90c3","height":116,"ironwood_tree_hex_len":260,"network":"test","orchard_tree_hex_len":6,"sapling_tree_hex_len":196,"time":1296688624}` |
| 5–25 (×21) | `GetTaddressTxids` | `{"address":"tmFLszfkjgim4zoUMAXpuohnFBAKy99rr2i","end":116,"start":1} … (first shown)` | 0 messages |
| 26–28 (×3) | `GetSubtreeRoots` | `{"max_entries":0,"protocol":"sapling","start_index":0} … (first shown)` | 0 messages |
| 29 | `GetBlockRange` | `{"end":10,"pool_types":["SAPLING","ORCHARD","IRONWOOD"],"start":1}` | 10 blocks 1..=10, 0 txs; sapling 0 spends / 0 outputs; orchard 0 actions; ironwood 0 actions |
| 30 | `GetBlockRange` | `{"end":116,"pool_types":["SAPLING","ORCHARD","IRONWOOD"],"start":11}` | 106 blocks 11..=116, 2 txs; sapling 0 spends / 2 outputs; orchard 0 actions; ironwood 4 actions |
| 31 | `GetTransaction` | `{"block":null,"index":0,"txid":"d265f780e10754a138ed581266cf9421318f479ec21f23309b21dedf1be1fbe4"}` | `{"height":114,"tx_bytes":11134,"tx_version":6,"version_group_id":"d884b698"}` |
| 32 | `GetLightdInfo` | `{}` | `{"block_height":119,"chain_name":"test","consensus_branch_id":"37a5165b","estimated_height":6614967,"lightwallet_protocol_version":"v0.5.0","sapling_activation_height":1,"taddr_support":true,"upgrade_height":1,"upgrade_name":"NU6.3","vendor":"ZingoLabs ZainoD","version":"0.10.1"}` |
| 33 | `GetLatestBlock` | `{}` | `{"hash":"f8c439607be29d01b643d263912a3a996e39f95f6c141e7e97bae6975604cbfc","height":119}` |
| 34 | `GetTreeState` | `{"block":119}` | `{"hash":"f8c439607be29d01b643d263912a3a996e39f95f6c141e7e97bae6975604cbfc","height":119,"ironwood_tree_hex_len":260,"network":"test","orchard_tree_hex_len":6,"sapling_tree_hex_len":196,"time":1296688624}` |
| 35 | `GetMempoolStream` | `{}` | 0 messages (stream closed by wallet) |
| 36–56 (×21) | `GetTaddressTxids` | `{"address":"tmFLszfkjgim4zoUMAXpuohnFBAKy99rr2i","end":119,"start":17} … (first shown)` | 0 messages |
| 57–59 (×3) | `GetSubtreeRoots` | `{"max_entries":0,"protocol":"sapling","start_index":0} … (first shown)` | 0 messages |
| 60 | `GetBlockRange` | `{"end":119,"pool_types":["SAPLING","ORCHARD","IRONWOOD"],"start":117}` | 3 blocks 117..=119, 1 txs; sapling 0 spends / 0 outputs; orchard 0 actions; ironwood 2 actions |
| 61 | `GetTransaction` | `{"block":null,"index":0,"txid":"329ccb1008616fe72529d119f7e69565b8e96001885ad3b0808ff4b703209822"}` | `{"height":117,"tx_bytes":9166,"tx_version":6,"version_group_id":"d884b698"}` |
| 62 | `SendTransaction` | `{"tx_bytes":9166}` | `{"error_code":0,"error_message":"f133e394771c82ad3a0f20f0a49ccff8c38d54bc086da61a45bbf7219d4e2033"}` |
| 63 | `GetLightdInfo` | `{}` | `{"block_height":122,"chain_name":"test","consensus_branch_id":"37a5165b","estimated_height":6614970,"lightwallet_protocol_version":"v0.5.0","sapling_activation_height":1,"taddr_support":true,"upgrade_height":1,"upgrade_name":"NU6.3","vendor":"ZingoLabs ZainoD","version":"0.10.1"}` |
| 64 | `GetLatestBlock` | `{}` | `{"hash":"3bd55ef5217f99c48a66dfdc010b5b799cc0e950cf34cac5fafdb1c90382fbc1","height":122}` |
| 65 | `GetMempoolStream` | `{}` | 0 messages (stream closed by wallet) |
| 66 | `GetTreeState` | `{"block":122}` | `{"hash":"3bd55ef5217f99c48a66dfdc010b5b799cc0e950cf34cac5fafdb1c90382fbc1","height":122,"ironwood_tree_hex_len":324,"network":"test","orchard_tree_hex_len":6,"sapling_tree_hex_len":196,"time":1296688625}` |
| 67–87 (×21) | `GetTaddressTxids` | `{"address":"tmFLszfkjgim4zoUMAXpuohnFBAKy99rr2i","end":122,"start":20} … (first shown)` | 0 messages |
| 88–90 (×3) | `GetSubtreeRoots` | `{"max_entries":0,"protocol":"sapling","start_index":0} … (first shown)` | 0 messages |
| 91 | `GetBlockRange` | `{"end":122,"pool_types":["SAPLING","ORCHARD","IRONWOOD"],"start":120}` | 3 blocks 120..=122, 1 txs; sapling 0 spends / 0 outputs; orchard 0 actions; ironwood 2 actions |
| 92 | `GetTransaction` | `{"block":null,"index":0,"txid":"f133e394771c82ad3a0f20f0a49ccff8c38d54bc086da61a45bbf7219d4e2033"}` | `{"height":120,"tx_bytes":9166,"tx_version":6,"version_group_id":"d884b698"}` |
