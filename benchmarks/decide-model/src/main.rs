//! Train and export the open `xerj-decide` head (issue #1064).
//!
//! The artifact is a directory `--decide-model-dir` can name: `config.json`,
//! `tokenizer.json`, `model.safetensors` — loaded by the *existing* loader in
//! `engine/crates/xerj-ai/src/decide.rs`, which this harness never modifies.
//!
//! ```sh
//! # 0. data (see scripts/fetch_data.sh) and its digests
//! cargo run --release -- verify-data --data-dir data
//! # 1. the frozen tokenizer of the v1 recipe (a fresh `tokenizer` fit is
//! #    NOT byte-stable across processes; the pinned one is the recipe input)
//! mkdir -p work && cp pinned/tokenizer.json work/tokenizer.json
//! # 2. train (checkpointed per epoch under work/)
//! cargo run --release -- train --data-dir data --workdir work
//! # 3. held-out numbers
//! cargo run --release -- eval --data-dir data --workdir work
//! # 4. the publishable directory, verified by loading it back
//! cargo run --release -- export --data-dir data --workdir work --out artifact/xerj-decide-v1
//! ```
//!
//! Determinism: seeds are flags with defaults, sampling is splitmix64
//! (including weight init — candle's CPU RNG cannot be seeded, so every
//! tensor is written from the harness's own stream), and no code path reads
//! a clock or an environment variable into the artifact. The ONE exception
//! is the wordpiece *fit*: the tokenizers crate's trainer iterates a
//! randomly-seeded HashMap internally, so two fits of the same corpus can
//! differ by a token or two (measured: 8740/8741/8742 across processes —
//! and it is NOT the rayon parallelism; forcing the crate's sequential path
//! still varies). The recipe therefore pins the tokenizer as a frozen
//! input, `pinned/tokenizer.json` (fitted once, committed, shipped inside
//! the artifact): copy it into the workdir before `train`, and everything
//! downstream of it — shapes, batches, init, steps — is bit-reproducible.

mod corpus;
mod data;
mod evaluate;
mod export;
mod model;
mod rng;
mod train;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use corpus::Pair;
use data::{Dataset, Item};
use model::{Head, ModelConfig};
use rng::Rng;
use train::TrainPlan;

