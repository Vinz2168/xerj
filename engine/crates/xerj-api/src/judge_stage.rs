//! The `judge` stage of `_search` — the zero-token second stage (issue #1060).
//!
//! Runs after the engine has produced and rendered its hits (and after the
//! hosted `rerank` stage, when both blocks are present: the two compose, each
//! stands alone). Unlike [`crate::rerank_stage`], this stage never leaves the
//! node: it hands the emitted page to a [`xerj_rerank::judge::LocalJudge`] in
//! process — no provider, no key, no tokens, no egress. That is the point of
//! the issue's "zero-token rerank": a caller can ask "how likely is each of
//! THESE hits to be relevant, as a number I can threshold" on every search
//! without an operator paying for it.
//!
//! # What the stage owns
//!
//! Two things: the ORDER of `hits.hits` (judged hits first, probability
//! descending, ties keeping the engine's order — a stable sort) and, with
//! `judge.min_p`, the MEMBERSHIP (hits below the threshold are dropped and
//! counted). Everything else in the response stays the engine's, exactly as
//! the rerank stage leaves it: `hits.total`, `aggregations`, `suggest` all
//! describe the full match set.
//!
//! The engine's `_score` is NOT replaced — the one deliberate difference from
//! the hosted stage. `_p_relevant` is added to each judged hit, so a client
//! sorting by `_score` keeps the engine's own number while a client
//! thresholding `_p_relevant` gets an absolute `0..=1` scale `_score` never
//! had. `hits.max_score` still describes `_score`, recomputed only because a
//! `min_p` drop can remove the hit that carried the maximum.
//!
//! # Which judge judged
//!
//! The response's `judged` block names the scorer. A node armed with a local
//! decision head (`XERJ_DECIDE_MODE=local`, feature `decide-local`) judges
//! with that head as a cross-encoder; every other node judges with the
//! deterministic lexical scorer, and SAYS SO — `judged.scorer` is `"lexical"`
//! or `"model"`, and `judged.model` appears only on the model arm, because a
//! response naming a "model" that is arithmetic would be the silent fake this
//! repository refuses everywhere. No quality claim is made for either arm
//! here: the BEIR gate in issue #1060 binds at release.
//!
//! # Strictness
//!
//! Same rules as the rerank stage, for the same reasons (see that module's
//! header): `sort` / `search_after` / `collapse` / `scroll` are refused
//! because the caller already fixed the order or the paging model cannot
//! survive a reorder-and-prune; `_source: false` with nothing that puts text
//! on a hit is refused rather than judged blind; `size: 0` is refused because
//! there is nothing to judge. Surfaces that do not run the stage refuse a body
//! carrying `judge` by name — dropping the block silently returns unjudged
//! hits to a caller who asked for judged ones, the accepted-and-ignored
//! defect class. `"judge": null` is the same as no `judge` key everywhere.

use serde_json::{json, Value};
use std::time::Instant;
use xerj_rerank::judge::{JudgeConfig, LocalJudge};
use xerj_rerank::{Candidate, RerankError};

use crate::es_compat::EsSearchBody;
use crate::responses::EsHit;
use crate::systemone_api::DecideSettings;

// Only the model arm names a model directory; the import (and the arm) exist
// under the same feature that compiles the decide head.
#[cfg(feature = "decide-local")]
use xerj_rerank::judge::ModelJudge;

/// A validated judge request, produced before the search runs.
#[derive(Debug)]
pub struct JudgePlan {
    cfg: JudgeConfig,
    query: String,
}

