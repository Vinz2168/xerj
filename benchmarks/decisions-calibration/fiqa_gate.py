#!/usr/bin/env python3
"""Measure the #1063 calibration gate against the FiQA rerank baseline.

Issue #1063's gate: Jev FiQA rerank ECE 0.3109 -> <= 0.10 on held-out data.
The baseline run (benchmarks/beir-hybrid/results/2026-09-20-rerank-full-fiqa)
retained the reliability curve as 10 BINNED AGGREGATES, not the 19,440 raw
pairs (per_query holds nDCG only), so this measurement works at bin
granularity: each bin's (mean_conf, acc, n) is expanded into n pairs at
mean_conf, positives = round(acc*n), exactly the reconstruction the Rust
module's unit tests use (engine/crates/xerj-common/src/calibration.rs,
FIQA_BASELINE_CURVE + bin_pairs).

Held-out split, deterministic (#940 rule — no RNG, no clock): even-index bins
(0,2,4,6,8) fit the calibration, odd-index bins (1,3,5,7,9) evaluate it.
Held-out at bin level means whole probability REGIONS the fit never saw —
stronger than a random pair split on interpolation, weaker than the full
pair-level re-run (which needs the model's raw scores; that is the rc.80
final form, noted in the README).

This script mirrors the Rust implementation exactly — same bin edges
(b = floor(p*10), p=1 -> bin 9), same PAVA with linear interpolation between
knots and clamping at the ends, same temperature bracket (ln T in
[ln(1/16), ln 16], 100 golden-section iterations, logit clamped at 1e-6) — so
agreement between the two is a cross-check on both. Stdlib only, no network,
no API calls: every number comes from the existing baseline file.

Usage: python3 fiqa_gate.py [--baseline PATH] [--out PATH]
"""

import argparse
import json
import math
import sys
from pathlib import Path

DEFAULT_BASELINE = (
    Path(__file__).resolve().parents[1]
    / "beir-hybrid"
    / "results"
    / "2026-09-20-rerank-full-fiqa"
    / "fiqa-full-run.json"
)
GATE = 0.10
BINS = 10
LOGIT_EPS = 1e-6
T_BRACKET = (math.log(1.0 / 16.0), math.log(16.0))
GOLDEN_ITERS = 100


# ── the algorithms, mirroring engine/crates/xerj-common/src/calibration.rs ──


def bin_of(p, bins=BINS):
    """b = floor(p*bins), p=1 -> the last bin (the Rust module's clamp)."""
    b = int(p * bins)
    return min(b, bins - 1)


def ece(pairs, bins=BINS):
    """ECE = sum_b (n_b/N)*|acc_b - conf_b| (Naeini et al. 2015)."""
    n = len(pairs)
    if n == 0:
        return 0.0
    sums = [[0, 0.0, 0.0] for _ in range(bins)]  # count, sum p, sum y
    for p, y in pairs:
        s = sums[bin_of(p, bins)]
        s[0] += 1
        s[1] += p
        s[2] += y
    total = 0.0
    for c, sp, sy in sums:
        if c:
            total += (c / n) * abs(sy / c - sp / c)
    return total


class Isotonic:
    """PAVA (pool-adjacent-violators) with the Rust module's knots:
    linear interpolation between block means, clamped at the ends
    (Barlow et al. 1972; Zadrozny & Elkan 2002 for calibration use).

    Fixed 2026-09-30: the block's first-x was recorded as len(blocks) at
    append time, which stops equaling the pooled-point index after the first
    backward pool — every knot created after a violation got an x shifted
    down, so apply() mapped probabilities through a compressed curve. This
    never bit on the bin-level gate (the even-bin fit points are strictly
    increasing, so no pooling occurs — both implementations agree at
    0.0330 there), but it corrupted any fit on data with local violations,
    i.e. pair-level data. Now mirrors engine/crates/xerj-common/src/
    calibration.rs exactly: blocks carry the pooled index i, and every pooled
    point carries its block's mean (the Rust expand step), so the knots and
    the interpolation bands are identical to the shipped implementation."""

    def __init__(self, pairs):
        pts = sorted(((p, y) for p, y in pairs), key=lambda t: (t[0], t[1]))
        # Pool tied p by weighted mean first.
        xs, ys, ws = [], [], []
        i = 0
        while i < len(pts):
            j = i
            p_sum = y_sum = 0.0
            while j < len(pts) and pts[j][0] == pts[i][0]:
                p_sum += pts[j][0]
                y_sum += pts[j][1]
                j += 1
            xs.append(p_sum / (j - i))
            ys.append(y_sum / (j - i))
            ws.append(float(j - i))
            i = j
        # Stack of blocks: [y_sum, n, first POOLED index] — i, the index of
        # the pooled point, NOT len(blocks), which diverges after a pool.
        blocks = []
        for i, (x, y, w) in enumerate(zip(xs, ys, ws)):
            blocks.append([y * w, w, i])
            while len(blocks) >= 2 and (
                blocks[-2][0] / blocks[-2][1] > blocks[-1][0] / blocks[-1][1]
            ):
                cur = blocks.pop()
                blocks[-1][0] += cur[0]
                blocks[-1][1] += cur[1]
        # Expand blocks to pooled points (the Rust form): every pooled x is a
        # knot carrying its block's mean.
        fitted = [0.0] * len(xs)
        for bi, (y_sum, n, first) in enumerate(blocks):
            last = blocks[bi + 1][2] if bi + 1 < len(blocks) else len(xs)
            mean = y_sum / n
            for j in range(first, last):
                fitted[j] = mean
        self.knots = list(zip(xs, fitted))

    def apply(self, p):
        k = self.knots
        if not k:
            return p
        if p <= k[0][0]:
            return k[0][1]
        if p >= k[-1][0]:
            return k[-1][1]
        lo, hi = 0, len(k) - 1
        while hi - lo > 1:
            mid = (lo + hi) // 2
            if k[mid][0] <= p:
                lo = mid
            else:
                hi = mid
        (x0, y0), (x1, y1) = k[lo], k[hi]
        if x1 == x0:
            return y0
        return y0 + (y1 - y0) * (p - x0) / (x1 - x0)


