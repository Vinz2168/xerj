//! Held-out measurement: accuracy and calibration on the datasets' own test
//! splits, scored exactly the way the server scores a request.
//!
//! The ECE here is the same estimator as the history-vote benchmark's
//! `eval.py` (10 equal-width bins) and as `xerj-common`'s `calibration.rs`
//! (`min(floor(p·10), 9)`): bin edges agree on every value, so the two
//! surfaces' calibration numbers are directly comparable.

use crate::corpus::{hypothesis, negation_hypothesis};
use crate::data::Item;
use crate::model::Head;

/// 10-bin expected calibration error, `Σ_b (n_b/N)·|acc_b − conf_b|`.
pub fn ece(conf_correct: &[(f32, bool)]) -> f32 {
    let bins = 10usize;
    let n = conf_correct.len();
    if n == 0 {
        return 0.0;
    }
    let mut count = vec![0usize; bins];
    let mut sum_c = vec![0f64; bins];
    let mut sum_k = vec![0f64; bins];
    for &(c, k) in conf_correct {
        let b = ((c * bins as f32) as usize).min(bins - 1);
        count[b] += 1;
        sum_c[b] += c as f64;
        sum_k[b] += k as i32 as f64;
    }
    let mut total = 0f64;
    for b in 0..bins {
        if count[b] > 0 {
            let acc = sum_k[b] / count[b] as f64;
            let conf = sum_c[b] / count[b] as f64;
            total += count[b] as f64 / n as f64 * (acc - conf).abs();
        }
    }
    total as f32
}

/// What one dataset's run measured.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EvalResult {
    pub dataset: String,
    pub mode: &'static str,
    pub n: usize,
    pub labels: usize,
    pub correct: usize,
    pub accuracy: f32,
    pub ece: f32,
    /// Share of items decided at confidence ≥ 0.8, and accuracy among them —
    /// the gate columns the history-vote table reports.
    pub decided_at_08: f32,
    pub accuracy_at_08: f32,
    pub mean_confidence: f32,
    /// Spam precision/recall/F1, for the binary dataset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub positive_prf: Option<[f32; 3]>,
    /// Wall-clock scoring time per item, milliseconds (includes tokenising).
    pub ms_per_item: f32,
}

/// Score a `choice`: every label's hypothesis competes, the winner is the
/// arg-max of the renormalised entailment distribution, the winner's share is
/// the confidence. This is `/v1/systemone`'s choice answer, computed by the
/// same `score` call the local tier uses.
pub fn eval_choice(
    head: &Head,
    dataset: &str,
    items: &[Item],
    labels: &[String],
) -> Result<EvalResult, anyhow::Error> {
    let hypotheses: Vec<String> = labels.iter().map(|l| hypothesis(l)).collect();
    let started = std::time::Instant::now();
    let mut conf_correct: Vec<(f32, bool)> = Vec::with_capacity(items.len());
    for item in items {
        let probs = head.score(&item.text, &hypotheses)?;
        let (best, conf) = probs
            .iter()
            .enumerate()
            .max_by(|(a, pa), (b, pb)| pa.partial_cmp(pb).unwrap().then(a.cmp(b)))
            .map(|(i, p)| (i, *p))
            .expect("non-empty hypotheses");
        conf_correct.push((conf, labels[best] == item.label));
    }
    Ok(finish(
        dataset,
        "choice",
        items,
        labels.len(),
        conf_correct,
        None,
        started,
    ))
}

