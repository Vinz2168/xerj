//! Export: write the model directory `--decide-model-dir` expects, with a
//! manifest, and prove the written artifact re-loads through the file-backed
//! path the server uses.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::modernbert::ModernBertForSequenceClassification;

use crate::corpus::{hypothesis, negation_hypothesis};
use crate::data::sha256_file;
use crate::model::Head;

/// The three files `DecideModel::load` requires, by name.
pub const REQUIRED_FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];

/// The manifest — provenance and integrity for a published directory. Every
/// field is a fact the run can state; nothing is asserted on behalf of a
/// dataset or a licence we do not hold.
#[derive(Debug, serde::Serialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub architecture: String,
    pub parameters: usize,
    pub dtype: &'static str,
    pub files: Vec<ManifestFile>,
    pub training: TrainingProvenance,
    pub licences: LicenceRecord,
}

#[derive(Debug, serde::Serialize)]
pub struct ManifestFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, serde::Serialize)]
pub struct TrainingProvenance {
    pub harness: String,
    pub seed: u64,
    pub corpus_seed: u64,
    pub epochs: usize,
    pub lr: f64,
    pub warmup_frac: f64,
    pub weight_decay: f64,
    pub vocab_size: usize,
    pub hidden_size: usize,
    pub num_hidden_layers: usize,
    pub intermediate_size: usize,
    pub train_pairs: usize,
    pub datasets: Vec<DatasetRecord>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DatasetRecord {
    pub name: String,
    pub rows_train: usize,
    pub rows_dev: usize,
    pub source: String,
    pub sha256: String,
    pub licence: String,
    pub citation: String,
}

/// Licence statements. The weights are Apache-2.0 from us; the datasets'
/// terms are recorded as the provenance of what the weights were derived
/// from, with the attribution CC BY 4.0 asks for — they are not a grant we
/// make over anyone else's work.
#[derive(Debug, serde::Serialize)]
pub struct LicenceRecord {
    pub weights: String,
    pub weights_notice: String,
    pub training_data: Vec<DatasetRecord>,
}

/// Write `dir` in the loader's layout, plus `VERSION.json` and
/// `MANIFEST.sha256`, then reload the written weights through
/// `VarBuilder::from_mmaped_safetensors` and require identical scores on a
/// fixed probe — the same load path the server's `DecideModel::load` takes,
/// so an artifact that cannot serve is never published.
#[allow(clippy::too_many_arguments)]
pub fn export(
    head: &Head,
    dir: &Path,
    name: &str,
    version: &str,
    provenance: TrainingProvenance,
    licences: LicenceRecord,
) -> Result<Manifest> {
    std::fs::create_dir_all(dir).with_context(|| format!("create model dir {}", dir.display()))?;

    // config.json — the JSON the loader's serde parse accepts.
    let config_json = head.cfg.config_json();
    std::fs::write(
        dir.join("config.json"),
        serde_json::to_string_pretty(&config_json)?,
    )
    .with_context(|| format!("write {}", dir.join("config.json").display()))?;

    // tokenizer.json.
    head.tokenizer
        .save(dir.join("tokenizer.json"), true)
        .map_err(|e| anyhow!("save tokenizer.json: {e}"))?;

    // model.safetensors — F32, under the names the loader asks for.
    let tensors: HashMap<String, Tensor> = head
        .varmap
        .data()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .map(|(k, v)| (k.clone(), v.as_tensor().clone()))
        .collect();
    let parameters: usize = tensors.values().map(|t| t.elem_count()).sum();
    if parameters == 0 {
        return Err(anyhow!(
            "the VarMap holds no tensors; refusing to publish an empty artifact"
        ));
    }
    let mut names: Vec<&String> = tensors.keys().collect();
    names.sort();
    println!(
        "[export] {} tensors, {} parameters ({}..{}…)",
        names.len(),
        parameters,
        names[0],
        names[1.min(names.len() - 1)]
    );
    candle_core::safetensors::save(&tensors, dir.join("model.safetensors"))
        .with_context(|| format!("write {}", dir.join("model.safetensors").display()))?;

    let manifest = Manifest {
        name: name.to_string(),
        version: version.to_string(),
        architecture: "ModernBertForSequenceClassification (candle-transformers 0.9)".to_string(),
        parameters,
        dtype: "F32",
        files: manifest_files(dir, &REQUIRED_FILES)?,
        training: provenance,
        licences,
    };
    std::fs::write(
        dir.join("VERSION.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "name": manifest.name,
            "version": manifest.version,
            "architecture": manifest.architecture,
            "parameters": manifest.parameters,
            "dtype": manifest.dtype,
        }))?,
    )?;
    std::fs::write(dir.join("MANIFEST.sha256"), render_manifest(&manifest))?;

    // The load-back proof.
    verify_loads(head, dir)?;
    Ok(manifest)
}

/// sha256 + size for each required file, in the order the loader checks them.
fn manifest_files(dir: &Path, files: &[&str]) -> Result<Vec<ManifestFile>> {
    let mut out = Vec::with_capacity(files.len() + 1);
    for name in files {
        let path = dir.join(name);
        let bytes = std::fs::metadata(&path)
            .with_context(|| format!("stat {}", path.display()))?
            .len();
        out.push(ManifestFile {
            path: (*name).to_string(),
            bytes,
            sha256: sha256_file(&path)?,
        });
    }
    // VERSION.json is optional at listing time: a fresh export has it (it is
    // written beside the manifest), an operator-supplied directory may not.
    let version_path = dir.join("VERSION.json");
    if version_path.exists() {
        let bytes = std::fs::metadata(&version_path)?.len();
        out.push(ManifestFile {
            path: "VERSION.json".to_string(),
            bytes,
            sha256: sha256_file(&version_path)?,
        });
    }
    Ok(out)
}

