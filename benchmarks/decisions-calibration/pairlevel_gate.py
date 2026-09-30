#!/usr/bin/env python3
"""Stage 2 (free, deterministic) of the #1063 rc.80 final-form gate.

Reads the pair-level rows written by pairlevel_run.py (query, doc, p_raw, gold),
splits them, fits the two shipped calibration methods on the fit side and
reports held-out ECE for both — the rc.80 final form of the gate, at pair level
instead of the binned-aggregate reconstruction fiqa_gate.py had to use.

Split rule (deterministic, #940 — no RNG library, no clock):
  held-out = queries with int(sha256(qid), 16) % 5 == 0   (~20 % of queries)
Query-level, not pair-level: calibration is fitted on history from PAST
queries and applied to NEW queries in production (/_decide/_calibration), so
the honest held-out unit is the query — a pair-level split would let the fit
see sibling pairs from the same query and leak per-query difficulty. A
pair-level hash split is reported as a secondary row for comparison.

Algorithms (PAVA isotonic, temperature on the log-odds, 10-bin equal-width
ECE) are imported from fiqa_gate.py so the pair-level number and the bin-level
number provably come from the same code the Rust module is pinned against
(engine/crates/xerj-common/src/calibration.rs).

Usage: python3 pairlevel_gate.py --results results/2026-09-30-pairlevel-fiqa
"""

import argparse
import collections
import hashlib
import json
import sys
from pathlib import Path

from fiqa_gate import GATE, Isotonic, Temperature, ece

HELD_MOD = 5  # % 5 == 0 -> held-out (~20 %)


def q_held_out(qid):
    return int(hashlib.sha256(qid.encode()).hexdigest(), 16) % HELD_MOD == 0


def pair_held_out(qid, docid):
    return int(hashlib.sha256(f"{qid}:{docid}".encode()).hexdigest(), 16) % HELD_MOD == 0


def brier(pairs):
    return sum((p - y) ** 2 for p, y in pairs) / len(pairs) if pairs else 0.0


def curve(pairs, bins=10):
    """The reliability curve to publish beside the probabilities."""
    agg = collections.defaultdict(lambda: [0.0, 0, 0])  # sum p, n, n_pos
    for p, y in pairs:
        b = min(int(p * bins), bins - 1)
        agg[b][0] += p
        agg[b][1] += 1
        agg[b][2] += y
    return {
        str(b): {
            "mean_conf": round(c / n, 4) if n else None,
            "acc": round(r / n, 4) if n else None,
            "n": n,
        }
        for b, (c, n, r) in sorted(agg.items())
    }


def arm(pairs_fit, pairs_held, fitter):
    m = fitter(pairs_fit)
    cal = [(m.apply(p), y) for p, y in pairs_held]
    out = {
        "held_out_ece": round(ece(cal), 4),
        "held_out_brier": round(brier(cal), 4),
        "meets_gate": ece(cal) <= GATE,
    }
    if hasattr(m, "knots"):
        out["knots"] = [[round(x, 4), round(y, 4)] for x, y in m.knots]
    if hasattr(m, "t"):
        out["t"] = round(m.t, 4)
    out["held_out_curve"] = curve(cal)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--results", type=Path, required=True, help="dir with pairs.jsonl (from pairlevel_run.py)")
    ap.add_argument("--out", type=Path, default=None, help="default <results>/gate.json")
    args = ap.parse_args()

    rows = [json.loads(l) for l in (args.results / "pairs.jsonl").read_text().splitlines() if l.strip()]
    pairs = [(r["p_raw"], r["gold"]) for r in rows]

    # split 1 (the gate): query level
    fit_q = [(r["p_raw"], r["gold"]) for r in rows if not q_held_out(r["qid"])]
    held_q = [(r["p_raw"], r["gold"]) for r in rows if q_held_out(r["qid"])]
    # split 2 (secondary): pair level
    fit_p = [(r["p_raw"], r["gold"]) for r in rows if not pair_held_out(r["qid"], r["docid"])]
    held_p = [(r["p_raw"], r["gold"]) for r in rows if pair_held_out(r["qid"], r["docid"])]

    n_queries = len({r["qid"] for r in rows})
    held_queries = len({r["qid"] for r in rows if q_held_out(r["qid"])})

    result = {
        "issue": 1063,
        "gate": "Jev FiQA rerank ECE 0.3109 -> <=0.10 on held-out; no probability shipped without an ECE beside it",
        "gate_threshold": GATE,
        "form": "rc.80 final form: pair-level rows (query, doc, p_raw, gold), not binned aggregates",
        "split_rule": {
            "primary": f"query-level: int(sha256(qid),16) % {HELD_MOD} == 0 -> held-out; the fit never sees ANY pair from a held-out query (deployment-faithful: fit on past queries, apply to new ones)",
            "secondary": f"pair-level: int(sha256(qid:docid),16) % {HELD_MOD} == 0 -> held-out; comparison only",
            "determinism": "sha256 of stable ids — no RNG, no clock (#940); reruns are bit-identical",
        },
        "pairs": {
            "total": len(rows),
            "positives": sum(y for _, y in pairs),
            "queries": n_queries,
            "query_split": {"fit": len(fit_q), "held_out": len(held_q), "held_out_queries": held_queries},
            "pair_split": {"fit": len(fit_p), "held_out": len(held_p)},
        },
        "raw": {
            "all_pairs_ece": round(ece(pairs), 4),
            "all_pairs_brier": round(brier(pairs), 4),
            "published_bin_level_ece": 0.3109,
            "held_out_ece_query_split": round(ece(held_q), 4),
            "held_out_brier_query_split": round(brier(held_q), 4),
            "held_out_curve": curve(held_q),
        },
        "isotonic_query_split": arm(fit_q, held_q, Isotonic),
        "temperature_query_split": arm(fit_q, held_q, Temperature),
        "isotonic_pair_split": arm(fit_p, held_p, Isotonic),
        "temperature_pair_split": arm(fit_p, held_p, Temperature),
    }
    result["gate_verdict"] = {
        "method": "isotonic (the shipped gate-meeting method; /_decide/_calibration publishes both arms' ECE)",
        "held_out_ece": result["isotonic_query_split"]["held_out_ece"],
        "PASS": result["isotonic_query_split"]["meets_gate"],
    }

    out = args.out or (args.results / "gate.json")
    out.write_text(json.dumps(result, indent=1) + "\n")
    print(json.dumps(result, indent=1))
    print(
        f"\ngate (isotonic, query-level held-out): "
        f"{result['isotonic_query_split']['held_out_ece']:.4f} vs <= {GATE} -> "
        f"{'PASS' if result['isotonic_query_split']['meets_gate'] else 'FAIL'}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
