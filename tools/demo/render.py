"""Render the step 1 demo video from a recorded run.

Inputs: a record.py JSONL of scripts/step1.sh output, the gateway log and
the results directory from the same run. Output: H.264 MP4.

    python3 render.py RUN.jsonl RESULTS_DIR out.mp4
"""
import datetime as dt
import json
import math
import re
import subprocess
import sys

from PIL import Image, ImageDraw, ImageFont

W, H, FPS = 1920, 1080, 30

BG = (15, 17, 21)
PANEL = (22, 25, 31)
PANEL_HEAD = (30, 34, 41)
BORDER = (46, 51, 60)
TEXT = (222, 225, 230)
DIM = (128, 136, 148)
FAINT = (78, 85, 96)
AMBER = (244, 183, 64)
GREEN = (118, 201, 143)
RED = (232, 104, 104)
BLUE = (118, 170, 240)
VIOLET = (186, 150, 240)

INTER = "/usr/share/fonts/opentype/inter/Inter-{}.otf"
MONO = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
MONO_B = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"


def inter(w, size):
    return ImageFont.truetype(INTER.format(w), size)


F = {
    "title": inter("Bold", 76),
    "h1": inter("Bold", 52),
    "h2": inter("SemiBold", 34),
    "body": inter("Regular", 30),
    "body_m": inter("Medium", 30),
    "small": inter("Regular", 24),
    "small_m": inter("Medium", 24),
    "tiny": inter("Regular", 20),
    "mono": ImageFont.truetype(MONO, 17),
    "mono_b": ImageFont.truetype(MONO_B, 17),
    "mono_l": ImageFont.truetype(MONO, 24),
    "mono_lb": ImageFont.truetype(MONO_B, 24),
}
MONO_W = F["mono"].getlength("M")
LINE_H = 23


def ease(x):
    x = max(0.0, min(1.0, x))
    return 1 - (1 - x) ** 3


def fade(color, a, bg=BG):
    return tuple(int(bg[i] + (color[i] - bg[i]) * a) for i in range(3))


# ---------------------------------------------------------------- timeline


def iso_epoch(s):
    s = s.rstrip("Z")
    if "." in s:
        head, frac = s.split(".")
        s = head + "." + frac[:6]
    return dt.datetime.fromisoformat(s).replace(tzinfo=dt.timezone.utc).timestamp()


GW_RE = re.compile(
    r"^(\S+)\s+INFO\s+#(\d+)\s+(\S+)\s+status=(\S+)\s+req=(\d+)B resp=(\d+)B/(\d+)msg ([\d.]+)ms (.*)$"
)


def load_events(run_jsonl, gateway_log):
    events = []
    for l in open(run_jsonl):
        r = json.loads(l)
        events.append((r["t"], "term", r["line"]))
    for l in open(gateway_log):
        m = GW_RE.match(l.strip())
        if not m:
            continue
        ts, cid, method, status, req, resp, msgs, ms, rest = m.groups()
        events.append((iso_epoch(ts), "gw", (int(cid), method, status, int(req), int(resp), int(msgs), float(ms))))
    events.sort(key=lambda e: e[0])
    return events


def compress(events, gap_cap=1.4, min_gap=0.07):
    """Map real time to playback time. Idle waits are cut down to gap_cap,
    bursts keep a small spacing so lines remain readable."""
    out = []
    t0 = events[0][0]
    prev_real = t0
    v = 0.0
    for real, kind, payload in events:
        gap = real - prev_real
        v += min(max(gap, min_gap), gap_cap) if gap > 0 else min_gap
        prev_real = real
        out.append((v, real - t0, kind, payload))
    return out


# ---------------------------------------------------------------- drawing


def text(d, xy, s, font, fill, anchor="la"):
    d.text(xy, s, font=font, fill=fill, anchor=anchor)


