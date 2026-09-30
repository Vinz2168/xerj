//! End-to-end checks for the decide surface's calibration layer (issue
//! #1063): `p_cal` beside every `p_raw`, the fit's ECE riding beside both,
//! `GET /_decide/_calibration` publishing the reliability curve, and the
//! honesty rules — no `p_cal` at all when calibration is not configured,
//! `null` with a published reason when configured but unfitted, and never a
//! copy of `p_raw` pretending to be calibrated.
//!
//! The history is built so the arithmetic is exact and the answer visibly
//! moves: twenty outcome documents whose raw positive-label probability is
//! 0.25, half of them true — isotonic pools the one distinct raw probability
//! into a single knot at the base rate 0.5, so every `p_cal` is 0.5 whatever
//! the vote's share was. (0.25/0.75 are the exactly-representable pair:
//! 1 − 0.75 = 0.25 in f64, where 1 − 0.8 = 0.19999999999999996 — two raw
//! probabilities, no pooling. Ask how the first run of this file failed.) The strictly-parsed `/v1/systemone`
//! answer objects stay untouched (the wire-compatibility #1072 established):
//! `p_raw`/`p_cal` ride in `decisions.evidence`, never in the answer.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use xerj_api::state::AppState;
use xerj_common::calibration::CalibrationMethod;

// ─────────────────────────────────────────────────────────────────────────────
// The node
// ─────────────────────────────────────────────────────────────────────────────

struct Node {
    native: axum::Router,
    app: axum::Router,
    _dir: tempfile::TempDir,
}

/// A node whose `[decisions]` block names `index`, with the calibration
/// method overrideable — the layer's one new setting.
async fn node_with(index: &str, calibration: CalibrationMethod) -> Node {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.decisions.index = index.to_string();
    config.decisions.calibration = calibration;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = AppState::new(config, engine, metrics);
    Node {
        native: xerj_api::router::build_native_router(state.clone()),
        app: xerj_api::router::build_es_compat_router(state),
        _dir: dir,
    }
}

impl Node {
    async fn call(
        &self,
        router: &axum::Router,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (
            status,
            serde_json::from_str(&String::from_utf8_lossy(&bytes)).unwrap_or(Value::Null),
        )
    }

    async fn systemone(&self, body: Value) -> (StatusCode, Value) {
        self.call(&self.native.clone(), "POST", "/v1/systemone", body)
            .await
    }
    async fn decide(&self, body: Value) -> (StatusCode, Value) {
        self.call(&self.app.clone(), "POST", "/_decide", body).await
    }
    async fn calibration(&self, query: &str) -> (StatusCode, Value) {
        self.call(&self.app.clone(), "GET", query, json!({})).await
    }
    async fn put_doc(&self, index: &str, id: &str, doc: Value) {
        let (st, b) = self
            .call(
                &self.app.clone(),
                "PUT",
                &format!("/{index}/_doc/{id}"),
                doc,
            )
            .await;
        assert!(st.is_success(), "index {index}/{id}: {st} {b}");
    }
    async fn create_index(&self, index: &str) {
        let (st, b) = self
            .call(
                &self.app.clone(),
                "PUT",
                &format!("/{index}"),
                json!({"mappings": {"properties": {
                    "text": {"type": "text"},
                    "label": {"type": "keyword"}
                }}}),
            )
            .await;
        assert!(st.is_success(), "create {index}: {st} {b}");
    }
    async fn refresh(&self, index: &str) {
        let (st, b) = self
            .call(
                &self.app.clone(),
                "POST",
                &format!("/{index}/_refresh"),
                json!({}),
            )
            .await;
        assert!(st.is_success(), "refresh {index}: {st} {b}");
    }
}