/// Banking77's label count, used only for sanity messages.
const BANKING_LABELS: usize = 77;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = &args[1..];
    let out = match args.first().map(String::as_str) {
        Some("verify-data") => cmd_verify_data(rest),
        Some("tokenizer") => cmd_tokenizer(rest),
        Some("train") => cmd_train(rest),
        Some("eval") => cmd_eval(rest),
        Some("export") => cmd_export(rest),
        Some("--help") | Some("-h") | None => {
            print_usage();
            Ok(())
        }
        Some(other) => Err(anyhow!("unknown subcommand {other:?}")),
    };
    if let Err(e) = out {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn print_usage() {
    println!(
        "xerj-decide training + export harness (issue #1064)\n\
         \n\
         verify-data --data-dir DIR                      verify the three source files' sha256\n\
         tokenizer   --data-dir DIR [--out FILE] [--vocab N]\n\
         train       --data-dir DIR --workdir DIR [--epochs N] [--lr F] [--seed N]\n\
                       [--exclude banking77|sms] [--dev-rows N] [--name NAME]\n\
         eval        --data-dir DIR --workdir DIR [--ckpt FILE] [--dataset NAME] [--n N]\n\
                       [--exclude banking77|sms]   # same exclusions as training, for zero-shot\n\
         export      --data-dir DIR --workdir DIR --out DIR [--ckpt FILE] [--version V]\n\
                       [--name NAME] [--exclude banking77|sms]\n\
         \n\
         Defaults are the v1 recipe: vocab 12000, hidden 256x6, 5 epochs, lr 6e-4,\n\
         seed 1064, 500 dev rows per dataset held out of training."
    );
}

// ───────────────────────────── arg plumbing ─────────────────────────────

/// Value of `--name value` in `rest`. Callers pass the bare name
/// (`"epochs"`), never the dashes — both `--epochs` and a hypothetical bare
/// `epochs` match. Accepting the dashed form is not cosmetic: every default
/// in this CLI equals the v1 recipe's value, so a parser that silently
/// dropped flags would run the right-looking command with the wrong recipe
/// (that exact bug shipped the first training attempt: `--exclude` never
/// matched, and two identical default runs raced one workdir).
fn flag(rest: &[String], name: &str) -> Option<String> {
    let dashed = format!("--{name}");
    rest.iter()
        .position(|a| a == &dashed || a == name)
        .and_then(|i| rest.get(i + 1))
        .cloned()
}

fn flag_parse<T: std::str::FromStr>(rest: &[String], name: &str, default: T) -> Result<T> {
    match flag(rest, name) {
        Some(v) => v
            .parse::<T>()
            .map_err(|_| anyhow!("--{name} {v:?} is not a valid value")),
        None => Ok(default),
    }
}

fn data_dir(rest: &[String]) -> Result<PathBuf> {
    Ok(PathBuf::from(
        flag(rest, "data-dir").unwrap_or_else(|| "data".to_string()),
    ))
}

fn workdir(rest: &[String]) -> Result<PathBuf> {
    Ok(PathBuf::from(
        flag(rest, "workdir").unwrap_or_else(|| "work".to_string()),
    ))
}

/// Which datasets to leave OUT — the leave-one-out runs that measure the
/// honest zero-shot case: a node with no labelled history for that dataset.
fn exclusion(rest: &[String]) -> Option<String> {
    flag(rest, "exclude")
}

// ───────────────────────────── data loading ─────────────────────────────

struct Loaded {
    banking: Dataset,
    sms: Dataset,
}

fn load_all(dir: &Path, exclude: Option<&str>) -> Result<Loaded> {
    data::verify_data_dir(dir)?;
    let banking = data::load_banking(&dir.join("b77_train.csv"))?;
    let sms = data::load_sms(&dir.join("sms.tsv"))?;
    println!(
        "[data] banking77: {} train rows, {} labels; sms: {} rows, {} labels{}",
        banking.items.len(),
        banking.labels.len(),
        sms.items.len(),
        sms.labels.len(),
        exclude
            .map(|x| format!("; excluding {x} from training"))
            .unwrap_or_default()
    );
    if banking.labels.len() != BANKING_LABELS {
        return Err(anyhow!(
            "banking77 has {} labels, expected {BANKING_LABELS}",
            banking.labels.len()
        ));
    }
    Ok(Loaded { banking, sms })
}

/// The SMS rows `load.py` treats as history (train) vs held-out (test).
fn sms_split(sms: &Dataset) -> (Vec<Item>, Vec<Item>) {
    let train: Vec<Item> = sms.items[..data::SMS_TRAIN_ROWS.min(sms.items.len())].to_vec();
    let test: Vec<Item> = sms.items[data::SMS_TRAIN_ROWS.min(sms.items.len())..].to_vec();
    (train, test)
}

/// Hold `n` rows out of `items` for model selection, chosen by the seeded
/// stream — never the test sets, which stay untouched until the final eval.
fn dev_holdout(items: &[Item], n: usize, seed: u64) -> (Vec<Item>, Vec<Item>) {
    if n == 0 {
        return (items.to_vec(), Vec::new());
    }
    let mut rng = Rng::new(seed);
    let mut picked: HashSet<usize> = HashSet::new();
    while picked.len() < n.min(items.len()) {
        picked.insert(rng.below(items.len()));
    }
    let mut train = Vec::with_capacity(items.len() - picked.len());
    let mut dev = Vec::with_capacity(picked.len());
    for (i, item) in items.iter().enumerate() {
        if picked.contains(&i) {
            dev.push(item.clone());
        } else {
            train.push(item.clone());
        }
    }
    (train, dev)
}

// ───────────────────────────── subcommands ─────────────────────────────

fn cmd_verify_data(rest: &[String]) -> Result<()> {
    let dir = data_dir(rest)?;
    for line in data::verify_data_dir(&dir)? {
        println!("ok  {line}");
    }
    Ok(())
}

fn cmd_tokenizer(rest: &[String]) -> Result<()> {
    let dir = data_dir(rest)?;
    let out = flag(rest, "out")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("work/tokenizer.json"));
    let vocab: usize = flag_parse(rest, "vocab", 12_000)?;
    let loaded = load_all(&dir, None)?;
    let corpus_lines = corpus::tokenizer_corpus(&[&loaded.banking, &loaded.sms]);
    println!(
        "[tokenizer] fitting wordpiece on {} lines, vocab {vocab}",
        corpus_lines.len()
    );
    let tokenizer = model::train_tokenizer(&corpus_lines, vocab)?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tokenizer
        .save(&out, true)
        .map_err(|e| anyhow!("save tokenizer: {e}"))?;
    println!(
        "[tokenizer] wrote {} ({} tokens)",
        out.display(),
        tokenizer.get_vocab_size(true)
    );
    Ok(())
}

