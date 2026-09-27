//! Deterministic pack emission: merged records → the portable artifact.
//!
//! A pack is a directory (a zip of it later) that ANY tool can consume:
//! sharded `records-*.jsonl`, a `manifest.json` with per-file sha256s, the
//! recipe that built it, and a `SHA256SUMS` over everything. Determinism is
//! the load-bearing property: shard assignment is `xxh3_64(id) % shards`,
//! records are id-ordered, and every record carries the SAME key set (a
//! key absent from one record is `[]` for array keys, `null` otherwise) —
//! so an unchanged input rebuilds byte-identical shard files, and the
//! `_id`s the engine derives stay stable across rebuilds (autoindex
//! clusters datasets by field-name set; an omitted optional field would
//! silently move a record between datasets).
//!
//! `manifest.json` and `SHA256SUMS` necessarily vary (timestamp,
//! generation) — determinism is claimed for the RECORDS, never the wrapper.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use xxhash_rust::xxh3::xxh3_64;

use super::recipe::Recipe;

/// Per-source counts for the manifest: what this run actually did.
pub struct SourceStat {
    pub slug: String,
    pub kind: String,
    pub path: Option<String>,
    pub url: Option<String>,
    pub licence: String,
    pub records: usize,
    pub new: usize,
    pub skipped: usize,
    pub pruned: usize,
}

pub struct RunStats {
    pub envelopes: usize,
    pub per_source: Vec<SourceStat>,
}

#[derive(Debug)]
pub struct PackResult {
    pub dir: PathBuf,
    pub generation: u64,
    pub records: usize,
    pub shards: usize,
}

