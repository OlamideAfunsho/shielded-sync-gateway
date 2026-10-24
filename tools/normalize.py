"""Normalize zingo-cli output for comparing two copies of one wallet.

Two things legitimately differ between copies that scanned the same chain:

* `notes all` lists notes in an internal map order, so notes are sorted;
* a transaction the wallet marks failed or pending carries the *local
  clock time* at which that copy noticed it, not a block time, so that
  time is blanked for entries that are not confirmed.

Everything else is compared byte for byte.

    python3 normalize.py FILE
"""
import json
import re
import sys

path = sys.argv[1]
text = open(path).read()

if path.endswith("notes.json"):
    try:
        data = json.loads(text)
    except ValueError:
        print(text, end="")
        sys.exit(0)
    for pool in data.values():
        notes = pool.get("note_summaries", [])
        for n in notes:
            if not str(n.get("status", "")).startswith("confirmed"):
                n["time"] = None
        notes.sort(key=lambda n: (n.get("txid", ""), n.get("output_index", 0)))
    print(json.dumps(data, indent=2, sort_keys=True))
elif path.endswith((".txt",)) and ("transactions" in path or "value_transfers" in path):
    # Entries are brace-delimited blocks at column 0.
    out, block = [], []
    for line in text.splitlines():
        block.append(line)
        if line == "}":
            if not any(re.match(r"\s*status: confirmed", l) for l in block):
                block = [re.sub(r"^(\s*datetime: ).*", r"\1<local clock>", l) for l in block]
            out.extend(block)
            block = []
    out.extend(block)
    print("\n".join(out))
else:
    print(text, end="")
