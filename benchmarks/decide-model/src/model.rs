//! The head: one candle `ModernBertForSequenceClassification`, built trainable
//! and served through the same forward call the server uses.
//!
//! `engine/crates/xerj-ai/src/decide.rs` loads this architecture from
//! `model.safetensors` through `VarBuilder::from_mmaped_safetensors`. Here the
//! same `load` runs against a `VarMap`-backed `VarBuilder`, so training and
//! serving execute the identical `candle_transformers` forward — there is no
//! second implementation of the architecture to drift. Tensor names, and the
//! three-file directory layout (`config.json`, `tokenizer.json`,
//! `model.safetensors`), come out of the box.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::{VarBuilder, VarMap};
use candle_transformers::models::modernbert::{Config, ModernBertForSequenceClassification};
use tokenizers::{Encoding, Tokenizer, Trainer, TruncationParams};

use crate::corpus::ENTAILMENT;

/// Cap on tokens per pair — the loader's `MAX_TOKENS`, mirrored so training
/// never sees a sequence the server would truncate differently.
pub const MAX_TOKENS: usize = 512;
/// Rows per forward pass — the loader's `MAX_BATCH_ROWS`, mirrored so the
/// training loss and the serving batch shapes agree.
pub const MAX_BATCH_ROWS: usize = 64;
/// The loader's `PADDED_TOKEN_BUDGET`, mirrored.
pub const PADDED_TOKEN_BUDGET: usize = 4_096;

/// Shape of the head. Sized for the tier's job: short SMS- and ticket-shaped
/// payloads, scored once per candidate label on CPU, where every parameter is
/// paid N times per request.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct ModelConfig {
    pub vocab_size: usize,
    pub hidden_size: usize,
    pub num_hidden_layers: usize,
    pub num_attention_heads: usize,
    pub intermediate_size: usize,
    pub max_position_embeddings: usize,
    pub layer_norm_eps: f64,
    pub pad_token_id: u32,
    pub global_attn_every_n_layers: usize,
    pub global_rope_theta: f64,
    pub local_attention: usize,
    pub local_rope_theta: f64,
}

impl ModelConfig {
    /// The v1 shape: 6 layers × 256 hidden, wordpiece vocabulary fitted to
    /// the training corpora. ~8.3M parameters, 33 MB as F32 safetensors —
    /// inside the loader's ≤ 300 MB download budget with room to spare.
    pub fn v1(vocab_size: usize) -> Self {
        Self {
            vocab_size,
            hidden_size: 256,
            num_hidden_layers: 6,
            num_attention_heads: 4,
            intermediate_size: 768,
            max_position_embeddings: MAX_TOKENS,
            layer_norm_eps: 1e-12,
            pad_token_id: 0,
            global_attn_every_n_layers: 3,
            global_rope_theta: 10_000.0,
            local_attention: 128,
            local_rope_theta: 10_000.0,
        }
    }

    /// The candle config the loader parses, with the NLI classifier block
    /// flattened in at the top level — the shape the loader's fixture proved.
    pub fn candle(&self) -> Config {
        let id2label: HashMap<String, String> = crate::corpus::ID2LABEL
            .iter()
            .enumerate()
            .map(|(i, l)| (i.to_string(), l.to_string()))
            .collect();
        let label2id: HashMap<String, String> = crate::corpus::ID2LABEL
            .iter()
            .enumerate()
            .map(|(i, l)| (l.to_string(), i.to_string()))
            .collect();
        Config {
            vocab_size: self.vocab_size,
            hidden_size: self.hidden_size,
            num_hidden_layers: self.num_hidden_layers,
            num_attention_heads: self.num_attention_heads,
            intermediate_size: self.intermediate_size,
            max_position_embeddings: self.max_position_embeddings,
            layer_norm_eps: self.layer_norm_eps,
            pad_token_id: self.pad_token_id,
            global_attn_every_n_layers: self.global_attn_every_n_layers,
            global_rope_theta: self.global_rope_theta,
            local_attention: self.local_attention,
            local_rope_theta: self.local_rope_theta,
            classifier_config: Some(candle_transformers::models::modernbert::ClassifierConfig {
                id2label,
                label2id,
                classifier_pooling: candle_transformers::models::modernbert::ClassifierPooling::CLS,
            }),
        }
    }