/// The `sha256sum -c`-shaped text, so an operator verifies a download with a
/// stock tool: `cd xerj-decide-v1 && sha256sum -c MANIFEST.sha256`.
pub fn render_manifest(manifest: &Manifest) -> String {
    let mut out = String::new();
    out.push_str("# xerj-decide model manifest — verify with:\n");
    out.push_str("#   sha256sum -c MANIFEST.sha256\n");
    for file in &manifest.files {
        out.push_str(&format!("{}  {}\n", file.sha256, file.path));
    }
    out.push_str("\n--- provenance ---\n");
    out.push_str(&format!("name:          {}\n", manifest.name));
    out.push_str(&format!("version:       {}\n", manifest.version));
    out.push_str(&format!("architecture:  {}\n", manifest.architecture));
    out.push_str(&format!("parameters:    {}\n", manifest.parameters));
    out.push_str(&format!("dtype:         {}\n", manifest.dtype));
    let t = &manifest.training;
    out.push_str(&format!(
        "harness:       {} (seed {}, corpus seed {})\n",
        t.harness, t.seed, t.corpus_seed
    ));
    out.push_str(&format!(
        "recipe:        {} epochs, lr {}, warmup {}, wd {}, vocab {}, hidden {}x{}\n",
        t.epochs,
        t.lr,
        t.warmup_frac,
        t.weight_decay,
        t.vocab_size,
        t.num_hidden_layers,
        t.hidden_size
    ));
    out.push_str(&format!("train pairs:   {}\n", t.train_pairs));
    for ds in &t.datasets {
        out.push_str(&format!(
            "trained on:    {} — {} train / {} dev rows, sha256 {}, {}\n",
            ds.name, ds.rows_train, ds.rows_dev, ds.sha256, ds.licence
        ));
    }
    out.push_str(&format!("weights:       {}\n", manifest.licences.weights));
    out
}

/// Reload the exported weights the way the server does and require the same
/// scores on a fixed probe set.
fn verify_loads(head: &Head, dir: &Path) -> Result<()> {
    let weights_path = dir.join("model.safetensors");
    let device = Device::Cpu;
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(
            std::slice::from_ref(&weights_path),
            DType::F32,
            &device,
        )
    }
    .with_context(|| format!("map {}", weights_path.display()))?;
    let reloaded = ModernBertForSequenceClassification::load(vb, &head.cfg.candle())
        .map_err(|e| anyhow!("reload exported weights: {e}"))?;

    let probes: Vec<(String, Vec<String>)> = vec![
        (
            "i still have not received my card".into(),
            vec![
                hypothesis("card_arrival"),
                hypothesis("card_payment_not_recognised"),
                hypothesis("spam"),
            ],
        ),
        (
            "urgent you have won a prize call now".into(),
            vec![hypothesis("spam"), negation_hypothesis("spam")],
        ),
        (
            "hi it is me again about dinner tonight".into(),
            vec![hypothesis("ham"), negation_hypothesis("ham")],
        ),
    ];
    for (premise, hypotheses) in probes {
        // In-memory head.
        let tokenizer = &head.tokenizer;
        let pairs: Vec<(String, String)> = hypotheses
            .iter()
            .map(|h| (premise.clone(), h.clone()))
            .collect();
        let encodings = tokenizer
            .encode_batch(pairs.clone(), true)
            .map_err(|e| anyhow!("encode probe: {e}"))?;
        let lengths: Vec<usize> = encodings
            .iter()
            .map(|e| e.get_ids().len().min(crate::model::MAX_TOKENS))
            .collect();
        let mut expected = vec![0f32; lengths.len()];
        for rows in Head::batch_plan(&lengths) {
            let seq_len = rows.iter().map(|&i| lengths[i]).max().unwrap_or(0).max(1);
            let probs = head.forward_rows(&encodings, &rows, &lengths, seq_len)?;
            let vec = probs
                .to_vec2::<f32>()
                .map_err(|e| anyhow!("read probe probs: {e}"))?;
            for (batch_idx, &row) in rows.iter().enumerate() {
                expected[row] = vec[batch_idx][crate::corpus::ENTAILMENT];
            }
        }
        // Reloaded model, through the loader-shaped call.
        let mut ids: Vec<u32> = Vec::new();
        let mut mask: Vec<u32> = Vec::new();
        let seq_len = lengths.iter().copied().max().unwrap_or(1);
        for enc in &encodings {
            let len = enc.get_ids().len().min(crate::model::MAX_TOKENS);
            ids.extend_from_slice(&enc.get_ids()[..len]);
            ids.resize(ids.len() + (seq_len - len), head.cfg.pad_token_id);
            mask.extend_from_slice(&enc.get_attention_mask()[..len]);
            mask.resize(mask.len() + (seq_len - len), 0);
        }
        let input_ids = Tensor::from_vec(ids, (lengths.len(), seq_len), &device)?;
        let attention_mask = Tensor::from_vec(mask, (lengths.len(), seq_len), &device)?;
        let reloaded_probs = reloaded
            .forward(&input_ids, &attention_mask)
            .map_err(|e| anyhow!("reloaded forward: {e}"))?
            .to_vec2::<f32>()
            .map_err(|e| anyhow!("read reloaded probs: {e}"))?;
        for (i, want) in expected.iter().enumerate() {
            let got = reloaded_probs[i][crate::corpus::ENTAILMENT];
            if (got - want).abs() > 1e-5 {
                return Err(anyhow!(
                    "exported weights score differently after reload: probe pair {i} \
                     in-memory {want:.6} vs reloaded {got:.6}"
                ));
            }
        }
    }
    Ok(())
}
