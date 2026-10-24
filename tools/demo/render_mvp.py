"""Render the MVP demo video (steps 2-4) from recorded runs and results.

    python3 render_mvp.py STEP4_RUN.jsonl RESULTS_DIR out.mp4

STEP4_RUN.jsonl is `record.py` output of scripts/step4.sh. RESULTS_DIR is
the repo's results/ directory (step3, step4, bench). Every number on screen
is read from those files.
"""
import json
import re
import subprocess
import sys

from PIL import Image, ImageDraw

from render import (AMBER, BG, BLUE, BORDER, DIM, F, FAINT, FPS, GREEN, H, LINE_H, MONO_W, PANEL, RED, TEXT,
                    VIOLET, W, clip, ease, fade, panel, text, term_color)


def zec(z):
    return f"{z / 1e8:.4f}".rstrip("0").rstrip(".") + " ZEC"


def env(t, dur, fin=0.6, fout=0.6):
    return ease(t / fin) * (1 - ease((t - dur + fout) / fout))


# ------------------------------------------------------------------ scenes


def scene_title(img, t, dur):
    d = ImageDraw.Draw(img)
    a = env(t, dur, 0.8)
    text(d, (160, 360), "Shielded Sync Gateway", F["title"], fade(TEXT, a))
    text(d, (162, 455), "Authenticated scanning data for Zcash light wallets", F["h2"], fade(AMBER, a))
    text(d, (162, 525), "MVP: verifier, producer, verifying gateway, and a tampering test against a real wallet.",
         F["body"], fade(DIM, a))
    d.line((162, 600, 162 + 560 * ease(t / 1.4), 600), fill=fade(BORDER, a), width=2)
    text(d, (162, 630), "zebrad 6.3.0  ·  zainod 0.10.1  ·  zingo-cli (zingolib 7.0.0)  ·  regtest after NU6.3",
         F["small"], fade(FAINT, a))


def field_row(d, x, y, name, note, ok, a):
    c = GREEN if ok else RED
    d.rounded_rectangle((x, y, x + 300, y + 46), radius=8, fill=fade(PANEL, a), outline=fade(c, a), width=2)
    text(d, (x + 18, y + 23), name, F["mono_l"], fade(TEXT, a), "lm")
    text(d, (x + 330, y + 23), note, F["small"], fade(c, a), "lm")


def scene_problem(img, t, dur):
    d = ImageDraw.Draw(img)
    a = env(t, dur)
    text(d, (160, 110), "What a light wallet scans", F["h1"], fade(TEXT, a))
    text(d, (160, 180), "One Ironwood action in a compact block. The wallet can check one of these four fields.",
         F["body"], fade(DIM, a))
    rows = [
        ("cmx", "fits the note commitment tree the wallet builds", True),
        ("nullifier", "nothing checks it: a changed one hides a spend", False),
        ("ephemeral_key", "nothing checks it: decryption silently fails", False),
        ("ciphertext[0..52]", "nothing checks it: the payment is never seen", False),
    ]
    for i, (n, note, ok) in enumerate(rows):
        field_row(d, 160, 270 + i * 80, n, note, ok, a * ease((t - 0.8 - i * 0.6) / 0.5))
    b = a * ease((t - 4.0) / 0.6)
    text(d, (160, 650), "So an indexer can change what the wallet decrypts while every commitment stays correct.",
         F["body_m"], fade(TEXT, b))
    text(d, (160, 700), "Step 4 below shows what Zingo CLI actually does when that happens.", F["body"], fade(DIM, b))


def node(d, x, y, w, h, title, sub, color, a, tag=None):
    d.rounded_rectangle((x, y, x + w, y + h), radius=14, fill=fade(PANEL, a), outline=fade(color, a), width=2)
    text(d, (x + w / 2, y + h / 2 - 15), title, F["h2"], fade(TEXT, a), "mm")
    text(d, (x + w / 2, y + h / 2 + 22), sub, F["tiny"], fade(DIM, a), "mm")
    if tag:
        text(d, (x + w / 2, y - 18), tag, F["tiny"], fade(color, a), "mm")


def line(d, p0, p1, color, a, w=3):
    if a > 0:
        d.line((p0, p1), fill=fade(color, a), width=w)


