//! End-to-end checks for the local judge — `POST /v1/systemone` (native
//! router) and `POST /_decide` (ES-compat router) — over HTTP, against real
//! in-process indexes of labelled history.
//!
//! THE VOTE IS RETRIEVAL, NOT A MODEL. It is a weighted nearest-neighbour vote
//! over an index the test seeds, so every probability here is checkable
//! arithmetic. Nothing in this file says anything about judge-model quality —
//! that is measured in `benchmarks/beir-hybrid/results/`, not by these tests.
//!
//! The `systemone` tests speak the request/response shape TypeSafe documents
//! and `jev-reranker` sends, because that client — unmodified — is the
//! acceptance gate for this endpoint:
//!
//! ```text
//! request  { state: { query, documents: { doc_0: "...", ... } }, model,
//!            questions: { d0: { type: "noul",
//!                               instructions: "Is `documents.doc_0` relevant?" } } }
//! response { model, answers: { d0: { type: "noul", noul: 0..1 } }, usage }
//! ```
//!
//! Two deliberate wire breaks are asserted here too, so they cannot regress
//! into silence: `model` is echoed as this node's own id (never the requested
//! Jev name), and a question with zero support is a 422 naming the question —
//! never a fabricated 0.5.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

// ─────────────────────────────────────────────────────────────────────────────
// The node
// ─────────────────────────────────────────────────────────────────────────────

struct Node {
    /// The XERJ-native REST API (`/v1/...`): /v1/systemone, /v1/models.
    native: axum::Router,
    /// The ES-compat API: index creation, _search, /_decide.
    app: axum::Router,
    _dir: tempfile::TempDir,
}

/// A node whose `[decisions]` block names `index`. Empty string leaves the
/// endpoints unconfigured (503), which is itself under test.
async fn node_with_decisions(index: &str, positive_label: &str, min_confidence: f64) -> Node {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.decisions.index = index.to_string();
    config.decisions.positive_label = positive_label.to_string();
    config.decisions.min_confidence = min_confidence;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    Node {
        native: xerj_api::router::build_native_router(state.clone()),
        app: xerj_api::router::build_es_compat_router(state),
        _dir: dir,
    }
}

/// The refund-history node: two `true` docs about refunds, one `false` doc
/// about shipping, so a refund question votes 1.0 and a shipping question 0.0
/// whatever the internal ordering — every neighbour of "refund …" is `true`.
async fn gate_node() -> Node {
    let node = node_with_decisions("history", "true", 0.0).await;
    seed(
        &node,
        "history",
        &[
            ("h1", "refund refund refund asked for money back", "true"),
            ("h2", "refund policy says money back", "true"),
            ("h3", "shipping delivery times", "false"),
        ],
    )
    .await;
    node
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
    async fn models(&self) -> (StatusCode, Value) {
        self.call(&self.native.clone(), "GET", "/v1/models", json!({}))
            .await
    }
    async fn decide(&self, body: Value) -> (StatusCode, Value) {
        self.call(&self.app.clone(), "POST", "/_decide", body).await
    }
}

/// Create an index of `text`/`label` documents and refresh it.
async fn seed(node: &Node, index: &str, docs: &[(&str, &str, &str)]) {
    let (st, b) = node
        .call(
            &node.app.clone(),
            "PUT",
            &format!("/{index}"),
            json!({"mappings": {"properties": {
                "text": {"type": "text"},
                "label": {"type": "keyword"}
            }}}),
        )
        .await;
    assert!(st.is_success(), "create {index}: {st} {b}");
    for (id, text, label) in docs {
        let (st, b) = node
            .call(
                &node.app.clone(),
                "PUT",
                &format!("/{index}/_doc/{id}"),
                json!({"text": text, "label": label}),
            )
            .await;
        assert!(st.is_success(), "index {index}/{id}: {st} {b}");
    }
    let (st, b) = node
        .call(
            &node.app.clone(),
            "POST",
            &format!("/{index}/_refresh"),
            json!({}),
        )
        .await;
    assert!(st.is_success(), "refresh {index}: {st} {b}");
}

