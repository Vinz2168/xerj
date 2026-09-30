//! The training loop: AdamW over the pair corpus, epoch-checkpointed.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use candle_nn::{AdamW, Optimizer, ParamsAdamW};

use crate::corpus::Pair;
use crate::model::{pair_loss, target_histogram, Head, MAX_BATCH_ROWS};
use crate::rng::{stream_seed, Rng};

/// Everything that shapes a run. Defaults are the v1 recipe; every field is
/// a CLI flag so the card's numbers always name the exact recipe that made
/// them.
#[derive(Debug, Clone)]
pub struct TrainPlan {
    pub epochs: usize,
    pub lr: f64,
    pub min_lr_frac: f64,
    /// Fraction of total steps spent warming up linearly from ~0.
    pub warmup_frac: f64,
    pub beta1: f64,
    pub beta2: f64,
    pub eps: f64,
    pub weight_decay: f64,
    pub seed: u64,
    /// Pairs are grouped for length-homogeneous batches in windows of this
    /// many pairs: small enough to keep the shuffle, large enough to keep
    /// the padding waste low.
    pub bucket_window: usize,
}

impl Default for TrainPlan {
    fn default() -> Self {
        Self {
            epochs: 5,
            lr: 6e-4,
            min_lr_frac: 0.05,
            warmup_frac: 0.06,
            beta1: 0.9,
            beta2: 0.999,
            eps: 1e-8,
            weight_decay: 0.01,
            seed: 1064,
            bucket_window: 24 * MAX_BATCH_ROWS,
        }
    }
}

/// One epoch's outcome — the training log, machine-readable.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EpochStat {
    pub epoch: usize,
    pub steps: usize,
    pub pairs: usize,
    pub mean_loss: f32,
    pub seconds: f32,
    pub lr_first: f64,
    pub lr_last: f64,
}

/// AdamW with bias correction, as candle ships it — the reference
/// implementation of Loshchilov & Hutter's decoupled-weight-decay Adam
/// (arXiv:1711.05101); nothing here is a bespoke optimiser.
struct Schedule {
    total_steps: usize,
    warmup_steps: usize,
    base_lr: f64,
    min_lr: f64,
}

impl Schedule {
    /// Step `t` (0-based) to a learning rate: linear warmup, then cosine
    /// decay to `min_lr_frac * base_lr`. Cosine schedules for transformers
    /// are from Vaswani et al. 2017's original warmup-plus-decay family;
    /// the cosine form specifically follows the widely-replicated BERT
    /// fine-tuning recipe (Devlin et al. 2019, and the fix of Rong 2019).
    fn lr_at(&self, t: usize) -> f64 {
        if self.total_steps == 0 {
            return self.base_lr;
        }
        if t < self.warmup_steps {
            return self.base_lr * (t as f64 + 1.0) / self.warmup_steps as f64;
        }
        let span = self.total_steps.saturating_sub(self.warmup_steps).max(1);
        let progress = (t - self.warmup_steps) as f64 / span as f64;
        let cos = (std::f64::consts::PI * progress).cos();
        self.min_lr + 0.5 * (self.base_lr - self.min_lr) * (1.0 + cos)
    }
}

