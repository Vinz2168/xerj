# xerj-decide — the open tier-2 decide model

`xerj-decide-v1` is the trained local head of the decide ladder: the thing
`xerj-server --decide-mode local --decide-model-dir <dir>` loads so a `noul`
or `choice` question can be answered with **no API key, no network call, and
no history** — tier 2 in `docs/DECISIONS.md`'s ladder (tier 1 = history vote,
tier 3 = hosted model). This card states what the model is, every number we
measured for it, and how those numbers compare with the ladder's other tiers.

**Status: artifact built and verified against the existing loader;
publication to a model hub is PENDING operator credentials — no published
URL exists yet and none should be claimed.**

- Weights: **Apache-2.0** (xerj-org).
- Training data: two public **CC-BY-4.0** datasets (details below) — recorded
  in the manifest as provenance with the attribution that licence asks for.
- Harness: [`benchmarks/decide-model/`](../benchmarks/decide-model/)
  (deterministic, seeds pinned, stdlib-plus-candle, CPU-only).

## What the model is

A `ModernBertForSequenceClassification` in the exact shape the existing
loader in `engine/crates/xerj-ai/src/decide.rs` parses — the harness trains
through the same `candle_transformers::models::modernbert` `load` call the
server loads with, against the same tensor names, so trainer and server run
one forward implementation, not two that might drift.

| property | value |
|---|---|
| architecture | ModernBERT-class sequence-pair classifier, CLS pooling |
| hidden × layers × heads | 256 × 6 × 4 |
| intermediate (FFN) | 768 |
| vocabulary | wordpiece fitted to the training corpora, frozen at 8,742 tokens (`pinned/tokenizer.json`) |
| parameters | 7,419,907 (F32) |
| weights file | `model.safetensors`, 29,683,844 bytes |
| directory layout | `config.json` + `tokenizer.json` + `model.safetensors` (+ `VERSION.json`, `MANIFEST.sha256`) |

It is deliberately **small**: this head is scored once per candidate label
per question on CPU, so every parameter is paid N times per request. A
149M-parameter encoder would be un-servable at that duty cycle in-process.

## How it answers

A label `L` becomes a hypothesis through the loader's fixed templates,
applied verbatim:

- positive: `This example is {L}.`
- negation: `This example is not {L}.`

- **noul** (`/_decide` with no index, `/v1/systemone` type `noul`): the
  positive hypothesis is scored against its own negation; the positive's
  share of the two-way entailment mass is `p_raw`.
- **choice** (type `choice` with N criteria): all N hypotheses are scored in
  one batched pass; the renormalised entailment shares are the per-option
  probabilities and the arg-max is the answer.

Training mirrors this exactly: each labelled row yields three pairs —
`(text, hyp(gold)) → entailment`, `(text, hyp(random wrong label)) →
neutral`, `(text, neghyp(gold)) → contradiction` — the three-way NLI shape
that a choice among mostly-wrong hypotheses actually exercises.

## Measured — held-out accuracy and calibration

All numbers below were produced by `cargo run --release -- eval` in
`benchmarks/decide-model/` (command noted per table), scoring through
`Head::score`, the same computation the server performs. The ECE estimator
is 10 equal-width bins — identical to the history-vote benchmark's `eval.py`
and to `xerj-common`'s `calibration.rs` (a unit test pins the same
hand-computed case through both).

### xerj-decide-v1 (trained on both datasets)

Command: `cargo run --release -- eval --data-dir data --workdir work`
(checkpoint `ckpt-v1-epoch004.safetensors`, the run's final epoch; training
loss 1.099 → 0.198 over 5 epochs). Banking77 is scored as the full 3,080-row
test split, 77-way `choice`; SMS as the full 1,574 held-out rows, `noul`.

| dataset | mode | n | accuracy | ECE | conf≥0.8 | acc@≥0.8 | ms/item | P/R/F1 (spam) |
|---|---|---|---|---|---|---|---|---|
| Banking77 test | choice (77) | 3,080 | **0.1185** | 0.072 | 0.0% | — | 272 | — |
| SMS held-out | noul | 1,574 | **0.8850** | 0.310 | 19.3% | 0.6700 | 13.1 | 0.534 / 0.961 / 0.686 |

### Leave-one-out (zero-shot: the dataset was never trained on)