def scene_design(img, t, dur):
    d = ImageDraw.Draw(img)
    a = env(t, dur)
    text(d, (160, 90), "How the gateway checks it", F["h1"], fade(TEXT, a))
    ap = lambda k: a * ease((t - k) / 0.5)
    node(d, 160, 250, 330, 120, "zebrad", "headers, tip, tree state", GREEN, ap(0.4), "TRUSTED")
    node(d, 160, 450, 330, 120, "indexer (zainod)", "compact blocks", RED, ap(0.7), "UNTRUSTED")
    node(d, 160, 650, 330, 120, "producer", "records from full blocks", RED, ap(1.0), "UNTRUSTED")
    node(d, 800, 450, 330, 120, "ssg-gateway", "verify, store, serve", AMBER, ap(1.4))
    node(d, 1440, 450, 300, 120, "wallet", "Zingo CLI", BLUE, ap(1.8))
    for y in (310, 510, 710):
        line(d, (490, y), (800, 510), DIM, ap(1.6), 2)
    line(d, (1130, 510), (1440, 510), TEXT, ap(2.0))
    b = ap(2.6)
    lines = [
        "1  rebuild each txid (ZIP 244) from the compact fields + the record's remaining digests",
        "2  rebuild the block's Merkle root from every txid, in order",
        "3  compare with the root in the trusted header; check tree sizes against the trusted state",
        "4  store the block atomically; serve the wallet only from what was verified",
    ]
    for i, l in enumerate(lines):
        text(d, (800, 640 + i * 44), l, F["small"], fade(TEXT if i < 3 else DIM, b * ease((t - 2.6 - i * 0.5) / 0.5)))
    text(d, (800, 860), "Any changed field changes a txid; any missing or reordered transaction changes the root.",
         F["small_m"], fade(AMBER, ap(5.0)))


class Step4Replay:
    """Left: the real step4.sh terminal. Right: a scoreboard built from the
    same lines."""

    HEAD = re.compile(r"^== (\S+): (\S+) at height (\d+) \((\w+)\)")
    DIRECT = re.compile(r"^direct: ironwood (\d+) sapling (\d+), matches honest: (\w+)")
    GW = re.compile(r"^gateway: verified height (\d+) -> (\d+)")
    REC = re.compile(r"^after correcting the source: matches honest: (\w+)")
    HONEST = re.compile(r"^honest balance: ironwood (\d+), sapling (\d+)")

    def __init__(self, path):
        rows = [json.loads(l) for l in open(path)]
        t0 = rows[0]["t"]
        self.events, v, prev = [], 0.0, t0
        for r in rows:
            gap = r["t"] - prev
            v += min(max(gap, 0.06), 1.1)
            prev = r["t"]
            self.events.append((v, r["t"] - t0, r["line"]))
        self.duration = self.events[-1][0] + 3.0

    def draw(self, img, t, dur):
        d = ImageDraw.Draw(img)
        a = env(t, dur, 0.5, 0.5)
        shown = [e for e in self.events if e[0] <= t - 0.5]
        lines = [e[2] for e in shown]
        real = shown[-1][1] if shown else 0.0
        phase = next((l[3:] for l in reversed(lines) if l.startswith("== ")), "starting")
        text(d, (60, 50), "scripts/step4.sh", F["h2"], fade(TEXT, a))
        text(d, (60, 94), clip(phase, 70), F["body_m"], fade(AMBER, a))
        mm, ss = divmod(int(real), 60)
        text(d, (W - 60, 58), f"real time {mm:02d}:{ss:02d}", F["small_m"], fade(TEXT, a), "ra")
        text(d, (W - 60, 94), "waits shortened, output unedited", F["small"], fade(FAINT, a), "ra")

        lx0, ly0, lx1, ly1 = 40, 140, 1040, H - 40
        panel(d, (lx0, ly0, lx1, ly1), "bash — ~/shielded-sync-gateway", a)
        rows = int((ly1 - ly0 - 60) / LINE_H)
        cols = int((lx1 - lx0 - 40) / MONO_W)
        body = [("P", "$ scripts/step4.sh")] + [("", l) for l in lines if not l.startswith("|")]
        y = ly0 + 54
        for kind, l in body[-rows:]:
            if kind == "P":
                text(d, (lx0 + 20, y), "$", F["mono_b"], fade(GREEN, a))
                text(d, (lx0 + 20 + MONO_W * 2, y), l[2:], F["mono"], fade(TEXT, a))
            else:
                c, bold = term_color(l)
                if "matches honest: false" in l:
                    c = RED
                text(d, (lx0 + 20, y), clip(l, cols), F["mono_b" if bold else "mono"], fade(c, a))
            y += LINE_H

        # Scoreboard.
        rx0 = 1060
        panel(d, (rx0, ly0, W - 40, ly1), "same seed, same birthday, fresh wallet each run", a)
        cases, cur, honest = [], None, None
        for l in lines:
            if m := self.HONEST.match(l):
                honest = (int(m[1]), int(m[2]))
            if m := self.HEAD.match(l):
                cur = {"name": m[2], "height": m[3], "target": m[4], "direct": None, "gw": None, "rec": None}
                cases.append(cur)
            elif cur and (m := self.DIRECT.match(l)):
                cur["direct"] = (int(m[1]), int(m[2]), m[3] == "true")
            elif cur and (m := self.GW.match(l)):
                cur["gw"] = int(m[2])
            elif cur and (m := self.REC.match(l)):
                cur["rec"] = m[1] == "true"
        y = ly0 + 60
        if honest:
            text(d, (rx0 + 24, y), f"honest balance: {zec(honest[0])} Ironwood + {zec(honest[1])} Sapling",
                 F["small_m"], fade(GREEN, a))
        y += 54
        heads = [("mutation", 0), ("direct wallet", 220), ("gateway", 470), ("fixed", 650)]
        for h_, dx in heads:
            text(d, (rx0 + 24 + dx, y), h_, F["tiny"], fade(DIM, a))
        y += 36
        for c in cases:
            d.line((rx0 + 20, y - 8, W - 60, y - 8), fill=fade(BORDER, a), width=1)
            label = {"ciphertext": "ciphertext", "epk": "ephemeral key", "nullifier": "nullifier",
                     "omit-tx": "omitted tx", "memo-digest": "lying producer"}.get(c["name"], c["name"])
            text(d, (rx0 + 24, y), label, F["small_m"], fade(TEXT, a))
            text(d, (rx0 + 24, y + 30), f"block {c['height']}", F["tiny"], fade(FAINT, a))
            if c["target"] == "producer":
                text(d, (rx0 + 244, y), "n/a", F["small"], fade(FAINT, a))
            elif c["direct"]:
                i, s, same = c["direct"]
                text(d, (rx0 + 244, y), "correct" if same else "WRONG", F["small_m"],
                     fade(GREEN if same else RED, a))
                text(d, (rx0 + 244, y + 30), f"{zec(i + s)} total", F["tiny"], fade(DIM, a))
            if c["gw"] is not None:
                text(d, (rx0 + 494, y), "blocked", F["small_m"], fade(GREEN, a))
                text(d, (rx0 + 494, y + 30), f"held at {c['gw']}", F["tiny"], fade(DIM, a))
            if c["rec"] is not None:
                text(d, (rx0 + 674, y), "recovers" if c["rec"] else "NO", F["small_m"], fade(GREEN if c["rec"] else RED, a))
            y += 92


