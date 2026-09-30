//! The training corpus: labelled rows turned into NLI-shaped pairs.
//!
//! The loader's contract (engine/crates/xerj-ai/src/decide.rs) is a
//! sequence-pair scorer whose `id2label` names `entailment`. At serving time
//! a question's labels become hypotheses through two fixed templates:
//!
//! * positive: `This example is {label}.`
//! * negation: `This example is not {label}.`
//!
//! Training therefore uses exactly those templates on exactly the labels the
//! datasets carry — nothing is prettified, because the server passes labels
//! through verbatim and the head must be scored in the form it is served.
//!
//! # The three-way construction
//!
//! For a row `(text, gold)` three pairs are derived:
//!
//! | pair | target | reading |
//! |---|---|---|
//! | `(text, hyp(gold))` | entailment | the payload does say this |
//! | `(text, hyp(other))` | neutral | a wrong label is *not asserted false* by the text — it merely is not entailed |
//! | `(text, neghyp(gold))` | contradiction | the negation of the true label is asserted false |
//!
//! The neutral row is the one that makes the head NLI-shaped rather than
//! binary: a `choice` among N options is scored by comparing entailment
//! across N hypotheses, and most of those hypotheses are neither entailed
//! nor contradicted — exactly the wrong-label case. The negation row is what
//! a `noul` actually scores (positive against its own negation).
//!
//! `other` is drawn uniformly from the dataset's remaining labels with the
//! seeded stream (unbiased [`Rng::below`]), so hard negatives are sampled in
//! proportion to how often they compete, not cherry-picked.

use crate::data::Dataset;
use crate::rng::Rng;

/// The positive template — byte-identical to `xerj_ai::decide::hypothesis`.
pub fn hypothesis(label: &str) -> String {
    format!("This example is {label}.")
}

/// The negation template — byte-identical to
/// `xerj_ai::decide::negation_hypothesis`.
pub fn negation_hypothesis(label: &str) -> String {
    format!("This example is not {label}.")
}

/// The three NLI target rows, in `id2label` index order
/// (0 = contradiction, 1 = neutral, 2 = entailment).
pub const CONTRADICTION: usize = 0;
pub const NEUTRAL: usize = 1;
pub const ENTAILMENT: usize = 2;

pub const ID2LABEL: [&str; 3] = ["contradiction", "neutral", "entailment"];

/// One training pair.
#[derive(Debug, Clone)]
pub struct Pair {
    pub premise: String,
    pub hypothesis: String,
    pub target: usize,
}

/// Build the pair corpus for one dataset.
///
/// `negation_every` controls how often the contradiction row is emitted: the
/// negation of a *specific* gold label is a weaker signal than the
/// entailment/neutral contrast (there are only as many distinct negation
/// sentences as there are labels), so the default emits it for every row —
/// but it stays a knob so the ablation is one flag, not a code change.
pub fn build(dataset: &Dataset, seed: u64, negation_every: usize) -> Vec<Pair> {
    let mut rng = Rng::new(seed ^ 0x1064);
    let mut pairs = Vec::with_capacity(dataset.items.len() * 3);
    for (i, item) in dataset.items.iter().enumerate() {
        pairs.push(Pair {
            premise: item.text.clone(),
            hypothesis: hypothesis(&item.label),
            target: ENTAILMENT,
        });
        // A wrong label, uniform over the rest. A single-label dataset has
        // no wrong label and no neutral row — that is honest, not a bug.
        if dataset.labels.len() > 1 {
            let gold = dataset.label_index(&item.label).expect("label in vocab");
            let mut other = rng.below(dataset.labels.len() - 1);
            if other >= gold {
                other += 1;
            }
            pairs.push(Pair {
                premise: item.text.clone(),
                hypothesis: hypothesis(&dataset.labels[other]),
                target: NEUTRAL,
            });
        }
        if negation_every > 0 && i % negation_every == 0 {
            pairs.push(Pair {
                premise: item.text.clone(),
                hypothesis: negation_hypothesis(&item.label),
                target: CONTRADICTION,
            });
        }
    }
    pairs
}

/// The text the tokenizer is trained on: every premise and every hypothesis
/// the corpus can build, so the label words are in-vocabulary subwords.
pub fn tokenizer_corpus(datasets: &[&Dataset]) -> Vec<String> {
    let mut lines = Vec::new();
    for ds in datasets {
        for item in &ds.items {
            lines.push(item.text.clone());
        }
        for label in &ds.labels {
            lines.push(hypothesis(label));
            lines.push(negation_hypothesis(label));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Item;

    fn tiny() -> Dataset {
        Dataset {
            name: "tiny",
            items: vec![
                Item {
                    text: "where is my card".into(),
                    label: "card_arrival".into(),
                },
                Item {
                    text: "win a prize now".into(),
                    label: "spam".into(),
                },
            ],
            labels: vec!["card_arrival".into(), "spam".into()],
        }
    }

    #[test]
    fn templates_match_the_loader_byte_for_byte() {
        assert_eq!(hypothesis("spam"), "This example is spam.");
        assert_eq!(negation_hypothesis("spam"), "This example is not spam.");
    }

    #[test]
    fn every_row_yields_entailment_neutral_and_negation() {
        let ds = tiny();
        let pairs = build(&ds, 1064, 1);
        assert_eq!(pairs.len(), 6, "2 rows x 3 pairs");
        assert_eq!(pairs[0].target, ENTAILMENT);
        assert_eq!(pairs[1].target, NEUTRAL);
        assert!(
            pairs[1].hypothesis.contains("spam"),
            "wrong label is the other one"
        );
        assert_eq!(pairs[2].target, CONTRADICTION);
        assert_eq!(pairs[2].hypothesis, "This example is not card_arrival.");
    }

    #[test]
    fn the_same_seed_builds_the_same_corpus() {
        let ds = tiny();
        let a = build(&ds, 42, 1);
        let b = build(&ds, 42, 1);
        assert_eq!(a.len(), b.len());
        assert!(a
            .iter()
            .zip(b.iter())
            .all(|(x, y)| x.hypothesis == y.hypothesis));
    }
}