/// Train on `pairs`, checkpointing `workdir/ckpt-<name>-<epoch>.safetensors`
/// after every epoch (so a long run resumes rather than restarts), and
/// return per-epoch stats.
pub fn run(
    head: &mut Head,
    pairs: &[Pair],
    plan: &TrainPlan,
    workdir: &Path,
    name: &str,
) -> Result<Vec<EpochStat>> {
    // Encode once: token ids do not change across epochs, and re-tokenising
    // 40k pairs per epoch is pure waste.
    let encoded = head.encode(
        &pairs
            .iter()
            .map(|p| (p.premise.clone(), p.hypothesis.clone()))
            .collect::<Vec<_>>(),
    )?;
    let lengths: Vec<usize> = encoded
        .iter()
        .map(|e| e.get_ids().len().min(crate::model::MAX_TOKENS))
        .collect();

    let total_steps_est = plan.epochs * (pairs.len() / MAX_BATCH_ROWS + 1);
    let schedule = Schedule {
        total_steps: total_steps_est,
        warmup_steps: ((total_steps_est as f64) * plan.warmup_frac) as usize,
        base_lr: plan.lr,
        min_lr: plan.lr * plan.min_lr_frac,
    };

    // A warmup forward materialises every variable in the VarMap, so the
    // optimiser is built over the complete parameter set (a VarMap grows
    // lazily as modules first ask for their tensors).
    let warmup = head.forward_rows(&encoded, &[0usize], &lengths, lengths[0].max(1))?;
    drop(warmup);
    let mut opt = AdamW::new(
        head.varmap.all_vars(),
        ParamsAdamW {
            lr: schedule.lr_at(0),
            beta1: plan.beta1,
            beta2: plan.beta2,
            eps: plan.eps,
            weight_decay: plan.weight_decay,
        },
    )
    .map_err(|e| anyhow::anyhow!("build adamw: {e}"))?;

    let mut stats = Vec::with_capacity(plan.epochs);
    let mut global_step = 0usize;
    for epoch in 0..plan.epochs {
        let started = Instant::now();
        // Epoch-seeded order: resume after a crash reproduces the same epoch.
        let mut order: Vec<usize> = (0..pairs.len()).collect();
        Rng::new(stream_seed(plan.seed, epoch)).shuffle(&mut order);

        let mut loss_sum = 0f32;
        let mut seen = 0usize;
        let mut steps = 0usize;
        let mut lr_first = None;
        let mut lr_last = None;
        for window in order.chunks(plan.bucket_window) {
            // Sort inside the window only: the shuffle still governs which
            // pairs meet in a batch, while each batch is length-homogeneous.
            let mut rows: Vec<usize> = window.to_vec();
            rows.sort_by_key(|&i| lengths[i]);
            for batch in rows.chunks(MAX_BATCH_ROWS) {
                let seq_len = batch.iter().map(|&i| lengths[i]).max().unwrap_or(0).max(1);
                let targets: Vec<usize> = batch.iter().map(|&i| pairs[i].target).collect();
                let lr = schedule.lr_at(global_step);
                opt.set_learning_rate(lr);
                lr_first.get_or_insert(lr);
                lr_last = Some(lr);
                let probs = head.forward_rows(&encoded, batch, &lengths, seq_len)?;
                let loss = pair_loss(&probs, &targets)?;
                let value = loss.to_scalar::<f32>().unwrap_or(f32::NAN);
                if !value.is_finite() {
                    anyhow::bail!(
                        "loss went non-finite at epoch {epoch} step {steps} (lr {lr:.2e}); \
                         lower --lr or shorten --epochs rather than shipping NaNs"
                    );
                }
                opt.backward_step(&loss)
                    .map_err(|e| anyhow::anyhow!("step: {e}"))?;
                loss_sum += value * batch.len() as f32;
                seen += batch.len();
                steps += 1;
                global_step += 1;
            }
        }
        let stat = EpochStat {
            epoch,
            steps,
            pairs: seen,
            mean_loss: loss_sum / seen.max(1) as f32,
            seconds: started.elapsed().as_secs_f32(),
            lr_first: lr_first.unwrap_or(plan.lr),
            lr_last: lr_last.unwrap_or(plan.lr),
        };
        println!(
            "[train:{name}] epoch {:>2}  loss {:.4}  steps {}  pairs {}  hist {:?}  {:.1}s",
            stat.epoch,
            stat.mean_loss,
            stat.steps,
            stat.pairs,
            target_histogram(&pairs.iter().map(|p| p.target).collect::<Vec<_>>()),
            stat.seconds,
        );
        head.save_checkpoint(&checkpoint_path(workdir, name, stat.epoch))?;
        stats.push(stat);
    }
    Ok(stats)
}

/// Where an epoch checkpoint lands.
pub fn checkpoint_path(workdir: &Path, name: &str, epoch: usize) -> PathBuf {
    workdir.join(format!("ckpt-{name}-epoch{epoch:03}.safetensors"))
}
