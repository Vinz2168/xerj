//! Input-format adapters: one function per known record FORMAT, each turning
//! a source value into flat key paths.
//!
//! An adapter knows the INPUT format, never our domain: `osv` understands
//! the ossf/osv-schema shape (shared by osv.dev dumps and github-reviewed
//! GHSA files); `rustsec-md` understands RustSec's native advisory file
//! (TOML frontmatter + markdown — the mechanism is generic frontmatter
//! extraction, the curated table names are upstream's); `flat` does generic
//! flattening for everything else. Which keys of the result feed the query
//! envelope is the recipe's job, not the adapter's — the same `osv` adapter
//! would serve a Go vulnerability corpus unchanged.
//!
//! Flattening mirrors `extract::flatten_object` semantics (two levels of
//! nesting become `a_b` keys; deeper structure and arrays-of-objects are
//! stored as JSON strings; arrays of scalars stay arrays) so a record
//! normalized here does not transform a second time at index time.
//!
//! One deliberate omission: OSV `database_specific` is NEVER read. The
//! schema marks it "intended for internal use only" and "subject to change
//! without notice" — building on it means building on a field its owner
//! says may vanish. Ecosystem-specific fields ARE read (they are the
//! documented extension point upstreams actually publish to, and RustSec's
//! affected-functions list lives there).

use serde_json::{Map, Value};

use super::recipe::Format;

/// Normalize one record under `format` into flat key paths.
pub fn normalize(format: Format, v: &Value) -> Map<String, Value> {
    match format {
        Format::Flat => flatten(v),
        Format::Osv => normalize_osv(v),
        Format::RustsecMd => match v {
            Value::String(s) => normalize_rustsec_md(s),
            other => flatten(other),
        },
    }
}

// ── generic flattening ──────────────────────────────────────────────────────

/// Flatten any JSON value into flat keys, `extract::flatten_object`
/// semantics. A non-object top level becomes `{"value": v}` so envelope
/// mapping still has something to point at.
pub fn flatten(v: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    match v {
        Value::Object(m) => {
            for (k, vv) in m {
                flatten_into(k, vv.clone(), 0, &mut out);
            }
        }
        other => {
            out.insert("value".to_string(), other.clone());
        }
    }
    out
}

fn flatten_into(key: &str, v: Value, depth: usize, out: &mut Map<String, Value>) {
    if out.len() >= super::MAX_FIELDS {
        return;
    }
    match v {
        Value::Object(m) => {
            if depth < 2 {
                for (k, vv) in m {
                    flatten_into(&format!("{key}_{k}"), vv, depth + 1, out);
                }
            } else {
                out.insert(
                    key.to_string(),
                    Value::String(serde_json::to_string(&m).unwrap_or_default()),
                );
            }
        }
        Value::Array(a) => {
            if a.iter().all(|e| !e.is_object() && !e.is_array()) {
                out.insert(key.to_string(), Value::Array(a));
            } else {
                out.insert(
                    key.to_string(),
                    Value::String(serde_json::to_string(&a).unwrap_or_default()),
                );
            }
        }
        other => {
            out.insert(key.to_string(), other);
        }
    }
}

// ── OSV ─────────────────────────────────────────────────────────────────────

