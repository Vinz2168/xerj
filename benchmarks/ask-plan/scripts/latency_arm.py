#!/usr/bin/env python3
"""latency_arm.py — the /_ask latency arm of the ask-plan harness.

Measures the release-time latency gate of #1056: "p50 <= 300 ms CPU budget"
per POST /_ask. run.sh's ask arm times every call but scores F1 only — this
arm records the milliseconds.

Honest scope, stated up front:

- The gate line says CPU budget; what is measurable from outside the process
  is WALL-CLOCK over loopback HTTP on an otherwise idle node. On a loopback
  connection against a single-node server the transport overhead is well
  under a millisecond, so wall-clock p50 is a faithful upper bound on CPU
  time per request — but it is wall-clock, and the summary says so.
- Prompts are sent with their gold `index` (exactly like the ask arm), so
  catalog routing is NOT in the measured path. Routing has its own acceptance
  line in #1056 and is not this arm's claim.
- One warmup pass runs before the measured passes and its distribution is
  reported separately. First-touch requests pay page-cache and allocator
  setup that a serving node has long since paid; the gate is about the
  steady state, and hiding warmup would flatter the number — reporting both
  keeps the choice visible instead.
- Deterministic: pairs.jsonl order, no shuffling, no sampling. Every prompt
  in every pass is recorded raw.

Usage (boot + load first, as run.sh does):
  XERJ_URL=http://127.0.0.1:9610 python3 scripts/ask_arm.py load
  XERJ_URL=... python3 scripts/latency_arm.py --out DIR [--passes 3]
"""
import argparse
import json
import os
import pathlib
import platform
import time
import urllib.request

from ask_arm import pairs, req  # noqa: E402 — same-process helpers, same env

U = os.environ.get("XERJ_URL", "http://127.0.0.1:9610").rstrip("/")

GATE_MS = 300.0  # #1056's release-time line: "p50 <= 300 ms CPU budget"


def ask_ms(prompt, index):
    body = json.dumps({"index": index, "prompt": prompt})
    t0 = time.perf_counter()
    code, _ = req("POST", "/_ask", body, timeout=300)
    return code, (time.perf_counter() - t0) * 1000.0


def one_pass(records, tag):
    """Every pair once, in file order; per-request ms appended raw."""
    codes = {}
    for p in pairs():
        code, ms = ask_ms(p["prompt"], p["gold"]["index"])
        codes[code] = codes.get(code, 0) + 1
        records.append({"pass": tag, "id": p["id"], "http": code, "ms": ms})
    return codes


def stats(values):
    """Count/mean/p50/p90/p95/p99/max — the quantile is the nearest-rank
    interpolation-free form (sorted[floor(q*(n-1))]), so the same raw list
    reproduces the same number anywhere."""
    if not values:
        return {"n": 0}
    s = sorted(values)

    def at(q):
        return s[int(q * (len(s) - 1))]

    return {
        "n": len(s),
        "mean_ms": round(sum(s) / len(s), 3),
        "p50_ms": round(at(0.50), 3),
        "p90_ms": round(at(0.90), 3),
        "p95_ms": round(at(0.95), 3),
        "p99_ms": round(at(0.99), 3),
        "max_ms": round(s[-1], 3),
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--out", required=True, help="run directory for raw + summary")
    ap.add_argument("--passes", type=int, default=3, help="measured passes (default 3)")
    args = ap.parse_args()

    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    # Endpoint presence — the same honest stopping point the ask arm has.
    code, _ = req("POST", "/_ask", json.dumps({"index": "ax-usgs-earthquakes",
                                               "prompt": "probe: every event"}), timeout=300)
    if code in (404, 405, 501):
        (out / "status.json").write_text(json.dumps(
            {"endpoint": "POST /_ask", "http": code, "measured": False,
             "note": "POST /_ask is not implemented; nothing measured"}, indent=1) + "\n")
        print(f"POST /_ask -> {code}: not implemented; nothing measured")
        return 0

    all_pairs = pairs()
    records = []
    warm_codes = one_pass(records, "warmup")
    measured_codes = {}
    for i in range(max(1, args.passes)):
        for k, v in one_pass(records, f"measured-{i + 1}").items():
            measured_codes[k] = measured_codes.get(k, 0) + v

    with open(out / "latency-raw.jsonl", "w") as fh:
        for r in records:
            fh.write(json.dumps(r) + "\n")

    warm_ms = [r["ms"] for r in records if r["pass"] == "warmup" and r["http"] == 200]
    measured_ms = [r["ms"] for r in records if r["pass"].startswith("measured")
                   and r["http"] == 200]
    p50 = stats(measured_ms)["p50_ms"] if measured_ms else None
    summary = {
        "endpoint": "POST /_ask",
        "measured": True,
        "pairs_per_pass": len(all_pairs),
        "measured_passes": max(1, args.passes),
        "http_counts": {"warmup": warm_codes, "measured": measured_codes},
        "warmup": stats(warm_ms),
        "measured_pooled": stats(measured_ms),
        "gate": {"line": "#1056 release gate: p50 <= 300 ms", "threshold_ms": GATE_MS,
                 "p50_ms": p50,
                 "pass": (p50 is not None and p50 <= GATE_MS)},
        "method_notes": [
            "wall-clock over loopback HTTP on an idle node, not in-process CPU "
            "time — an upper bound on the CPU budget, stated as wall-clock",
            "gold `index` sent with every prompt: catalog routing is not in "
            "the measured path (own acceptance line in #1056)",
            "one warmup pass reported separately from the measured passes",
            "quantiles are nearest-rank over the pooled measured passes; "
            "latency-raw.jsonl carries every request",
        ],
        "host": {"system": platform.system(), "release": platform.release(),
                 "machine": platform.machine(),
                 "python": platform.python_version()},
    }
    (out / "latency-summary.json").write_text(json.dumps(summary, indent=1) + "\n")
    print(json.dumps(summary["measured_pooled"]))
    print(f"gate p50 <= {GATE_MS} ms: {'PASS' if summary['gate']['pass'] else 'FAIL'}"
          f" (p50 = {p50} ms)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
