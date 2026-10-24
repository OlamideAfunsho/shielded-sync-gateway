"""Run a command and record every output line with its wall-clock time.

    python3 record.py out.jsonl -- scripts/step1.sh
"""
import json, subprocess, sys, time

out = sys.argv[1]
cmd = sys.argv[sys.argv.index("--") + 1:]
with open(out, "w") as f:
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
    for line in p.stdout:
        f.write(json.dumps({"t": time.time(), "line": line.rstrip("\n")}) + "\n")
        f.flush()
        sys.stdout.write(line)
    sys.exit(p.wait())