    /// `config.json` as written into the model directory — the JSON the
    /// server's `serde` parse accepts (same field set as the loader's own
    /// fixture, so an operator can diff a trained dir against the fixture).
    pub fn config_json(self) -> serde_json::Value {
        let id2label: serde_json::Map<String, serde_json::Value> = crate::corpus::ID2LABEL
            .iter()
            .enumerate()
            .map(|(i, l)| (i.to_string(), serde_json::json!(l)))
            .collect();
        let label2id: serde_json::Map<String, serde_json::Value> = crate::corpus::ID2LABEL
            .iter()
            .enumerate()
            .map(|(i, l)| (l.to_string(), serde_json::json!(i.to_string())))
            .collect();
        serde_json::json!({
            "model_type": "modernbert",
            "architectures": ["ModernBertForSequenceClassification"],
            "vocab_size": self.vocab_size,
            "hidden_size": self.hidden_size,
            "num_hidden_layers": self.num_hidden_layers,
            "num_attention_heads": self.num_attention_heads,
            "intermediate_size": self.intermediate_size,
            "max_position_embeddings": self.max_position_embeddings,
            "layer_norm_eps": self.layer_norm_eps,
            "pad_token_id": self.pad_token_id,
            "global_attn_every_n_layers": self.global_attn_every_n_layers,
            "global_rope_theta": self.global_rope_theta,
            "local_attention": self.local_attention,
            "local_rope_theta": self.local_rope_theta,
            "id2label": id2label,
            "label2id": label2id,
            "classifier_pooling": "cls",
        })
    }
}

/// Train a wordpiece tokenizer on `corpus` — the same normalizer,
/// pre-tokenizer and pair post-processing the loader's fixture uses, so a
/// pair encodes to `[CLS] premise [SEP] hypothesis [SEP]` on both sides.
pub fn train_tokenizer(corpus: &[String], vocab_size: usize) -> Result<Tokenizer> {
    use tokenizers::models::wordpiece::{WordPiece, WordPieceTrainer};
    use tokenizers::normalizers::BertNormalizer;
    use tokenizers::pre_tokenizers::bert::BertPreTokenizer;
    use tokenizers::processors::template::TemplateProcessing;
    use tokenizers::AddedToken;

    // Train the vocab through the Trainer trait directly (`feed` then
    // `train`): `Tokenizer::train` is generic over the wrapper's model type
    // and the wordpiece trainer is not, so the feed closure below mirrors
    // the one `TokenizerImpl::train` itself uses (tokenizers-0.21.4
    // src/tokenizer/mod.rs, `train` → `feed`) — normalize, pre-tokenize,
    // hand the splits to the trainer.
    let mut model = WordPiece::builder()
        .unk_token("[UNK]".into())
        .continuing_subword_prefix("##".into())
        .build()
        .map_err(|e| anyhow!("build wordpiece: {e}"))?;
    let normalizer = BertNormalizer::default();
    let pre_tokenizer = BertPreTokenizer;
    let mut trainer = WordPieceTrainer::builder()
        .vocab_size(vocab_size)
        .min_frequency(2)
        .show_progress(false)
        .special_tokens(vec![
            AddedToken::from("[PAD]", true),
            AddedToken::from("[UNK]", true),
            AddedToken::from("[CLS]", true),
            AddedToken::from("[SEP]", true),
            AddedToken::from("[MASK]", true),
        ])
        .build();
    {
        use tokenizers::{Normalizer, OffsetReferential, OffsetType, PreTokenizer};
        trainer
            .feed(corpus.iter().cloned(), |seq| {
                let mut normalized = tokenizers::NormalizedString::from(seq);
                normalizer.normalize(&mut normalized)?;
                let mut pre_tokenized = tokenizers::PreTokenizedString::from(normalized);
                pre_tokenizer.pre_tokenize(&mut pre_tokenized)?;
                Ok(pre_tokenized
                    .get_splits(OffsetReferential::Original, OffsetType::Byte)
                    .into_iter()
                    .map(|(s, _, _)| s.to_owned())
                    .collect::<Vec<_>>())
            })
            .map_err(|e| anyhow!("feed wordpiece trainer: {e}"))?;
    }
    let added = trainer
        .train(&mut model)
        .map_err(|e| anyhow!("train wordpiece: {e}"))?;
    let mut tokenizer = Tokenizer::new(model);
    tokenizer.with_normalizer(Some(BertNormalizer::default()));
    tokenizer.with_pre_tokenizer(Some(BertPreTokenizer));
    tokenizer.add_tokens(&added);

    let post = TemplateProcessing::builder()
        .try_single("[CLS] $A [SEP]")
        .map_err(|e| anyhow!("single template: {e}"))?
        .try_pair("[CLS] $A [SEP] $B:1 [SEP]:1")
        .map_err(|e| anyhow!("pair template: {e}"))?
        .special_tokens(vec![("[CLS]", 2u32), ("[SEP]", 3u32)])
        .build()
        .map_err(|e| anyhow!("template processor: {e}"))?;
    tokenizer.with_post_processor(Some(post));
    tokenizer.with_padding(None);
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: MAX_TOKENS,
            ..Default::default()
        }))
        .map_err(|e| anyhow!("tokenizer truncation: {e}"))?;
    Ok(tokenizer)
}