/// Seed the outcome history: `per_side` `refund` texts labelled positive
/// (`true`, `p: 0.25`) and `per_side` `outage` texts labelled negative
/// (`false`, `p: 0.75`). Each document's `p` is the probability the system
/// served FOR ITS OWN LABEL, so both sides convert to the same positive-label
/// probability: the positives are (0.25, 1), the negatives (1 − 0.75 = 0.25,
/// 0) — exactly, because 0.75 and 0.25 are exactly representable (1 − 0.8 is
/// not, and the two-tenths the first draft used landed on two DIFFERENT raw
/// probabilities: 0.19999999999999996 and 0.2). Every pair therefore sits at
/// raw 0.25 against a 0.5 base rate — isotonic pools the one distinct raw
/// probability into a single knot at 0.5, so every `p_cal` on this history is
/// exactly 0.5, visibly not a copy of any vote share. The outage texts share
/// no query term with the refund texts, so a refund-shaped question retrieves
/// only refund documents and votes 1.0.
async fn seed_outcomes(node: &Node, index: &str, per_side: usize) {
    node.create_index(index).await;
    for i in 0..per_side {
        node.put_doc(
            index,
            &format!("refund-{i}"),
            json!({
                "text": format!("refund request number {i} for the annual subscription"),
                "label": "true",
                "p": 0.25,
                "ts": "2026-09-28T10:00:00.000Z",
            }),
        )
        .await;
        node.put_doc(
            index,
            &format!("outage-{i}"),
            json!({
                "text": format!("outage report number {i} for the weekend window"),
                "label": "false",
                "p": 0.75,
                "ts": "2026-09-29T10:00:00.000Z",
            }),
        )
        .await;
    }
    node.refresh(index).await;
}

// ─────────────────────────────────────────────────────────────────────────────
// /_decide
// ─────────────────────────────────────────────────────────────────────────────

/// The core contract: `p_raw` beside `p_cal`, the calibration block naming
/// the method and its ECE, and `p_cal` moved by the fit — 0.5 on this
/// history, whatever the vote's share was — never a copy of `p_raw`.
#[tokio::test]
async fn decide_ships_p_cal_beside_p_raw_with_the_fit_ece() {
    let node = node_with("judgements", CalibrationMethod::Isotonic).await;
    seed_outcomes(&node, "judgements", 10).await;

    let (st, body) = node
        .decide(json!({"index": "judgements", "question": "refund request for the annual subscription", "k": 10}))
        .await;
    assert!(st.is_success(), "{st} {body}");
    // The vote: all ten neighbours are `refund` docs, so the positive share
    // is 1.0 — a confident raw probability.
    assert_eq!(body["p_raw"], 1.0, "p_raw is the positive label's share");
    assert_eq!(
        body["p_cal"], 0.5,
        "the fit pooled the one distinct raw probability to the base rate: {body}"
    );
    assert_eq!(body["confidence"], 1.0, "confidence keeps its raw meaning");
    let cal = &body["calibration"];
    assert_eq!(cal["method"], "isotonic", "{cal}");
    assert_eq!(cal["scope"], "noul");
    assert!(cal["ece"]["raw"].is_number(), "the ECE rides beside: {cal}");
    assert!(cal["ece"]["calibrated"].is_number(), "{cal}");
    assert_eq!(cal["labelled_pairs"], 20);
    assert_eq!(cal["fitted_on"], 16, "the deterministic 80/20: {cal}");
    assert_eq!(cal["held_out"], 4);
}

/// An uncalibrated node ships `p_raw` alone: no `p_cal` field at all, and a
/// calibration block that says so — never a p_cal copying p_raw.
#[tokio::test]
async fn without_calibration_there_is_no_p_cal_at_all() {
    let node = node_with("judgements", CalibrationMethod::None).await;
    seed_outcomes(&node, "judgements", 10).await;

    let (st, body) = node
        .decide(json!({"index": "judgements", "question": "refund request for the annual subscription"}))
        .await;
    assert!(st.is_success(), "{st} {body}");
    assert_eq!(body["p_raw"], 1.0);
    assert!(
        body.get("p_cal").is_none(),
        "no calibration configured → no p_cal field: {body}"
    );
    assert_eq!(body["calibration"], json!({"configured": "none"}));
}