/// Curated extraction of the OSV v1.x fields that carry meaning across
/// sources, plus generic flattening of anything else. See the module docs
/// for why `database_specific` is absent.
fn normalize_osv(v: &Value) -> Map<String, Value> {
    let obj: Map<String, Value> = match v {
        Value::Object(m) => m.clone(),
        other => return flatten(other),
    };
    let mut out = Map::new();

    // Direct scalars that exist in every OSV record class.
    for k in [
        "id",
        "modified",
        "published",
        "withdrawn",
        "summary",
        "details",
    ] {
        if let Some(s) = obj.get(k).and_then(Value::as_str) {
            if !s.is_empty() {
                out.insert(k.to_string(), Value::String(s.to_string()));
            }
        }
    }

    // `aliases` / `related`: scalar string arrays — they survive flattening
    // as arrays, and identity unions on every element.
    for k in ["aliases", "related"] {
        if let Some(arr) = string_array(obj.get(k)) {
            if !arr.is_empty() {
                out.insert(k.to_string(), Value::Array(arr));
            }
        }
    }

    // `severity`: [{type, score, source}] — first entry wins for the scalar,
    // all scores kept for evidence.
    if let Some(sev) = obj.get("severity").and_then(Value::as_array) {
        let scores: Vec<Value> = sev
            .iter()
            .filter_map(|s| s.get("score").and_then(Value::as_str))
            .map(|s| Value::String(s.to_string()))
            .collect();
        if let Some(first) = scores.first() {
            out.insert("severity".to_string(), first.clone());
        }
        if !scores.is_empty() {
            out.insert("severity_scores".to_string(), Value::Array(scores));
        }
    }

    // `affected[]`: version knowledge. OSV packs multiple introduced/fixed
    // pairs into one SEMVER range (RustSec shape) while GHSA splits them
    // across entries; both land in the same flat lists here so the merge
    // unions intervals instead of comparing shapes.
    let mut ecosystems: Vec<Value> = Vec::new();
    let mut packages: Vec<Value> = Vec::new();
    let mut versions: Vec<Value> = Vec::new();
    let mut introduced: Vec<Value> = Vec::new();
    let mut fixed: Vec<Value> = Vec::new();
    let mut last_affected: Vec<Value> = Vec::new();
    for aff in obj
        .get("affected")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
    {
        if let Some(name) = aff
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
        {
            packages.push(Value::String(name.to_string()));
        }
        if let Some(eco) = aff
            .get("package")
            .and_then(|p| p.get("ecosystem"))
            .and_then(Value::as_str)
        {
            ecosystems.push(Value::String(eco.to_string()));
        }
        if let Some(vs) = string_array(aff.get("versions")) {
            versions.extend(vs);
        }
        for range in aff
            .get("ranges")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            for ev in range
                .get("events")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new())
            {
                for (key, sink) in [
                    ("introduced", &mut introduced),
                    ("fixed", &mut fixed),
                    ("last_affected", &mut last_affected),
                ] {
                    if let Some(s) = ev.get(key).and_then(Value::as_str) {
                        sink.push(Value::String(s.to_string()));
                    }
                }
            }
        }
    }
    dedup_into(&mut out, "ecosystems", ecosystems);
    dedup_into(&mut out, "packages", packages);
    dedup_into(&mut out, "affected_versions", versions);
    dedup_into(&mut out, "introduced_versions", introduced);
    dedup_into(&mut out, "fixed_versions", fixed);
    dedup_into(&mut out, "last_affected_versions", last_affected);

    // `references[]`: {type, url} — flattened into parallel scalar arrays.
    let mut urls: Vec<Value> = Vec::new();
    let mut types: Vec<Value> = Vec::new();
    for r in obj
        .get("references")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
    {
        if let Some(u) = r.get("url").and_then(Value::as_str) {
            urls.push(Value::String(u.to_string()));
        }
        if let Some(t) = r.get("type").and_then(Value::as_str) {
            types.push(Value::String(t.to_string()));
        }
    }
    dedup_into(&mut out, "reference_urls", urls);
    dedup_into(&mut out, "reference_types", types);

    // `ecosystem_specific`: the documented upstream extension point. RustSec
    // publishes affected FUNCTIONS here (`affects.functions` — canonical
    // paths keyed to version ranges), which no other source in the stack
    // carries; that is the one structured extraction worth doing.
    if let Some(eco) = obj.get("ecosystem_specific") {
        if let Some(functions) = eco
            .pointer("/affects/functions")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
        {
            if !functions.is_empty() {
                out.insert(
                    "affected_functions".to_string(),
                    Value::Array(functions.into_iter().map(Value::String).collect::<Vec<_>>()),
                );
            }
        }
        if let Some(info) = eco.get("informational").and_then(Value::as_str) {
            out.insert("informational".to_string(), Value::String(info.to_string()));
        }
    }

    // Everything not already curated flows through the generic flattener so
    // unknown-but-real OSV fields survive into the pack (curated keys win on
    // collision — they are the ones with merge semantics).
    for (k, vv) in &obj {
        if matches!(k.as_str(), "database_specific") {
            continue;
        }
        if out.contains_key(k) {
            continue;
        }
        flatten_into(k, vv.clone(), 0, &mut out);
    }
    out
}

