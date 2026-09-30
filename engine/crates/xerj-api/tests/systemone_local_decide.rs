//! End-to-end checks for tier 2 of the decide ladder — the local zero-shot
//! head answering what the history vote cannot (issue #1057).
//!
//! Every test here arms a node with a TINY UNTRAINED ModernBERT-class fixture
//! (`xerj_ai::decide::testing::write_fixture`). The fixture proves the
//! loading/scoring path end to end; its probabilities are reproducible
//! arithmetic, NOT judgement quality — nothing here measures accuracy (that
//! is `benchmarks/decisions-as-retrieval` against the real trained model,
//! issue #1064). What IS under test:
//!
//! - the 503/422 → 200 transition: a node with no `[decisions] index` at all,
//!   and a question whose payload retrieves no labelled neighbour, answer
//!   from the local head instead of erroring;
//! - the tier ORDER: where the history vote has support it still wins,
//!   per-question, in the same request;
//! - the model echo: `xerj-decide-local-1` / `xerj-history-vote-1`, never a
//!   Jev name;
//! - honest failure: a local tier armed at a missing model directory fails
//!   loudly naming the path, never with a fabricated probability.
//!
//! Compiled only with the `decide-local` feature — the default build's
//! contract is `systemone_http.rs`'s business.

#![cfg(feature = "decide-local")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use xerj_api::state::AppState;
use xerj_api::systemone_api::{DecideSettings, LOCAL_MODEL_ID, MODEL_ID};

struct Node {
    native: axum::Router,
    app: axum::Router,
    _dir: tempfile::TempDir,
    _model_dir: tempfile::TempDir,
}