/// Configured but unfitted: `p_cal` is `null`, and the reason rides beside
/// it in the calibration block — the operator can act on the string.
#[tokio::test]
async fn a_configured_but_unfitted_node_ships_null_with_the_reason() {
    let node = node_with("judgements", CalibrationMethod::Isotonic).await;
    seed_outcomes(&node, "judgements", 2).await; // 4 pairs < the 10-pair minimum

    let (st, body) = node
        .decide(json!({"index": "judgements", "question": "refund request for the annual subscription"}))
        .await;
    assert!(st.is_success(), "{st} {body}");
    assert!(
        body["p_raw"].is_number(),
        "p_raw ships whatever the four-document vote said: {body}"
    );
    assert!(
        body["p_cal"].is_null(),
        "configured but unfitted → null, not a copy: {body}"
    );
    let reason = body["calibration"]["reason"]
        .as_str()
        .expect("the reason is published beside the null");
    assert!(reason.contains("4 labelled calibration pairs"), "{reason}");
    assert!(
        reason.contains("source"),
        "the flywheel exclusion is explained: {reason}"
    );
}

/// Temperature calibrates the same quantity through the same seam. On this
/// history every fit pair sits at p 0.2 with a 0.5 base rate, so the NLL
/// optimum is "as flat as the bracket allows": T pins at its ceiling (16) and
/// the saturated p_raw 1.0 pulls most of the way back toward the base rate —
/// σ(logit(1)/16) ≈ 0.703. Rank preserved, confidence honestly deflated,
/// never all the way to a pretend 0.5.
#[tokio::test]
async fn temperature_calibrates_the_same_quantity() {
    let node = node_with("judgements", CalibrationMethod::Temperature).await;
    seed_outcomes(&node, "judgements", 10).await;

    let (st, body) = node
        .decide(json!({"index": "judgements", "question": "refund request for the annual subscription"}))
        .await;
    assert!(st.is_success(), "{st} {body}");
    assert_eq!(body["p_raw"], 1.0);
    let p_cal = body["p_cal"].as_f64().expect("a number");
    assert!(
        (p_cal - 0.703_386).abs() < 0.005,
        "T pinned at its ceiling pulls σ(logit(1)/16) back toward the base rate: {p_cal}"
    );
    assert!(
        p_cal < body["p_raw"].as_f64().expect("p_raw"),
        "the saturated raw probability deflates: {p_cal}"
    );
    assert_eq!(body["calibration"]["method"], "temperature");
}

// ─────────────────────────────────────────────────────────────────────────────
// /v1/systemone — the strictly-parsed wire stays untouched
// ─────────────────────────────────────────────────────────────────────────────