impl JudgePlan {
    /// Validate the `judge` block against the request it rides on.
    ///
    /// Must run AFTER the URL parameters have been merged into `body`, and
    /// after [`crate::rerank_stage::RerankPlan::prepare`] (the one stage
    /// allowed to widen the page — the judge judges the page as emitted and
    /// widens nothing). Returns `Ok(None)` when the body has no `judge` block;
    /// `Err` carries the reason for a 400.
    pub fn prepare(body: &mut EsSearchBody, scrolling: bool) -> Result<Option<Self>, String> {
        let Some(raw) = body.judge.as_ref() else {
            return Ok(None);
        };
        let cfg = JudgeConfig::from_json(raw).map_err(|e| e.to_string())?;

        if scrolling {
            return Err(
                "`judge` cannot be combined with `scroll`: a scroll streams the engine's \
                 order page by page, and a stage that reorders and prunes a page would \
                 break the scroll's own position mid-stream"
                    .into(),
            );
        }
        if body.sort.is_some() {
            return Err(
                "`judge` cannot be combined with `sort`: an explicit sort already fixes the \
                 order, and judging would reorder around it"
                    .into(),
            );
        }
        if body.search_after.is_some() {
            return Err("`judge` cannot be combined with `search_after`".into());
        }
        if body.collapse.is_some() {
            return Err("`judge` cannot be combined with `collapse`".into());
        }
        // Mirrors the rerank stage's rule: `_source: false` on its own leaves
        // no text on a hit, so there is certainly nothing to judge. Beside a
        // clause that puts values on the hit (`fields`, `docvalue_fields`,
        // `stored_fields`, `script_fields`) it is coherent and is let through
        // to the check on the rendered hits in `apply`.
        let non_empty = |v: &Option<Value>| match v {
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Object(o)) => !o.is_empty(),
            Some(Value::String(s)) => !s.trim().is_empty(),
            _ => false,
        };
        let asks_for_fields = non_empty(&body.fields)
            || non_empty(&body.docvalue_fields)
            || non_empty(&body.stored_fields)
            || non_empty(&body.script_fields);
        if matches!(body.source, Some(Value::Bool(false))) && !asks_for_fields {
            return Err(
                "`judge` needs document text: `_source: false` with no `fields` (or \
                 `docvalue_fields`, `stored_fields`, `script_fields`) clause leaves nothing \
                 to judge. Return the text through `fields`, or drop `_source: false`"
                    .into(),
            );
        }
        if body.size == 0 {
            return Err(
                "`judge` cannot be combined with `size: 0`: the response returns no hits, \
                 so there is nothing to judge. Drop `judge` for an aggregation-only request, \
                 or ask for at least one hit"
                    .into(),
            );
        }

        // The same inference rule as the rerank stage: read the question only
        // from the shapes that carry exactly one, refuse to guess otherwise.
        let query = match cfg.query.clone() {
            Some(q) => q,
            None => body
                .query
                .as_ref()
                .and_then(crate::rerank_stage::infer_query)
                .ok_or_else(|| {
                    "`judge.query` is required: the search query is not a shape a single \
                     question can be read from (supported for inference: match, match_phrase, \
                     multi_match, semantic, simple_query_string). A `bool`, a `hybrid`, a \
                     top-level `knn` with no text query, or no `query` at all needs the \
                     question spelled out"
                        .to_string()
                })?,
        };
        // `judge.query` was checked against its ceiling by the parser; an
        // inferred question is the same string on the same wire and gets the
        // same ceiling.
        if cfg.query.is_none() {
            xerj_rerank::check_inferred_question(&query).map_err(|e| e.to_string())?;
        }