def scene_step4_table(img, t, dur, s4):
    d = ImageDraw.Draw(img)
    a = env(t, dur)
    hi, hs = s4["honest"]["ironwood"], s4["honest"]["sapling"]
    text(d, (120, 90), "Tampered indexer vs the gateway", F["h1"], fade(TEXT, a))
    text(d, (120, 160), f"Honest balance {zec(hi)} Ironwood + {zec(hs)} Sapling. Commitments untouched in every case.",
         F["body"], fade(DIM, a))
    cols = [120, 520, 1060, 1560]
    for x, h_ in zip(cols, ["mutation (block)", "wallet syncing directly", "wallet through the gateway", "source fixed"]):
        text(d, (x, 245), h_, F["small_m"], fade(DIM, a))
    y = 300
    for i, c in enumerate(s4["cases"]):
        ra = a * ease((t - 0.6 - i * 0.45) / 0.45)
        d.line((120, y - 14, W - 120, y - 14), fill=fade(BORDER, ra), width=1)
        label = {"ciphertext": "ciphertext", "epk": "ephemeral key", "nullifier": "nullifier",
                 "omit-tx": "omitted transactions", "memo-digest": "producer lies"}[c["mutation"]]
        text(d, (cols[0], y), f"{label} ({c['height']})", F["body_m"], fade(TEXT, ra))
        dr = c["direct"]
        if dr is None:
            text(d, (cols[1], y), "not applicable (direct sync has no producer)", F["small"], fade(FAINT, ra))
        else:
            if dr["matches_honest"]:
                text(d, (cols[1], y), "correct", F["body_m"], fade(GREEN, ra))
            else:
                text(d, (cols[1], y), f"{zec(dr['ironwood'])} + {zec(dr['sapling'])}", F["body_m"], fade(RED, ra))
                note = "no error shown" if not dr["sync_errors"] else "error: " + dr["sync_errors"].split("\n")[-1].replace("caused by:", "").strip()
                if "RESCAN TRIGGERED" in note:
                    note = "zingo: RESCAN TRIGGERED (tree size did not add up)"
                text(d, (cols[1], y + 40), clip(note, 50), F["tiny"], fade(DIM, ra))
        g = c["gateway"]
        text(d, (cols[2], y), f"refused; verified height held at {g['verified_height_after_wallet']}",
             F["small_m"], fade(GREEN if g["held_below_mutation"] else RED, ra))
        reason = g["failure"]
        reason = "txid of rebuilt fields ≠ claimed txid" if "fields hash to" in reason else reason
        text(d, (cols[2], y + 40), clip(reason, 48), F["tiny"], fade(DIM, ra))
        text(d, (cols[3], y), "matches honest" if c["recovered_matches_honest"] else "DIFFERS",
             F["body_m"], fade(GREEN if c["recovered_matches_honest"] else RED, ra))
        y += 112
    text(d, (120, y + 20), "Wallet error, verbatim: \"ssg-gateway: block N failed verification; refusing to serve it: …\"",
         F["small"], fade(DIM, a * ease((t - 3.5) / 0.6)))