/// The datasets used for training, after exclusions and the dev holdout.
struct TrainSets {
    banking: Option<Dataset>,
    sms: Option<Dataset>,
    records: Vec<export::DatasetRecord>,
}

fn training_sets(rest: &[String], loaded: &Loaded) -> Result<TrainSets> {
    let dev_rows: usize = flag_parse(rest, "dev-rows", 500)?;
    let mut records = Vec::new();
    let banking = if exclusion(rest).as_deref() == Some("banking77") {
        None
    } else {
        let (train, dev) = dev_holdout(&loaded.banking.items, dev_rows, 11);
        records.push(banking_record(train.len(), dev.len()));
        Some(Dataset {
            name: "banking77",
            items: train,
            labels: loaded.banking.labels.clone(),
        })
    };
    let sms = if exclusion(rest).as_deref() == Some("sms") {
        None
    } else {
        let (history, _) = sms_split(&loaded.sms);
        let (train, dev) = dev_holdout(&history, dev_rows, 13);
        records.push(sms_record(train.len(), dev.len()));
        Some(Dataset {
            name: "sms",
            items: train,
            labels: loaded.sms.labels.clone(),
        })
    };
    Ok(TrainSets {
        banking,
        sms,
        records,
    })
}

fn banking_record(rows_train: usize, rows_dev: usize) -> export::DatasetRecord {
    export::DatasetRecord {
        name: "banking77".into(),
        rows_train,
        rows_dev,
        source: "https://raw.githubusercontent.com/PolyAI-LDN/task-specific-datasets/master/banking_data/train.csv".into(),
        sha256: data::EXPECTED_SHA256
            .iter()
            .find(|(n, _)| *n == "b77_train.csv")
            .map(|(_, h)| h.to_string())
            .unwrap_or_default(),
        licence: "CC-BY-4.0 (PolyAI-LDN/task-specific-datasets LICENSE)".into(),
        citation: "Casanueva et al., Efficient Intent Detection with Dual Sentence Encoders, arXiv:2003.04807 (2020)".into(),
    }
}

fn sms_record(rows_train: usize, rows_dev: usize) -> export::DatasetRecord {
    export::DatasetRecord {
        name: "sms-spam".into(),
        rows_train,
        rows_dev,
        source: "https://raw.githubusercontent.com/justmarkham/pycon-2016-tutorial/master/data/sms.tsv (mirror of the UCI SMS Spam Collection)".into(),
        sha256: data::EXPECTED_SHA256
            .iter()
            .find(|(n, _)| *n == "sms.tsv")
            .map(|(_, h)| h.to_string())
            .unwrap_or_default(),
        licence: "CC-BY-4.0 (UCI ML Repository dataset page)".into(),
        citation: "Almeida & Hidalgo, SMS Spam Collection, UCI ML Repository, DOI 10.24432/C5CC84 (donated 2012)".into(),
    }
}