def logit(p):
    p = min(max(p, LOGIT_EPS), 1.0 - LOGIT_EPS)
    return math.log(p / (1.0 - p))


def sigmoid(z):
    if z >= 0:
        return 1.0 / (1.0 + math.exp(-z))
    e = math.exp(z)
    return e / (1.0 + e)


def temperature_nll(pairs, ln_t):
    """NLL of the pairs under p' = sigmoid(logit(p)/T) (Platt 1999; Guo et
    al. 2017). Saturated pairs contribute the eps-clamped log-probability —
    the softplus form the Rust module uses."""
    t = math.exp(ln_t)
    nll = 0.0
    for p, y in pairs:
        q = min(max(sigmoid(logit(p) / t), LOGIT_EPS), 1.0 - LOGIT_EPS)
        nll += -(y * math.log(q) + (1.0 - y) * math.log(1.0 - q))
    return nll


class Temperature:
    def __init__(self, pairs):
        lo, hi = T_BRACKET
        gr = (math.sqrt(5.0) - 1.0) / 2.0
        a, b = lo, hi
        c = b - gr * (b - a)
        d = a + gr * (b - a)
        fc, fd = temperature_nll(pairs, c), temperature_nll(pairs, d)
        for _ in range(GOLDEN_ITERS):
            if fc < fd:
                b, d, fd = d, c, fc
                c = b - gr * (b - a)
                fc = temperature_nll(pairs, c)
            else:
                a, c, fc = c, d, fd
                d = a + gr * (b - a)
                fd = temperature_nll(pairs, d)
        self.t = math.exp((a + b) / 2.0)

    def apply(self, p):
        return sigmoid(logit(p) / self.t)


# ── the measurement ──────────────────────────────────────────────────────────


def bin_pairs(bins):
    """Expand binned aggregates into pairs — the same reconstruction as the
    Rust module's bin_pairs test helper."""
    out = []
    for conf, acc, n in bins:
        positives = round(acc * n)
        out.extend((conf, 1.0 if i < positives else 0.0) for i in range(n))
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    run = json.loads(args.baseline.read_text())
    cal = run["calibration"]
    curve = cal["curve"]
    bins = [
        (curve[str(i)]["mean_conf"], curve[str(i)]["acc"], curve[str(i)]["n"])
        for i in range(cal["bins"])
    ]
    all_pairs = bin_pairs(bins)
    fit_pairs = bin_pairs(bins[::2])  # even bins
    held_pairs = bin_pairs(bins[1::2])  # odd bins — whole regions unseen

    raw_all = ece(all_pairs)
    raw_held = ece(held_pairs)

    iso = Isotonic(fit_pairs)
    iso_calibrated = [(iso.apply(p), y) for p, y in held_pairs]
    iso_held = ece(iso_calibrated)

    temp = Temperature(fit_pairs)
    temp_calibrated = [(temp.apply(p), y) for p, y in held_pairs]
    temp_held = ece(temp_calibrated)

    result = {
        "issue": 1063,
        "gate": "Jev FiQA rerank ECE 0.3109 -> <= 0.10 on held-out",
        "gate_threshold": GATE,
        "baseline_run": str(args.baseline),
        "method_note": (
            "The baseline retained 10 binned aggregates (n_pairs=19440), not "
            "the raw pairs; pairs are reconstructed per bin at mean_conf with "
            "positives = round(acc*n), and the held-out split is at BIN "
            "granularity: fit on even bins, evaluate on odd bins (whole "
            "probability regions the fit never saw). The pair-level held-out "
            "re-run needs the model's raw scores and is the rc.80 final form."
        ),
        "pairs": {
            "total": len(all_pairs),
            "fitted_on": len(fit_pairs),
            "held_out": len(held_pairs),
        },
        "raw_ece": {
            "all_bins": round(raw_all, 4),
            "published": cal["ece"],
            "held_out_bins": round(raw_held, 4),
        },
        "isotonic": {
            "held_out_ece": round(iso_held, 4),
            "meets_gate": iso_held <= GATE,
            "knots": [[round(x, 4), round(y, 4)] for x, y in iso.knots],
        },
        "temperature": {
            "held_out_ece": round(temp_held, 4),
            "meets_gate": temp_held <= GATE,
            "t": round(temp.t, 4),
        },
    }

    print(json.dumps(result, indent=2))
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(result, indent=2) + "\n")
        print(f"\nwrote {args.out}", file=sys.stderr)

    ok = raw_all <= cal["ece"] + 0.0005 and iso_held <= GATE
    if not ok:
        print(
            "GATE CHECK FAILED: reproduction "
            f"{raw_all:.4f} vs published {cal['ece']}, isotonic held-out "
            f"{iso_held:.4f} vs gate {GATE}",
            file=sys.stderr,
        )
        return 1
    print(
        f"\ngate met: raw {raw_held:.4f} held-out -> isotonic {iso_held:.4f} "
        f"(<= {GATE}); temperature {temp_held:.4f}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
