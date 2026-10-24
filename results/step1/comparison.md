# Step 1 comparison

### receive-direct vs receive-gateway

- `balance.txt`: identical (450 bytes)
- `height.json`: identical (20 bytes)
- `notes.json`: identical (898 bytes)
- `transactions.txt`: identical (766 bytes)
- `value_transfers.txt`: identical (779 bytes)
- sync time: 2.03s direct, 2.03s through gateway

### spend-direct vs spend-gateway

- `balance.txt`: identical (450 bytes)
- `height.json`: identical (20 bytes)
- `notes.json`: same notes, listed in a different order
- `transactions.txt`: identical (1892 bytes)
- `value_transfers.txt`: identical (1267 bytes)
- sync time: 2.02s direct, 2.02s through gateway

### send-direct vs send-gateway

- `balance.txt`: identical (448 bytes)
- `height.json`: identical (20 bytes)
- `notes.json`: identical (1739 bytes)
- `transactions.txt`: identical (3025 bytes)
- `value_transfers.txt`: identical (1764 bytes)
- sync time: 2.02s direct, 2.02s through gateway