fn reason(r: &Value) -> String {
    r["error"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /v1/models
// ─────────────────────────────────────────────────────────────────────────────

/// The SDKs list models before calling. One entry, and it is this node's own
/// id — never a Jev model name, which would claim a hosted judge that is not
/// answering.
#[tokio::test]
async fn models_lists_one_truthful_entry() {
    let node = gate_node().await;
    let (st, r) = node.models().await;
    assert!(st.is_success(), "{st} {r}");
    let models = r["models"].as_array().expect("models array");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["name"], "xerj-history-vote-1");
    assert_ne!(models[0]["name"], "jev-1.13.0");
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /v1/systemone — the wire
// ─────────────────────────────────────────────────────────────────────────────

/// The acceptance-gate shape exactly as `jev-reranker`'s `_payload()` builds
/// it: object state, string instructions carrying backtick references into
/// that state, ids `d0`/`d1`. The answer set must be keyed identically, each
/// answer carrying only its documented fields.
#[tokio::test]
async fn answers_a_jev_reranker_shaped_request_with_backtick_references() {
    let node = gate_node().await;
    let (st, r) = node
        .systemone(json!({
            "state": {
                "query": "complaint",
                "documents": {
                    "doc_0": "refund refund asked for money back twice charged",
                    "doc_1": "where is my parcel shipping delivery"
                }
            },
            "model": "jev-1.13.0",
            "questions": {
                "d0": {"type": "noul", "instructions": "Is `documents.doc_0` relevant?"},
                "d1": {"type": "noul", "instructions": "Is `documents.doc_1` relevant?"}
            }
        }))
        .await;

    assert_eq!(st, StatusCode::OK, "{r}");
    // Deliberate wire break #1: the model echo is truthful. The requested
    // name rides at decisions.requested_model.
    assert_eq!(r["model"], "xerj-history-vote-1");
    assert_eq!(r["decisions"]["requested_model"], "jev-1.13.0");
    // Answers keyed exactly by the ids sent — no more, no fewer.
    let answers = r["answers"].as_object().expect("answers object");
    assert_eq!(answers.len(), 2, "{answers:?}");
    for id in ["d0", "d1"] {
        let a = answers.get(id).unwrap_or(&Value::Null);
        // Clients parse answers strictly: exactly the documented fields.
        let keys: Vec<&str> = a
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(keys, vec!["type", "noul"], "{id}: {a}");
        assert_eq!(a["type"], "noul");
    }
    // doc_0 resolved to refund text → every neighbour is `true` → 1.0.
    // doc_1 resolved to shipping text → the only neighbour is `false` → 0.0.
    assert_eq!(r["answers"]["d0"]["noul"], json!(1.0));
    assert_eq!(r["answers"]["d1"]["noul"], json!(0.0));
    // usage must be JSON integers — the client's metering does dict.get, no
    // coercion.
    assert!(r["usage"]["input_tokens"].as_u64().is_some(), "{r}");
    assert!(r["usage"]["output_tokens"].as_u64().is_some(), "{r}");
    // Evidence rides at the top level, where every verified client ignores.
    assert_eq!(r["decisions"]["index"], "history");
    assert_eq!(r["decisions"]["evidence"]["d0"]["label"], "true");
    assert_eq!(r["decisions"]["evidence"]["d0"]["support"], json!(1.0));
}

/// The other documented state shape: a plain string, with instructions as an
/// object (the shape XERJ's own rerank client sends at providers).
#[tokio::test]
async fn a_plain_string_state_with_object_instructions_also_votes() {
    let node = gate_node().await;
    let (st, r) = node
        .systemone(json!({
            "state": "refund money back",
            "model": "jev-latest",
            "questions": {
                "q1": {"type": "noul", "instructions": {
                    "question": "Is this relevant to the query?",
                    "document": "asked for a refund"
                }}
            }
        }))
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["answers"]["q1"]["noul"], json!(1.0));
}

/// #1000: instruction wording must not change the answer. The instruction is
/// a question ABOUT the payload; its vocabulary is not retrieval text. The
/// issue measured a 32-point accuracy swing on a real SMS history (0.9867
/// empty instruction vs 0.6700 criteria-rich) purely from wording — here the
/// same state is asked three times with three different instructions and all
/// three answers must be identical.
#[tokio::test]
async fn instruction_wording_does_not_change_the_answer() {
    let node = gate_node().await;
    // A ham-shaped state: its own terms retrieve the `false` doc and nothing
    // else, whatever the prose says.
    let state = json!("shipping delivery times");
    let rich = "Is this SMS message spam? prize claims, paid subscriptions, \
                marketing, premium-rate numbers, refund, money back";
    let mut answers = Vec::new();
    for instructions in [
        None,
        Some(json!("Is this message spam?")),
        Some(json!(rich)),
        // the criteria-block shape a Jev migrant sends
        Some(json!({"question": "Is this message spam?", "criteria": {
            "spam": "prize claims, paid subscriptions, marketing",
            "ham": "plain personal correspondence"}})),
    ] {
        let mut q = serde_json::Map::new();
        q.insert("type".into(), json!("noul"));
        if let Some(i) = instructions {
            q.insert("instructions".into(), i);
        }
        let (st, r) = node
            .systemone(json!({
                "state": state,
                "questions": {"d0": Value::Object(q)}
            }))
            .await;
        assert_eq!(st, StatusCode::OK, "{r}");
        answers.push(r["answers"]["d0"]["noul"].as_f64().unwrap_or(-1.0));
    }
    // All four answers identical, and the vote followed the STATE: a
    // shipping text over this history is `false` outright.
    assert!(
        answers.iter().all(|a| (a - answers[0]).abs() < 1e-9),
        "instruction wording changed the answer: {answers:?}"
    );
    assert!(
        (answers[0] - 0.0).abs() < 1e-9,
        "the state's own vote is `false`, got {answers:?}"
    );
}

/// #1001: an object state votes on ALL its string leaves. The issue measured
/// accuracy 0.1733 — exactly the spam base rate — for `{"message": …}` (every
/// item scored from identical instruction text) against 0.9667 for the same
/// message as a string or as `{"query": …}`. The key name must not decide
/// whether the payload is searched.
#[tokio::test]
async fn an_object_state_votes_on_all_its_string_leaves() {
    let node = gate_node().await;
    let (st, r) = node
        .systemone(json!({
            "state": {"message": "refund refund asked for money back twice charged"},
            "questions": {
                "is_spam": {"type": "noul", "instructions": "Is this SMS message spam?"}
            }
        }))
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["answers"]["is_spam"]["noul"], json!(1.0), "{r}");
}

