//! Identity resolution: turn N source records into unique logical records.
//!
//! Union-find over identity-edge VALUES: every value a recipe declares as
//! an edge (`id`, each element of `aliases`, …) is a node, and a record
//! unions with each of its own values. Two records sharing ANY value merge,
//! transitively and cyclically — exactly the alias closure the sources
//! themselves do not provide (OSV ships one record per source ID, linked
//! only by `aliases`; it does not merge them for you).
//!
//! Derived relations (CVE↔commit, say) are deliberately NOT edges: a single
//! commit fixing two CVEs would fuse two advisories that are distinct
//! findings. Identity is what the sources SAY is the same thing, never what
//! we infer from shared references.
//!
//! Merge is first-non-empty-wins under the recipe's precedence, with arrays
//! unioned — deterministic given the member order, which is itself
//! (precedence rank, store key), never filesystem accident.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::recipe::{
    slug_rank, Derived, DerivedOp, Envelope, Identity, Merge, LICENCE, SRC, SRC_PATH, SRC_URL,
};

/// The store-key sentinel `store::read_all` injects. Sorting by it keeps
/// member order deterministic when two members share a source rank.
pub(crate) const KEY: &str = "\u{0}key";

/// One cluster of envelopes that are the same logical record.
pub struct Cluster {
    /// Indices into the envelope slice, in canonical order.
    pub members: Vec<usize>,
}

/// Group envelopes into clusters. Deterministic: iteration follows the
/// envelope order, and cluster output order follows each cluster's minimum
/// member index.
pub fn cluster(envelopes: &[Map<String, Value>], identity: &Identity) -> Vec<Cluster> {
    // DSU over [0..envelopes) ∪ value nodes (mapped to >= envelopes). Value
    // nodes always lose unions (records have smaller indices), so every
    // root stays below envelopes.len().
    let mut parent: Vec<usize> = (0..envelopes.len()).collect();
    let mut value_nodes: HashMap<String, usize> = HashMap::new();

    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]]; // path halving
            x = parent[x];
        }
        x
    }

    for (i, env) in envelopes.iter().enumerate() {
        for value in identity_values(env, &identity.edges) {
            let node = *value_nodes.entry(value).or_insert_with(|| {
                parent.push(parent.len());
                parent.len() - 1
            });
            let (ra, rb) = (find(&mut parent, i), find(&mut parent, node));
            if ra != rb {
                let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
                parent[hi] = lo;
            }
        }
    }

    let mut by_root: Vec<Vec<usize>> = vec![Vec::new(); envelopes.len()];
    for i in 0..envelopes.len() {
        by_root[find(&mut parent, i)].push(i);
    }
    by_root
        .into_iter()
        .filter(|m| !m.is_empty())
        .map(|mut members| {
            members.sort_by_key(|&i| order_key(&envelopes[i], &identity.canonical_source_order));
            Cluster { members }
        })
        .collect()
}

/// The identity-edge values of one envelope. `each = true` explodes string
/// arrays element-wise; a plain string field is one value; empty strings
/// and other types contribute nothing.
fn identity_values(env: &Map<String, Value>, edges: &[super::recipe::IdentityEdge]) -> Vec<String> {
    let mut out = Vec::new();
    for e in edges {
        match env.get(&e.field) {
            Some(Value::String(s)) if !s.is_empty() => out.push(s.clone()),
            Some(Value::Array(a)) if e.each => {
                for v in a {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() {
                            out.push(s.to_string());
                        }
                    }
                }
            }
            // each=false on an array: the array as a whole is not an
            // identity — arrays vary by source even for the same record.
            _ => {}
        }
    }
    out
}

/// Sort key for members: the order list's rank first, then the envelope's
/// store key — stability that does not depend on directory iteration order.
fn order_key(env: &Map<String, Value>, order: &[String]) -> (usize, String) {
    let slug = env.get(SRC).and_then(Value::as_str).unwrap_or("");
    let key = env.get(KEY).and_then(Value::as_str).unwrap_or("");
    (slug_rank(order, slug), key.to_string())
}

/// Which member "wins" a scalar: the first (by merge order) carrying a
/// non-empty value.
fn first_str<'a>(members: &[&'a Map<String, Value>], key: &str) -> Option<&'a str> {
    members
        .iter()
        .filter_map(|m| m.get(key).and_then(Value::as_str))
        .find(|s| !s.is_empty())
}

