//! `bulk_sink` — event-driven forwarding of a selected subset of local
//! indices to an external ES-compat cluster (Elasticsearch, OpenSearch, or
//! another xerj node) over the standard `_bulk` NDJSON wire format.
//!
//! ## Relationship to the durability WAL
//!
//! This is a **second, independent, read-only consumer** of the same WAL
//! (`xerj-storage/src/wal.rs`) used for local crash recovery. It never
//! prunes, truncates, or otherwise mutates WAL files — it only calls
//! [`xerj_storage::wal::replay_all_sorted`], the same read path used by
//! crash recovery, and tracks its own progress in a checkpoint file that is
//! completely separate from the recovery `.wchk` files. A stalled or
//! failing external target therefore cannot affect local durability: WAL
//! generations are pruned exactly as before, based solely on the recovery
//! consumer's verified-durable predicate.
//!
//! ## Scope
//!
//! Single-node only (see `Config::validate` — `bulk_sink.enabled` and
//! `cluster.enabled` are mutually exclusive). One process, one local WAL
//! per index, one cursor per index. There is deliberately no story here for
//! checkpointing across a multi-node region split.
//!
//! ## Polling, not push, at the WAL layer
//!
//! There is no "new WAL entry" notification hook in `WalWriter` today (the
//! WAL is designed around synchronous, lock-held appends — see
//! `xerj-storage/src/wal.rs`), so the sink polls each selected index's WAL
//! on `flush_interval_ms` cadence rather than being woken on write. Each
//! tick re-reads every not-yet-forwarded entry from `replay_all_sorted`
//! (the same idempotent-consumer pattern the WAL's own docs describe for
//! recovery) and filters by `seq_no > checkpoint`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use xerj_common::config::BulkSinkConfig;
use xerj_common::XerjError;
use xerj_storage::wal::WalEntry;

use crate::{es_compat::glob_match_simple, state::AppState};

type Result<T> = std::result::Result<T, XerjError>;

const CHECKPOINT_FILE_NAME: &str = "bulk_sink_checkpoint.json";
const MAX_RETRIES: u32 = 5;
const BASE_BACKOFF: Duration = Duration::from_millis(200);
const MAX_BACKOFF: Duration = Duration::from_secs(10);

// ─────────────────────────────────────────────────────────────────────────────
// Index selection
// ─────────────────────────────────────────────────────────────────────────────

/// Whether `name` should be forwarded given the configured `indices`
/// glob patterns.
///
/// Names with a `.` prefix (`.xerj_dashboards`, `.xerj_sessions`,
/// `.xerj_users`, ...) are **always** excluded — hardcoded, not
/// configurable, regardless of what `indices` matches. An empty pattern
/// list means the sink forwards nothing, even when `enabled = true`.
fn is_index_selected(name: &str, patterns: &[String]) -> bool {
    if name.starts_with('.') {
        return false;
    }
    patterns.iter().any(|p| glob_match_simple(p, name))
}

// ─────────────────────────────────────────────────────────────────────────────
// Runtime (mutable-at-runtime) config
// ─────────────────────────────────────────────────────────────────────────────

/// The subset of [`BulkSinkConfig`] that `PUT /v1/bulk-sink/config` may
/// change at runtime, without a restart. `enabled` and `target_kind` are
/// deliberately excluded: `enabled` is a startup-only gate (already
/// validated against `cluster.enabled`), and pause/resume cover the
/// runtime on/off switch instead.
///
/// Changes here live only for the current process — `xerj.toml` remains
/// the source of truth for the next restart, which then re-applies the
/// file's values. This is a deliberate scope decision: no config-file
/// rewriting, no risk of a runtime experiment silently becoming permanent.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeConfig {
    target_url: String,
    target_api_key: Option<String>,
    indices: Vec<String>,
    batch_size: usize,
    flush_interval_ms: u64,
}