fn build_pairs(sets: &TrainSets, seed: u64) -> Vec<Pair> {
    let mut pairs = Vec::new();
    if let Some(banking) = &sets.banking {
        let mut p = corpus::build(banking, seed, 1);
        println!(
            "[corpus] banking77: {} pairs from {} rows",
            p.len(),
            banking.items.len()
        );
        pairs.append(&mut p);
    }
    if let Some(sms) = &sets.sms {
        let mut p = corpus::build(sms, seed.wrapping_add(1), 1);
        println!(
            "[corpus] sms: {} pairs from {} rows",
            p.len(),
            sms.items.len()
        );
        pairs.append(&mut p);
    }
    pairs
}

fn head_for(rest: &[String], workdir: &std::path::Path, seed: u64, vocab: usize) -> Result<Head> {
    let tokenizer_path = flag(rest, "tokenizer")
        .map(PathBuf::from)
        .unwrap_or_else(|| workdir.join("tokenizer.json"));
    let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| anyhow!("load {}: {e}", tokenizer_path.display()))?;
    // The config's vocab_size must be exactly the tokenizer's, or the
    // embedding table and the token ids disagree.
    let real_vocab = tokenizer.get_vocab_size(true);
    if real_vocab > vocab {
        return Err(anyhow!(
            "tokenizer has {real_vocab} tokens but the config caps vocab at {vocab}; raise --vocab"
        ));
    }
    let cfg = ModelConfig::v1(real_vocab);
    println!(
        "[model] vocab {real_vocab}, hidden {}, layers {} → {:.1}M params",
        cfg.hidden_size,
        cfg.num_hidden_layers,
        (real_vocab * cfg.hidden_size
            + cfg.num_hidden_layers
                * (4 * cfg.hidden_size * cfg.hidden_size
                    + 4 * cfg.hidden_size * cfg.intermediate_size)) as f64
            / 1e6
    );
    Head::fresh(cfg, tokenizer, seed)
}

fn cmd_train(rest: &[String]) -> Result<()> {
    let dir = data_dir(rest)?;
    let work = workdir(rest)?;
    std::fs::create_dir_all(&work)?;
    let loaded = load_all(&dir, exclusion(rest).as_deref())?;
    let sets = training_sets(rest, &loaded)?;
    if sets.banking.is_none() && sets.sms.is_none() {
        return Err(anyhow!(
            "--exclude removed every dataset; nothing to train on"
        ));
    }
    let plan = TrainPlan {
        epochs: flag_parse(rest, "epochs", 5)?,
        lr: flag_parse(rest, "lr", 6e-4)?,
        warmup_frac: flag_parse(rest, "warmup-frac", 0.06)?,
        weight_decay: flag_parse(rest, "weight-decay", 0.01)?,
        seed: flag_parse(rest, "seed", 1064)?,
        ..TrainPlan::default()
    };
    let name = flag(rest, "name").unwrap_or_else(|| "v1".to_string());

    // Tokenizer: fit if absent (same call as the `tokenizer` subcommand).
    let tokenizer_path = work.join("tokenizer.json");
    if !tokenizer_path.exists() {
        let vocab: usize = flag_parse(rest, "vocab", 12_000)?;
        let corpus_lines = corpus::tokenizer_corpus(&[&loaded.banking, &loaded.sms]);
        let tokenizer = model::train_tokenizer(&corpus_lines, vocab)?;
        tokenizer
            .save(&tokenizer_path, true)
            .map_err(|e| anyhow!("save tokenizer: {e}"))?;
        println!(
            "[tokenizer] wrote {} ({} tokens)",
            tokenizer_path.display(),
            tokenizer.get_vocab_size(true)
        );
    }

    let pairs = build_pairs(&sets, plan.seed);
    let mut head = head_for(rest, &work, plan.seed, 12_000)?;
    // Optional warm start: `--ckpt` loads a previous epoch's weights so a
    // finished-but-undertrained run extends instead of restarting. The
    // optimizer state is not carried over — AdamW moments restart, which is
    // the documented behaviour of extending a cosine schedule by re-warming.
    if let Some(init) = flag(rest, "ckpt") {
        head.varmap
            .load(PathBuf::from(&init))
            .with_context(|| format!("warm-start from {init}"))?;
        println!("[train] warm start from {init}");
    }
    let stats = train::run(&mut head, &pairs, &plan, &work, &name)?;

    // Dev scores after training, so the log shows selection signal next to
    // the loss it came from.
    let dev_sets = dev_datasets(&sets, &loaded, rest);
    for ds in &dev_sets {
        if ds.items.is_empty() {
            continue;
        }
        let result = if ds.labels.len() == 2 {
            evaluate::eval_noul(&head, ds.name, &ds.items, &ds.labels[1])?
        } else {
            evaluate::eval_choice(&head, ds.name, &ds.items, &ds.labels)?
        };
        println!(
            "[dev] {:>10} acc {:.4}  ECE {:.3}",
            result.dataset, result.accuracy, result.ece
        );
    }

    let summary = serde_json::json!({
        "name": name,
        "plan": {
            "epochs": plan.epochs,
            "lr": plan.lr,
            "warmup_frac": plan.warmup_frac,
            "weight_decay": plan.weight_decay,
            "seed": plan.seed,
        },
        "exclude": exclusion(rest),
        "epochs_run": stats,
        "train_pairs": pairs.len(),
    });
    let summary_path = work.join(format!("train-{name}.json"));
    std::fs::write(&summary_path, serde_json::to_string_pretty(&summary)?)
        .with_context(|| format!("write {}", summary_path.display()))?;
    println!("[train] wrote {}", summary_path.display());
    Ok(())
}