        Ok(Some(Self { cfg, query }))
    }

    /// Judge `hits` in place and return the `judged` block for the response.
    ///
    /// `Err` means a fault the caller must see (see
    /// [`xerj_rerank::Policy`]): a config problem is a 400, a local model that
    /// cannot load or score is a 503. There is deliberately no degrade path —
    /// a caller who asked for judged hits and silently got unjudged ones (or
    /// a different scorer than the response names) has been misled.
    pub async fn apply(
        &self,
        decide: &DecideSettings,
        hits: &mut Vec<EsHit>,
        max_score: &mut Option<f64>,
    ) -> Result<Value, RerankError> {
        let started = Instant::now();
        let judge = local_judge_for(decide);

        // The judge reads the hit exactly as the response carries it — the
        // same builder the hosted stage uses, so the two stages can never
        // drift on what text a hit carries.
        let all: Vec<Candidate> = hits
            .iter()
            .enumerate()
            .map(|(i, h)| {
                crate::rerank_stage::unrestricted_candidate(
                    i,
                    h,
                    xerj_rerank::DEFAULT_MAX_DOC_CHARS,
                )
            })
            .collect();
        // A hit with no prose at all is not judged: the verdict on nothing
        // means nothing, and the lexical arm would score it a meaningless 0.0
        // next to a real miss that EARNED its 0.0.
        let sendable: Vec<Candidate> = all
            .iter()
            .filter(|c| c.title.is_some() || !c.text.trim().is_empty())
            .cloned()
            .collect();

        if !hits.is_empty() && sendable.is_empty() {
            return Err(RerankError::Config(
                "nothing to judge: none of the hits carries a string field in the response. \
                 The judge only sees what the response returns — make sure `_source` (or \
                 `fields`) returns the text fields"
                    .into(),
            ));
        }

        let scores = judge.judge(&self.query, &sendable).await?;
        let judged = scores.len();

        // Judged probabilities by the caller's ordinal. One verdict per hit:
        // `sendable` holds each ordinal once, and the judge contract returns
        // one `Scored` per candidate in input order.
        let mut by_ordinal: Vec<Option<f64>> = vec![None; hits.len()];
        for s in &scores {
            if let Some(slot) = by_ordinal.get_mut(s.ordinal) {
                *slot = Some(s.score);
            }
        }

        // Judged hits first, probability descending. `sort_by` is stable, so
        // equal probabilities keep the engine's relative order — the judge
        // breaks ties toward what the first stage said.
        let mut ordered: Vec<(usize, f64)> = by_ordinal
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.map(|p| (i, p)))
            .collect();
        ordered.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut slots: Vec<Option<EsHit>> = std::mem::take(hits).into_iter().map(Some).collect();
        let mut out = Vec::with_capacity(slots.len());
        let mut kept = 0usize;
        let mut dropped = 0usize;
        let mut unjudged = 0usize;
        for (ordinal, p) in ordered {
            // Take the slot FIRST. A `continue` before the take would leave
            // the hit in its slot, and the sweep over the leftovers below
            // would count the same dropped hit twice.
            let Some(mut h) = slots.get_mut(ordinal).and_then(Option::take) else {
                continue;
            };
            if self.cfg.min_p.is_some_and(|min| p < min) {
                dropped += 1;
                continue;
            }
            // The engine's `_score` stays; the probability is its own field.
            h.p_relevant = Some(p);
            kept += 1;
            out.push(h);
        }
        // What is left in the slots is the hits the judge never scored (no
        // text at all). With a threshold they cannot be shown to clear it, so
        // they are dropped and counted; without one they follow in the
        // engine's order, marked unjudged by the absence of `_p_relevant`.
        for h in slots.into_iter().flatten() {
            if self.cfg.min_p.is_some() {
                dropped += 1;
            } else {
                unjudged += 1;
                out.push(h);
            }
        }
        *hits = out;

        // `max_score` describes the emitted page's `_score`, which the judge
        // did not touch — but a `min_p` drop may have removed the hit that
        // carried the maximum, so it is recomputed over what is left.
        *max_score = hits
            .iter()
            .filter_map(|h| h.score)
            .fold(None, |acc, s| match acc {
                Some(m) if m >= s => Some(m),
                _ => Some(s),
            });

        let mut info = json!({
            "applied": true,
            "scorer": judge.name(),
            "query": self.query,
            "judged": judged,
            "kept": kept,
            "dropped": dropped,
            "took_ms": started.elapsed().as_millis() as u64,
        });
        // Present only when the caller set one: `min_p` is the pruning
        // threshold, absent means "judge and reorder, drop nothing".
        if let Some(min_p) = self.cfg.min_p {
            info["min_p"] = json!(min_p);
        }
        // Present only when something was left unjudged.
        if unjudged > 0 {
            info["unjudged"] = json!(unjudged);
        }
        // The model arm only: never name a "model" that is arithmetic.
        if let Some(model) = judge.model_id() {
            info["model"] = json!(model);
        }
        Ok(info)
    }
}

