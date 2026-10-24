Chain: 1018 blocks. Full blocks total 1,700,503 bytes. 3 runs per path, medians shown.

| | direct | gateway (warm) | gateway (cold) |
|---|---:|---:|---:|
| wallet sync time (s) | 3.02 | 2.02 | 3.02 |
| bytes wallet received | 131,020 | 131,047 | 131,047 |
| bytes wallet sent | 1,453 | 1,453 | 1,453 |
| RPC calls | 51 | 51 | 51 |
| wallet peak RSS (kB) | 26,648 | 26,836 | 26,832 |

Gateway side, one verification pass from the checkpoint to the tip:

| | |
|---|---:|
| blocks verified | 1,018 |
| wall time to reach tip (incl. fetching) | 1.29 s |
| verification CPU time | 0.396 s (0.3888 ms/block) |
| compact blocks pulled from indexer | 157,856 bytes |
| records pulled from producer | 98,013 bytes (5.8% of full blocks) |
| JSON-RPC from trusted node | 4,571,437 bytes |
| gateway peak RSS | 12,888 kB |
