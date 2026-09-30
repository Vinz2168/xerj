# decisions-calibration — the #1063 gate measurement

Issue #1063's gate: **Jev FiQA rerank ECE 0.3109 → ≤ 0.10 on held-out**,
baseline `benchmarks/beir-hybrid/results/2026-09-20-rerank-full-fiqa`.
Measured here from the baseline's own retained curve, with no new model
calls, no network, no RNG.

## Result

| quantity | ECE | meets ≤ 0.10? |
|---|---|---|
| raw, all 19,440 pairs (reproduces the published number exactly) | **0.3109** | — |
| raw, held-out bins only | 0.3831 | no |
| **isotonic (PAVA), held-out** | **0.0330** | **yes** |
| temperature, held-out | 0.3903 | no |

`results/2026-09-30-calibration-gate.json` is the machine-readable run.

Reproduce: `python3 fiqa_gate.py` (stdlib only; writes nothing unless `--out`).

## Method, and its honest scope

The baseline run retained the reliability curve as **10 binned aggregates**
(`calibration.curve`: mean_conf, acc, n per bin; `per_query` holds nDCG only) —
not the 19,440 raw (p, y) pairs. So this measurement works at **bin
granularity**:

- pairs are reconstructed per bin — `n` pairs at `mean_conf`,
  `round(acc·n)` of them positive — the same reconstruction the Rust
  module's unit tests pin (`FIQA_BASELINE_CURVE` + `bin_pairs` in
  `engine/crates/xerj-common/src/calibration.rs`);
- the held-out split is **even bins fit (0,2,4,6,8), odd bins evaluate
  (1,3,5,7,9)** — deterministic (#940: no RNG, no clock), and bin-level
  held-out means whole probability *regions* the fit never saw, which is a
  stronger test of interpolation than a random pair split;
- the raw held-out ECE (0.3831) is *harder* than the published 0.3109,
  because the odd bins carry the pathological top bin (0.9297 conf → 0.3378
  acc). The gate was not met by evaluating on an easy split.

What this does NOT claim: a pair-level held-out measurement. The fit here
sees five points, not 15,000 — bin-level aggregates are already smoothed, and
smoothing flatters calibration fits. The pair-level re-run needs the model's
raw scores re-emitted per query (the baseline kept only nDCG); that costs API
calls and is queued as the **rc.80 final form** of this gate. The production
path (`/_decide/_calibration`) fits on raw pairs by construction, so the
serving behaviour is the stronger form already.

## Why temperature fails, and is still shipped

Temperature is one scalar on the log-odds: `p' = σ(logit(p)/T)`. The FiQA
curve needs 0.93 → 0.34 at the top while keeping 0.05 → 0.0002 at the bottom —
a steep monotone *remap*, not the uniform flattening a single T can express.
Its NLL optimum here is T = 1.1517 (barely softer than raw), and its held-out
ECE (0.3903) barely moves. Isotonic is the gate-meeting method; temperature
is shipped because it is the right tool when the history is thin (few knots
to overfit) or monotone-in-log-odds — and `/_decide/_calibration` publishes
both ECEs, so a node running either method shows its own number.

## Cross-check

`fiqa_gate.py` mirrors the Rust implementation exactly — same bin edges
(`floor(p·10)`, p=1 → bin 9), same PAVA with linear interpolation between
knots and end-clamping, same temperature bracket (ln T ∈ [ln(1/16), ln 16],
100 golden-section iterations, logit clamped at 1e-6). Agreement between the
two (0.0330 here; the Rust pinning test asserts ≤ 0.10 on the identical
split) is a cross-check on both.