/// A question whose backtick references resolve to nothing is REFUSED with
/// the paths named — not silently voted on whatever text is left. A confident
/// answer computed from a payload the question never saw is the #1001 defect
/// class in another coat.
#[tokio::test]
async fn a_backtick_reference_that_resolves_nowhere_is_named_not_guessed() {
    let node = gate_node().await;
    let (st, r) = node
        .systemone(json!({
            "state": {"message": "refund money back"},
            "questions": {
                "q1": {"type": "noul", "instructions": "Is `typo.path` relevant?"}
            }
        }))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert_eq!(r["error"]["type"], "illegal_argument", "{r}");
    let why = reason(&r);
    assert!(why.contains("`q1`"), "{why}");
    assert!(why.contains("`typo.path`"), "{why}");
}

/// A payload with no text at all is a 422 naming the question — not a vote
/// on the instruction prose over a history that never saw the payload.
#[tokio::test]
async fn a_state_with_no_text_is_refused_by_name() {
    let node = gate_node().await;
    let (st, r) = node
        .systemone(json!({
            "state": {"spam_count": 42, "flags": [true, false]},
            "questions": {"q1": {"type": "noul", "instructions": "classify"}}
        }))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert_eq!(r["error"]["type"], "illegal_argument", "{r}");
    let why = reason(&r);
    assert!(why.contains("`q1`"), "{why}");
    assert!(why.contains("state"), "{why}");
}

/// The data-field pattern — the question carries its payload inside
/// `instructions` and names it in backticks, the shape xerj-rerank's stage
/// sends and the TypeSafe docs prescribe. The referenced field IS the vote
/// text, and a state whose vocabulary would pollute the vote stays out of
/// it: the question selected its payload.
#[tokio::test]
async fn an_instructions_data_field_is_the_payload_and_the_state_stays_out() {
    let node = gate_node().await;
    let (st, r) = node
        .systemone(json!({
            "state": "shipping delivery times",
            "questions": {
                "d0": {"type": "noul", "instructions": {
                    "question": "Judge `document` against the query in the state.",
                    "document": "refund refund asked for money back"
                }}
            }
        }))
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    // The refund document votes `true` outright — the misleading state string
    // never entered the query.
    assert_eq!(r["answers"]["d0"]["noul"], json!(1.0), "{r}");
    assert_eq!(r["decisions"]["evidence"]["d0"]["label"], "true", "{r}");
}