/// The trainable head plus its tokenizer.
pub struct Head {
    pub varmap: VarMap,
    pub model: ModernBertForSequenceClassification,
    pub tokenizer: Tokenizer,
    pub cfg: ModelConfig,
    pub device: Device,
}

impl Head {
    /// Build with fresh weights. `seed` drives [`init_deterministic`] — the
    /// module constructors draw from candle's CPU RNG, which cannot be seeded
    /// (`Device::set_seed` is a `bail!` on CPU in candle 0.9), so every
    /// tensor is re-initialised from the harness's own splitmix64 stream
    /// right after the modules materialise. One seed, one init, on any
    /// machine, forever.
    pub fn fresh(cfg: ModelConfig, tokenizer: Tokenizer, seed: u64) -> Result<Self> {
        let device = Device::Cpu;
        let varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);
        let model = ModernBertForSequenceClassification::load(vb, &cfg.candle())
            .map_err(|e| anyhow!("build modernbert classifier: {e}"))?;
        let mut head = Self {
            varmap,
            model,
            tokenizer,
            cfg,
            device,
        };
        // Materialise every variable (a VarMap grows lazily as modules first
        // ask for their tensors), then replace each with a deterministic
        // value — including the ones the loader would have randomised.
        head.materialise()?;
        init_deterministic(&mut head.varmap, &head.device, seed)?;
        Ok(head)
    }

    /// One dummy forward so every module asks its VarBuilder for its tensors.
    fn materialise(&self) -> Result<()> {
        let ids = Tensor::from_vec(vec![0u32, 0u32], (1, 2), &self.device)?;
        let mask = Tensor::from_vec(vec![1u32, 1u32], (1, 2), &self.device)?;
        self.model
            .forward(&ids, &mask)
            .map_err(|e| anyhow!("materialise: {e}"))?;
        Ok(())
    }

    /// Encode (premise, hypothesis) pairs, no padding — padding is per batch.
    pub fn encode(&self, pairs: &[(String, String)]) -> Result<Vec<Encoding>> {
        self.tokenizer
            .encode_batch(pairs.to_vec(), true)
            .map_err(|e| anyhow!("encode pairs: {e}"))
    }

    /// One rectangular forward pass over `rows`; the classifier's softmax
    /// probabilities per row. Mirrors `DecideModel::forward_padded`.
    pub fn forward_rows(
        &self,
        encodings: &[Encoding],
        rows: &[usize],
        lengths: &[usize],
        seq_len: usize,
    ) -> Result<Tensor> {
        let batch = rows.len();
        let mut ids: Vec<u32> = Vec::with_capacity(batch * seq_len);
        let mut mask: Vec<u32> = Vec::with_capacity(batch * seq_len);
        for &row in rows {
            let len = lengths[row];
            ids.extend_from_slice(&encodings[row].get_ids()[..len]);
            ids.resize(ids.len() + (seq_len - len), self.cfg.pad_token_id);
            mask.extend_from_slice(&encodings[row].get_attention_mask()[..len]);
            mask.resize(mask.len() + (seq_len - len), 0);
        }
        let input_ids = Tensor::from_vec(ids, (batch, seq_len), &self.device)
            .map_err(|e| anyhow!("input_ids: {e}"))?;
        let attention_mask = Tensor::from_vec(mask, (batch, seq_len), &self.device)
            .map_err(|e| anyhow!("attention_mask: {e}"))?;
        self.model
            .forward(&input_ids, &attention_mask)
            .map_err(|e| anyhow!("modernbert forward: {e}"))
    }

    /// Batch plan for a set of token lengths — the loader's
    /// `group_by_padded_cost` (xerj-ai/src/microbatch.rs), reimplemented here
    /// so the standalone crate does not depend on the engine: sort by length,
    /// then pack rows up to `MAX_BATCH_ROWS` and `PADDED_TOKEN_BUDGET`.
    pub fn batch_plan(lengths: &[usize]) -> Vec<Vec<usize>> {
        let mut order = (0..lengths.len()).collect::<Vec<_>>();
        order.sort_by_key(|&i| lengths[i]);
        let mut batches = Vec::new();
        let mut batch: Vec<usize> = Vec::new();
        let mut longest = 0usize;
        for i in order {
            let length = lengths[i];
            let next_longest = longest.max(length);
            if !batch.is_empty()
                && (batch.len() >= MAX_BATCH_ROWS
                    || next_longest.saturating_mul(batch.len() + 1) > PADDED_TOKEN_BUDGET)
            {
                batches.push(std::mem::take(&mut batch));
                longest = 0;
            }
            longest = longest.max(length);
            batch.push(i);
        }
        if !batch.is_empty() {
            batches.push(batch);
        }
        batches
    }

    /// Entailment probability per pair, input order, then per-question
    /// renormalisation — the serving computation
    /// (`DecideModel::score_blocking`) applied to `(premise, hypotheses)`.
    pub fn score(&self, premise: &str, hypotheses: &[String]) -> Result<Vec<f32>> {
        if hypotheses.is_empty() {
            return Err(anyhow!("no hypotheses to score"));
        }
        let pairs: Vec<(String, String)> = hypotheses
            .iter()
            .map(|h| (premise.to_string(), h.clone()))
            .collect();
        let encodings = self.encode(&pairs)?;
        let lengths: Vec<usize> = encodings
            .iter()
            .map(|e| e.get_ids().len().min(MAX_TOKENS))
            .collect();
        let mut entailment = vec![0f32; lengths.len()];
        for rows in Self::batch_plan(&lengths) {
            let seq_len = rows.iter().map(|&i| lengths[i]).max().unwrap_or(0);
            if seq_len == 0 {
                continue;
            }
            let probs = self.forward_rows(&encodings, &rows, &lengths, seq_len)?;
            let column = probs
                .i((.., ENTAILMENT))
                .map_err(|e| anyhow!("entailment column: {e}"))?
                .to_vec1::<f32>()
                .map_err(|e| anyhow!("entailment probs: {e}"))?;
            for (row, p) in rows.into_iter().zip(column) {
                entailment[row] = p;
            }
        }
        let total: f32 = entailment.iter().sum();
        if !total.is_finite() || total <= 0.0 {
            return Err(anyhow!(
                "zero entailment mass for a question's hypotheses; the server refuses to \
                 fabricate a uniform distribution and so does the trainer"
            ));
        }
        Ok(entailment.iter().map(|p| p / total).collect())
    }

    /// Persist the VarMap (checkpoint / resume).
    pub fn save_checkpoint(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.varmap
            .save(path)
            .with_context(|| format!("save checkpoint {}", path.display()))
    }
}