Same recipe, one dataset removed from training (`--exclude`); the excluded
dataset's held-out rows are scored by a head that never saw them. Commands:
`train … --exclude X --epochs 5 --name loo-X` then `eval … --exclude X`.

| head | dataset scored | in training? | n | accuracy | ECE | note |
|---|---|---|---|---|---|---|
| loo-banking (SMS only) | Banking77 test, 77-way | no | 3,080 | **0.0094** | 0.010 | chance is 1/77 ≈ 0.013; confidence ≈ uniform (mean 0.020) — the head does not pretend |
| loo-banking (SMS only) | SMS held-out, noul | yes | 1,574 | **0.9568** | 0.261 | in-domain binary works (F1 0.850) |
| loo-sms (Banking77 only) | SMS held-out, noul | no | 1,574 | **0.1982** | 0.693 | below always-ham (0.867): the label vocabulary never trained |
| loo-sms (Banking77 only) | Banking77 test, 77-way | yes | 3,080 | **0.0416** | 0.018 | see below — pair-level NLI does not scale to 77-way ranking at this size |

**What these numbers say, plainly.**

1. **Zero-shot transfer to an unseen label vocabulary is ≈ chance in both
   directions.** The head answers hypotheses it was trained to rank; a
   domain with new labels needs those labels in training. That is the
   boundary of tier 2's "no history" claim, now measured rather than
   assumed.
2. **Binary `noul` on a trained vocabulary is where the head works.**
   SMS at 0.957 (single-dataset head) / 0.885 (both-dataset head) — the
   history vote's BM25 baseline is 0.983, so tier 1 stays ahead on its home
   ground, as it should: it has labelled history, tier 2 has none.
3. **Wide `choice` is not solved by this recipe.** 77-way Banking77 tops out
   at 0.119 (both-dataset head; 0.042 single-dataset) against the history
   vote's 0.819 BM25 / 0.937 hybrid. The training signal — one entailment,
   one randomly-sampled wrong label, one negation per row — teaches
   pair-level NLI (loss 0.198) but not sharp ranking across 76 competitors.
   The plausible next recipe (more sampled negatives per row, more epochs)
   is a follow-up, not a claim.
4. **Calibration is honest but not good.** ECE 0.072 on Banking77 comes with
   near-uniform confidence (0% of items at ≥0.8); the SMS noul ECE of
   0.31 with mean confidence 0.70 means the head is underconfident when
   right and overconfident when wrong — exactly what the #1063 calibration
   layer (`p_cal`, isotonic/temperature) exists to correct on live nodes.

### Comparison — history vote (tier 1) on the same datasets

From `benchmarks/decisions-as-retrieval/results/run-2026-09-18.txt` — 1,000
test items per dataset, k=10 history neighbours:

| dataset | method | accuracy | ECE | conf≥0.8 | acc@≥0.8 | ms/item |
|---|---|---|---|---|---|---|
| Banking77 (77-way) | bm25 history vote | 0.8190 | 0.089 | 50.6% | 0.9960 | 2.2 |
| Banking77 (77-way) | minilm history vote | 0.9330 | 0.012 | 86.9% | 0.9793 | 8.6 |
| Banking77 (77-way) | hybrid rrf | 0.9370 | 0.052 | 79.2% | 0.9886 | 12.4 |
| SMS (noul) | bm25 history vote | 0.9830 | 0.017 | 95.4% | 0.9916 | 0.9 |
| SMS (noul) | minilm history vote | 0.9820 | 0.009 | 95.8% | 0.9885 | 74.5 |
| SMS (noul) | hybrid rrf | 0.9890 | 0.015 | 95.7% | 0.9969 | 67.2 |

**Comparability caveats, stated plainly.** The history-vote numbers score a
1,000-item sample drawn by Python's Mersenne-Twister shuffle; the
xerj-decide numbers score the **full** test split (every Banking77 test row,
every SMS held-out row) — a superset, not the same sample. The history vote
needs a populated history index; xerj-decide needs nothing. Both score
identical source rows from `load.py`'s datasets.

## Measured — serving latency

Node: private port, throwaway data dir,
`xerj --insecure --port 9410 --data-dir <throwaway> --decide-mode local
--decide-model-dir artifact/xerj-decide-v1` (release build of this branch,
`decide-local` feature, CPU, idle machine, single sequential client —
`scripts/eval_latency.py --n 100`). The "cold" column is the first request
after boot; p50/p99 exclude it.

