# Step 1 repeat runs

Each run starts from an empty chain and empty wallets (scripts/step1.sh).

- run 1: PASS: gateway sync matches direct sync (receive, spend detection, send)
- run 2: PASS: gateway sync matches direct sync (receive, spend detection, send)
- run 3: PASS: gateway sync matches direct sync (receive, spend detection, send)
- run 4: PASS: gateway sync matches direct sync (receive, spend detection, send)
- run 5 (binaries from a clean scripts/build-deps.sh): PASS: gateway sync matches direct sync (receive, spend detection, send)
- run 6 (this directory, final verification pass after steps 2-4 landed): PASS: gateway sync matches direct sync (receive, spend detection, send)