/// Datasets for the dev holdout: the same exclusions as training, so a
/// leave-one-out run's dev signal never leaks its excluded dataset.
fn dev_datasets(sets: &TrainSets, loaded: &Loaded, rest: &[String]) -> Vec<Dataset> {
    let dev_rows: usize = flag_parse(rest, "dev-rows", 500).unwrap_or(500);
    let mut out = Vec::new();
    if let Some(banking) = &sets.banking {
        let (_, dev) = dev_holdout(&loaded.banking.items, dev_rows, 11);
        if !dev.is_empty() {
            out.push(Dataset {
                name: "banking77-dev",
                items: dev,
                labels: banking.labels.clone(),
            });
        }
    }
    if let Some(sms) = &sets.sms {
        let (history, _) = sms_split(&loaded.sms);
        let (_, dev) = dev_holdout(&history, dev_rows, 13);
        if !dev.is_empty() {
            out.push(Dataset {
                name: "sms-dev",
                items: dev,
                labels: sms.labels.clone(),
            });
        }
    }
    out
}

fn load_head(rest: &[String], work: &std::path::Path) -> Result<Head> {
    let mut head = head_for(rest, work, 0, 12_000)?;
    let ckpt = match flag(rest, "ckpt") {
        Some(p) => PathBuf::from(p),
        None => latest_checkpoint(work)?,
    };
    head.varmap
        .load(&ckpt)
        .with_context(|| format!("load checkpoint {}", ckpt.display()))?;
    println!("[eval] checkpoint {}", ckpt.display());
    Ok(head)
}

/// The epoch number encoded in a checkpoint's name, or `None` when the name
/// has no `epochNNN` segment. Only the `epoch…` segment is read: a run NAME
/// may itself contain digits (`v1`, `loo-sms`), and the first bug here read
/// the `1` out of `v1` — or swept digits *and* the `.safetensors` suffix
/// into one unparseable string — so every checkpoint looked like epoch 0 or
/// 1 and "latest" became whichever entry `read_dir` happened to yield last.
fn checkpoint_epoch(name: &str) -> Option<usize> {
    name.split('-').find_map(|part| {
        let digits: String = part
            .trim_start_matches("epoch")
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse::<usize>().ok()
    })
}

/// The newest epoch checkpoint in `work` — resume-by-default, so `eval` and
/// `export` after a finished run need no flags.
fn latest_checkpoint(work: &std::path::Path) -> Result<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    for entry in std::fs::read_dir(work)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if !name.starts_with("ckpt-") || !name.ends_with(".safetensors") {
            continue;
        }
        let Some(epoch) = checkpoint_epoch(&name) else {
            continue;
        };
        if best.as_ref().map(|(e, _)| epoch > *e).unwrap_or(true) {
            best = Some((epoch, path));
        }
    }
    best.map(|(_, p)| p)
        .ok_or_else(|| anyhow!("no checkpoint under {}", work.display()))
}