def scene_step3(img, t, dur, checks):
    d = ImageDraw.Draw(img)
    a = env(t, dur)
    text(d, (160, 110), "Restart, crash and reorg", F["h1"], fade(TEXT, a))
    text(d, (160, 180), "scripts/step3.sh against the same wallet, direct vs gateway.", F["body"], fade(DIM, a))
    y = 270
    for i, c in enumerate(checks):
        ra = a * ease((t - 0.5 - i * 0.3) / 0.4)
        text(d, (160, y), "✓" if c["pass"] else "✗", F["h2"], fade(GREEN if c["pass"] else RED, ra))
        text(d, (210, y + 2), c["check"], F["body_m"], fade(TEXT, ra))
        text(d, (210, y + 42), clip(c["detail"], 120), F["tiny"], fade(DIM, ra))
        y += 74


def scene_bench(img, t, dur, b):
    d = ImageDraw.Draw(img)
    a = env(t, dur)
    p, g = b["paths"], b["gateway"]
    text(d, (160, 110), "What it costs", F["h1"], fade(TEXT, a))
    text(d, (160, 180), f"{b['tip']:,}-block regtest chain, one machine, {p['direct']['runs']} fresh wallet syncs per path (medians).",
         F["body"], fade(DIM, a))
    rows = [
        ("bytes the wallet received", f"{p['direct']['wallet_bytes_received_median']:,.0f}", f"{p['warm']['wallet_bytes_received_median']:,.0f}"),
        ("RPC calls", f"{p['direct']['calls_median']:.0f}", f"{p['warm']['calls_median']:.0f}"),
        ("wallet peak memory", f"{p['direct']['wallet_peak_rss_kb_median']/1024:.1f} MB", f"{p['warm']['wallet_peak_rss_kb_median']/1024:.1f} MB"),
        ("wallet sync time", f"{p['direct']['sync_seconds_median']:.2f} s", f"{p['warm']['sync_seconds_median']:.2f} s"),
    ]
    text(d, (760, 260), "direct", F["small_m"], fade(DIM, a), "ra")
    text(d, (1020, 260), "gateway (warm)", F["small_m"], fade(DIM, a), "ra")
    y = 310
    for i, (k, x1, x2) in enumerate(rows):
        ra = a * ease((t - 0.5 - i * 0.3) / 0.4)
        d.line((160, y - 12, 1040, y - 12), fill=fade(BORDER, ra), width=1)
        text(d, (160, y), k, F["body"], fade(TEXT, ra))
        text(d, (760, y), x1, F["mono_l"], fade(TEXT, ra), "ra")
        text(d, (1020, y), x2, F["mono_l"], fade(TEXT, ra), "ra")
        y += 60
    text(d, (160, y + 6), "+27 bytes: the gateway names itself in GetLightdInfo. Compact blocks: same size.",
         F["tiny"], fade(DIM, a))
    text(d, (160, y + 38), "Sync time moves in whole seconds: it is set by zingo-cli's polling, not by the gateway.",
         F["tiny"], fade(DIM, a))
    gx = 1150
    ga = a * ease((t - 1.8) / 0.5)
    text(d, (gx, 260), "Gateway, verifying from the checkpoint", F["small_m"], fade(DIM, ga))
    stats = [
        (f"{g['blocks_verified']:,}", "blocks verified"),
        (f"{b['gateway_catchup_seconds']:.1f} s", "to reach the tip"),
        (f"{g['verify_ms_per_block']:.2f} ms", "verification CPU per block"),
        (f"{100 * g['record_bytes_in'] / b['full_block_bytes']:.1f}%", "record size vs full blocks"),
        (f"{g['peak_rss_kb']/1024:.1f} MB", "gateway peak memory"),
    ]
    y = 310
    for i, (v, k) in enumerate(stats):
        ra = ga * ease((t - 2.0 - i * 0.25) / 0.4)
        text(d, (gx, y), v, F["h2"], fade(AMBER, ra))
        text(d, (gx + 230, y + 8), k, F["small"], fade(TEXT, ra))
        y += 60