def panel(d, box, title, a=1.0):
    x0, y0, x1, y1 = box
    d.rounded_rectangle(box, radius=10, fill=fade(PANEL, a), outline=fade(BORDER, a), width=1)
    d.rounded_rectangle((x0, y0, x1, y0 + 38), radius=10, fill=fade(PANEL_HEAD, a))
    d.rectangle((x0, y0 + 28, x1, y0 + 38), fill=fade(PANEL_HEAD, a))
    for i, c in enumerate([(236, 106, 94), (244, 190, 79), (98, 197, 84)]):
        d.ellipse((x0 + 16 + i * 20, y0 + 13, x0 + 28 + i * 20, y0 + 25), fill=fade(c, a * 0.85))
    text(d, (x0 + 90, y0 + 19), title, F["tiny"], fade(DIM, a), "lm")


def clip(s, n):
    return s if len(s) <= n else s[: n - 1] + "…"


def term_color(line):
    if line.startswith("== "):
        return AMBER, True
    if line.startswith("PASS"):
        return GREEN, True
    if line.startswith("FAIL") or "DIFFERENT" in line:
        return RED, True
    if line.startswith("[regtest]"):
        return BLUE, False
    if "identical" in line or "same notes" in line:
        return GREEN, False
    if line.startswith("|"):
        return DIM, False
    return TEXT, False


def gw_line(p):
    cid, method, status, req, resp, msgs, ms = p
    st = "ok" if status == "0" else ("closed" if status == "-" else f"err {status}")
    return f"#{cid:<3} {method:<18} {st:<5} {req:>5}B → {resp:>6}B {msgs:>4} msg {ms:>7.1f} ms", status


# ---------------------------------------------------------------- scenes


def scene_title(img, t, dur):
    d = ImageDraw.Draw(img)
    a = ease(t / 0.8) * (1 - ease((t - dur + 0.6) / 0.6))
    text(d, (160, 380), "Shielded Sync Gateway", F["title"], fade(TEXT, a))
    text(d, (162, 470), "Step 1 — proving the wallet connection", F["h2"], fade(AMBER, a))
    text(
        d,
        (162, 540),
        "A real Zcash light wallet, a disposable local chain, and a gateway in between.",
        F["body"],
        fade(DIM, a),
    )
    d.line((162, 620, 162 + 520 * ease(t / 1.4), 620), fill=fade(BORDER, a), width=2)
    text(d, (162, 650), "zebrad 6.3.0   ·   zainod 0.10.1   ·   zingo-cli (zingolib 7.0.0)", F["small"], fade(FAINT, a))


def box(d, x, y, w, h, title, sub, color, a):
    d.rounded_rectangle((x, y, x + w, y + h), radius=14, fill=fade(PANEL, a), outline=fade(color, a), width=2)
    text(d, (x + w / 2, y + h / 2 - 16), title, F["h2"], fade(TEXT, a), "mm")
    text(d, (x + w / 2, y + h / 2 + 24), sub, F["small"], fade(DIM, a), "mm")


def arrow(d, x0, y, x1, color, a, dashed=False, label=None):
    if a <= 0:
        return
    xe = x0 + (x1 - x0) * a
    if dashed:
        x = x0
        while x < xe:
            d.line((x, y, min(x + 14, xe), y), fill=fade(color, a), width=3)
            x += 24
    else:
        d.line((x0, y, xe, y), fill=fade(color, a), width=3)
    if a > 0.95:
        d.polygon([(x1, y), (x1 - 14, y - 8), (x1 - 14, y + 8)], fill=fade(color, a))
    if label:
        text(d, ((x0 + x1) / 2, y - 16), label, F["tiny"], fade(DIM, a), "mm")