/// `jev-reranker` ≥ 0.1.2's `rerank_relevance()` preset ships its rubric
/// object in `state` and names it in backticks ("Apply all remaining
/// evaluation rules in `rubric`."). The rubric is judge rules, not payload:
/// it must be acknowledged without its string leaves joining the vote —
/// embedding them is exactly the #1000 defect (criteria-rich prose). The
/// vote runs on the text references: the document and the query. On the
/// pre-skip code this shape was a 422 ("`rubric` resolves to nothing").
#[tokio::test]
async fn a_structure_reference_like_the_clients_rubric_is_not_vote_text() {
    let node = gate_node().await;
    let (st, r) = node
        .systemone(json!({
            "state": {
                "query": "triage inbox unsolicited correspondence",
                "documents": {"doc_0": "shipping delivery times when does it arrive"},
                "rubric": {
                    "instructions": "refund refund asked for money back prize winner claim",
                    "criteria": {"true": "retain", "false": "discard"}
                }
            },
            "model": "jev-1.13.0",
            "questions": {
                "doc_0": {"type": "noul",
                    "instructions": "Does `documents.doc_0` help answer `query` and \
                     deserve a high position in its search results? Apply all \
                     remaining evaluation rules in `rubric`.",
                    "criteria": {"true": "retain", "false": "discard"}}
            }
        }))
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    // The shipping document drives the vote to `false` outright — the rubric's
    // refund/spam vocabulary never entered the query, and the neutral query's
    // terms retrieve nothing over this history.
    assert_eq!(r["answers"]["doc_0"]["noul"], json!(0.0), "{r}");
    assert_eq!(r["decisions"]["evidence"]["doc_0"]["label"], "false", "{r}");
}

/// A `choice` question answers with a probability for EVERY criterion option —
/// options with no neighbour weight appear as 0.0, not as missing keys — the
/// winner is the argmax, and its confidence is its own probability.
#[tokio::test]
async fn choice_answers_carry_a_probability_for_every_option() {
    let node = node_with_decisions("mixed", "billing", 0.0).await;
    seed(
        &node,
        "mixed",
        &[
            ("m1", "cancel cancel cancel my subscription", "billing"),
            ("m2", "cancel subscription please", "tech"),
            ("m3", "subscription renewal", "tech"),
        ],
    )
    .await;

    // The vote text is exactly what the question's `ticket` reference
    // resolves to — instruction prose is the question, never the query — so
    // it is the same string /_decide is asked below, making the arithmetic
    // cross-checkable between the two surfaces.
    let vote = "cancel my subscription";
    let (st, r) = node
        .systemone(json!({
            "state": {"ticket": vote},
            "model": "jev-1.13.0",
            "questions": {
                "c1": {"type": "choice",
                       "instructions": "Route `ticket`.",
                       "criteria": {"billing": "about money", "tech": "about software"}}
            }
        }))
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");

    let a = &r["answers"]["c1"];
    assert_eq!(a["type"], "choice");
    let probs = a["probabilities"].as_object().expect("probabilities");
    let mut keys: Vec<&str> = probs.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["billing", "tech"], "{probs:?}");
    let sum: f64 = probs.values().filter_map(Value::as_f64).sum();
    assert!(
        (sum - 1.0).abs() < 1e-4,
        "probabilities sum to {sum}: {probs:?}"
    );
    // The choice is the argmax of the reported probabilities, and the
    // confidence is that option's own probability.
    let (argmax, _) = probs
        .iter()
        .map(|(k, v)| (k, v.as_f64().unwrap_or(0.0)))
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
        .expect("non-empty");
    assert_eq!(a["choice"], json!(argmax));
    assert_eq!(a["confidence"], probs[argmax]);
    assert_eq!(r["decisions"]["evidence"]["c1"]["support"], a["confidence"]);

    // The same vote through /_decide names its neighbours; the probabilities
    // above must be the weighted shares of exactly those neighbours.
    let (st2, d) = node
        .decide(json!({"index": "mixed", "question": vote}))
        .await;
    assert!(st2.is_success(), "{d}");
    let total: f64 = d["neighbours"]
        .as_array()
        .expect("neighbours")
        .iter()
        .map(|n| n["weight"].as_f64().unwrap_or(0.0))
        .sum();
    assert!(total > 0.0, "{d}");
    for (option, p) in probs {
        let w: f64 = d["neighbours"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| &n["label"] == option)
            .map(|n| n["weight"].as_f64().unwrap_or(0.0))
            .sum();
        let expected = (w / total * 1_000_000.0).round() / 1_000_000.0;
        assert!(
            (p.as_f64().unwrap_or(-1.0) - expected).abs() < 1e-6,
            "option {option}: reported {}, weighted share {expected}",
            p
        );
    }
}

