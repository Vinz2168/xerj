//! Mapping-resolved query fields and semantic-capable index discovery.

use serde_json::Value;

/// These exact fields and these exact (flat) weights were measured, not
/// chosen. Swept 7 variants over 6 ground-truth queries on a 324-file Rust
/// corpus (`body, defs, title` flat: top3 6/6 — the winner; boosting `defs`
/// favours test modules; `title^2 body`: top3 2/6). Full table in
/// tools/xerj-code/SKILL.md; provenance: measure/SERVER_UPLIFT_SCORECARD.md.
///
/// Two fields are deliberately absent:
/// * `symbols.name` — `symbols` is an array of objects with no searchable
///   `.name` subpath; including it makes the whole multi_match return ZERO
///   hits with no error at all (it took an Aho-Corasick query from 0 hits to
///   3 correct files just to remove it).
/// * `"*"` — a bare wildcard flattens every score to the same value
///   (measured: every hit scored exactly 2.0).
///
/// `defs_expanded^0.5` is a low-weight RECALL field (per-symbol signatures +
/// identifier sub-words) boosted BELOW 1.0 on purpose. It is NOT safe to send
/// unconditionally — see [`resolve_fields`].
pub const FIELDS: &[&str] = &["body", "defs", "title", "defs_expanded^0.5"];

/// Drop query fields that no index under the prefix actually maps.
///
/// This engine does NOT ignore an unmapped field in `multi_match` the way ES
/// does — including one silently collapses a MULTI-TOKEN query to ZERO hits.
/// Measured 2026-08-06, one index, exact totals (`relation: eq`):
///
/// ```text
/// query "log merge policy segment size buckets"
/// fields=["body"]                                 -> 673 hits
/// fields=["body","defs"]                          -> 673 hits
/// fields=["body","defs","title"]                  -> 673 hits
/// fields=["body","defs","title","defs_expanded"]  ->   0 hits
/// ```
///
/// `defs_expanded` was mapped in exactly 0 of the corpus's 219 indices, so
/// every multi-word query returned "no passage matches" — the silent-zero
/// failure shape. Full reproducer: measure/MULTIMATCH_DEFECT.md.
///
/// `mapping` is `None` when the mapping could not be read (transport hiccup,
/// auth): send the list UNCHANGED rather than silently narrowing it. The
/// floor is `["body"]` — never an empty field list.
pub fn resolve_fields(mapping: Option<&Value>) -> Vec<String> {
    let Some(mapping) = mapping else {
        return FIELDS.iter().map(|s| s.to_string()).collect();
    };
    let Some(obj) = mapping.as_object() else {
        return FIELDS.iter().map(|s| s.to_string()).collect();
    };
    let mut present = std::collections::BTreeSet::new();
    for m in obj.values() {
        if let Some(props) = m.pointer("/mappings/properties").and_then(Value::as_object) {
            for key in props.keys() {
                present.insert(key.clone());
            }
        }
    }
    let mut out: Vec<String> = FIELDS
        .iter()
        .filter(|f| {
            let base = f.split('^').next().unwrap_or(f);
            present.contains(base)
        })
        .map(|s| s.to_string())
        .collect();
    // The plain-text extraction family (`.txt` mirrors, logs) puts record
    // content in `text`, not `body` — every other family (code, markdown,
    // adoc) uses `body`. Mapping-gated so the unreadable-mapping fallback
    // above never grows a field an index may not map (the MULTIMATCH_DEFECT
    // silent-zero). Found via the zalando G7 1/5: all 25 renamed `.txt`
    // chapters were invisible to `xerj code` while README.md matched.
    //
    // #1238: `text` joins BELOW 1.0, the same recall-leg posture as
    // `defs_expanded^0.5`. Measured on the live exploit group (5,644
    // indices, query "MCPJam inspector 23744", 36 needle PoC repos): at
    // full weight one plain-text sibling-CVE demo index outscored every
    // code-family hit and took the whole top-10; at ^0.5 the family stays
    // searchable (same 39 total hits) while the needle docs keep their
    // code-family scores.
    if present.contains("text") {
        out.push("text^0.5".to_string());
    }
    if out.is_empty() {
        // #1158: a raw-JSON corpus (ghsa-db's advisory mirrors, OSV) maps
        // NONE of the standard content fields — every record carries its
        // text in schema-named fields like `summary`/`details`. The old
        // `["body"]` floor here is a field no index maps, and an unmapped
        // multi_match field collapses a multi-token query to ZERO hits
        // with no error: the entire corpus answered "no passage matches"
        // while the same indices returned 10,000+ hits queried directly.
        // Fall back to the corpus's OWN text-typed fields (never `ax_*`
        // provenance) — still mapping-gated, so every field sent is one
        // at least one index really maps.
        let own = own_text_fields(obj);
        if own.is_empty() {
            vec!["body".to_string()]
        } else {
            own
        }
    } else {
        out
    }
}