/// Score a `noul`: the positive label's hypothesis against its own negation,
/// predicted positive when the positive carries the majority of the two-way
/// mass — the same two-hypothesis comparison the local tier scores.
pub fn eval_noul(
    head: &Head,
    dataset: &str,
    items: &[Item],
    positive: &str,
) -> Result<EvalResult, anyhow::Error> {
    let started = std::time::Instant::now();
    let mut conf_correct: Vec<(f32, bool)> = Vec::with_capacity(items.len());
    let mut tp = 0usize;
    let mut fp = 0usize;
    let mut fn_ = 0usize;
    for item in items {
        let probs = head.score(
            &item.text,
            &[hypothesis(positive), negation_hypothesis(positive)],
        )?;
        let p = probs[0];
        let predicted_positive = p > 0.5;
        let gold_positive = item.label == positive;
        conf_correct.push((p.max(1.0 - p), predicted_positive == gold_positive));
        tp += usize::from(predicted_positive && gold_positive);
        fp += usize::from(predicted_positive && !gold_positive);
        fn_ += usize::from(!predicted_positive && gold_positive);
    }
    let precision = tp as f32 / (tp + fp).max(1) as f32;
    let recall = tp as f32 / (tp + fn_).max(1) as f32;
    let f1 = 2.0 * precision * recall / (precision + recall).max(1e-9);
    Ok(finish(
        dataset,
        "noul",
        items,
        2,
        conf_correct,
        Some([precision, recall, f1]),
        started,
    ))
}

#[allow(clippy::too_many_arguments)]
fn finish(
    dataset: &str,
    mode: &'static str,
    _items: &[Item],
    labels: usize,
    conf_correct: Vec<(f32, bool)>,
    positive_prf: Option<[f32; 3]>,
    started: std::time::Instant,
) -> EvalResult {
    let n = conf_correct.len();
    let correct = conf_correct.iter().filter(|(_, k)| *k).count();
    let hi: Vec<bool> = conf_correct.iter().map(|&(c, _)| c >= 0.8).collect();
    let hi_n = hi.iter().filter(|h| **h).count();
    let hi_correct = conf_correct
        .iter()
        .zip(hi.iter())
        .filter(|(_, h)| **h)
        .filter(|((_, k), _)| *k)
        .count();
    EvalResult {
        dataset: dataset.to_string(),
        mode,
        n,
        labels,
        correct,
        accuracy: correct as f32 / n.max(1) as f32,
        ece: ece(&conf_correct),
        decided_at_08: hi_n as f32 / n.max(1) as f32,
        accuracy_at_08: if hi_n > 0 {
            hi_correct as f32 / hi_n as f32
        } else {
            0.0
        },
        mean_confidence: conf_correct.iter().map(|(c, _)| *c).sum::<f32>() / n.max(1) as f32,
        positive_prf,
        ms_per_item: started.elapsed().as_secs_f32() * 1000.0 / n.max(1) as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ece_is_zero_for_a_perfectly_calibrated_curve() {
        // 10 items per bin, each bin's confidence equal to its hit rate.
        let mut pairs = Vec::new();
        for b in 0..10 {
            let conf = (b as f32 + 0.5) / 10.0;
            for i in 0..10 {
                pairs.push((conf, i < (conf * 10.0) as usize));
            }
        }
        assert!(ece(&pairs) < 0.06, "coarse binning leaves a small residue");
    }

    #[test]
    fn ece_matches_the_benchmark_estimator_on_a_known_case() {
        // eval.py's ece(): bin edges `b/10 < c <= (b+1)/10`, c == 0 in bin 0.
        let pairs = vec![
            (0.95, true),
            (0.95, false),
            (0.10, true),
            (0.85, true),
            (0.85, true),
        ];
        // bin9 {(0.95,T),(0.95,F)} -> |0.5-0.95| = 0.45, weight 2/5 -> 0.18
        // bin1 {(0.10,T)}          -> |1.0-0.10| = 0.90, weight 1/5 -> 0.18
        //   (eval.py's (b/10, (b+1)/10] edges put c=0.10 in bin 0 — same
        //    contribution, since bin 0's edges span the same conf value)
        // bin8 {(0.85,T) x2}       -> |1.0-0.85| = 0.15, weight 2/5 -> 0.06
        // total 0.42
        assert!((ece(&pairs) - 0.42).abs() < 1e-5, "got {}", ece(&pairs));
    }
}