/// Deliberate wire break #2: `score` has no vote analogue, and saying so with
/// the alternative beats inventing one.
#[tokio::test]
async fn a_score_question_is_refused_with_an_alternative() {
    let node = gate_node().await;
    let (st, r) = node
        .systemone(json!({
            "state": "refund",
            "questions": {"s1": {"type": "score", "instructions": "How relevant?"}}
        }))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    let why = reason(&r);
    assert!(why.contains("score"), "{why}");
    assert!(why.contains("/_decide"), "{why}");
}

/// Deliberate wire break #3: zero support is an error naming the question ids.
/// A fabricated 0.5 would rank a document by nothing.
#[tokio::test]
async fn zero_support_is_an_error_naming_the_question_not_a_half() {
    let node = gate_node().await;
    let (st, r) = node
        .systemone(json!({
            "state": "zzzqqq nothing matches this",
            "questions": {
                "d0": {"type": "noul", "instructions": "zzzqqq"},
                "d1": {"type": "noul", "instructions": "wwwwww also nothing"}
            }
        }))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert_eq!(r["error"]["type"], "no_support");
    let why = reason(&r);
    assert!(why.contains("`d0`"), "{why}");
    assert!(why.contains("`d1`"), "{why}");
    assert!(!r.to_string().contains("0.5"), "no fabricated halves: {r}");
    assert!(
        r["answers"].is_null(),
        "no half-answered judgement history: {r}"
    );
}

/// Until `[decisions] index` names an index, the endpoint says 503 — not a
/// vote over nothing.
#[tokio::test]
async fn systemone_503s_until_an_index_is_configured() {
    let node = node_with_decisions("", "true", 0.0).await;
    let (st, r) = node
        .systemone(json!({"state": "refund", "questions": {
            "d0": {"type": "noul", "instructions": "refund"}
        }}))
        .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{r}");
    assert_eq!(r["error"]["type"], "not_configured");
}

/// A configured index that does not exist is a different failure from
/// unconfigured, and is surfaced — not answered from an empty history.
#[tokio::test]
async fn a_configured_but_missing_index_is_surfaced_as_an_index_error() {
    let node = node_with_decisions("nope", "true", 0.0).await;
    let (st, r) = node
        .systemone(json!({"state": "refund", "questions": {
            "d0": {"type": "noul", "instructions": "refund"}
        }}))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert_eq!(r["error"]["type"], "history_index_error");
    assert!(reason(&r).contains("nope"), "{r}");
}

#[tokio::test]
async fn questions_must_be_a_non_empty_object() {
    let node = gate_node().await;
    for bad in [json!({}), json!([]), json!("d0"), Value::Null] {
        let (st, r) = node
            .systemone(json!({"state": "refund", "questions": bad}))
            .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {r}");
    }
}

#[tokio::test]
async fn a_question_of_unknown_type_is_refused_by_id() {
    let node = gate_node().await;
    let (st, r) = node
        .systemone(json!({"state": "refund", "questions": {
            "d0": {"type": "noul", "instructions": "refund"},
            "dx": {"type": "haiku", "instructions": "refund"}
        }}))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    let why = reason(&r);
    assert!(why.contains("`dx`"), "{why}");
    assert!(why.contains("haiku"), "{why}");
}