// ── RustSec native advisory ─────────────────────────────────────────────────

/// Split a RustSec advisory into (frontmatter TOML text, markdown body). The
/// file is one ```toml fenced block followed by prose; a file without the
/// fence yields no frontmatter and the whole text as body.
fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let t = text.trim_start_matches('\u{feff}');
    let Some(after) = t.strip_prefix("```toml\n") else {
        return (None, t);
    };
    match after.find("\n```\n") {
        Some(i) => (Some(&after[..i]), &after[i + 5..]),
        // a fence that never closes: treat everything after the opener as
        // frontmatter-shaped text we cannot trust, keep it as body instead
        None => (None, t),
    }
}

/// RustSec's native `crates/<crate>/RUSTSEC-*.md`: a ```toml frontmatter
/// (`[advisory]`/`[versions]`/`[affected]` tables) plus the advisory's
/// markdown prose. The `osv`-branch conversion drops `affected.functions`
/// and nulls `ecosystem_specific` (measured 2026-09-27: 0 of 1,252 records
/// carry functions there), so the native files are the ONLY source of the
/// curated vulnerable-function paths.
///
/// Facts both adapters carry land in the SAME key names (`id`, `aliases`,
/// `summary`, `severity`, `affected_functions`, `withdrawn`, …) so a recipe
/// can merge this source with an `osv` one without a bridge; RustSec-only
/// facts (`patched_versions`, `categories`, `keywords`, `date`) keep their
/// own names. Uncurated frontmatter keys flow through the generic
/// flattener prefixed by their table (`advisory_foo`), mirroring the osv
/// adapter's passthrough.
fn normalize_rustsec_md(text: &str) -> Map<String, Value> {
    let (fm, body) = split_frontmatter(text);
    let mut out = Map::new();

    let parsed = fm
        .map(|f| f.parse::<toml::Value>())
        .transpose()
        .ok()
        .flatten();
    let tables: Option<&toml::value::Table> = match parsed.as_ref() {
        Some(toml::Value::Table(t)) => Some(t),
        _ => None,
    };

    // Track what curated extraction consumed so the passthrough loop below
    // does not duplicate it under a prefixed name.
    let mut curated: Vec<(&str, &str)> = Vec::new();
    let sub = |name: &str| tables.and_then(|t| t.get(name)).and_then(|v| v.as_table());
    let advisory = sub("advisory");
    let versions = sub("versions");
    let affected = sub("affected");
    let put_str = |out: &mut Map<String, Value>, key: &str, s: String| {
        if !s.is_empty() {
            out.insert(key.to_string(), Value::String(s));
        }
    };

    if let Some(advisory) = advisory {
        for k in ["id", "date", "url", "title", "withdrawn", "informational"] {
            if let Some(s) = advisory.get(k).and_then(|v| v.as_str()) {
                curated.push(("advisory", k));
                put_str(&mut out, k, s.to_string());
            }
        }
        // `cvss` is a bare vector string here; the osv adapter keeps the
        // first score in `severity` and all in `severity_scores` — same.
        if let Some(cvss) = advisory.get("cvss").and_then(|v| v.as_str()) {
            curated.push(("advisory", "cvss"));
            put_str(&mut out, "severity", cvss.to_string());
            out.insert(
                "severity_scores".to_string(),
                Value::Array(vec![Value::String(cvss.to_string())]),
            );
        }
        if let Some(pkg) = advisory.get("package").and_then(|v| v.as_str()) {
            curated.push(("advisory", "package"));
            out.insert(
                "packages".to_string(),
                Value::Array(vec![Value::String(pkg.to_string())]),
            );
        }
        for k in ["aliases", "related", "categories", "keywords"] {
            if let Some(arr) = advisory.get(k).and_then(|v| v.as_array()) {
                let vals: Vec<Value> = arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| Value::String(s.to_string()))
                    .collect();
                if !vals.is_empty() {
                    curated.push(("advisory", k));
                    out.insert(k.to_string(), Value::Array(vals));
                }
            }
        }
    }
    if let Some(versions) = versions {
        for (src, dst) in [
            ("patched", "patched_versions"),
            ("unaffected", "unaffected_versions"),
        ] {
            if let Some(arr) = versions.get(src).and_then(|v| v.as_array()) {
                let vals: Vec<Value> = arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| Value::String(s.to_string()))
                    .collect();
                if !vals.is_empty() {
                    curated.push(("versions", src));
                    out.insert(dst.to_string(), Value::Array(vals));
                }
            }
        }
    }
    if let Some(affected) = affected {
        // `functions` is an inline table of function path → version ranges;
        // the PATHS are the joinable fact (same key the osv adapter's
        // ecosystem_specific extraction produces). Sorted: the file's key
        // order must not reach the pack bytes.
        if let Some(fns) = affected.get("functions").and_then(|v| v.as_table()) {
            let mut paths: Vec<String> = fns.keys().cloned().collect();
            paths.sort();
            if !paths.is_empty() {
                curated.push(("affected", "functions"));
                out.insert(
                    "affected_functions".to_string(),
                    Value::Array(paths.into_iter().map(Value::String).collect()),
                );
            }
        }
    }

    // Everything not curated flows through the generic flattener prefixed by
    // its table, so unknown-but-real frontmatter survives into the pack.
    if let Some(t) = tables {
        for (tname, table_val) in t {
            let Some(sub) = table_val.as_table() else {
                continue;
            };
            for (k, v) in sub {
                if curated.iter().any(|(tn, ck)| *tn == tname && ck == k) {
                    continue;
                }
                let toml_json =
                    |tv: &toml::Value| -> Value { serde_json::to_value(tv).unwrap_or(Value::Null) };
                flatten_into(&format!("{tname}_{k}"), toml_json(v), 0, &mut out);
            }
        }
    }

    // The prose: first `# ` heading is the advisory's title when the
    // frontmatter did not carry one; the whole body is the `details`.
    let heading = body
        .lines()
        .find(|l| l.starts_with("# "))
        .map(|l| l[2..].trim().to_string());
    if let Some(h) = heading {
        put_str(&mut out, "summary", h);
    }
    let trimmed = body.trim();
    if !trimmed.is_empty() {
        put_str(&mut out, "details", trimmed.to_string());
    }
    out
}