| surface | hypotheses | cold | p50 | p99 |
|---|---|---|---|---|
| `POST /_decide` (no index, `positive_label=spam`) | 2 | 15.1 ms | **13.0 ms** | 13.9 ms |
| `POST /v1/systemone`, one noul | 2 | 12.9 ms | **13.2 ms** | 14.3 ms |
| `POST /v1/systemone`, one 5-option choice | 5 | 20.5 ms | **21.6 ms** | 23.5 ms |
| `POST /v1/systemone`, one 77-option choice (full Banking77 vocabulary) | 77 | 232.2 ms | **228.1 ms** | 237.1 ms |

Wire spot-checks from the same run (the response names the tier that
answered): the spam noul answered `spam` at 0.99973 with
`"tier": "local", "model": "xerj-decide-local-1"`; the ham noul answered
`not spam` at 0.574 — correct label, weak margin, consistent with the
calibration row above. A 5-way intent choice on "i topped up but the
balance still shows the old amount" picked `top_up_failed` at 0.837 over
`balance_not_updating` at 0.015 — defensible, not the label the probe
author expected.

## Reproducing

```sh
cd benchmarks/decide-model
./scripts/fetch_data.sh data                                     # download + sha256 pin check
mkdir -p work && cp pinned/tokenizer.json work/                  # the tokenizer is a frozen input (see below)
cargo run --release -- train --data-dir data --workdir work --epochs 5 --name v1
cargo run --release -- eval  --data-dir data --workdir work      # the tables above
cargo run --release -- export --data-dir data --workdir work --out artifact/xerj-decide-v1
./scripts/publish_bundle.sh artifact/xerj-decide-v1 dist/xerj-decide-v1
```

The training run is deterministic given the pinned tokenizer (splitmix64 for
every random choice including weight init — candle's CPU RNG cannot be
seeded, so init is written from the harness's own stream; epoch order from
`stream_seed`; no clock or environment reads in the artifact path). The one
step that is NOT stable across processes is the wordpiece *fit* itself: the
tokenizers crate's trainer iterates a randomly-seeded HashMap, so two fits
of the same corpus differ by a token or two (measured: 8,740/8,741/8,742 —
and it is not the rayon parallelism; the crate's sequential path varies the
same way). The recipe therefore ships one frozen fit
(`pinned/tokenizer.json`, committed and mirrored inside the artifact) and
every run — including the leave-one-out runs — trains from it.

Leave-one-out runs: add `--exclude banking77` (or `--exclude sms`) to both
`train` and `eval`; the excluded dataset's held-out rows are then scored by
a head that never saw the dataset, with the same frozen tokenizer —
mirroring a published model whose vocabulary is fixed before it meets a new
domain.

## Licence and provenance

**Weights: Apache-2.0 from xerj-org.** They were trained on:

- **Banking77** — `PolyAI-LDN/task-specific-datasets`, `banking_data/`
  (train 10,003 rows, test 3,080 rows, 77 intents). The repository's LICENSE
  is Creative Commons Attribution 4.0 International. Casanueva et al.,
  *Efficient Intent Detection with Dual Sentence Encoders*, 2020
  (arXiv:2003.04807).
- **SMS Spam Collection** — UCI ML Repository (Almeida & Hidalgo, 2012, DOI
  10.24432/C5CC84), fetched via the `justmarkham/pycon-2016-tutorial` mirror
  that `benchmarks/decisions-as-retrieval/load.py` uses. CC BY 4.0 per the
  UCI dataset page.

Those terms govern the datasets and ride with the manifest as the
provenance of what the weights were derived from — they are not a grant
xerj-org makes. `MANIFEST.sha256` in the artifact records each training
file's sha256, licence, and citation beside the weights' own licence.

## What is not claimed

- No published download URL yet — publication needs operator credentials
  this build environment does not have; `scripts/publish_bundle.sh`
  assembles the complete upload bundle (weights, licence, this card,
  digest-checked manifest) and prints the exact commands.
- No tier-3 (hosted model) comparison here — the Banking77-replay and
  tier-3 gates are rc.80 scope, not this card's.
- Latency numbers are one machine, one client, CPU; they are measurements,
  not guarantees.