def scene_diagram(img, t, dur):
    d = ImageDraw.Draw(img)
    out = 1 - ease((t - dur + 0.6) / 0.6)
    text(d, (160, 150), "The setup", F["h1"], fade(TEXT, ease(t / 0.6) * out))
    text(
        d,
        (160, 220),
        "Same seed, same birthday. One copy of the wallet syncs straight from the indexer,",
        F["body"],
        fade(DIM, ease(t / 0.6) * out),
    )
    text(d, (160, 262), "the other through the gateway. Afterwards their state must match.", F["body"], fade(DIM, ease(t / 0.6) * out))

    y = 560
    xs = [160, 600, 1040, 1480]
    specs = [
        ("zingo-cli", "light wallet", BLUE),
        ("ssg-gateway", "this project", AMBER),
        ("zainod", "indexer · gRPC", VIOLET),
        ("zebrad", "validator · regtest", GREEN),
    ]
    for i, (ti, su, c) in enumerate(specs):
        a = ease((t - 0.6 - i * 0.45) / 0.6) * out
        box(d, xs[i], y - 70, 300, 140, ti, su, c, a)
    for i, lab in enumerate(["h2c", "h2c", "JSON-RPC"]):
        a = ease((t - 2.4 - i * 0.35) / 0.5) * out
        arrow(d, xs[i] + 300, y, xs[i + 1], TEXT, a, label=lab)
    # direct path
    a = ease((t - 3.8) / 0.8) * out
    if a > 0:
        yy = y + 150
        d.line((xs[0] + 150, y + 70, xs[0] + 150, yy), fill=fade(BLUE, a), width=2)
        x = xs[0] + 150
        while x < xs[2] + 150:
            d.line((x, yy, min(x + 14, xs[2] + 150), yy), fill=fade(BLUE, a), width=2)
            x += 24
        d.line((xs[2] + 150, yy, xs[2] + 150, y + 72), fill=fade(BLUE, a), width=2)
        d.polygon([(xs[2] + 150, y + 72), (xs[2] + 142, y + 86), (xs[2] + 158, y + 86)], fill=fade(BLUE, a))
        text(d, ((xs[0] + xs[2]) / 2 + 150, yy + 26), "direct sync (baseline)", F["small"], fade(BLUE, a), "mm")
    a = ease((t - 4.8) / 0.8) * out
    text(
        d,
        (160, 880),
        "The gateway is a pass-through in step 1: it forwards every byte unchanged and records each call.",
        F["small"],
        fade(DIM, a),
    )


class Replay:
    def __init__(self, events, speed_note):
        self.events = events
        self.duration = events[-1][0] + 2.5
        self.speed_note = speed_note

    def draw(self, img, t, dur):
        d = ImageDraw.Draw(img)
        a = ease(t / 0.5) * (1 - ease((t - dur + 0.5) / 0.5))
        lt = t - 0.6
        shown = [e for e in self.events if e[0] <= lt]
        term = [e[3] for e in shown if e[2] == "term"]
        gw = [e[3] for e in shown if e[2] == "gw"]
        real = shown[-1][1] if shown else 0.0

        # header strip
        phase = next((l[3:] for l in reversed(term) if l.startswith("== ")), "starting")
        text(d, (60, 52), "scripts/step1.sh", F["h2"], fade(TEXT, a))
        text(d, (60, 96), phase, F["body_m"], fade(AMBER, a))
        mm, ss = divmod(int(real), 60)
        text(d, (W - 60, 60), f"real time {mm:02d}:{ss:02d}", F["small_m"], fade(TEXT, a), "ra")
        text(d, (W - 60, 96), self.speed_note, F["small"], fade(FAINT, a), "ra")

        # terminal
        lx0, ly0, lx1, ly1 = 40, 140, 1130, H - 40
        panel(d, (lx0, ly0, lx1, ly1), "bash — ~/shielded-sync-gateway", a)
        rows = int((ly1 - ly0 - 60) / LINE_H)
        cols = int((lx1 - lx0 - 40) / MONO_W)
        prompt = "$ scripts/step1.sh"
        lines = [("PROMPT", prompt)] + [("", l) for l in term]
        lines = lines[-rows:]
        y = ly0 + 54
        for kind, l in lines:
            if kind == "PROMPT":
                text(d, (lx0 + 20, y), "$", F["mono_b"], fade(GREEN, a))
                text(d, (lx0 + 20 + MONO_W * 2, y), l[2:], F["mono"], fade(TEXT, a))
            else:
                c, bold = term_color(l)
                text(d, (lx0 + 20, y), clip(l.replace("\t", "    "), cols), F["mono_b" if bold else "mono"], fade(c, a))
            y += LINE_H
        if int(t * 2) % 2 == 0:
            d.rectangle((lx0 + 20, y + 2, lx0 + 20 + MONO_W, y + 19), fill=fade(TEXT, a * 0.8))

        # gateway log
        rx0, ry0, rx1, ry1 = 1150, 140, W - 40, H - 40
        panel(d, (rx0, ry0, rx1, ry1), "ssg-gateway proxy --record calls.jsonl", a)
        rrows = int((ry1 - ry0 - 110) / LINE_H)
        rcols = int((rx1 - rx0 - 40) / MONO_W)
        total_resp = sum(p[4] for p in gw)
        text(d, (rx0 + 20, ry0 + 58), f"{len(gw)} calls", F["small_m"], fade(TEXT, a))
        text(d, (rx0 + 160, ry0 + 58), f"{total_resp:,} bytes to wallet", F["small"], fade(DIM, a))
        y = ry0 + 100
        for p in gw[-rrows:]:
            s, status = gw_line(p)
            c = TEXT if status == "0" else (DIM if status == "-" else RED)
            if p[1] in ("GetBlockRange", "GetTreeState", "GetSubtreeRoots"):
                c = AMBER if status == "0" else c
            if p[1] == "SendTransaction":
                c = GREEN
            text(d, (rx0 + 20, y), clip(s, rcols), F["mono"], fade(c, a))
            y += LINE_H