/// `p_raw`/`p_cal` ride in `decisions.evidence` (the extras channel), the
/// answer object keeps exactly its documented fields, a noul calibrates, and
/// a choice's per-option shares — a different quantity — do not.
#[tokio::test]
async fn systemone_calibrates_nouls_in_evidence_and_leaves_the_wire_alone() {
    let node = node_with("judgements", CalibrationMethod::Isotonic).await;
    seed_outcomes(&node, "judgements", 10).await;

    let (st, body) = node
        .systemone(json!({
            "state": {"documents": {
                "doc_0": "refund request for the annual subscription",
                "doc_1": "outage report for the weekend window"
            }},
            "questions": {
                "n": {"type": "noul", "instructions": "Judge `documents.doc_0` for a refund request."},
                "c": {"type": "choice", "instructions": "Route `documents.doc_1`.", "criteria": {"true": {}, "false": {}}}
            }
        }))
        .await;
    assert!(st.is_success(), "{st} {body}");

    // The wire objects: exactly the documented fields, no p_raw/p_cal inside.
    let noul = &body["answers"]["n"];
    assert_eq!(
        noul.as_object().map(|o| o.len()),
        Some(2),
        "type + noul only: {noul}"
    );
    let choice = &body["answers"]["c"];
    assert!(
        choice.get("p_raw").is_none() && choice.get("p_cal").is_none(),
        "the answer object is strictly parsed and stays untouched: {choice}"
    );

    // The evidence: the noul calibrates, the choice does not.
    let evidence = &body["decisions"]["evidence"];
    assert_eq!(evidence["n"]["p_raw"], 1.0, "the noul's p_raw: {evidence}");
    assert_eq!(
        evidence["n"]["p_cal"], 0.5,
        "the pooled base rate: {evidence}"
    );
    assert!(
        evidence["c"].get("p_raw").is_none() && evidence["c"].get("p_cal").is_none(),
        "a choice's per-option shares are a different quantity, out of this fit's scope: {evidence}"
    );

    // The block with the ECE rides at the decisions level.
    let cal = &body["decisions"]["calibration"];
    assert_eq!(cal["method"], "isotonic");
    assert_eq!(cal["scope"], "noul");
    assert!(cal["ece"]["calibrated"].is_number(), "{cal}");
    assert_eq!(cal["labelled_pairs"], 20);
}