/// Which local judge this node runs, from the decide settings.
///
/// A node with the local decide tier armed judges with that head — the SAME
/// loaded copy the decide ladder uses (the handle shares the process-wide
/// model registry), one model in memory, two questions asked of it. A node
/// without one (mode `history`, feature not compiled in, no model directory)
/// judges with the deterministic lexical scorer, and the response says so in
/// `judged.scorer`. That is a named fallback, not a degrade: nothing failed,
/// and no caller was told a model judged when arithmetic did.
fn local_judge_for(decide: &DecideSettings) -> LocalJudge {
    #[cfg(feature = "decide-local")]
    {
        if let Some(tier) = decide.local_tier() {
            return LocalJudge::Model(ModelJudge::from_model_dir(tier.model_dir().to_path_buf()));
        }
        LocalJudge::Lexical
    }
    #[cfg(not(feature = "decide-local"))]
    {
        let _ = decide;
        LocalJudge::Lexical
    }
}

/// Whether a raw search body carries a `judge` block that means something.
///
/// `"judge": null` is treated as absent everywhere — the same rule as
/// `rerank` (see [`crate::rerank_stage::carries_rerank`]). One rule.
pub fn carries_judge(body: &Value) -> bool {
    body.get("judge").is_some_and(|j| !j.is_null())
}

/// Whether a raw search body carries EITHER second-stage block (`rerank` or
/// `judge`). For the surfaces that run neither and refuse both — `_msearch`,
/// the search templates, `_rank_eval` — so a body carrying only `judge` does
/// not get a refusal that names `rerank`.
pub fn carries_second_stage(body: &Value) -> bool {
    crate::rerank_stage::carries_rerank(body) || carries_judge(body)
}

/// HTTP status for a judge fault that must reach the caller.
pub fn status_for(e: &RerankError) -> u16 {
    match e {
        RerankError::Config(_) => 400,
        // The judge's local model could not load or could not score the page:
        // server state, not the caller's request and not a gateway fault —
        // the same class the rerank stage gives its own unconfigured-server
        // cases.
        RerankError::LocalModel(_) => 503,
        other => crate::rerank_stage::status_for(other),
    }
}

/// ES-shaped error `type` for a judge fault: a request the caller can fix is
/// an `illegal_argument_exception` like every other 400; the rest are XERJ's,
/// and this stage's — not `rerank_exception`, which would name the stage that
/// did not run.
pub fn error_type_for(e: &RerankError) -> &'static str {
    match e {
        RerankError::Config(_) => "illegal_argument_exception",
        RerankError::LocalModel(_) => "judge_exception",
        other => crate::rerank_stage::error_type_for(other),
    }
}

/// Why an endpoint that does not run the judge stage refuses the block. One
/// sentence, shared by every surface, so they cannot tell a caller three
/// different things.
pub fn unsupported_reason(endpoint: &str) -> String {
    format!(
        "`judge` is not supported on {endpoint}: only `POST /{{index}}/_search` runs the judge \
         stage. Send this search there, or remove the `judge` block — it is refused here \
         rather than silently ignored"
    )
}

/// The 400 for an endpoint that does not run the judge stage.
///
/// Same rule as the rerank stage's: parsers here ignore keys they do not
/// know, so without this a `judge` block vanished without a word and the
/// caller got unjudged, unpruned hits back under a 200.
pub fn unsupported_on(endpoint: &str) -> Value {
    let reason = unsupported_reason(endpoint);
    json!({
        "error": {
            "root_cause": [{ "type": "illegal_argument_exception", "reason": reason }],
            "type": "illegal_argument_exception",
            "reason": reason,
        },
        "status": 400,
    })
}