impl From<&BulkSinkConfig> for RuntimeConfig {
    fn from(cfg: &BulkSinkConfig) -> Self {
        Self {
            target_url: cfg.target_url.clone(),
            target_api_key: cfg.target_api_key.clone(),
            indices: cfg.indices.clone(),
            batch_size: cfg.batch_size,
            flush_interval_ms: cfg.flush_interval_ms,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Service
// ─────────────────────────────────────────────────────────────────────────────

/// Per-index sink status, refreshed at the end of every tick that examines
/// that index.
#[derive(Debug, Clone, Default, Serialize)]
struct IndexSinkStatus {
    /// Last WAL `seq_no` successfully forwarded (`_bulk` returned 2xx and
    /// the checkpoint was persisted), or `null` if nothing has been
    /// forwarded yet.
    checkpoint_seq_no: Option<u64>,
    /// WAL entries observed at or after the checkpoint on the last scan
    /// that have not yet been successfully forwarded.
    lag: u64,
    last_push_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

/// Background service: owns the live (runtime-mutable) config, the
/// pause/resume switch, per-index checkpoints + status, and the HTTP
/// client used to push `_bulk` bodies to the target.
pub struct BulkSinkService {
    /// Fixed at startup — see `RuntimeConfig` doc comment.
    enabled: bool,
    target_kind: String,
    runtime: RwLock<RuntimeConfig>,
    paused: AtomicBool,
    status: DashMap<String, IndexSinkStatus>,
    checkpoint_path: PathBuf,
    client: reqwest::Client,
}

impl BulkSinkService {
    pub fn new(cfg: &BulkSinkConfig, data_dir: &std::path::Path) -> Arc<Self> {
        let checkpoint_path = data_dir.join(CHECKPOINT_FILE_NAME);
        let status: DashMap<String, IndexSinkStatus> = DashMap::new();

        // Best-effort load of persisted checkpoints from a prior run. A
        // missing or corrupt file just means "start from scratch" — it is
        // never treated as fatal, since the sink is a best-effort mirror,
        // not the durability path.
        if let Ok(bytes) = std::fs::read(&checkpoint_path) {
            if let Ok(map) = serde_json::from_slice::<HashMap<String, u64>>(&bytes) {
                for (index, seq_no) in map {
                    status.insert(
                        index,
                        IndexSinkStatus {
                            checkpoint_seq_no: Some(seq_no),
                            ..Default::default()
                        },
                    );
                }
            }
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Arc::new(Self {
            enabled: cfg.enabled,
            target_kind: cfg.target_kind.clone(),
            runtime: RwLock::new(RuntimeConfig::from(cfg)),
            paused: AtomicBool::new(false),
            status,
            checkpoint_path,
            client,
        })
    }

    fn checkpoint_of(&self, index: &str) -> Option<u64> {
        self.status.get(index).and_then(|s| s.checkpoint_seq_no)
    }

    /// Persist every known checkpoint as one JSON map. Best-effort: a
    /// failure here only means a possible re-send of already-forwarded
    /// entries on the next restart (idempotent for `index`/`delete`
    /// actions), never data loss on the recovery path.
    fn persist_checkpoints(&self) {
        let map: HashMap<String, u64> = self
            .status
            .iter()
            .filter_map(|e| e.value().checkpoint_seq_no.map(|s| (e.key().clone(), s)))
            .collect();
        match serde_json::to_vec_pretty(&map) {
            Ok(bytes) => {
                if let Err(e) = xerj_engine::index::write_file_atomic(&self.checkpoint_path, &bytes)
                {
                    warn!(error = %e, "bulk_sink: failed to persist checkpoint file");
                }
            }
            Err(e) => warn!(error = %e, "bulk_sink: failed to serialize checkpoints"),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Background loop
// ─────────────────────────────────────────────────────────────────────────────

/// Entry point for the background task spawned once at server startup
/// (mirrors `es_compat::run_metrics_gauge_loop`'s wiring in
/// `xerj-server/src/main.rs`). No-ops immediately (and forever) when the
/// sink is disabled in config, so a disabled sink costs nothing beyond the
/// one-time check.
pub async fn run_loop(state: AppState) {
    let sink = state.bulk_sink.clone();
    if !sink.enabled {
        return;
    }
    info!("bulk_sink: enabled, starting poll loop");

    loop {
        let interval_ms = sink.runtime.read().await.flush_interval_ms.max(200);
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;

        if sink.paused.load(Ordering::Acquire) {
            continue;
        }

        let (patterns, batch_size, target_url, api_key) = {
            let cfg = sink.runtime.read().await;
            (
                cfg.indices.clone(),
                cfg.batch_size.max(1),
                cfg.target_url.clone(),
                cfg.target_api_key.clone(),
            )
        };

        // Sink stays inactive (even though `enabled = true`) until both an
        // index pattern and a target are configured — "never forward
        // everything by default".
        if patterns.is_empty() || target_url.trim().is_empty() {
            continue;
        }

        let all_indices = state.engine.list_indices().await;
        for info in all_indices {
            if !is_index_selected(&info.name, &patterns) {
                continue;
            }
            if let Err(e) = tick_one_index(
                &sink,
                &state,
                &info.name,
                &target_url,
                api_key.as_deref(),
                batch_size,
            )
            .await
            {
                warn!(index = %info.name, error = %e, "bulk_sink: tick failed, will retry next interval");
                sink.status.entry(info.name.clone()).or_default().last_error = Some(e.to_string());
            }
        }
    }
}

/// Read every not-yet-forwarded WAL entry for `index_name`, batch it into
/// `_bulk` bodies of at most `batch_size` entries, and push each batch in
/// order. The checkpoint (in memory + on disk) only advances past a batch
/// once that batch's `_bulk` POST returns 2xx; a failed batch stops
/// processing for this index this tick (later, still-pending batches are
/// retried next tick, in the same order) so the on-disk checkpoint is
/// always exactly "the last entry we know the target actually accepted."
async fn tick_one_index(
    sink: &BulkSinkService,
    state: &AppState,
    index_name: &str,
    target_url: &str,
    api_key: Option<&str>,
    batch_size: usize,
) -> Result<()> {
    let idx = state
        .engine
        .get_index(index_name)
        .map_err(|e| XerjError::wal(format!("index '{index_name}' unavailable: {e}")))?;

    let wal_dir = idx.data_dir().join("wal");
    let checkpoint = sink.checkpoint_of(index_name);

    // Read-only replay of the same WAL crash recovery uses. Never prunes,
    // never writes a `.wchk` — see module docs.
    let mut entries = xerj_storage::wal::replay_all_sorted(&wal_dir);
    entries.retain(|e| checkpoint.is_none_or(|c| e.seq_no > c));

    sink.status.entry(index_name.to_string()).or_default().lag = entries.len() as u64;

    if entries.is_empty() {
        return Ok(());
    }

    for chunk in entries.chunks(batch_size) {
        let chunk_max_seq = chunk.last().expect("non-empty chunk").seq_no;

        match build_bulk_body(index_name, chunk) {
            None => {
                // Entire chunk was non-doc entries (e.g. UpdateMapping) —
                // nothing to push, but still advance past it so it is not
                // re-examined forever.
                advance_checkpoint(sink, index_name, chunk_max_seq);
            }
            Some(body) => {
                push_with_retry(sink, target_url, api_key, &body).await?;
                advance_checkpoint(sink, index_name, chunk_max_seq);
                let mut status = sink.status.entry(index_name.to_string()).or_default();
                status.last_push_at = Some(Utc::now());
                status.last_error = None;
                let remaining = entries.iter().filter(|e| e.seq_no > chunk_max_seq).count();
                status.lag = remaining as u64;
            }
        }
    }

    Ok(())
}

fn advance_checkpoint(sink: &BulkSinkService, index_name: &str, seq_no: u64) {
    sink.status
        .entry(index_name.to_string())
        .or_default()
        .checkpoint_seq_no = Some(seq_no);
    sink.persist_checkpoints();
}

/// Build one `_bulk` NDJSON body from a chunk of WAL entries, or `None` if
/// the chunk contains no forwardable document operations (e.g. it is
/// entirely `UpdateMapping` entries — schema changes are not `_bulk` doc
/// actions and are not forwarded by this prototype).
fn build_bulk_body(index_name: &str, chunk: &[xerj_storage::wal::ReplayEntry]) -> Option<String> {
    let mut body = String::new();
    for entry in chunk {
        match &entry.entry {
            WalEntry::Index { doc_id, source } => {
                let action = serde_json::json!({
                    "index": { "_index": index_name, "_id": doc_id }
                });
                body.push_str(&action.to_string());
                body.push('\n');
                body.push_str(&source.to_string());
                body.push('\n');
            }
            WalEntry::Delete { doc_id } => {
                let action = serde_json::json!({
                    "delete": { "_index": index_name, "_id": doc_id }
                });
                body.push_str(&action.to_string());
                body.push('\n');
            }
            WalEntry::UpdateMapping { .. } => {
                // Not a doc op — nothing to forward via `_bulk`.
            }
        }
    }
    if body.is_empty() {
        None
    } else {
        Some(body)
    }
}

/// POST `body` to `<target_url>/_bulk`, retrying transient failures with
/// exponential backoff. Same spirit as `xerj-ai/src/embed.rs`'s
/// `send_with_retry`: 5xx / 429 / 408 and transport errors are transient
/// and retried; any other 4xx is a permanent misconfiguration (bad
/// index/mapping on the target, bad auth) and fails fast instead of
/// stalling the poll loop.
async fn push_with_retry(
    sink: &BulkSinkService,
    target_url: &str,
    api_key: Option<&str>,
    body: &str,
) -> Result<()> {
    let mut backoff = BASE_BACKOFF;
    let mut last_err = XerjError::wal("bulk_sink: no attempt made");

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            warn!(attempt, max = MAX_RETRIES, "bulk_sink: retrying push");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }

        match push_once(sink, target_url, api_key, body).await {
            Ok(()) => return Ok(()),
            Err((err, retryable)) if !retryable => {
                warn!(error = %err, "bulk_sink: push failed (non-transient, not retrying)");
                return Err(err);
            }
            Err((err, _)) => {
                debug!(attempt, error = %err, "bulk_sink: push attempt failed (transient)");
                last_err = err;
            }
        }
    }
    Err(last_err)
}

async fn push_once(
    sink: &BulkSinkService,
    target_url: &str,
    api_key: Option<&str>,
    body: &str,
) -> std::result::Result<(), (XerjError, bool)> {
    let url = format!("{}/_bulk", target_url.trim_end_matches('/'));
    let mut req = sink
        .client
        .post(&url)
        .header("Content-Type", "application/x-ndjson");
    if let Some(key) = api_key {
        req = req.header("Authorization", format!("ApiKey {key}"));
    }

    let resp = req
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| (XerjError::wal(format!("HTTP request to target: {e}")), true))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let retryable = status.as_u16() >= 500 || status.as_u16() == 429 || status.as_u16() == 408;
        let text = resp.text().await.unwrap_or_default();
        return Err((
            XerjError::wal(format!("target returned {status}: {text}")),
            retryable,
        ));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// REST handlers — /v1/bulk-sink/*
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct StatusResponse {
    enabled: bool,
    paused: bool,
    target_url: String,
    target_kind: String,
    /// Currently existing local indices that match `indices` (after the
    /// hardcoded `.`-prefix exclusion) — i.e. what would actually be
    /// forwarded right now, not just the configured patterns.
    active_indices: Vec<String>,
    indices_patterns: Vec<String>,
    batch_size: usize,
    flush_interval_ms: u64,
    per_index: HashMap<String, IndexSinkStatus>,
}

pub async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let sink = &state.bulk_sink;
    let cfg = sink.runtime.read().await.clone();

    let active_indices: Vec<String> = state
        .engine
        .list_indices()
        .await
        .into_iter()
        .map(|i| i.name)
        .filter(|name| is_index_selected(name, &cfg.indices))
        .collect();

    let per_index: HashMap<String, IndexSinkStatus> = sink
        .status
        .iter()
        .map(|e| (e.key().clone(), e.value().clone()))
        .collect();

    Json(StatusResponse {
        enabled: sink.enabled,
        paused: sink.paused.load(Ordering::Acquire),
        target_url: cfg.target_url,
        target_kind: sink.target_kind.clone(),
        active_indices,
        indices_patterns: cfg.indices,
        batch_size: cfg.batch_size,
        flush_interval_ms: cfg.flush_interval_ms,
        per_index,
    })
    .into_response()
}

#[derive(Debug, Serialize)]
struct ConfigResponse {
    enabled: bool,
    target_url: String,
    /// Never the key itself — only whether one is configured.
    target_api_key_set: bool,
    target_kind: String,
    indices: Vec<String>,
    batch_size: usize,
    flush_interval_ms: u64,
    #[serde(rename = "_note")]
    note: &'static str,
}

pub async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    let sink = &state.bulk_sink;
    let cfg = sink.runtime.read().await.clone();
    Json(ConfigResponse {
        enabled: sink.enabled,
        target_url: cfg.target_url,
        target_api_key_set: cfg.target_api_key.is_some(),
        target_kind: sink.target_kind.clone(),
        indices: cfg.indices,
        batch_size: cfg.batch_size,
        flush_interval_ms: cfg.flush_interval_ms,
        note: "runtime-only: changes here apply to this running process only; xerj.toml is unchanged and remains authoritative for the next restart",
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct PutConfigRequest {
    pub indices: Option<Vec<String>>,
    pub batch_size: Option<usize>,
    pub flush_interval_ms: Option<u64>,
    pub target_url: Option<String>,
    pub target_api_key: Option<String>,
}

pub async fn put_config(
    State(state): State<AppState>,
    Json(body): Json<PutConfigRequest>,
) -> impl IntoResponse {
    if let Some(patterns) = &body.indices {
        for p in patterns {
            if p.trim().is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "indices patterns must be non-empty strings"
                    })),
                )
                    .into_response();
            }
        }
    }
    if let Some(bs) = body.batch_size {
        if bs == 0 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "batch_size must be > 0" })),
            )
                .into_response();
        }
    }
    if let Some(fi) = body.flush_interval_ms {
        if fi == 0 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "flush_interval_ms must be > 0" })),
            )
                .into_response();
        }
    }
    if let Some(url) = &body.target_url {
        if url.trim().is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "target_url must not be empty" })),
            )
                .into_response();
        }
    }

    let sink = &state.bulk_sink;
    {
        let mut cfg = sink.runtime.write().await;
        if let Some(v) = body.indices {
            cfg.indices = v;
        }
        if let Some(v) = body.batch_size {
            cfg.batch_size = v;
        }
        if let Some(v) = body.flush_interval_ms {
            cfg.flush_interval_ms = v;
        }
        if let Some(v) = body.target_url {
            cfg.target_url = v;
        }
        if let Some(v) = body.target_api_key {
            cfg.target_api_key = Some(v);
        }
    }

    info!("bulk_sink: runtime config updated via PUT /v1/bulk-sink/config");
    get_config(State(state)).await.into_response()
}

pub async fn pause(State(state): State<AppState>) -> impl IntoResponse {
    state.bulk_sink.paused.store(true, Ordering::Release);
    Json(serde_json::json!({ "paused": true })).into_response()
}

pub async fn resume(State(state): State<AppState>) -> impl IntoResponse {
    state.bulk_sink.paused.store(false, Ordering::Release);
    Json(serde_json::json!({ "paused": false })).into_response()
}