/// A node with the local tier armed at a fresh fixture, and `[decisions]
/// index` set to `index` ("" for none — the no-history node this feature
/// exists for).
async fn local_node(index: &str) -> Node {
    let dir = tempfile::tempdir().expect("tempdir");
    let model_dir = tempfile::tempdir().expect("model tempdir");
    xerj_ai::decide::testing::write_fixture(model_dir.path())
        .unwrap_or_else(|e| panic!("write decide fixture: {e:#}"));

    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.decisions.index = index.to_string();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let mut state = AppState::new(config, engine, metrics);
    // The injection seam, not the environment: replacing the field keeps
    // parallel tests from racing on process-wide env state.
    state.decide = std::sync::Arc::new(DecideSettings::local(model_dir.path().to_path_buf()));
    Node {
        native: xerj_api::router::build_native_router(state.clone()),
        app: xerj_api::router::build_es_compat_router(state),
        _dir: dir,
        _model_dir: model_dir,
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
    async fn put_doc(&self, index: &str, id: &str, text: &str, label: &str) {
        let (st, b) = self
            .call(
                &self.app.clone(),
                "PUT",
                &format!("/{index}/_doc/{id}"),
                json!({"text": text, "label": label}),
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

// ─────────────────────────────────────────────────────────────────────────────
// The 503 → 200 transition: no [decisions] index at all
// ─────────────────────────────────────────────────────────────────────────────

/// The node this tier exists for: no labelled history anywhere, and /v1/systemone
/// still answers both question kinds — 200 where the same request used to 503,
/// with probabilities that are real numbers in range, and a model echo that is
/// the local head's own id, never the Jev name the client asked for.
#[tokio::test]
async fn no_history_node_answers_noul_and_choice_from_the_local_head() {
    let node = local_node("").await;
    let (st, r) = node
        .systemone(json!({
            "state": {
                "documents": {
                    "doc_0": "urgent prize claim call now",
                    "doc_1": "hi it is me again about dinner"
                }
            },
            "model": "jev-1.13.0",
            "questions": {
                "d0": {"type": "noul", "instructions": "Is `documents.doc_0` spam?"},
                "d1": {"type": "noul", "instructions": "Is `documents.doc_1` spam?"},
                "c0": {"type": "choice", "instructions": "Route `documents.doc_0`.",
                       "criteria": {"billing": "", "tech": "", "spam": ""}}
            }
        }))
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["model"], MODEL_ID, "wire echo stays the vote id: {r}");
    assert_ne!(r["model"], "jev-1.13.0");
    assert_eq!(r["decisions"]["local_model"], LOCAL_MODEL_ID, "{r}");
    for id in ["d0", "d1"] {
        let noul = r["answers"][id]["noul"]
            .as_f64()
            .unwrap_or_else(|| panic!("{id} noul is a number: {r}"));
        assert!((0.0..=1.0).contains(&noul), "{id} noul {noul}: {r}");
    }
    let probs = r["answers"]["c0"]["probabilities"]
        .as_object()
        .expect("choice probabilities: {r}");
    assert_eq!(probs.len(), 3, "{r}");
    let sum: f64 = probs.values().filter_map(|v| v.as_f64()).sum();
    assert!((sum - 1.0).abs() < 1e-3, "choice probs sum to {sum}: {r}");
    assert!(
        r["answers"]["c0"]["choice"]
            .as_str()
            .is_some_and(|c| probs.contains_key(c)),
        "winner is one of the options: {r}"
    );
    // Every answer says which tier produced it.
    for id in ["d0", "d1", "c0"] {
        assert_eq!(r["decisions"]["evidence"][id]["tier"], "local", "{r}");
        assert_eq!(
            r["decisions"]["evidence"][id]["model"], LOCAL_MODEL_ID,
            "{r}"
        );
    }
}

/// The local head never rescues request-shape refusals: a question with no
/// payload to classify is still a 422 naming it.
#[tokio::test]
async fn local_mode_does_not_rescue_a_question_with_no_payload() {
    let node = local_node("").await;
    let (st, r) = node
        .systemone(json!({
            "state": {"documents": {}},
            "questions": {
                "d0": {"type": "noul", "instructions": "Is `documents.doc_0` spam?"}
            }
        }))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert!(
        r["error"]["reason"]
            .as_str()
            .is_some_and(|s| s.contains("`d0`")),
        "{r}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The tier order, in one request
// ─────────────────────────────────────────────────────────────────────────────

/// With a seeded history AND the local tier armed, history answers what it
/// can (refund → noul 1.0 by vote arithmetic, evidence tier "history") and the
/// local head answers what it cannot (a payload with no labelled neighbour) —
/// the same request, 200, per-question evidence.
#[tokio::test]
async fn history_still_wins_where_it_has_support_local_answers_the_rest() {
    let node = local_node("history").await;
    node.create_index("history").await;
    node.put_doc(
        "history",
        "h1",
        "refund refund refund asked for money back",
        "true",
    )
    .await;
    node.put_doc("history", "h2", "refund policy says money back", "true")
        .await;
    node.refresh("history").await;

    let (st, r) = node
        .systemone(json!({
            "state": {
                "documents": {
                    "doc_0": "refund asked for money back",
                    "doc_1": "zzzqqq nothing like the history"
                }
            },
            "questions": {
                "d0": {"type": "noul", "instructions": "Is `documents.doc_0` about refunds?"},
                "d1": {"type": "noul", "instructions": "Is `documents.doc_1` about refunds?"}
            }
        }))
        .await;
    assert_eq!(st, StatusCode::OK, "mixed support is not an error: {r}");
    // History tier: every neighbour of doc_0 is `true` → noul 1.0, and the
    // evidence shows the vote, not the model.
    assert!(
        (r["answers"]["d0"]["noul"].as_f64().unwrap_or(0.0) - 1.0).abs() < 1e-9,
        "{r}"
    );
    assert_eq!(r["decisions"]["evidence"]["d0"]["tier"], "history", "{r}");
    // Local tier for the question history had no neighbour for.
    assert_eq!(r["decisions"]["evidence"]["d1"]["tier"], "local", "{r}");
    assert_eq!(
        r["decisions"]["evidence"]["d1"]["model"], LOCAL_MODEL_ID,
        "{r}"
    );
    let noul = r["answers"]["d1"]["noul"].as_f64().unwrap_or(-1.0);
    assert!((0.0..=1.0).contains(&noul), "{r}");
}

/// /_decide with no `index` field at all: a 200 tier-tagged local answer where
/// the history-only node 422s. (The audit surface keeps its abstain semantics:
/// the local answer abstains by the same min_confidence rule.)
#[tokio::test]
async fn decide_without_an_index_answers_from_the_local_tier() {
    let node = local_node("").await;
    let (st, r) = node
        .decide(json!({"question": "urgent prize claim call now", "positive_label": "spam"}))
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["tier"], "local", "{r}");
    assert_eq!(r["model"], LOCAL_MODEL_ID, "{r}");
    assert_eq!(r["positive_label"], "spam", "{r}");
    let label = r["label"].as_str().expect("label: {r}");
    assert!(
        label == "spam" || label == "not spam",
        "one of the two competing hypotheses: {r}"
    );
    let confidence = r["confidence"].as_f64().expect("confidence: {r}");
    assert!((0.0..=1.0).contains(&confidence), "{r}");
    assert_eq!(
        r["neighbours"].as_array().map(Vec::len).unwrap_or_default(),
        0,
        "tier 2 has no neighbours — that is what it means: {r}"
    );
}

/// The other half of the /_decide ladder: with an index configured but missing
/// on the node (Unusable), the history-only contract is a 422 — the local tier
/// turns it into a 200 local answer instead.
#[tokio::test]
async fn decide_over_a_missing_index_falls_to_the_local_tier() {
    let node = local_node("history").await;
    // No such index is created; the tier is armed.
    let (st, r) = node
        .decide(json!({"index": "history", "question": "urgent prize claim call now"}))
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["tier"], "local", "{r}");
}

// ─────────────────────────────────────────────────────────────────────────────
// Honest failure
// ─────────────────────────────────────────────────────────────────────────────

/// A local tier armed at a directory with no model in it: the request fails
/// loudly, naming the path and the env var — never a fabricated probability
/// and never a silent fallthrough to the error the operator switched away
/// from.
#[tokio::test]
async fn an_armed_tier_with_no_model_fails_loudly_naming_the_directory() {
    let empty_model = tempfile::tempdir().expect("model tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = data_dir.path().to_string_lossy().into_owned();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let mut state = AppState::new(config, engine, metrics);
    state.decide = std::sync::Arc::new(DecideSettings::local(empty_model.path().to_path_buf()));
    let native = xerj_api::router::build_native_router(state.clone());
    let response = native
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/systemone")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "state": "urgent prize claim call now",
                        "questions": {"q": {"type": "noul", "instructions": "spam?"}}
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let r: Value = serde_json::from_str(&String::from_utf8_lossy(&bytes)).unwrap_or(Value::Null);
    let reason = r["error"]["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("config.json") && reason.contains("XERJ_DECIDE_MODEL_DIR"),
        "the error names the missing files and the env var: {r}"
    );
    assert_eq!(r["error"]["type"], "local_decide_unavailable", "{r}");
}

/// /v1/models keeps its discipline: the local head appears under its own
/// never-a-Jev-name id when armed.
#[tokio::test]
async fn models_lists_the_local_head_under_its_own_id() {
    let node = local_node("").await;
    let (st, r) = node
        .call(&node.native.clone(), "GET", "/v1/models", json!({}))
        .await;
    assert!(st.is_success(), "{st} {r}");
    let names: Vec<&str> = r["models"]
        .as_array()
        .expect("models")
        .iter()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert!(names.contains(&MODEL_ID), "{names:?}");
    assert!(names.contains(&LOCAL_MODEL_ID), "{names:?}");
    assert!(!names.iter().any(|n| n.starts_with("jev")), "{names:?}");
}