/// An uncalibrated wire node carries no calibration block at all and no
/// p_cal in any evidence.
#[tokio::test]
async fn systemone_without_calibration_carries_no_block() {
    let node = node_with("judgements", CalibrationMethod::None).await;
    seed_outcomes(&node, "judgements", 10).await;

    let (st, body) = node
        .systemone(json!({
            "state": "refund request for the annual subscription",
            "questions": {"n": {"type": "noul", "instructions": "Is this a refund request?"}}
        }))
        .await;
    assert!(st.is_success(), "{st} {body}");
    assert!(
        body["decisions"].get("calibration").is_none(),
        "no calibration configured → no block: {body}"
    );
    assert_eq!(body["decisions"]["evidence"]["n"]["p_raw"], 1.0);
    assert!(
        body["decisions"]["evidence"]["n"].get("p_cal").is_none(),
        "and no p_cal: {body}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /_decide/_calibration
// ─────────────────────────────────────────────────────────────────────────────

/// The publication: the reliability curve (binned p_raw against empirical
/// frequency, with bin counts), the ECE raw and calibrated, the fit's
/// parameters, and what it was fitted on. On this history every pair sits at
/// raw 0.2, so the raw curve is one bin of 20 at rate 0.5.
#[tokio::test]
async fn calibration_publishes_the_reliability_curve() {
    let node = node_with("judgements", CalibrationMethod::Isotonic).await;
    seed_outcomes(&node, "judgements", 10).await;

    let (st, body) = node
        .calibration("/_decide/_calibration?index=judgements")
        .await;
    assert!(st.is_success(), "{st} {body}");
    assert_eq!(body["index"], "judgements");
    assert_eq!(body["configured"], "isotonic");
    assert_eq!(body["method"], "isotonic");
    assert_eq!(body["scope"], "noul");
    assert_eq!(body["labelled_pairs"], 20);
    assert_eq!(body["fitted_on"], 16);
    assert_eq!(body["held_out"], 4);
    assert_eq!(body["bins"], 10);

    let raw_curve = body["curve"]["raw"].as_array().expect("the raw curve");
    assert_eq!(
        raw_curve.len(),
        1,
        "one occupied bin (all pairs at 0.25): {raw_curve:?}"
    );
    let bin = &raw_curve[0];
    assert_eq!(bin["n"], 20, "bin counts publish");
    assert_eq!(bin["lo"], 0.2);
    assert_eq!(bin["hi"], 0.3); // the bin edge: (b+1)/10 in f64
    assert!((bin["mean_p"].as_f64().unwrap() - 0.25).abs() < 1e-12);
    assert!((bin["positive_rate"].as_f64().unwrap() - 0.5).abs() < 1e-12);
    assert!(
        body["curve"]["calibrated"].is_array(),
        "the calibrated curve publishes too: {body}"
    );
    assert!(body["ece"]["raw"].is_number() && body["ece"]["calibrated"].is_number());
    let knots = body["params"]["knots"].as_array().expect("the fit's knots");
    assert_eq!(
        knots.len(),
        1,
        "one distinct raw probability → one knot: {knots:?}"
    );
    assert_eq!(knots[0][0], 0.25);
    assert_eq!(knots[0][1], 0.5);
    assert_eq!(
        body["ts_range"],
        json!(["2026-09-28T10:00:00.000Z", "2026-09-29T10:00:00.000Z"]),
        "the fitted documents' date range publishes"
    );
}

/// The endpoint also publishes WITHOUT a configured calibration — the raw
/// reliability of the history is audit information in its own right — with a
/// reason naming the setting that would fit it.
#[tokio::test]
async fn calibration_publishes_the_raw_curve_even_when_not_configured() {
    let node = node_with("judgements", CalibrationMethod::None).await;
    seed_outcomes(&node, "judgements", 10).await;

    let (st, body) = node
        .calibration("/_decide/_calibration?index=judgements")
        .await;
    assert!(st.is_success(), "{st} {body}");
    assert_eq!(body["configured"], "none");
    assert_eq!(body["labelled_pairs"], 20);
    assert!(
        body["curve"]["raw"].is_array(),
        "the raw curve still publishes: {body}"
    );
    assert!(body["ece"]["raw"].is_number(), "{body}");
    let reason = body["reason"]
        .as_str()
        .expect("the reason names the setting");
    assert!(reason.contains("calibration"), "{reason}");
}

/// Cached flywheel answers (`source`) are predictions, not outcomes: they do
/// not feed the fit, whatever else they do to the vote.
#[tokio::test]
async fn cached_answers_do_not_feed_the_fit() {
    let node = node_with("judgements", CalibrationMethod::Isotonic).await;
    seed_outcomes(&node, "judgements", 10).await;
    // Five cached local-tier answers — confident, and self-consistent by
    // construction. If they fed the fit they would drag it off the recorded
    // outcomes' base rate.
    for i in 0..5 {
        node.put_doc(
            "judgements",
            &format!("cached-{i}"),
            json!({
                "text": format!("cached answer number {i}"),
                "label": "true",
                "p": 0.99,
                "source": "local",
                "ts": "2026-09-30T10:00:00.000Z",
            }),
        )
        .await;
    }
    node.refresh("judgements").await;

    let (st, body) = node
        .calibration("/_decide/_calibration?index=judgements")
        .await;
    assert!(st.is_success(), "{st} {body}");
    assert_eq!(
        body["labelled_pairs"], 20,
        "the five `source` documents are predictions, not outcomes: {body}"
    );
    assert_eq!(body["fitted_on"], 16);
}

/// No index to read: the endpoint refuses naming what it needs, instead of
/// publishing an empty curve as if it were a measurement.
#[tokio::test]
async fn calibration_with_no_index_refuses() {
    // A node with no [decisions] index and nothing named per request.
    let node = node_with("", CalibrationMethod::Isotonic).await;
    let (st, body) = node.calibration("/_decide/_calibration").await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{st} {body}");
    let reason = body["error"]["reason"].as_str().expect("the refusal");
    assert!(reason.contains("index"), "{reason}");
}

/// A named index that does not exist is the vote's own error shape — the
/// calibration surface never invents a fit from an unreadable history.
#[tokio::test]
async fn calibration_on_a_missing_index_errors() {
    let node = node_with("judgements", CalibrationMethod::Isotonic).await;
    let (st, body) = node.calibration("/_decide/_calibration?index=nope").await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{st} {body}");
    assert_eq!(body["error"]["type"], "history_index_error", "{body}");
}