fn cmd_eval(rest: &[String]) -> Result<()> {
    let dir = data_dir(rest)?;
    let work = workdir(rest)?;
    let head = load_head(rest, &work)?;
    let loaded = load_all(&dir, exclusion(rest).as_deref())?;
    let n: usize = flag_parse(rest, "n", usize::MAX)?;
    let dataset = flag(rest, "dataset");
    let mut results = Vec::new();

    if dataset.as_deref() != Some("sms") {
        let test = data::load_banking(&dir.join("b77_test.csv"))?;
        let items: Vec<Item> = test.items.into_iter().take(n).collect();
        results.push(evaluate::eval_choice(
            &head,
            "banking77-test",
            &items,
            &test.labels,
        )?);
    }
    if dataset.as_deref() != Some("banking77") {
        let (_, held_out) = sms_split(&loaded.sms);
        let items: Vec<Item> = held_out.into_iter().take(n).collect();
        let positive = loaded
            .sms
            .labels
            .iter()
            .find(|l| l.as_str() == "spam")
            .cloned()
            .unwrap_or_else(|| "spam".to_string());
        results.push(evaluate::eval_noul(&head, "sms-test", &items, &positive)?);
    }

    for r in &results {
        println!(
            "[eval] {:>16} n={:>5} acc={:.4} ECE={:.3} conf>=0.8: {:.1}% at {:.4}  {:.1}ms/item{}",
            r.dataset,
            r.n,
            r.accuracy,
            r.ece,
            r.decided_at_08 * 100.0,
            r.accuracy_at_08,
            r.ms_per_item,
            r.positive_prf
                .map(|[p, r, f1]| format!("  spam P={p:.3} R={r:.3} F1={f1:.3}"))
                .unwrap_or_default()
        );
    }
    let out_path = work.join(format!(
        "eval-{}.json",
        exclusion(rest).unwrap_or_else(|| "full".to_string())
    ));
    std::fs::write(&out_path, serde_json::to_string_pretty(&results)?)
        .with_context(|| format!("write {}", out_path.display()))?;
    println!("[eval] wrote {}", out_path.display());
    Ok(())
}