/// One request cannot ask for more questions than the rerank stage would
/// judge documents (300): the vote is a search per question.
#[tokio::test]
async fn more_than_300_questions_is_refused() {
    let node = gate_node().await;
    let mut questions = serde_json::Map::new();
    for i in 0..301 {
        questions.insert(
            format!("d{i}"),
            json!({"type": "noul", "instructions": "refund"}),
        );
    }
    let (st, r) = node
        .systemone(json!({"state": "refund", "questions": Value::Object(questions)}))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert!(reason(&r).contains("300"), "{r}");
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /_decide — the audit surface
// ─────────────────────────────────────────────────────────────────────────────

/// /_decide shows its work: the neighbours, their labels, engine scores and
/// vote weights, plus the label and confidence derived from them. k is
/// per-request.
#[tokio::test]
async fn decide_returns_its_neighbours_and_honours_per_request_k() {
    let node = gate_node().await;

    let (st, r) = node
        .decide(json!({"index": "history", "question": "refund money back", "k": 1}))
        .await;
    assert!(st.is_success(), "{r}");
    assert_eq!(r["index"], "history");
    assert_eq!(r["k"], 1);
    assert_eq!(r["label"], "true");
    assert!(
        (r["confidence"].as_f64().unwrap_or(0.0) - 1.0).abs() < 1e-9,
        "{r}"
    );
    assert_eq!(r["abstain"], json!(false));
    let neighbours = r["neighbours"].as_array().expect("neighbours");
    assert_eq!(neighbours.len(), 1, "k=1: {neighbours:?}");
    let n = &neighbours[0];
    assert_eq!(n["_id"], "h1");
    assert_eq!(n["label"], "true");
    assert!(n["_score"].as_f64().is_some(), "engine score: {n}");
    assert!(
        n["text"].as_str().is_some_and(|t| t.contains("refund")),
        "{n}"
    );

    // Full k: both refund docs are neighbours, weights reciprocal in rank.
    let (_, r) = node
        .decide(json!({"index": "history", "question": "refund money back"}))
        .await;
    let neighbours = r["neighbours"].as_array().expect("neighbours");
    assert_eq!(neighbours.len(), 2, "{neighbours:?}");
    let w0 = neighbours[0]["weight"].as_f64().unwrap_or(0.0);
    let w1 = neighbours[1]["weight"].as_f64().unwrap_or(0.0);
    assert!((w0 - 1.0).abs() < 1e-9, "rank 1 weight: {w0}");
    assert!((w1 - 0.5).abs() < 1e-9, "rank 2 weight: {w1}");
}

/// An operator who raises `decisions.min_confidence` gets abstentions with the
/// reason, and a question with no neighbours abstains rather than erroring —
/// abstention lives on /_decide, errors on the wire surface.
#[tokio::test]
async fn decide_abstains_below_min_confidence_and_on_no_neighbours() {
    let strict = node_with_decisions("history", "true", 0.99).await;
    seed(
        &strict,
        "history",
        &[
            ("h1", "refund refund refund asked for money back", "true"),
            ("h2", "refund policy says money back", "true"),
            ("h3", "shipping delivery times", "false"),
        ],
    )
    .await;

    // Mixed neighbourhood: refund docs plus the shipping doc at rank 3 →
    // confidence 1.5/1.8333 = 0.818 < 0.99 → abstain.
    let (st, r) = strict
        .decide(json!({"index": "history", "question": "refund shipping delivery"}))
        .await;
    assert!(st.is_success(), "{r}");
    assert_eq!(r["abstain"], json!(true));
    assert!(
        r["reason"]
            .as_str()
            .is_some_and(|s| s.contains("min_confidence")),
        "{r}"
    );

    // Nothing matches at all: abstain with the no-neighbour reason.
    let (st, r) = strict
        .decide(json!({"index": "history", "question": "zzzqqq nothing"}))
        .await;
    assert!(st.is_success(), "{r}");
    assert_eq!(r["abstain"], json!(true));
    assert!(r["label"].is_null(), "{r}");
    assert!(
        r["reason"]
            .as_str()
            .is_some_and(|s| s.contains("no labelled neighbour")),
        "{r}"
    );
}

#[tokio::test]
async fn decide_validates_its_request_before_searching() {
    let node = gate_node().await;
    for (body, needle) in [
        (json!({"question": "refund"}), "index"),
        (json!({"index": "history"}), "question"),
        (json!({"index": "  ", "question": "refund"}), "index"),
    ] {
        let (st, r) = node.decide(body.clone()).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {r}");
        assert!(reason(&r).contains(needle), "{body}: {r}");
    }
    // An index that does not exist is an error naming it, not a 503 and not
    // an abstain over an empty history.
    let (st, r) = node
        .decide(json!({"index": "nope", "question": "refund"}))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert_eq!(r["error"]["type"], "history_index_error");
}