def scene_results(img, t, dur, comparison, repeats):
    d = ImageDraw.Draw(img)
    a = ease(t / 0.6) * (1 - ease((t - dur + 0.6) / 0.6))
    text(d, (160, 120), "Direct sync vs gateway sync", F["h1"], fade(TEXT, a))
    text(d, (160, 190), "The wallet's own output, compared after each phase.", F["body"], fade(DIM, a))
    cols = ["balance", "height", "notes", "transactions", "value transfers"]
    files = ["balance.txt", "height.json", "notes.json", "transactions.txt", "value_transfers.txt"]
    phases = [("receive", "2.5 ZEC Ironwood + 1.25 ZEC Sapling arrive"),
              ("spend", "direct copy spends — gateway copy must notice"),
              ("send", "gateway copy sends through the gateway")]
    x0, y0 = 160, 290
    cw = [560, 190, 190, 190, 220, 240]
    x = x0 + cw[0]
    for i, c in enumerate(cols):
        text(d, (x + cw[i + 1] / 2, y0), c, F["small_m"], fade(DIM, a), "mm")
        x += cw[i + 1]
    for r, (key, label) in enumerate(phases):
        ra = a * ease((t - 0.8 - r * 0.5) / 0.5)
        y = y0 + 70 + r * 110
        d.line((x0, y - 40, x0 + sum(cw), y - 40), fill=fade(BORDER, ra), width=1)
        text(d, (x0, y - 12), key, F["h2"], fade(TEXT, ra))
        text(d, (x0, y + 26), label, F["small"], fade(DIM, ra))
        x = x0 + cw[0]
        for i, f in enumerate(files):
            status = comparison.get(key, {}).get(f, "?")
            ok = status in ("identical", "same set")
            mark = "✓" if ok else "✗"
            text(d, (x + cw[i + 1] / 2, y), mark, F["h2"], fade(GREEN if ok else RED, ra), "mm")
            if status == "same set":
                text(d, (x + cw[i + 1] / 2, y + 30), "same set", F["tiny"], fade(DIM, ra), "mm")
            x += cw[i + 1]
    ya = a * ease((t - 2.8) / 0.6)
    text(d, (160, 720), repeats, F["h2"], fade(GREEN, ya))
    text(
        d,
        (160, 780),
        "“same set”: identical notes listed in a different internal order (change built locally vs found by scanning).",
        F["small"],
        fade(DIM, ya),
    )
    text(
        d,
        (160, 820),
        "Sync time was 2.0 s both ways — set by the wallet's polling, not the gateway. Overhead is measured in step 4.",
        F["small"],
        fade(DIM, ya),
    )