/// The 400 for a surface that refuses BOTH second stages, naming the block
/// the body actually carries: `_msearch` with a `judge` block must not tell
/// the caller about `rerank`.
pub fn unsupported_second_stage_on(endpoint: &str, body: &Value) -> Value {
    if crate::rerank_stage::carries_rerank(body) {
        crate::rerank_stage::unsupported_on(endpoint)
    } else {
        unsupported_on(endpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search_body(judge: Value) -> EsSearchBody {
        let mut body: EsSearchBody = serde_json::from_value(json!({
            "query": {"match": {"body": "vitamin d"}},
            "size": 10,
            "judge": judge,
        }))
        .expect("a valid body");
        body.judge = Some(judge);
        body
    }

    #[test]
    fn no_judge_block_is_inert() {
        let mut body: EsSearchBody =
            serde_json::from_value(json!({"query": {"match_all": {}}})).unwrap();
        assert!(JudgePlan::prepare(&mut body, false).unwrap().is_none());
    }

    #[test]
    fn null_judge_block_is_inert() {
        let mut body: EsSearchBody =
            serde_json::from_value(json!({"query": {"match_all": {}}, "judge": null})).unwrap();
        assert!(JudgePlan::prepare(&mut body, false).unwrap().is_none());
    }

    #[test]
    fn parses_min_p_and_defers_the_query_to_inference() {
        let mut body = search_body(json!({"local": true, "min_p": 0.5}));
        body.judge = Some(json!({"local": true, "min_p": 0.5}));
        body.query = Some(json!({"match": {"body": "vitamin d"}}));
        let plan = JudgePlan::prepare(&mut body, false)
            .expect("valid")
            .expect("a plan");
        assert_eq!(plan.query, "vitamin d");
    }

    #[test]
    fn refuses_the_shapes_it_cannot_judge_coherently() {
        let scrolling = {
            let mut b = search_body(json!({}));
            b.judge = Some(json!({}));
            JudgePlan::prepare(&mut b, true)
        };
        assert!(scrolling.unwrap_err().contains("scroll"));

        let mut b = search_body(json!({}));
        b.sort = Some(json!("_doc"));
        let err = JudgePlan::prepare(&mut b, false).unwrap_err();
        assert!(err.contains("`sort`"), "{err}");

        let mut b = search_body(json!({}));
        b.search_after = Some(json!([1]));
        let err = JudgePlan::prepare(&mut b, false).unwrap_err();
        assert!(err.contains("`search_after`"), "{err}");

        let mut b = search_body(json!({}));
        b.collapse = Some(json!({"field": "title"}));
        let err = JudgePlan::prepare(&mut b, false).unwrap_err();
        assert!(err.contains("`collapse`"), "{err}");

        let mut b = search_body(json!({}));
        b.size = 0;
        let err = JudgePlan::prepare(&mut b, false).unwrap_err();
        assert!(err.contains("`size: 0`"), "{err}");

        // `_source: false` alone: nothing to judge.
        let mut b = search_body(json!({}));
        b.source = Some(json!(false));
        let err = JudgePlan::prepare(&mut b, false).unwrap_err();
        assert!(err.contains("nothing"), "{err}");
        // ...but beside a `fields` clause it is coherent ("judge on that").
        b.fields = Some(json!(["body"]));
        assert!(JudgePlan::prepare(&mut b, false).is_ok());
    }

    #[test]
    fn a_question_is_required_when_the_query_has_no_single_one() {
        let mut b = search_body(json!({}));
        b.judge = Some(json!({}));
        b.query = Some(json!({"bool": {"must": [{"match": {"a": "x"}}]}}));
        let err = JudgePlan::prepare(&mut b, false).unwrap_err();
        assert!(err.contains("`judge.query` is required"), "{err}");
        // Spelled out, the same body judges.
        b.judge = Some(json!({"query": "the question"}));
        assert!(JudgePlan::prepare(&mut b, false).is_ok());
    }

    #[test]
    fn config_errors_from_the_parser_surface_as_prepare_errors() {
        let mut b = search_body(json!({"local": false}));
        b.judge = Some(json!({"local": false}));
        let err = JudgePlan::prepare(&mut b, false).unwrap_err();
        assert!(err.contains("cannot be false"), "{err}");

        let mut b = search_body(json!({"min_p": 1.5}));
        b.judge = Some(json!({"min_p": 1.5}));
        let err = JudgePlan::prepare(&mut b, false).unwrap_err();
        assert!(err.contains("between 0 and 1"), "{err}");

        let mut b = search_body(json!({"windwo": 1}));
        b.judge = Some(json!({"windwo": 1}));
        let err = JudgePlan::prepare(&mut b, false).unwrap_err();
        assert!(err.contains("unknown `judge` field `windwo`"), "{err}");
    }

    #[test]
    fn fault_statuses() {
        assert_eq!(status_for(&RerankError::Config("x".into())), 400);
        assert_eq!(status_for(&RerankError::LocalModel("no dir".into())), 503);
        assert_eq!(
            error_type_for(&RerankError::Config("x".into())),
            "illegal_argument_exception"
        );
        assert_eq!(
            error_type_for(&RerankError::LocalModel("no dir".into())),
            "judge_exception"
        );
    }

    #[test]
    fn second_stage_detection_and_refusals_name_the_block_carried() {
        assert!(carries_judge(&json!({"judge": {}})));
        assert!(!carries_judge(&json!({"judge": null})));
        assert!(!carries_judge(&json!({})));
        assert!(carries_second_stage(&json!({"rerank": {}})));
        assert!(carries_second_stage(&json!({"judge": {}})));
        assert!(!carries_second_stage(&json!({"size": 10})));

        // A body carrying only `judge` gets the `judge` refusal; one carrying
        // `rerank` keeps the rerank refusal.
        let judge_only = unsupported_second_stage_on("_msearch", &json!({"judge": {}}));
        assert!(judge_only["error"]["reason"]
            .as_str()
            .unwrap()
            .starts_with("`judge` is not supported on _msearch"));
        let rerank_too = unsupported_second_stage_on("_msearch", &json!({"rerank": {}}));
        assert!(rerank_too["error"]["reason"]
            .as_str()
            .unwrap()
            .starts_with("`rerank` is not supported on _msearch"));
        assert_eq!(unsupported_on("_search/scroll")["status"], 400);
        assert!(unsupported_reason("the native search API").contains("`judge`"));
    }

    // ── apply ─────────────────────────────────────────────────────────────

    fn hit(id: &str, score: f64, text: &str) -> EsHit {
        serde_json::from_value(json!({
            "_index": "i", "_id": id, "_score": score,
            "_source": {"body": text},
            "matched_queries": null
        }))
        .expect("an EsHit")
    }

    fn plan(min_p: Option<f64>) -> JudgePlan {
        let cfg = JudgeConfig::from_json(&match min_p {
            Some(p) => json!({"query": "vitamin", "min_p": p}),
            None => json!({"query": "vitamin"}),
        })
        .expect("a valid block");
        JudgePlan {
            cfg,
            query: "vitamin".into(),
        }
    }

    fn ids(hits: &[EsHit]) -> Vec<String> {
        hits.iter().map(|h| h.id.clone()).collect()
    }

    #[tokio::test]
    async fn judges_reorders_adds_p_and_leaves_score_alone() {
        // Engine order by BM25 says b, a, c. The lexical judge (query
        // "vitamin", page-local IDF) says a — both query words repeated —
        // then b — one occurrence — then c, which shares no query word and
        // scores exactly 0.0.
        let mut hits = vec![
            hit("b", 5.0, "vitamin once appears here"),
            hit("a", 4.0, "vitamin vitamin"),
            hit("c", 3.0, "nothing shared at all"),
        ];
        let mut max_score = Some(5.0);
        let info = plan(None)
            .apply(&DecideSettings::history_only(), &mut hits, &mut max_score)
            .await
            .expect("judged");

        // Reordered by the judge.
        assert_eq!(ids(&hits), ["a", "b", "c"]);
        // `_score` untouched (the engine's own numbers), `_p_relevant` added
        // on every judged hit.
        assert_eq!(hits[0].score, Some(4.0));
        assert_eq!(hits[1].score, Some(5.0));
        let ps: Vec<f64> = hits.iter().map(|h| h.p_relevant.unwrap()).collect();
        assert!(ps[0] > ps[1], "{ps:?}");
        assert_eq!(ps[2], 0.0, "no query word in the doc is exactly 0.0");
        // All three were judged, none dropped.
        assert_eq!(info["judged"], 3);
        assert_eq!(info["kept"], 3);
        assert_eq!(info["dropped"], 0);
        assert!(info["unjudged"].is_null());
        assert_eq!(info["scorer"], "lexical");
        assert!(info["model"].is_null(), "the lexical arm is not a model");
        // max_score still describes the page's `_score` values.
        assert_eq!(max_score, Some(5.0));
    }

    #[tokio::test]
    async fn min_p_drops_and_counts_below_threshold_hits() {
        let mut hits = vec![
            hit("b", 5.0, "vitamin once appears here"),
            hit("a", 4.0, "vitamin vitamin"),
            hit("c", 3.0, "nothing shared at all"),
        ];
        let mut max_score = Some(5.0);
        let info = plan(Some(0.1))
            .apply(&DecideSettings::history_only(), &mut hits, &mut max_score)
            .await
            .expect("judged");
        // c scored 0.0 < 0.1: dropped, and counted; a and b cleared it.
        assert_eq!(ids(&hits), ["a", "b"]);
        assert!(hits.iter().all(|h| h.p_relevant.is_some()));
        assert_eq!(info["judged"], 3);
        assert_eq!(info["kept"], 2);
        assert_eq!(info["dropped"], 1);
        assert_eq!(info["min_p"], 0.1);
        assert_eq!(max_score, Some(5.0));
    }

    #[tokio::test]
    async fn a_drop_that_removes_the_max_score_carrier_recomputes_it() {
        let mut hits = vec![
            hit("c", 9.0, "nothing shared at all"),
            hit("a", 4.0, "vitamin vitamin"),
        ];
        let mut max_score = Some(9.0);
        let info = plan(Some(0.5))
            .apply(&DecideSettings::history_only(), &mut hits, &mut max_score)
            .await
            .expect("judged");
        assert_eq!(ids(&hits), ["a"]);
        assert_eq!(info["dropped"], 1);
        // `max_score` described c's 9.0; c was dropped, so it describes the
        // page that is actually emitted now.
        assert_eq!(max_score, Some(4.0));
    }

    #[tokio::test]
    async fn a_hit_with_no_text_is_kept_unjudged_or_dropped_with_a_threshold() {
        let bare = serde_json::from_value(json!({
            "_index": "i", "_id": "bare", "_score": 7.0, "matched_queries": null
        }))
        .expect("an EsHit");
        let mut hits = vec![hit("a", 4.0, "vitamin vitamin"), bare];
        let mut max_score = Some(7.0);
        let info = plan(None)
            .apply(&DecideSettings::history_only(), &mut hits, &mut max_score)
            .await
            .expect("judged");
        // No threshold: the bare hit follows in the engine's order, marked
        // unjudged by the absence of `_p_relevant` (never a made-up 0.0).
        assert_eq!(ids(&hits), ["a", "bare"]);
        assert!(hits[1].p_relevant.is_none());
        assert_eq!(info["judged"], 1);
        assert_eq!(info["unjudged"], 1);
        assert_eq!(info["dropped"], 0);

        // With one, a hit the judge never scored cannot be shown to clear it.
        let bare = serde_json::from_value(json!({
            "_index": "i", "_id": "bare", "_score": 7.0, "matched_queries": null
        }))
        .expect("an EsHit");
        let mut hits = vec![hit("a", 4.0, "vitamin vitamin"), bare];
        let info = plan(Some(0.5))
            .apply(&DecideSettings::history_only(), &mut hits, &mut max_score)
            .await
            .expect("judged");
        assert_eq!(ids(&hits), ["a"]);
        assert_eq!(info["dropped"], 1);
        assert_eq!(info["kept"], 1);
        assert!(info["unjudged"].is_null());
    }

    #[tokio::test]
    async fn a_page_with_no_text_at_all_is_refused_not_judged_blind() {
        let mut hits = vec![serde_json::from_value(json!({
            "_index": "i", "_id": "x", "_score": 1.0, "matched_queries": null
        }))
        .unwrap()];
        let mut max_score = Some(1.0);
        let err = plan(None)
            .apply(&DecideSettings::history_only(), &mut hits, &mut max_score)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nothing to judge"), "{err}");
    }

    #[tokio::test]
    async fn empty_pages_judge_to_empty() {
        let mut hits: Vec<EsHit> = Vec::new();
        let mut max_score = None;
        let info = plan(None)
            .apply(&DecideSettings::history_only(), &mut hits, &mut max_score)
            .await
            .expect("judged");
        assert_eq!(info["judged"], 0);
        assert_eq!(info["kept"], 0);
        assert!(hits.is_empty());
    }
}