fn string_array(v: Option<&Value>) -> Option<Vec<Value>> {
    match v {
        Some(Value::Array(a)) if a.iter().all(|e| e.is_string()) => Some(a.to_vec()),
        Some(Value::String(s)) => Some(vec![Value::String(s.clone())]),
        _ => None,
    }
}

fn dedup_into(out: &mut Map<String, Value>, key: &str, vals: Vec<Value>) {
    if vals.is_empty() {
        return;
    }
    let mut seen: Vec<String> = Vec::new();
    let mut kept: Vec<Value> = Vec::new();
    for v in vals {
        if let Some(s) = v.as_str() {
            if s.is_empty() || seen.iter().any(|x| x == s) {
                continue;
            }
            seen.push(s.to_string());
            kept.push(v);
        }
    }
    if !kept.is_empty() {
        out.insert(key.to_string(), Value::Array(kept));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flat_mirrors_extract_semantics() {
        let v = json!({
            "id": "x",
            "meta": {"a": 1, "b": {"deep": true}},
            "deeper": {"x": {"y": {"z": 1}}},
            "tags": ["a", "b"],
            "rows": [{"k": 1}]
        });
        let m = normalize(Format::Flat, &v);
        assert_eq!(m["id"], json!("x"));
        assert_eq!(m["meta_a"], json!(1));
        // three key segments survive (b's object is at depth 1 < 2)…
        assert_eq!(m["meta_b_deep"], json!(true));
        // …the object at depth 2 is stringified, exactly like flatten_object
        assert_eq!(m["deeper_x_y"], json!("{\"z\":1}"));
        assert_eq!(m["tags"], json!(["a", "b"]));
        assert!(m["rows"].is_string(), "arrays of objects stringify");
    }

    #[test]
    fn osv_extracts_identity_and_versions() {
        let v = json!({
            "id": "RUSTSEC-2021-0003",
            "aliases": ["CVE-2021-25900", "GHSA-43w2-9j62-hq99"],
            "summary": "Out of bounds write in SmallVec::insert_many",
            "modified": "2023-06-20T00:00:00Z",
            "affected": [{
                "package": {"ecosystem": "crates.io", "name": "smallvec"},
                "ranges": [{
                    "type": "SEMVER",
                    "events": [
                        {"introduced": "0.6.3"}, {"fixed": "0.6.14"},
                        {"introduced": "1.0.0"}, {"fixed": "1.6.1"}
                    ]
                }],
                "versions": ["0.6.5"]
            }],
            "references": [
                {"type": "WEB", "url": "https://github.com/servo/rust-smallvec/commit/abc"},
                {"type": "PACKAGE", "url": "https://crates.io/crates/smallvec"}
            ],
            "ecosystem_specific": {"affects": {"functions": {"smallvec::SmallVec::insert_many": ["<1.6.1"]}}},
            "database_specific": {"severity": "HIGH", "cwe_ids": ["CWE-787"]}
        });
        let m = normalize(Format::Osv, &v);
        assert_eq!(m["id"], json!("RUSTSEC-2021-0003"));
        assert_eq!(
            m["aliases"],
            json!(["CVE-2021-25900", "GHSA-43w2-9j62-hq99"])
        );
        assert_eq!(m["packages"], json!(["smallvec"]));
        assert_eq!(m["ecosystems"], json!(["crates.io"]));
        // both introduced/fixed pairs of the packed SEMVER range survive
        assert_eq!(m["introduced_versions"], json!(["0.6.3", "1.0.0"]));
        assert_eq!(m["fixed_versions"], json!(["0.6.14", "1.6.1"]));
        assert_eq!(m["affected_versions"], json!(["0.6.5"]));
        assert_eq!(
            m["affected_functions"],
            json!(["smallvec::SmallVec::insert_many"])
        );
        assert_eq!(m["reference_urls"].as_array().unwrap().len(), 2);
        assert!(!m.contains_key("database_specific"));
        assert!(!m.contains_key("database_specific_severity"));
    }

    #[test]
    fn osv_ghsa_split_ranges_land_in_the_same_lists() {
        // GHSA splits one advisory's two fixed ranges across two `affected`
        // entries; the union must equal the RustSec packed shape.
        let v = json!({
            "id": "GHSA-43w2-9j62-hq99",
            "affected": [
                {"package": {"name": "smallvec"}, "ranges": [{"events": [{"introduced": "0.6.3"}, {"fixed": "0.6.14"}]}]},
                {"package": {"name": "smallvec"}, "ranges": [{"events": [{"introduced": "1.0.0"}, {"fixed": "1.6.1"}]}]}
            ]
        });
        let m = normalize(Format::Osv, &v);
        assert_eq!(m["introduced_versions"], json!(["0.6.3", "1.0.0"]));
        assert_eq!(m["fixed_versions"], json!(["0.6.14", "1.6.1"]));
        assert_eq!(m["packages"], json!(["smallvec"]), "deduped across entries");
    }

    #[test]
    fn osv_unknown_fields_passthrough() {
        let v = json!({"id": "x", "schema_version": "1.9.0", "upstream": {"of": "y"}});
        let m = normalize(Format::Osv, &v);
        assert_eq!(m["schema_version"], json!("1.9.0"));
        assert_eq!(m["upstream_of"], json!("y"));
    }

    // ── rustsec-md ─────────────────────────────────────────────────────────

    /// The real shape of crates/smallvec/RUSTSEC-2021-0003.md, trimmed.
    const RUSTSEC_MD: &str = r#"```toml
[advisory]
id = "RUSTSEC-2021-0003"
package = "smallvec"
aliases = ["CVE-2021-25900", "GHSA-43w2-9j62-hq99"]
cvss = "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"
date = "2021-01-08"
url = "https://github.com/servo/rust-smallvec/issues/252"
categories = ["memory-corruption"]
keywords = ["buffer-overflow", "heap-overflow", "unsound"]

[versions]
patched = [">= 0.6.14, < 1.0.0", ">= 1.6.1"]
unaffected = ["< 0.6.3"]

[affected]
functions = { "smallvec::SmallVec::insert_many" = [">= 0.6.3, < 0.6.14"] }
```

# Buffer overflow in SmallVec::insert_many

A bug in the `SmallVec::insert_many` method caused it to allocate a buffer
that was smaller than needed.
"#;

    #[test]
    fn rustsec_md_extracts_the_curated_tables() {
        let m = normalize(Format::RustsecMd, &json!(RUSTSEC_MD));
        assert_eq!(m["id"], json!("RUSTSEC-2021-0003"));
        assert_eq!(
            m["aliases"],
            json!(["CVE-2021-25900", "GHSA-43w2-9j62-hq99"])
        );
        assert_eq!(m["packages"], json!(["smallvec"]));
        assert_eq!(
            m["summary"],
            json!("Buffer overflow in SmallVec::insert_many")
        );
        assert_eq!(
            m["severity"],
            json!("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H")
        );
        assert_eq!(
            m["affected_functions"],
            json!(["smallvec::SmallVec::insert_many"])
        );
        assert_eq!(
            m["patched_versions"],
            json!([">= 0.6.14, < 1.0.0", ">= 1.6.1"])
        );
        assert_eq!(m["unaffected_versions"], json!(["< 0.6.3"]));
        assert_eq!(m["categories"], json!(["memory-corruption"]));
        assert_eq!(
            m["keywords"],
            json!(["buffer-overflow", "heap-overflow", "unsound"])
        );
        assert_eq!(m["date"], json!("2021-01-08"));
        assert_eq!(
            m["url"],
            json!("https://github.com/servo/rust-smallvec/issues/252")
        );
        assert!(
            m["details"]
                .as_str()
                .unwrap()
                .starts_with("# Buffer overflow"),
            "the markdown body is the details"
        );
        assert!(
            !m.contains_key("advisory_id"),
            "curated keys are not duplicated by the passthrough"
        );
    }

    #[test]
    fn rustsec_md_key_names_line_up_with_osv_for_merge() {
        // The whole point of the shared names: a recipe merges a rustsec-md
        // source with an osv one over `id` without a field_precedence bridge.
        let md = normalize(Format::RustsecMd, &json!(RUSTSEC_MD));
        let osv = normalize(
            Format::Osv,
            &json!({
                "id": "RUSTSEC-2021-0003",
                "aliases": ["CVE-2021-25900", "GHSA-43w2-9j62-hq99"],
                "summary": "Out of bounds write in SmallVec::insert_many",
                "modified": "2023-06-20T00:00:00Z"
            }),
        );
        for key in ["id", "aliases"] {
            assert_eq!(md[key], osv[key], "{key} must carry identical meaning");
        }
    }

    #[test]
    fn rustsec_md_without_a_fence_degrades_to_body() {
        let m = normalize(
            Format::RustsecMd,
            &json!("# just prose\n\nno frontmatter here"),
        );
        assert!(!m.contains_key("id"));
        assert_eq!(m["summary"], json!("just prose"));
        assert!(m["details"].as_str().unwrap().contains("no frontmatter"));
    }

    #[test]
    fn rustsec_md_unknown_frontmatter_survives_prefixed() {
        const ADVICE: &str = "```toml\n[advisory]\nid = \"RUSTSEC-2000-0001\"\ncwe = \"CWE-787\"\n\n[versions]\npatched = [\">= 1.0\"]\n```\n\n# Title\n";
        let m = normalize(Format::RustsecMd, &json!(ADVICE));
        assert_eq!(
            m["advisory_cwe"],
            json!("CWE-787"),
            "uncurated key, prefixed by table"
        );
        assert_eq!(m["id"], json!("RUSTSEC-2000-0001"));
    }
}