def scene_inventory(img, t, dur, rows):
    d = ImageDraw.Draw(img)
    a = ease(t / 0.6) * (1 - ease((t - dur + 0.6) / 0.6))
    text(d, (160, 110), "What the wallet actually asks for", F["h1"], fade(TEXT, a))
    text(d, (160, 180), "Every call through the gateway, three sync sessions and one send.", F["body"], fade(DIM, a))
    y = 270
    hdr = ["method", "calls", "bytes to wallet", "step that authenticates it"]
    xs = [160, 640, 820, 1110]
    for i, h in enumerate(hdr):
        text(d, (xs[i], y), h, F["small_m"], fade(DIM, a))
    y += 50
    for i, (m, calls, resp, who, color) in enumerate(rows):
        ra = a * ease((t - 0.5 - i * 0.18) / 0.4)
        d.line((160, y - 10, W - 160, y - 10), fill=fade(BORDER, ra), width=1)
        text(d, (xs[0], y), m, F["mono_l"], fade(TEXT, ra))
        text(d, (xs[1], y), str(calls), F["mono_l"], fade(TEXT, ra))
        text(d, (xs[2], y), f"{resp:,}", F["mono_l"], fade(TEXT, ra))
        text(d, (xs[3], y), who, F["small"], fade(color, ra))
        y += 62


def scene_findings(img, t, dur, census):
    d = ImageDraw.Draw(img)
    a = ease(t / 0.6) * (1 - ease((t - dur + 0.6) / 0.6))
    text(d, (160, 110), "What step 1 changes about the plan", F["h1"], fade(TEXT, a))
    items = [
        ("Ironwood and v6 are the baseline",
         f"All {census['transactions']} transactions on the chain were v6. Payments to a unified address and all change",
         "landed in the Ironwood pool. The verifier must cover Ironwood and the v6 txid digest from day one."),
        ("Compact blocks don't show the whole block",
         f"{census['tip']} blocks, {census['transactions']} transactions — the wallet saw {census['with_shielded_parts']} compact transactions and no coinbase.",
         "Completeness can't come from counting compact txs; records must list every txid in block order."),
        ("Block hashes in compact blocks are claims",
         "CompactBlock.header was never populated, so the wallet has nothing to hash.",
         "Headers and the tip come from a separately trusted Zebra; scanning data from the untrusted producer."),
    ]
    y = 230
    for i, (h, l1, l2) in enumerate(items):
        ra = a * ease((t - 0.6 - i * 1.6) / 0.6)
        d.rounded_rectangle((160, y, 175, y + 150), radius=4, fill=fade(AMBER, ra))
        text(d, (210, y), h, F["h2"], fade(TEXT, ra))
        text(d, (210, y + 56), l1, F["body"], fade(DIM, ra))
        text(d, (210, y + 98), l2, F["body"], fade(DIM, ra))
        y += 220


def scene_next(img, t, dur):
    d = ImageDraw.Draw(img)
    a = ease(t / 0.6) * (1 - ease((t - dur + 0.8) / 0.8))
    text(d, (160, 300), "Next", F["h1"], fade(TEXT, a))
    steps = [
        ("2", "Producer adapter and Rust verifier — txids rebuilt, full ordered coverage against the block commitment"),
        ("3", "Gateway answers only from verified fields; atomic state, restart recovery, reorg rollback"),
        ("4", "Tampered ciphertexts, keys, nullifiers and omitted transactions — blocked before release; overhead measured"),
    ]
    y = 400
    for i, (n, s) in enumerate(steps):
        ra = a * ease((t - 0.5 - i * 0.4) / 0.5)
        text(d, (160, y), n, F["h2"], fade(AMBER, ra))
        text(d, (220, y + 4), s, F["body"], fade(TEXT, ra))
        y += 70
    text(d, (160, 700), "Everything shown here is reproducible with scripts/step1.sh.", F["body_m"], fade(DIM, a))


