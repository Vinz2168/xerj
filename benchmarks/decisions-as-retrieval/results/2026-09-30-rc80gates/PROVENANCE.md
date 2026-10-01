# Provenance — inputs to the 2026-09-30 rc.80 decide-gate runs

## Binaries

| binary | what it is | how obtained |
|---|---|---|
| `/tmp/xerj-rc80gates-bin/xerj` | the staged release binary, main @ `a179e1ef3`, version string `xerj v1.0.0-rc.78` | provided by the release-cut orchestrator |
| `/tmp/xtarget-decide/release/xerj` | **same commit** `a179e1ef3`, built with `cargo build --release --locked -p xerj-server --features decide-local` (the AC's own documented arming path, printed by `--help`) | built by this run; the stock release build does NOT include the `decide-local` cargo feature (see `stock-binary-noop.txt`) |

All quality/latency numbers below were measured on the decide-local build of
`a179e1ef3`; the staged release binary was used only to measure what `--decide-mode
local` does on a stock release build (answer: nothing — a boot warning, tier stays off).

## Model artifact

`/tmp/xerj-rc80gates-artifact/xerj-decide-v1` (staged, read-only):
`model.safetensors` sha256 `2dfdb8c33be5ace69243ea6d8a20a9ee905cfcfdf454f7eb7c49c05f73982bf0`
— matches the artifact's own `MANIFEST.sha256` and the weights hash the task brief states.
Verified with `sha256sum -c MANIFEST.sha256` (all three files OK).

## Datasets (all staged by the orchestrator at /tmp/xerj-rc80gates-data/)

| file | sha256 | licence | source |
|---|---|---|---|
| `b77_train.csv` | `b06e26ac675513959a63135f11b94ea7786ed02da65db93a5650d8838cbc664b` | CC-BY-4.0 | PolyAI-LDN/task-specific-datasets `banking_data/train.csv` — hash matches the xerj-decide-v1 manifest's recorded training input |
| `b77_test.csv` | `d12d6e3bc4c3103966ae786dc435913c0c563dfa328f5a3646d0e62cfeeb474d` | CC-BY-4.0 | same repo, `test.csv` (3,080 rows) |
| `sms.tsv` | `7d039a24a6083ed9ef0f806ebad56bbb976e3aeb8de05669173bfdc4996c239d` | CC-BY-4.0 (UCI page) | justmarkham/pycon-2016-tutorial mirror — hash matches the xerj-decide-v1 manifest |

SMS held-out split: `load.py`'s exact method — `random.Random(7).shuffle` over the
5,574 rows, first 4,000 = indexed history (not used here), remainder = the 1,574
held-out rows scored.

## AG News (fetched by this run, 2026-09-30)

- File: `ag_news_test.csv` — 7,600 rows, no header, columns `class,title,description`
  (class 1..4 = World / Sports / Business / SciTech).
- sha256 `521465c2428ed7f02f8d6db6ffdd4b5447c1c701962353eb2c40d548c3c85699`
- URL: `https://raw.githubusercontent.com/mhjabreel/CharCnn_Keras/master/data/ag_news_csv/test.csv`
- Cross-check: the HuggingFace `fancyzhx/ag_news` card (whose parquet this repo's
  2026-09-20 AG News run used) declares test = 7,600 examples — same split size.
  The HF parquet (sha256 `71de87ec66bc5737752a2502204dfa6d7fe9856ade3ea444dc6317789a4f13fb`)
  was fetched too but not parsed (no pyarrow in this sandbox); the CSV mirror is the
  same Zhang et al. test split.
- **Licence: "unknown"** — that is the HuggingFace dataset card's own declaration
  (`cardData.license = ["unknown"]`), and the CharCnn_Keras mirror repo carries no
  licence file. Original corpus: Zhang, Zhao, LeCun, *Character-level Convolutional
  Networks for Text Classification*, NeurIPS 2015 (arXiv:1509.01626). The file is used
  here for evaluation only and is NOT committed to this repository — only its hash and
  the measured numbers are.
- Text fed to the model: `title + " " + description`, CSV-parsed, AG's original
  escape sequences (e.g. `\ `, `#36;`) left verbatim.