/// Cross-entropy from the classifier's softmax probabilities. The forward
/// already softmaxes, so `-log p_target` *is* the CE loss; the clamp keeps
/// `log` finite for a confidently-wrong row instead of producing an inf
/// gradient that poisons the step. The whole expression stays in tensors —
/// pulling values out to the host would cut the autodiff graph.
pub fn pair_loss(probs: &Tensor, targets: &[usize]) -> Result<Tensor> {
    let idx = Tensor::from_vec(
        targets.iter().map(|&t| t as u32).collect::<Vec<_>>(),
        (targets.len(), 1),
        probs.device(),
    )
    .map_err(|e| anyhow!("targets tensor: {e}"))?;
    let picked = probs
        .gather(&idx, 1)
        .map_err(|e| anyhow!("gather target probabilities: {e}"))?;
    picked
        .clamp(1e-9, 1.0)
        .and_then(|p| p.log())
        .and_then(|l| l.mean_all())
        .and_then(|m| m.affine(-1.0, 0.0))
        .map_err(|e| anyhow!("cross-entropy: {e}"))
}

/// Count of each target row in a batch — a sanity printer for training logs.
pub fn target_histogram(targets: &[usize]) -> [usize; 3] {
    let mut hist = [0usize; 3];
    for &t in targets {
        hist[t.min(2)] += 1;
    }
    hist
}

