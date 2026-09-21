//! Issue #950: `POST /{index}/_cache/clear` was a stub — it resolved the
//! indices and reported an honest `_shards` block but freed nothing, so the
//! one operator lever for a node pinned above its memory watermark was not
//! connected to `Index::release_memory`. It must flush, release the hydrated
//! per-segment caches, keep the response shape, and lose no data.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path);
    let body = if body.is_null() {
        Body::empty()
    } else {
        b = b.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let res = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// FAIL-BEFORE: with the handler reverted to the stub, `after == 0` fails
/// (the caches stay resident) and the unrefreshed doc count is unchanged only
/// because nothing ran.
#[tokio::test]
async fn cache_clear_releases_hydrated_caches_and_loses_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    let engine = state.engine.clone();
    let app = xerj_api::router::build_es_compat_router(state);

    call(
        &app,
        "PUT",
        "/docs",
        json!({"mappings":{"properties":{"body":{"type":"text"}}}}),
    )
    .await;
    for i in 0..40 {
        call(
            &app,
            "PUT",
            &format!("/docs/_doc/d{i}"),
            json!({"body": format!("document number {i}")}),
        )
        .await;
    }
    call(&app, "POST", "/docs/_refresh", Value::Null).await;
    // Hydrate the per-segment caches with a size>0 search.
    let (st, _) = call(
        &app,
        "POST",
        "/docs/_search",
        json!({"query":{"match":{"body":"document"}},"size":10}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let idx = engine.get_index("docs").expect("index");
    let before = idx.total_cache_entries();
    assert!(
        before > 0,
        "a query should have hydrated the caches, got {before}"
    );

    // A doc written but never refreshed must survive the clear (flush first).
    call(
        &app,
        "PUT",
        "/docs/_doc/late",
        json!({"body": "document late"}),
    )
    .await;

    let (st, body) = call(&app, "POST", "/docs/_cache/clear", Value::Null).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        body,
        json!({"_shards": {"total": 1, "successful": 1, "failed": 0}})
    );

    let after = idx.total_cache_entries();
    assert_eq!(
        after, 0,
        "#950: _cache/clear must release caches (was {before}, now {after})"
    );

    call(&app, "POST", "/docs/_refresh", Value::Null).await;
    let (_, c) = call(
        &app,
        "POST",
        "/docs/_count",
        json!({"query":{"match_all":{}}}),
    )
    .await;
    assert_eq!(c["count"], 41, "clear must lose no document: {c}");
}