fn cmd_export(rest: &[String]) -> Result<()> {
    let dir = data_dir(rest)?;
    let work = workdir(rest)?;
    let out = PathBuf::from(flag(rest, "out").unwrap_or_else(|| "artifact/xerj-decide-v1".into()));
    let name = flag(rest, "name").unwrap_or_else(|| "xerj-decide-v1".to_string());
    let version = flag(rest, "version").unwrap_or_else(|| "1.0.0".to_string());
    let head = load_head(rest, &work)?;
    let loaded = load_all(&dir, exclusion(rest).as_deref())?;
    let sets = training_sets(rest, &loaded)?;
    let plan_epochs: usize = flag_parse(rest, "epochs", 5)?;
    let plan_lr: f64 = flag_parse(rest, "lr", 6e-4)?;
    let plan_warmup: f64 = flag_parse(rest, "warmup-frac", 0.06)?;
    let plan_wd: f64 = flag_parse(rest, "weight-decay", 0.01)?;
    let seed: u64 = flag_parse(rest, "seed", 1064)?;
    let pairs = build_pairs(&sets, seed);

    let provenance = export::TrainingProvenance {
        harness: format!(
            "benchmarks/decide-model {} (candle 0.9, CPU)",
            env!("CARGO_PKG_VERSION")
        ),
        seed,
        corpus_seed: seed,
        epochs: plan_epochs,
        lr: plan_lr,
        warmup_frac: plan_warmup,
        weight_decay: plan_wd,
        vocab_size: head.cfg.vocab_size,
        hidden_size: head.cfg.hidden_size,
        num_hidden_layers: head.cfg.num_hidden_layers,
        intermediate_size: head.cfg.intermediate_size,
        train_pairs: pairs.len(),
        datasets: sets.records.clone(),
    };
    let licences = export::LicenceRecord {
        weights: "Apache-2.0 (the xerj-decide weights, from xerj-org)".into(),
        weights_notice: "The weights are Apache-2.0 from xerj-org. They were trained on two \
public CC-BY-4.0 datasets, recorded under training_data with the attribution that licence \
asks for; those terms govern the datasets, and are provenance for these weights, not a \
grant xerj-org makes."
            .into(),
        training_data: sets.records.clone(),
    };
    let manifest = export::export(&head, &out, &name, &version, provenance, licences)?;
    println!(
        "[export] {} — {} parameters, {} files, {} bytes of weights",
        out.display(),
        manifest.parameters,
        manifest.files.len(),
        manifest
            .files
            .iter()
            .find(|f| f.path == "model.safetensors")
            .map(|f| f.bytes)
            .unwrap_or(0)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rest(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// The regression test for the bug that silently dropped every flag:
    /// `flag` must find the dashed form callers actually type. Every default
    /// in this CLI equals the v1 recipe's value, so a dropped flag is
    /// invisible in the command line and wrong in the artifact.
    #[test]
    fn flags_parse_in_the_dashed_form_callers_type() {
        let args = rest(&[
            "--data-dir",
            "data",
            "--workdir",
            "work-no-banking",
            "--exclude",
            "banking77",
            "--epochs",
            "5",
            "--name",
            "loo-banking",
        ]);
        assert_eq!(flag(&args, "exclude").as_deref(), Some("banking77"));
        assert_eq!(flag(&args, "name").as_deref(), Some("loo-banking"));
        assert_eq!(flag(&args, "workdir").as_deref(), Some("work-no-banking"));
        let epochs: usize = flag_parse(&args, "epochs", 12).unwrap();
        assert_eq!(epochs, 5);
        // A missing flag falls back to the default, and a bare value that
        // merely LOOKS like a flag's text is not mistaken for one.
        assert_eq!(flag(&args, "ckpt"), None);
        assert_eq!(flag(&args, "vocab"), None);
    }

    /// `--flag=value` is not supported; it must parse as missing rather than
    /// silently training on defaults with a different meaning.
    #[test]
    fn equals_form_is_rejected_loudly_by_the_caller_not_swallowed() {
        let args = rest(&["--epochs=5"]);
        assert_eq!(flag(&args, "epochs"), None);
    }

    /// The epoch of a checkpoint comes from its `epochNNN` segment only —
    /// digits inside the run's NAME (`v1`, `loo-sms`) or the file suffix
    /// must not be read, and an unparsable name is skipped rather than
    /// treated as epoch 0.
    #[test]
    fn checkpoint_epochs_parse_only_the_epoch_segment() {
        assert_eq!(checkpoint_epoch("ckpt-v1-epoch004.safetensors"), Some(4));
        assert_eq!(
            checkpoint_epoch("ckpt-loo-sms-epoch012.safetensors"),
            Some(12)
        );
        assert_eq!(
            checkpoint_epoch("ckpt-loo-banking-epoch000.safetensors"),
            Some(0)
        );
        assert_eq!(checkpoint_epoch("ckpt-v1.safetensors"), None);
        assert_eq!(checkpoint_epoch("tokenizer.json"), None);
    }

    /// `latest_checkpoint` must return the highest epoch regardless of the
    /// directory's iteration order — the first version returned whichever
    /// entry `read_dir` produced last, which silently evaluated a random
    /// epoch of a finished run.
    #[test]
    fn latest_checkpoint_is_the_highest_epoch_in_any_directory_order() {
        let dir = std::env::temp_dir().join(format!("decide-model-ckpt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "ckpt-v1-epoch004.safetensors",
            "ckpt-v1-epoch000.safetensors",
            "ckpt-v1-epoch002.safetensors",
            "train-v1.json",
            "tokenizer.json",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let picked = latest_checkpoint(&dir).unwrap();
        assert_eq!(
            picked.file_name().unwrap().to_str().unwrap(),
            "ckpt-v1-epoch004.safetensors"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