/// Deterministic initialisation of every tensor in the VarMap.
///
/// candle's CPU RNG cannot be seeded, so instead of inheriting whatever the
/// module constructors drew, each tensor is written from the harness's
/// splitmix64 stream with the standard scheme for this architecture:
///
/// * `*norm.weight` → 1 (a LayerNorm starts as the identity),
/// * token embeddings → N(0, 0.02²) — BERT's embedding init (Devlin et
///   al. 2019 §A.2, and the same 0.02 ModernBERT trains from),
/// * every other weight (all ModernBERT linears are bias-free) →
///   uniform(−b, b) with `b = 1/√fan_in` — Kaiming-uniform with
///   a = √5, i.e. PyTorch's `nn.Linear` default (He et al. 2015 for the
///   family; the a=√5 variance argument is PyTorch's).
///
/// The stream is consumed in sorted-name order, so the mapping of random
/// values to tensors does not depend on a HashMap's iteration order.
pub fn init_deterministic(varmap: &mut VarMap, device: &Device, seed: u64) -> Result<()> {
    let mut rng = crate::rng::Rng::new(seed ^ 0x1064_5eed_0000_0001);
    let mut names: Vec<String> = {
        let data = varmap
            .data()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        data.keys().cloned().collect()
    };
    names.sort();
    for name in names {
        let (shape, is_norm, is_embedding) = {
            let data = varmap
                .data()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let var = data
                .get(&name)
                .ok_or_else(|| anyhow!("variable {name} vanished mid-init"))?;
            let dims = var.dims().to_vec();
            (
                dims,
                name.contains("norm.weight"),
                name == "model.embeddings.tok_embeddings.weight",
            )
        };
        let len: usize = shape.iter().product();
        let values: Vec<f32> = if is_norm {
            vec![1.0; len]
        } else if is_embedding {
            (0..len).map(|_| rng.normal(0.0, 0.02)).collect()
        } else {
            // fan_in is the input width: candle's linear weight is [out, in].
            let fan_in = shape.last().copied().unwrap_or(1).max(1) as f32;
            let bound = 1.0 / fan_in.sqrt();
            (0..len).map(|_| (rng.unit() * 2.0 - 1.0) * bound).collect()
        };
        let tensor = Tensor::from_vec(values, shape.as_slice(), device)
            .map_err(|e| anyhow!("init {name}: {e}"))?;
        varmap
            .set_one(&name, &tensor)
            .map_err(|e| anyhow!("set {name}: {e}"))?;
    }
    Ok(())
}