/// The corpus's own searchable content fields, for the no-standard-field
/// floor above: every property mapped `text` (or `semantic_text`) by any
/// index under the prefix, `ax_*` provenance excluded, names sorted, capped
/// so a wide schema cannot balloon the multi_match.
fn own_text_fields(obj: &serde_json::Map<String, Value>) -> Vec<String> {
    const AX: &str = "ax_";
    const MAX_OWN_FIELDS: usize = 24;
    let mut own = std::collections::BTreeSet::new();
    for m in obj.values() {
        let Some(props) = m.pointer("/mappings/properties").and_then(Value::as_object) else {
            continue;
        };
        for (key, spec) in props {
            let searchable = spec
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t == "text" || t == "semantic_text");
            if searchable && !key.starts_with(AX) {
                own.insert(key.clone());
            }
        }
    }
    own.into_iter().take(MAX_OWN_FIELDS).collect()
}

/// Indices under the prefix whose `field` (default `body`) is mapped as
/// `semantic_text`.
///
/// A `semantic` query against an index where the field is plain `text` does
/// not return fewer hits — it fails the WHOLE search with a 400, taking every
/// other index in the wildcard down with it. So the capable set is discovered
/// from the mapping and the vector arm is aimed ONLY at those indices.
///
/// Returns `(capable, total)`, names sorted. An unreadable/unparseable
/// mapping yields an empty capable set (degrade to BM25), never a guess.
pub fn semantic_capable(mapping: Option<&Value>) -> (Vec<String>, usize) {
    let Some(mapping) = mapping.and_then(Value::as_object) else {
        return (Vec::new(), 0);
    };
    let mut capable: Vec<String> = mapping
        .iter()
        .filter(|(_, m)| {
            m.pointer("/mappings/properties/body/type")
                .and_then(Value::as_str)
                .is_some_and(|t| t == "semantic_text")
        })
        .map(|(name, _)| name.clone())
        .collect();
    capable.sort();
    (capable, mapping.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mapping(indices: &[(&str, &[&str], bool)]) -> Value {
        let mut obj = serde_json::Map::new();
        for (name, fields, semantic_body) in indices {
            let mut props = serde_json::Map::new();
            for f in fields.iter() {
                props.insert(
                    f.to_string(),
                    json!({ "type": if *semantic_body && *f == "body" { "semantic_text" } else { "text" } }),
                );
            }
            obj.insert(
                name.to_string(),
                json!({ "mappings": { "properties": props } }),
            );
        }
        Value::Object(obj)
    }

    #[test]
    fn unmapped_fields_are_dropped_and_the_floor_is_body() {
        // The MULTIMATCH_DEFECT guard: `defs_expanded` unmapped in every
        // index must be dropped, not sent.
        let m = mapping(&[
            ("xc-kv-b1-000", &["body", "defs", "title"], false),
            ("xc-kv-b1-001", &["body", "defs"], false),
        ]);
        assert_eq!(
            resolve_fields(Some(&m)),
            vec!["body".to_string(), "defs".into(), "title".into()]
        );

        // No FIELDS member and NOTHING text-typed at all -> the ["body"]
        // floor, never empty, never a keyword field. (A text-typed field
        // no standard name covers is the #1158 own-fields case below.)
        let thin = json!({ "i": { "mappings": { "properties": {
            "unrelated": { "type": "keyword" }
        } } } });
        assert_eq!(resolve_fields(Some(&thin)), vec!["body".to_string()]);
    }

    #[test]
    fn an_unreadable_mapping_sends_the_full_list() {
        assert_eq!(
            resolve_fields(None),
            FIELDS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(
            resolve_fields(Some(&json!("junk"))),
            FIELDS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
    }

    /// #1139: the `.txt` family's content field is `text` — it joins the
    /// multi_match list ONLY when an index actually maps it, so the
    /// unreadable-mapping fallback list above stays exactly FIELDS.
    /// #1238: it joins at ^0.5 (recall leg), never at full weight.
    #[test]
    fn text_family_content_joins_only_when_mapped() {
        let with_text = mapping(&[("i", &["body", "text"], false)]);
        assert_eq!(
            resolve_fields(Some(&with_text)),
            vec!["body".to_string(), "text^0.5".to_string()]
        );
        // No FIELDS member AND no text-typed field -> the ["body"] floor
        // still holds (keyword placeholders — text-typed ones are #1158's
        // own-fields fallback, covered by its own test).
        let thin = json!({ "i": { "mappings": { "properties": {
            "unrelated": { "type": "keyword" }
        } } } });
        assert_eq!(resolve_fields(Some(&thin)), vec!["body".to_string()]);
    }

    #[test]
    fn capable_discovery_finds_only_semantic_text_indices() {
        let m = mapping(&[
            ("plain-1", &["body", "defs"], false),
            ("sem-1", &["body", "defs"], true),
            ("sem-2", &["body"], true),
        ]);
        let (capable, total) = semantic_capable(Some(&m));
        assert_eq!(capable, vec!["sem-1".to_string(), "sem-2".to_string()]);
        assert_eq!(total, 3);
        assert!(semantic_capable(None) == (Vec::new(), 0));
    }

    /// #1158: a raw-JSON corpus (ghsa-db advisories) maps none of the
    /// standard content fields — the old `["body"]` floor was a field no
    /// index maps, and an unmapped multi_match field collapses a
    /// multi-token query to zero hits. The corpus's own text-typed fields
    /// must take the floor's place, `ax_*` provenance excluded.
    #[test]
    fn a_raw_json_corpus_falls_back_to_its_own_text_fields() {
        let mut obj = serde_json::Map::new();
        let props = json!({
            "id":        { "type": "keyword" },
            "summary":   { "type": "text" },
            "details":   { "type": "text" },
            "severity":  { "type": "keyword" },
            "ax_path":   { "type": "text" },
            "ax_file":   { "type": "text" },
            "modified":  { "type": "date" }
        });
        obj.insert(
            "xc-ghsa-000".to_string(),
            json!({ "mappings": { "properties": props } }),
        );
        let m = Value::Object(obj);
        assert_eq!(
            resolve_fields(Some(&m)),
            vec!["details".to_string(), "summary".to_string()],
            "own text fields, sorted; ax_*/keyword/date excluded"
        );
    }

    /// The floor is unchanged when the corpus maps nothing searchable at
    /// all (only keywords/provenance): `["body"]`, never an empty list.
    #[test]
    fn a_corpus_with_no_text_typed_fields_keeps_the_body_floor() {
        let mut obj = serde_json::Map::new();
        let props = json!({
            "id":     { "type": "keyword" },
            "count":  { "type": "long" }
        });
        obj.insert(
            "xc-kv-000".to_string(),
            json!({ "mappings": { "properties": props } }),
        );
        assert_eq!(
            resolve_fields(Some(&Value::Object(obj))),
            vec!["body".to_string()]
        );
    }

    /// A mixed corpus where SOME index maps `body` keeps today's behaviour
    /// exactly — the own-fields fallback fires only on the no-standard-
    /// field floor, never as an extra leg beside `body`.
    #[test]
    fn a_corpus_with_body_mapped_never_grows_own_fields() {
        let m = mapping(&[("xc-mixed-000", &["body"], false)]);
        let mut obj = m.as_object().unwrap().clone();
        obj.insert(
            "xc-mixed-001".to_string(),
            json!({ "mappings": { "properties": {
                "id": { "type": "keyword" },
                "summary": { "type": "text" }
            } } }),
        );
        assert_eq!(
            resolve_fields(Some(&Value::Object(obj))),
            vec!["body".to_string()],
            "body present: no summary leg, no floor rewrite"
        );
    }
}