/// Merge one passthrough key: an ARRAY in any member makes the result an
/// array (union across members, deduped, scalars folded in as elements —
/// sources disagree on cardinality more often than they agree); scalars
/// everywhere keep it a scalar, first non-empty under precedence.
fn merge_field(ranked: &[&Map<String, Value>], key: &str) -> Option<Value> {
    let any_array = ranked
        .iter()
        .any(|m| matches!(m.get(key), Some(Value::Array(_))));
    if any_array {
        return union_arrays(ranked, key).map(Value::Array);
    }
    if let Some(s) = first_str(ranked, key) {
        return Some(Value::String(s.to_string()));
    }
    ranked
        .iter()
        .find_map(|m| m.get(key).filter(|v| !v.is_null() && !v.is_string()))
        .cloned()
}

/// Union arrays across members preserving member order, deduped. A lone
/// string folds in as a single element.
fn union_arrays(members: &[&Map<String, Value>], key: &str) -> Option<Vec<Value>> {
    let mut out: Vec<Value> = Vec::new();
    for m in members {
        match m.get(key) {
            Some(Value::Array(a)) => {
                for v in a {
                    if !out.contains(v) {
                        out.push(v.clone());
                    }
                }
            }
            Some(Value::String(s)) if !s.is_empty() => {
                let v = Value::String(s.clone());
                if !out.contains(&v) {
                    out.push(v);
                }
            }
            _ => {}
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The most-restrictive licence across members. A merged record
/// redistributes text from EVERY member, so a restricted member must be
/// able to veto the permissive ones — `is_restricted` is binary (the
/// licence tuple has no total order), so: any restricted member's licence
/// wins (precedence-first among them); otherwise precedence-first
/// non-empty. Attribution-grade obligations the binary predicate cannot
/// rank (CC-BY-4.0 vs CC0) stay visible per record through `sources[]`
/// and per pack through the manifest's source list — the `licence` field
/// is the redistribution-warning signal, not a legal settlement.
fn most_restrictive(members: &[&Map<String, Value>]) -> String {
    let mut first_non_empty = String::new();
    for m in members {
        let lic = m.get(LICENCE).and_then(Value::as_str).unwrap_or("");
        if lic.is_empty() {
            continue;
        }
        if xerj_common::xccode::licence::is_restricted(lic) {
            return lic.to_string();
        }
        if first_non_empty.is_empty() {
            first_non_empty = lic.to_string();
        }
    }
    first_non_empty
}

/// Merge one cluster into a single output record (envelope fields, then
/// passthrough, then derived). `order` is the merge precedence.
pub fn merge_cluster(
    cluster: &[&Map<String, Value>],
    order: &[String],
    envelope: &Envelope,
    merge: &Merge,
    derived: &[Derived],
) -> Map<String, Value> {
    let mut members: Vec<&Map<String, Value>> = cluster.to_vec();
    members.sort_by_key(|m| order_key(m, order));
    let winner = members.first().copied();

    let mut out = Map::new();

    // — envelope fields ────────────────────────────────────────────────────
    let id = first_of(&members, &envelope.id_from);
    out.insert("id".into(), Value::String(id.unwrap_or_default()));
    let title = first_of(&members, &envelope.title_from);
    out.insert("title".into(), Value::String(title.unwrap_or_default()));

    // body: per member, join its body_join fields; the first non-empty
    // joined body wins (joining ACROSS sources would duplicate the same
    // advisory prose written twice).
    let mut body = String::new();
    for m in &members {
        let parts: Vec<&str> = envelope
            .body_join
            .iter()
            .filter_map(|k| m.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()))
            .collect();
        if !parts.is_empty() {
            body = parts.join("\n\n");
            break;
        }
    }
    out.insert("body".into(), Value::String(body));

    // defs: union of the defs_from arrays across ALL members, space-joined —
    // this is the field `xerj code` matches definition queries against.
    let mut defs: Vec<String> = Vec::new();
    for m in &members {
        for k in &envelope.defs_from {
            if let Some(Value::Array(a)) = m.get(k) {
                for v in a {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() && !defs.iter().any(|d| d == s) {
                            defs.push(s.to_string());
                        }
                    }
                }
            }
        }
    }
    out.insert("defs".into(), Value::String(defs.join(" ")));

    out.insert("licence".into(), Value::String(most_restrictive(&members)));

    // sources: contributing slugs in precedence order; `source` names the
    // winner, `source_url`/`origin` point back into it.
    let mut slugs: Vec<Value> = Vec::new();
    for m in &members {
        if let Some(s) = m.get(SRC).and_then(Value::as_str) {
            let v = Value::String(s.to_string());
            if !slugs.contains(&v) {
                slugs.push(v);
            }
        }
    }
    out.insert("sources".into(), Value::Array(slugs));
    let get = |k: &str| {
        winner
            .and_then(|m| m.get(k))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    out.insert("source".into(), Value::String(get(SRC)));
    out.insert("source_url".into(), Value::String(get(SRC_URL)));
    out.insert("origin".into(), Value::String(get(SRC_PATH)));

    // modified: max member value (RFC3339 UTC strings compare correctly).
    let modified = members
        .iter()
        .filter_map(|m| m.get("modified").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .max()
        .unwrap_or("")
        .to_string();
    out.insert("modified".into(), Value::String(modified));

    // — passthrough ───────────────────────────────────────────────────────
    // Every data key any member carries, minus provenance sentinels and
    // what the envelope already fixed. Scalars are first-non-empty under
    // the field's precedence (global, or per-field when overridden); arrays
    // union across members regardless of precedence — evidence accumulates.
    if envelope.passthrough {
        let mut keys: Vec<&String> = members
            .iter()
            .flat_map(|m| m.keys())
            .filter(|k| {
                !k.starts_with('\u{0}') && !k.starts_with('_') && !out.contains_key(k.as_str())
            })
            .collect();
        keys.sort();
        keys.dedup();
        for k in keys {
            let order = merge
                .field_precedence
                .get(k)
                .map(Vec::as_slice)
                .unwrap_or(order);
            let mut ranked = members.clone();
            ranked.sort_by_key(|m| order_key(m, order));
            if let Some(v) = merge_field(&ranked, k) {
                out.insert(k.clone(), v);
            }
        }
    }

    // — derived fields ────────────────────────────────────────────────────
    for d in derived {
        let v = apply_derived(&out, &d.op);
        out.insert(d.name.clone(), v);
    }

    out
}

/// First non-empty string among any of the listed keys, scanning members in
/// order. The key list wins over member order: `title_from = ["summary",
/// "id"]` prefers a summary from ANY source over an id from the top source
/// — the human-readable name wherever it exists.
fn first_of(members: &[&Map<String, Value>], keys: &[String]) -> Option<String> {
    for k in keys {
        if let Some(s) = first_str(members, k) {
            return Some(s.to_string());
        }
    }
    None
}

fn apply_derived(rec: &Map<String, Value>, op: &DerivedOp) -> Value {
    match op {
        DerivedOp::RegexExtract { from, re, unique } => {
            let mut out: Vec<Value> = Vec::new();
            for k in from {
                let texts: Vec<String> = match rec.get(k) {
                    Some(Value::String(s)) => vec![s.clone()],
                    Some(Value::Array(a)) => a
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                    _ => Vec::new(),
                };
                for t in texts {
                    for m in re.find_iter(&t) {
                        let v = Value::String(m.as_str().to_string());
                        if !*unique || !out.contains(&v) {
                            out.push(v);
                        }
                    }
                }
            }
            Value::Array(out)
        }
        DerivedOp::Map {
            from,
            rules,
            default,
        } => {
            let text = rec.get(from).and_then(Value::as_str).unwrap_or("");
            for (re, value) in rules {
                if re.is_match(text) {
                    return Value::String(value.clone());
                }
            }
            Value::String(default.clone())
        }
        DerivedOp::Present { from } => {
            let present = match rec.get(from) {
                None | Some(Value::Null) => false,
                Some(Value::String(s)) => !s.is_empty(),
                Some(_) => true,
            };
            Value::Bool(present)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harvest::recipe::IdentityEdge;
    use regex::Regex;
    use serde_json::json;

    fn env(src: &str, fields: &[(&str, Value)]) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(SRC.into(), json!(src));
        m.insert(SRC_URL.into(), json!(format!("https://example/{src}")));
        m.insert(SRC_PATH.into(), json!(format!("{src}/record.json")));
        m.insert(LICENCE.into(), json!("CC0-1.0"));
        m.insert(KEY.into(), json!(format!("{src}-key")));
        for (k, v) in fields {
            m.insert(k.to_string(), v.clone());
        }
        m
    }

    fn edges(spec: &[(&str, bool)]) -> Vec<IdentityEdge> {
        spec.iter()
            .map(|(f, e)| IdentityEdge {
                field: f.to_string(),
                each: *e,
            })
            .collect()
    }

    fn identity_of(edges: Vec<IdentityEdge>) -> Identity {
        Identity {
            edges,
            canonical_source_order: vec![],
        }
    }

    #[test]
    fn alias_chain_and_cycle_collapse_to_one_cluster() {
        // rustsec --CVE--> ghsa and a direct alias edge, plus a cycle
        let a = env(
            "rustsec",
            &[("id", json!("R1")), ("aliases", json!(["C1"]))],
        );
        let b = env(
            "ghsa",
            &[("id", json!("G1")), ("aliases", json!(["C1", "R1"]))],
        );
        let c = env(
            "osv",
            &[("id", json!("C1")), ("aliases", json!(["R1", "G1"]))],
        );
        let identity = identity_of(edges(&[("id", false), ("aliases", true)]));
        let clusters = cluster(&[a, b, c], &identity);
        assert_eq!(clusters.len(), 1, "all three are one logical record");
        assert_eq!(clusters[0].members.len(), 3);
    }

    #[test]
    fn disjoint_records_stay_apart() {
        let a = env("a", &[("id", json!("X1"))]);
        let b = env("a", &[("id", json!("X2"))]);
        let clusters = cluster(&[a, b], &identity_of(edges(&[("id", false)])));
        assert_eq!(clusters.len(), 2);
    }

    #[test]
    fn shared_commit_is_not_an_identity() {
        // two CVEs referencing the same fixing commit must NOT merge
        let a = env(
            "a",
            &[
                ("id", json!("CVE-1")),
                ("commit_urls", json!(["https://x/commit/deadbeef"])),
            ],
        );
        let b = env(
            "a",
            &[
                ("id", json!("CVE-2")),
                ("commit_urls", json!(["https://x/commit/deadbeef"])),
            ],
        );
        let clusters = cluster(&[a, b], &identity_of(edges(&[("id", false)])));
        assert_eq!(
            clusters.len(),
            2,
            "commits are derived edges, never identity"
        );
    }

    fn envelope() -> Envelope {
        Envelope {
            id_from: vec!["id".into()],
            title_from: vec!["summary".into(), "id".into()],
            body_join: vec!["summary".into(), "details".into()],
            defs_from: vec!["affected_functions".into()],
            passthrough: true,
        }
    }

    #[test]
    fn merge_prefers_precedence_and_unions_arrays() {
        let r = env(
            "rustsec",
            &[
                ("id", json!("R1")),
                ("summary", json!("unsound insert_many")),
                ("details", json!("root cause prose")),
                ("aliases", json!(["CVE-1"])),
                (
                    "affected_functions",
                    json!(["smallvec::SmallVec::insert_many"]),
                ),
                ("fixed_versions", json!(["0.6.14"])),
            ],
        );
        let g = env(
            "ghsa",
            &[
                ("id", json!("G1")),
                ("aliases", json!(["CVE-1", "R1"])),
                ("fixed_versions", json!(["1.6.1", "0.6.14"])),
                ("severity", json!("HIGH")),
            ],
        );
        // g is CC-BY: non-restricted, so precedence-first CC0 wins the FIELD
        // (attribution still rides via sources[]/manifest)…
        let mut g = g;
        g.insert(LICENCE.into(), json!("CC-BY-4.0"));
        let merged = merge_cluster(
            &[&r, &g],
            &["rustsec".to_string(), "ghsa".to_string()],
            &envelope(),
            &Merge::default(),
            &[],
        );
        assert_eq!(
            merged["id"],
            json!("R1"),
            "precedence-first member names the id"
        );
        assert_eq!(merged["title"], json!("unsound insert_many"));
        assert!(merged["body"]
            .as_str()
            .unwrap()
            .contains("root cause prose"));
        assert_eq!(
            merged["defs"],
            json!("smallvec::SmallVec::insert_many"),
            "defs feeds xerj code's definition matching"
        );
        assert_eq!(merged["licence"], json!("CC0-1.0"));
        // …but one restricted member vetoes every permissive one
        let mut g2 = env(
            "cisa",
            &[("id", json!("C1")), ("aliases", json!(["CVE-1", "R1"]))],
        );
        g2.insert(LICENCE.into(), json!("GPL-3.0"));
        let merged = merge_cluster(
            &[&r, &g, &g2],
            &[
                "rustsec".to_string(),
                "ghsa".to_string(),
                "cisa".to_string(),
            ],
            &envelope(),
            &Merge::default(),
            &[],
        );
        assert_eq!(
            merged["licence"],
            json!("GPL-3.0"),
            "restricted wins outright"
        );
        assert_eq!(merged["sources"], json!(["rustsec", "ghsa", "cisa"]));
        assert_eq!(merged["source"], json!("rustsec"));
        assert_eq!(merged["source_url"], json!("https://example/rustsec"));
        assert_eq!(merged["origin"], json!("rustsec/record.json"));
        assert_eq!(
            merged["fixed_versions"],
            json!(["0.6.14", "1.6.1"]),
            "arrays union in member order, deduped"
        );
        assert_eq!(merged["aliases"], json!(["CVE-1", "R1"]));
        assert_eq!(merged["severity"], json!("HIGH"), "only g has it");
    }

    #[test]
    fn title_from_spans_sources_before_falling_back() {
        // a summary in a LOWER precedence source beats the id of a higher one
        let r = env("rustsec", &[("id", json!("R1"))]);
        let g = env(
            "ghsa",
            &[("id", json!("G1")), ("summary", json!("the real title"))],
        );
        let merged = merge_cluster(
            &[&r, &g],
            &["rustsec".to_string(), "ghsa".to_string()],
            &envelope(),
            &Merge::default(),
            &[],
        );
        assert_eq!(merged["title"], json!("the real title"));
    }

    #[test]
    fn modified_takes_the_latest_member_value() {
        let r = env(
            "rustsec",
            &[
                ("id", json!("R1")),
                ("modified", json!("2024-01-01T00:00:00Z")),
            ],
        );
        let g = env(
            "ghsa",
            &[
                ("id", json!("G1")),
                ("modified", json!("2025-06-01T00:00:00Z")),
            ],
        );
        let merged = merge_cluster(
            &[&r, &g],
            &["rustsec".to_string(), "ghsa".to_string()],
            &envelope(),
            &Merge::default(),
            &[],
        );
        assert_eq!(merged["modified"], json!("2025-06-01T00:00:00Z"));
    }

    #[test]
    fn derived_ops_run_over_the_merged_record() {
        let r = env(
            "rustsec",
            &[
                ("id", json!("RUSTSEC-2021-0003")),
                ("aliases", json!(["CVE-2021-25900"])),
                ("summary", json!("Unsound: insert_many")),
                ("withdrawn", Value::Null),
                (
                    "reference_urls",
                    json!([
                        "https://github.com/servo/rust-smallvec/commit/abc1234",
                        "https://github.com/servo/rust-smallvec/pull/9"
                    ]),
                ),
            ],
        );
        let cve = Regex::new(r"(?i)\bCVE-\d{4}-\d{4,}\b").unwrap();
        let commit =
            Regex::new(r"https://github\.com/[^/\s]+/[^/\s]+/commit/[0-9a-f]{7,40}").unwrap();
        let unsound = Regex::new(r"(?i)\bunsound\b").unwrap();
        let merged = merge_cluster(
            &[&r],
            &["rustsec".to_string()],
            &envelope(),
            &Merge::default(),
            &[
                Derived {
                    name: "cve_ids".into(),
                    op: DerivedOp::RegexExtract {
                        from: vec!["id".into(), "aliases".into()],
                        re: cve,
                        unique: true,
                    },
                },
                Derived {
                    name: "commit_urls".into(),
                    op: DerivedOp::RegexExtract {
                        from: vec!["reference_urls".into()],
                        re: commit,
                        unique: true,
                    },
                },
                Derived {
                    name: "record_kind".into(),
                    op: DerivedOp::Map {
                        from: "title".into(),
                        rules: vec![(unsound, "informational".into())],
                        default: "vulnerability".into(),
                    },
                },
                Derived {
                    name: "is_withdrawn".into(),
                    op: DerivedOp::Present {
                        from: "withdrawn".into(),
                    },
                },
            ],
        );
        assert_eq!(merged["cve_ids"], json!(["CVE-2021-25900"]));
        assert_eq!(
            merged["commit_urls"],
            json!(["https://github.com/servo/rust-smallvec/commit/abc1234"]),
            "the PR link is not a commit"
        );
        assert_eq!(merged["record_kind"], json!("informational"));
        assert_eq!(merged["is_withdrawn"], json!(false), "null is not present");
    }
}
