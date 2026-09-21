//! #965 — engine-level end-to-end for the object-storage backend.
//!
//! The storage crate's own tests pin the bundle/catalog mechanics
//! (`xerj-storage` object-store suite). What only the ENGINE can prove is the
//! composition: `storage_mode_from_config` turning `storage.backend = "s3"`
//! into a per-index object namespace, the flush path uploading through the
//! real Index wiring, the per-index meta objects (settings/schema/mapping)
//! riding the same bucket, and a FRESH node — an empty data directory whose
//! only local knowledge is which indices exist — booting from the bucket and
//! serving full-text search over hydrated families.
//!
//! The object store is the in-process filesystem simulation
//! (`XERJ_TEST_OBJECT_STORE_DIR`), never a network endpoint: local testing
//! only, no credentials, no cloud.
//!
//! ```bash
//! cargo test -p xerj-engine --test object_storage_e2e -- --nocapture
//! ```

use serde_json::json;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use xerj_common::config::{Config, StorageBackendType};
use xerj_common::types::Schema;
use xerj_engine::Engine;
use xerj_query::parse_request;

/// Point the object-store simulation at `root` for the lifetime of the guard.
///
/// `Drop` (not a trailing `remove_var`) clears it so a panicked assertion
/// cannot leak the override into anything that runs after this test in the
/// same process. The variable is read ONLY in the `backend = "s3"` arm of
/// `storage_mode_from_config`, so local-mode tests never see it either way.
struct ObjectStoreDir;

impl ObjectStoreDir {
    fn set(root: &Path) -> Self {
        std::env::set_var("XERJ_TEST_OBJECT_STORE_DIR", root);
        Self
    }
}

impl Drop for ObjectStoreDir {
    fn drop(&mut self) {
        std::env::remove_var("XERJ_TEST_OBJECT_STORE_DIR");
    }
}

/// An engine in object-storage mode over `data_dir`, sharing whatever bucket
/// `XERJ_TEST_OBJECT_STORE_DIR` names. Local defaults otherwise.
fn object_engine(data_dir: &Path) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = data_dir.to_str().unwrap().to_string();
    config.storage.backend = StorageBackendType::S3;
    config.storage.s3_bucket = "e2e-bucket".to_string();
    Engine::new(config).expect("engine::new in object-store mode")
}

/// Every file under `root`, recursively, as `/`-separated paths relative to it.
fn bucket_objects(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(root, &mut files);
    files
        .iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

// Multi-threaded runtime: the object-store bridges use
// `tokio::task::block_in_place`, which panics on the current-thread flavour
// `#[tokio::test]` defaults to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_s3_index_round_trips_to_a_fresh_node() {
    let bucket = TempDir::new().unwrap();
    let _object_store = ObjectStoreDir::set(bucket.path());

    // ── Node A: create the index, write, flush. ────────────────────────────
    // The flush is the moment the segment becomes a bucket object, so this
    // block owns the node until then; dropping the engine simulates the
    // process dying right after (no clean shutdown, no lingering tasks).
    let node_a = TempDir::new().unwrap();
    {
        let engine = object_engine(node_a.path());
        engine.create_index("objs", Schema::empty()).unwrap();
        let idx = engine.get_index("objs").unwrap();
        for i in 0..3u32 {
            idx.index_document(
                Some(format!("doc-{i}")),
                json!({
                    "seq": i,
                    "body": format!("zebrula payload number {i} of the object-storage round trip"),
                }),
            )
            .await
            .unwrap();
        }
        engine.flush_index("objs").await.unwrap();
    }

    // The bucket — and nothing but the bucket — now describes the index:
    // one immutable bundle per segment (two here: the very first document
    // triggers a dynamic-mapping flush before the rest accumulate — engine
    // behaviour in local mode too, not an object-store artefact), the
    // snapshot catalogue, and the per-index meta objects that make a fresh
    // node recreate the SAME index rather than a dynamically re-mapped
    // lookalike.
    let objects = bucket_objects(bucket.path());
    assert!(
        objects.iter().all(|o| o.starts_with("xerj/indices/objs/")),
        "the per-index namespace must be <s3_prefix>/indices/<name>/, got {objects:?}"
    );
    let catalog_path = objects
        .iter()
        .find(|o| o.ends_with("/snapshot.json"))
        .expect("the flush must publish the snapshot catalogue");
    let catalog: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(bucket.path().join(catalog_path)).unwrap())
            .unwrap();
    let segment_ids: Vec<&str> = catalog["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert!(
        !segment_ids.is_empty(),
        "the catalogue must name the flushed segments: {catalog}"
    );
    for id in &segment_ids {
        assert!(
            objects
                .iter()
                .any(|o| o == &format!("xerj/indices/objs/segments/{id}.bundle")),
            "every catalogued segment must be exactly one bundle object ({id}), got {objects:?}"
        );
    }
    // settings.json is only persisted when the create carried settings, so
    // what this index must publish is the schema and the analysis binding.
    for expected in [
        "xerj/indices/objs/meta/schema.json",
        "xerj/indices/objs/meta/analysis-binding.json",
    ] {
        assert!(
            objects.iter().any(|o| o == expected),
            "bucket must hold {expected} after the flush, got {objects:?}"
        );
    }

    // ── Node B: a fresh process with an empty data directory. ──────────────
    // Index EXISTENCE stays local in v1 (engine boot scans data_dir for
    // directories with a WAL subdir — a deployment manifest, effectively);
    // everything else — settings, schema, mapping, every segment family —
    // comes from the bucket on open.
    let node_b = TempDir::new().unwrap();
    std::fs::create_dir_all(node_b.path().join("objs").join("wal")).unwrap();
    let engine_b = object_engine(node_b.path());
    let idx = engine_b.get_index("objs").unwrap();

    // Full-text search over a hydrated family: the bundle unpacked into the
    // local segments dir, postings and all.
    let hits = idx
        .search(
            &parse_request(&json!({
                "query": { "match": { "body": "zebrula" } }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        hits.total.value, 3,
        "the fresh node must serve the flushed documents, got {}",
        hits.total.value
    );

    let hydrated = file_count(&node_b.path().join("objs").join("segments"));
    assert!(
        hydrated >= 2,
        "the family must have been hydrated locally (postings + stored at \
         minimum), found {hydrated} files"
    );

    // ── Node B becomes the writer: the catalog advances, coherently. ───────
    idx.index_document(
        Some("doc-3".into()),
        json!({"seq": 3, "body": "zebrula payload number 3 written by the fresh node"}),
    )
    .await
    .unwrap();
    engine_b.flush_index("objs").await.unwrap();

    let after = idx
        .search(
            &parse_request(&json!({
                "query": { "match": { "body": "zebrula" } },
                "size": 0
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(after.total.value, 4, "post-flush catalog must count 4 docs");

    let objects_after = bucket_objects(bucket.path());
    let bundles_after: Vec<&String> = objects_after
        .iter()
        .filter(|o| o.starts_with("xerj/indices/objs/segments/") && o.ends_with(".bundle"))
        .collect();
    assert_eq!(
        bundles_after.len(),
        segment_ids.len() + 1,
        "node B's flush must add exactly one bundle on top of the ones it \
         booted from, got {objects_after:?}"
    );
    assert!(
        objects_after
            .iter()
            .any(|o| o.ends_with("indices/objs/snapshot.json")),
        "the catalogue object must still be there after node B published"
    );
}

/// Files in `dir` (0 when the dir does not exist yet).
fn file_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|d| d.flatten().count())
        .unwrap_or(0)
}