/// Build (or rebuild) the pack under `<build_dir>/pack/<name>`.
pub fn emit(
    build_dir: &Path,
    recipe: &Recipe,
    recipe_toml: &str,
    mut records: Vec<Map<String, Value>>,
    stats: &RunStats,
) -> Result<PackResult> {
    let pack_dir = build_dir.join("pack").join(&recipe.name);
    let generation = previous_generation(&pack_dir)? + 1;
    std::fs::create_dir_all(&pack_dir)
        .with_context(|| format!("cannot create {}", pack_dir.display()))?;

    // Order first, then refuse duplicate ids: a pack with two records of
    // the same id breaks the engine-side _id determinism promise, and a
    // duplicate here means identity resolution failed silently.
    records.sort_by(|a, b| {
        a.get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("id").and_then(Value::as_str).unwrap_or(""))
    });
    let mut seen_id = String::new();
    for r in &records {
        let id = r.get("id").and_then(Value::as_str).unwrap_or("");
        if !seen_id.is_empty() && id == seen_id {
            bail!("duplicate emitted id '{id}': identity edges failed to merge two records that claim the same id");
        }
        seen_id = id.to_string();
    }

    let shards = recipe.emit.shards.min(records.len()).max(1);
    let uniform = uniform_keys(&records);

    // shard → line buffer; records are id-sorted, so appending preserves
    // in-shard order without a second sort.
    let mut shard_lines: Vec<String> = vec![String::new(); shards];
    for r in &records {
        let id = r.get("id").and_then(Value::as_str).unwrap_or("");
        let bucket = (xxh3_64(id.as_bytes()) % shards as u64) as usize;
        let mut full = r.clone();
        for (k, is_array) in &uniform {
            full.entry(k.clone()).or_insert_with(|| {
                if *is_array {
                    Value::Array(Vec::new())
                } else {
                    Value::Null
                }
            });
        }
        shard_lines[bucket].push_str(&super::store::canonical_json(&full));
        shard_lines[bucket].push('\n');
    }

    let mut files: BTreeMap<String, Value> = BTreeMap::new();
    for (i, lines) in shard_lines.iter().enumerate() {
        let name = format!("records-{:05}.jsonl", i);
        if lines.is_empty() {
            continue; // small corpora need fewer files than shards
        }
        let path = pack_dir.join(&name);
        std::fs::write(&path, lines.as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))?;
        let n = lines.lines().count();
        files.insert(
            name.clone(),
            serde_json::json!({ "sha256": hex(&Sha256::digest(lines.as_bytes())), "records": n }),
        );
    }

    // the recipe ships verbatim: it is the pack's provenance, licence
    // reasoning included
    std::fs::write(pack_dir.join("recipe.toml"), recipe_toml)?;
    let recipe_sha = hex(&Sha256::digest(recipe_toml.as_bytes()));

    let manifest = serde_json::json!({
        "format_version": 1,
        "kind": "harvested",
        "pack": recipe.name,
        "description": recipe.description,
        "created_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "generation": generation,
        "recipe_sha256": recipe_sha,
        "counts": {
            "envelopes": stats.envelopes,
            "records": records.len(),
            "shards": shards,
        },
        "sources": stats.per_source.iter().map(|s| serde_json::json!({
            "slug": s.slug, "kind": s.kind, "path": s.path, "url": s.url,
            "licence": s.licence, "records": s.records,
            "new": s.new, "skipped": s.skipped, "pruned": s.pruned,
        })).collect::<Vec<_>>(),
        "files": Value::Object(files.clone().into_iter().collect()),
        "tool": { "name": "xerj", "version": env!("CARGO_PKG_VERSION") },
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    std::fs::write(pack_dir.join("manifest.json"), &manifest_bytes)?;

    let mut sums = String::new();
    let mut names: Vec<String> = files.keys().cloned().collect();
    names.push("recipe.toml".to_string());
    names.push("manifest.json".to_string());
    names.sort();
    for n in &names {
        let bytes = std::fs::read(pack_dir.join(n)).with_context(|| format!("checksumming {n}"))?;
        sums.push_str(&hex(&Sha256::digest(&bytes)));
        sums.push_str("  ");
        sums.push_str(n);
        sums.push('\n');
    }
    std::fs::write(pack_dir.join("SHA256SUMS"), sums)?;

    Ok(PackResult {
        dir: pack_dir,
        generation,
        records: records.len(),
        shards,
    })
}

/// The uniform key set: every key any record carries, and whether missing
/// values should fill as `[]` (array wherever it appears) or `null`.
fn uniform_keys(records: &[Map<String, Value>]) -> BTreeMap<String, bool> {
    let mut out: BTreeMap<String, (bool /*any array*/, bool /*all non-null array*/)> =
        BTreeMap::new();
    for r in records {
        for (k, v) in r {
            let e = out.entry(k.clone()).or_insert((false, true));
            if v.is_array() {
                e.0 = true;
            } else if !v.is_null() {
                e.1 = false;
            }
        }
    }
    out.into_iter()
        .map(|(k, (any, all))| (k, any && all))
        .collect()
}

fn previous_generation(pack_dir: &Path) -> Result<u64> {
    let Ok(bytes) = std::fs::read(pack_dir.join("manifest.json")) else {
        return Ok(0);
    };
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Ok(v.get("generation").and_then(Value::as_u64).unwrap_or(0))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harvest::recipe::{Emit, Envelope, Identity, Merge};
    use serde_json::json;

    fn recipe(name: &str, shards: usize) -> Recipe {
        Recipe {
            name: name.to_string(),
            description: String::new(),
            envelope: Envelope {
                id_from: vec!["id".into()],
                title_from: vec!["id".into()],
                body_join: vec![],
                defs_from: vec![],
                passthrough: false,
            },
            sources: vec![],
            identity: Identity {
                edges: vec![],
                canonical_source_order: vec![],
            },
            merge: Merge::default(),
            derived: vec![],
            emit: Emit { shards },
        }
    }

    fn rec(id: &str, extra: &[(&str, Value)]) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("id".into(), json!(id));
        for (k, v) in extra {
            m.insert(k.to_string(), v.clone());
        }
        m
    }

    fn stats() -> RunStats {
        RunStats {
            envelopes: 0,
            per_source: vec![],
        }
    }

    #[test]
    fn uniform_keys_fill_and_shard_determinism() {
        let tmp = tempfile::tempdir().unwrap();
        let records = vec![
            rec("a", &[("aliases", json!(["X1"]))]),
            rec("b", &[("severity", json!("HIGH"))]),
        ];
        let r1 = emit(
            tmp.path(),
            &recipe("t", 4),
            "# recipe\n",
            records.clone(),
            &stats(),
        )
        .unwrap();
        assert_eq!(r1.records, 2);
        assert_eq!(r1.generation, 1);

        // every shard file's records carry BOTH keys
        for entry in std::fs::read_dir(&r1.dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("records-") {
                continue;
            }
            for line in std::fs::read_to_string(entry.path()).unwrap().lines() {
                let v: Value = serde_json::from_str(line).unwrap();
                assert!(v.get("aliases").is_some(), "array key filled");
                assert!(v.get("severity").is_some(), "scalar key filled");
                let id = v["id"].as_str().unwrap();
                if id == "a" {
                    assert_eq!(v["aliases"], json!(["X1"]));
                    assert_eq!(v["severity"], json!(null));
                } else {
                    assert_eq!(v["aliases"], json!([]), "array key fills empty");
                    assert_eq!(v["severity"], json!("HIGH"));
                }
            }
        }

        // rebuild from a DIFFERENT input order → byte-identical records
        let mut shuffled = records.clone();
        shuffled.reverse();
        let before: Vec<(String, Vec<u8>)> = std::fs::read_dir(&r1.dir)
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().to_string(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .filter(|(n, _)| n.starts_with("records-"))
            .collect();
        let r2 = emit(
            tmp.path(),
            &recipe("t", 4),
            "# recipe\n",
            shuffled,
            &stats(),
        )
        .unwrap();
        assert_eq!(r2.generation, 2, "generation advances");
        for (name, bytes) in before {
            assert_eq!(
                std::fs::read(r2.dir.join(&name)).unwrap(),
                bytes,
                "{name} identical"
            );
        }
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let records = vec![rec("a", &[]), rec("a", &[])];
        let err = emit(tmp.path(), &recipe("t", 2), "", records, &stats())
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate emitted id"), "{err}");
    }

    #[test]
    fn sums_cover_every_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let records = vec![rec("a", &[]), rec("b", &[]), rec("c", &[])];
        let r = emit(tmp.path(), &recipe("t", 2), "# r\n", records, &stats()).unwrap();
        let sums = std::fs::read_to_string(r.dir.join("SHA256SUMS")).unwrap();
        assert!(sums.contains("  manifest.json\n"));
        assert!(sums.contains("  recipe.toml\n"));
        for line in sums.lines() {
            let (sum, name) = line.split_once("  ").unwrap();
            let bytes = std::fs::read(r.dir.join(name)).unwrap();
            assert_eq!(sum, hex(&Sha256::digest(&bytes)), "{name} hash matches");
        }
        let m: Value =
            serde_json::from_str(&std::fs::read_to_string(r.dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(m["format_version"], json!(1));
        assert_eq!(m["kind"], json!("harvested"));
        assert_eq!(m["counts"]["records"], json!(3));
        // shard assignment is a hash — do not assert WHICH files exist, only
        // that the ones that do cover every record and none is empty
        let files = m["files"].as_object().unwrap();
        assert!(!files.is_empty());
        assert!(files.len() <= 2);
        let total: usize = files
            .values()
            .map(|f| f["records"].as_u64().unwrap() as usize)
            .sum();
        assert_eq!(total, 3);
        for f in files.values() {
            assert!(
                f["records"].as_u64().unwrap() > 0,
                "empty shard files are never written"
            );
        }
    }
}
