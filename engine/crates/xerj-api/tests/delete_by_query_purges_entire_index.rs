//! Issue #1019: `_delete_by_query` truncates at 10 000 documents per call.
//!
//! The handler built ONE search body — `{"query": …, "size": 10000, "from":
//! 0}` — deleted whatever came back, and reported `total` as the page length
//! with `batches` hardcoded to 1. An index with more than `max_result_window`
//! matching documents was therefore silently under-deleted by a single call
//! (the autoindex client works around it by looping up to 1 000 delete
//! passes, each paying the full search again). ES semantics: delete-by-query
//! has NO default document cap — it processes the entire matching result set
//! in `scroll_size` batches (default 1 000) unless `max_docs` limits it, and
//! `total` is the exact match count, not the truncated page length.
//!
//! These tests go through the real HTTP route. They fail before the fix at
//! `total`/`deleted` == 10 000 (and `_count` == 15 000 afterwards).
//!
//! Elasticsearch is referenced for wire semantics only (approach-only per the
//! licence rules); no ES code is reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    let body = if body.is_null() {
        Body::empty()
    } else {
        req = req.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let response = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Seed `n` docs into `index` via `_bulk` (chunked so no single request body
/// is enormous). Ids are zero-padded so lexicographic `_id` order is
/// deterministic — the paginated runner pages by `_id` keyset.
async fn seed_bulk(app: &axum::Router, index: &str, n: u64) {
    let (st, _) = call(
        app,
        "PUT",
        &format!("/{index}"),
        json!({"mappings": {"properties": {"v": {"type": "long"}}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create {index}");

    let width = n.to_string().len();
    let chunk = 2_500u64;
    let mut start = 0u64;
    while start < n {
        let end = (start + chunk).min(n);
        let mut ndjson = String::new();
        for i in start..end {
            ndjson.push_str(&format!(
                "{{\"index\":{{\"_id\":\"d{i:0width$}\"}}}}\n{{\"v\":{i}}}\n",
                width = width
            ));
        }
        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/{index}/_bulk"))
                    .header("content-type", "application/x-ndjson")
                    .body(Body::from(ndjson))
                    .unwrap(),
            )
            .await
            .expect("bulk request");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "bulk chunk {start}..{end}"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).expect("bulk body");
        assert_eq!(
            body["errors"],
            json!(false),
            "bulk chunk {start}..{end}: {body}"
        );
        start = end;
    }
    let (st, _) = call(app, "POST", &format!("/{index}/_refresh"), Value::Null).await;
    assert_eq!(st, StatusCode::OK, "refresh {index}");
}

async fn count(app: &axum::Router, index: &str) -> u64 {
    let (st, body) = call(
        app,
        "POST",
        &format!("/{index}/_count"),
        json!({"query": {"match_all": {}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "_count {index}");
    body["count"].as_u64().unwrap_or(u64::MAX)
}

/// One `_delete_by_query` over 25 000 matching docs must delete ALL of them —
/// no 10k truncation, `total` the exact match count, `batches` the real page
/// count. Fails before the fix at total/deleted == 10 000.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_by_query_purges_past_ten_thousand_in_one_call() {
    let (app, _dir) = app().await;
    const N: u64 = 25_000;
    seed_bulk(&app, "dbq-big", N).await;
    assert_eq!(count(&app, "dbq-big").await, N, "seeded");

    let (st, body) = call(
        &app,
        "POST",
        "/dbq-big/_delete_by_query",
        json!({"query": {"match_all": {}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["total"],
        json!(N),
        "total must be the exact match count, not the page length: {body}"
    );
    assert_eq!(
        body["deleted"],
        json!(N),
        "every matching doc deleted by ONE call: {body}"
    );
    let batches = body["batches"].as_u64().expect("batches");
    assert!(batches >= 3, "a 25k purge at the default scroll_size of 1000 must pull real batches, got {batches}: {body}");
    assert_eq!(body["failures"], json!([]), "{body}");

    assert_eq!(
        count(&app, "dbq-big").await,
        0,
        "index must be empty after the purge"
    );
}

/// ES `max_docs`: "The maximum number of documents to process. Defaults to
/// all documents." Exactly `max_docs` documents are deleted, `total` reports
/// the processed count, and a `max_docs <= scroll_size` run is a single
/// batch (the documented no-scroll fast path).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_by_query_honors_max_docs() {
    let (app, _dir) = app().await;
    seed_bulk(&app, "dbq-max", 20).await;

    let (st, body) = call(
        &app,
        "POST",
        "/dbq-max/_delete_by_query",
        json!({"query": {"match_all": {}}, "max_docs": 7}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["total"],
        json!(7),
        "with max_docs set, total is the processed count: {body}"
    );
    assert_eq!(body["deleted"], json!(7), "{body}");
    assert_eq!(
        body["batches"],
        json!(1),
        "max_docs <= scroll_size is a single batch: {body}"
    );

    assert_eq!(count(&app, "dbq-max").await, 13, "exactly max_docs deleted");
}

/// ES `scroll_size` (default 1 000): the size of the batch that powers the
/// operation — `batches` is the number of batches actually pulled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_by_query_reports_real_batch_count() {
    let (app, _dir) = app().await;
    seed_bulk(&app, "dbq-scroll", 10).await;

    let (st, body) = call(
        &app,
        "POST",
        "/dbq-scroll/_delete_by_query",
        json!({"query": {"match_all": {}}, "scroll_size": 3}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["total"], json!(10), "{body}");
    assert_eq!(body["deleted"], json!(10), "{body}");
    assert_eq!(
        body["batches"],
        json!(4),
        "10 docs at scroll_size 3 = 4 batches: {body}"
    );

    assert_eq!(count(&app, "dbq-scroll").await, 0);
}

/// A selective query must page exactly like match_all: every matching doc
/// deleted, non-matching untouched, total the exact match count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_by_query_paginates_selective_queries() {
    let (app, _dir) = app().await;
    seed_bulk(&app, "dbq-sel", 30).await;

    let (st, body) = call(
        &app,
        "POST",
        "/dbq-sel/_delete_by_query",
        json!({"query": {"range": {"v": {"gte": 10}}}, "scroll_size": 4}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["total"], json!(20), "docs with v >= 10: {body}");
    assert_eq!(body["deleted"], json!(20), "{body}");
    assert_eq!(
        body["batches"],
        json!(5),
        "20 matches at scroll_size 4 = 5 batches: {body}"
    );

    assert_eq!(count(&app, "dbq-sel").await, 10, "docs with v < 10 survive");
}