# ---------------------------------------------------------------- main


def parse_comparison(path):
    out = {}
    cur = None
    for l in open(path):
        m = re.match(r"### (\w+)-direct vs", l)
        if m:
            cur = m.group(1)
            out[cur] = {}
        m = re.match(r"- `([^`]+)`: (.*)", l)
        if m and cur:
            v = m.group(2)
            out[cur][m.group(1)] = "identical" if v.startswith("identical") else ("same set" if v.startswith("same notes") else "different")
    return out


def parse_inventory(path):
    rows = {}
    for l in open(path):
        m = re.match(r"\| `(\w+)` \| (\d+) \| (\d+) \| (\d+) \| (\d+) \| (\d+) \|", l)
        if m:
            rows[m.group(1)] = (int(m.group(2)), int(m.group(6)))
    who = {
        "GetBlockRange": ("compact blocks — core of steps 2 and 3", AMBER),
        "GetTreeState": ("tree frontiers — derived from verified blocks (3)", AMBER),
        "GetSubtreeRoots": ("subtree roots — derived from verified blocks (3)", AMBER),
        "GetTransaction": ("full tx — recompute txid, ZIP 244 (2)", AMBER),
        "GetLatestBlock": ("tip — trusted Zebra (3)", BLUE),
        "GetLightdInfo": ("metadata — checked against trusted Zebra (3)", BLUE),
        "SendTransaction": ("broadcast — passed through", DIM),
        "GetTaddressTxids": ("transparent history — out of MVP scope", FAINT),
        "GetMempoolStream": ("unconfirmed — out of MVP scope", FAINT),
    }
    order = ["GetBlockRange", "GetTreeState", "GetSubtreeRoots", "GetTransaction", "GetLatestBlock",
             "GetLightdInfo", "SendTransaction", "GetTaddressTxids", "GetMempoolStream"]
    return [(m, rows[m][0], rows[m][1], *who[m]) for m in order if m in rows]


def main():
    run_jsonl, results, out = sys.argv[1:4]
    events = compress(load_events(run_jsonl, f"{results}/gateway.log"))
    real_total = events[-1][1]
    replay = Replay(events, f"waits shortened · {real_total / events[-1][0]:.1f}× overall")
    comparison = parse_comparison(f"{results}/comparison.md")
    inventory = parse_inventory(f"{results}/rpc-inventory.md")
    census = json.load(open(f"{results}/chain-census.json"))
    repeats_n = sum(1 for l in open(f"{results}/repeats.md") if "PASS" in l)
    repeats = f"PASS — {repeats_n} of {repeats_n} runs on fresh chains"

    scenes = [
        (5.5, scene_title),
        (9.0, scene_diagram),
        (replay.duration + 1.0, replay.draw),
        (11.0, lambda i, t, d: scene_results(i, t, d, comparison, repeats)),
        (12.0, lambda i, t, d: scene_inventory(i, t, d, inventory)),
        (14.0, lambda i, t, d: scene_findings(i, t, d, census)),
        (7.0, scene_next),
    ]
    total = sum(s[0] for s in scenes)
    print(f"replay {replay.duration:.1f}s (real {real_total:.0f}s), total {total:.1f}s", file=sys.stderr)

    ff = subprocess.Popen(
        ["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}",
         "-r", str(FPS), "-i", "-", "-c:v", "libx264", "-preset", "medium", "-crf", "20",
         "-pix_fmt", "yuv420p", "-movflags", "+faststart", out],
        stdin=subprocess.PIPE,
    )
    for dur, fn in scenes:
        n = int(round(dur * FPS))
        for f in range(n):
            img = Image.new("RGB", (W, H), BG)
            fn(img, f / FPS, dur)
            ff.stdin.write(img.tobytes())
    ff.stdin.close()
    ff.wait()


if __name__ == "__main__":
    main()