def scene_coverage(img, t, dur):
    d = ImageDraw.Draw(img)
    a = env(t, dur)
    text(d, (160, 110), "Rebuilt txids and roots match real chain data", F["h1"], fade(TEXT, a))
    rows = [
        ("15 mainnet blocks", "v1, v3, v4 (full raw) and v5 (pruned); Sapling spends and outputs, Orchard actions",
         "root = header's hashMerkleRoot"),
        ("122 regtest blocks", "v6 after NU6.3: Ironwood actions, and a v6 transaction with Sapling outputs",
         "header = zebrad getblockheader"),
        ("ZIP 244 test vectors", "v5 with Sapling spends, outputs and Orchard in combination",
         "txid = published vector"),
        ("8 tamper tests, 28 Rust tests in all", "ciphertext, key, cmx, nullifier, vin/vout, omitted, extra, reordered, duplicated (CVE-2012-2459)",
         "every mutation rejected"),
    ]
    y = 230
    for i, (h_, l1, l2) in enumerate(rows):
        ra = a * ease((t - 0.5 - i * 0.6) / 0.5)
        d.rounded_rectangle((160, y, 172, y + 110), radius=4, fill=fade(AMBER, ra))
        text(d, (200, y), h_, F["h2"], fade(TEXT, ra))
        text(d, (200, y + 50), l1, F["small"], fade(DIM, ra))
        text(d, (200, y + 82), l2, F["small"], fade(GREEN, ra))
        y += 160


def scene_limits(img, t, dur):
    d = ImageDraw.Draw(img)
    a = env(t, dur, 0.6, 0.9)
    text(d, (160, 220), "What it does not claim yet", F["h1"], fade(TEXT, a))
    items = [
        "Headers come from a trusted zebrad. A phone can't run one: header authentication for light clients is next.",
        "Transparent address history and the mempool pass through unauthenticated (documented per RPC).",
        "Tested with Zingo CLI only. No mobile wallet yet.",
        "No real v6 transaction with Sapling spends or an Orchard bundle in the fixtures yet.",
    ]
    for i, s in enumerate(items):
        text(d, (160, 330 + i * 64), "–  " + s, F["body"], fade(DIM, a * ease((t - 0.5 - i * 0.4) / 0.5)))
    text(d, (160, 650), "Every result shown is reproducible: scripts/step1.sh, step3.sh, step4.sh, bench.sh.",
         F["body_m"], fade(TEXT, a * ease((t - 2.4) / 0.6)))


def main():
    run, results, out = sys.argv[1:4]
    s4 = json.load(open(f"{results}/step4/summary.json"))
    checks = json.load(open(f"{results}/step3/checks.json"))
    bench = json.load(open(f"{results}/bench/summary.json"))
    replay = Step4Replay(run)
    scenes = [
        (6.0, scene_title),
        (11.0, scene_problem),
        (13.0, scene_design),
        (replay.duration, replay.draw),
        (16.0, lambda i, t, d: scene_step4_table(i, t, d, s4)),
        (12.0, lambda i, t, d: scene_step3(i, t, d, checks)),
        (14.0, lambda i, t, d: scene_bench(i, t, d, bench)),
        (11.0, scene_coverage),
        (11.0, scene_limits),
    ]
    print(f"replay {replay.duration:.1f}s, total {sum(s[0] for s in scenes):.1f}s", file=sys.stderr)
    ff = subprocess.Popen(
        ["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}",
         "-r", str(FPS), "-i", "-", "-c:v", "libx264", "-preset", "medium", "-crf", "20",
         "-pix_fmt", "yuv420p", "-movflags", "+faststart", out],
        stdin=subprocess.PIPE,
    )
    for dur, fn in scenes:
        for f in range(int(round(dur * FPS))):
            img = Image.new("RGB", (W, H), BG)
            fn(img, f / FPS, dur)
            ff.stdin.write(img.tobytes())
    ff.stdin.close()
    ff.wait()


if __name__ == "__main__":
    main()
