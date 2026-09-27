//! The suggester — mapping and join-key suggestions from a sample.
//!
//! Nothing here mutates records or configures an index: autoindex still
//! infers the real mapping at index time ([`crate::infer`] over the live
//! dataset). What this module adds is the AUTHOR loop — after a first
//! `corpus build`, the recipe author reads what the data actually looks
//! like (field types, which fields look like aliases of each other) and
//! edits the recipe: identity edges, derived `regex_extract`, envelope
//! sources. Suggestions, not decisions.
//!
//! All statistics come from a deterministic stride sample of the merged
//! records (id-sorted, every ⌈n/cap⌉-th record), and every output is
//! built through ordered collections — the three artifacts are
//! byte-stable across rebuilds like the rest of the pack.
//!
//! Type verdicts reuse [`crate::infer`] VERBATIM (the FieldAcc ≥95%
//! rule, `DISTINCT_CAP`, entity and date elections). Forking those
//! heuristics here would produce two disagreeing opinions about the same
//! field, and the index always wins the disagreement, so the suggestion
//! would be wrong by construction.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::harvest::recipe::RESERVED_KEYS;
use crate::infer::{entities, FieldAcc, FieldSpec, DISTINCT_CAP};

/// The line every suggestions artifact carries. A pack consumer must never
/// read these files as configuration — they are one sample's opinion.
pub const SUGGEST_HEADER: &str =
    "Suggestions, not decisions — generated from a sample; the index infers the real mapping";

/// Fields excluded from relation analysis: the envelope's own mechanics.
/// `source` ⊆ `sources` by construction, `origin`/`source_url` are
/// provenance — reporting those as "joins" would be noise the recipe
/// already knows the answer to.
const RELATION_EXCLUDED: &[&str] = RESERVED_KEYS;

/// Most relations a pack will carry in these files. Truncation is said out
/// loud in the markdown, never silent.
const MAX_RELATIONS: usize = 64;

/// Overlap examples shown per relation.
const OVERLAP_EXAMPLES: usize = 3;

#[derive(Debug, Clone)]
pub struct Suggestions {
    pub total: usize,
    pub sampled: usize,
    pub fields: Vec<FieldSpec>,
    pub relations: Vec<Relation>,
    /// More relations existed than [`MAX_RELATIONS`]; the strongest were kept.
    pub truncated: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Relation {
    pub left: String,
    pub right: String,
    /// The shared value grammar: `cve`, `ghsa`, `rustsec`, `sha`, `url`,
    /// `email`, `ip`, `uuid` — or `value` for a shapeless exact-value join.
    pub grammar: String,
    pub jaccard: f64,
    /// |left ∩ right| / |left| — 1.0 means every left value appears in right.
    pub left_in_right: f64,
    pub right_in_left: f64,
    /// Structural, from array-ness in the sample: both array →
    /// many-to-many, and so on.
    pub cardinality: &'static str,
    /// Sorted shared values, at most [`OVERLAP_EXAMPLES`].
    pub overlap: Vec<String>,
}

/// One field's accumulated view: the reused [`FieldAcc`], plus what
/// relation analysis needs and FieldAcc does not expose (array-ness and a
/// capped value set).
#[derive(Default)]
struct FieldView {
    acc: FieldAcc,
    array_n: u64,
    scalar_n: u64,
    values: BTreeSet<String>,
    grammar_hits: BTreeMap<&'static str, u64>,
    value_n: u64,
}

impl FieldView {
    fn add(&mut self, v: &Value) {
        self.acc.add(v);
        if v.is_array() {
            for e in v.as_array().unwrap() {
                if e.is_null() || e.is_array() || e.is_object() {
                    continue;
                }
                self.array_n += 1;
                self.note(e);
            }
            return;
        }
        if v.is_null() || v.is_object() {
            return;
        }
        self.scalar_n += 1;
        self.note(v);
    }

    /// Numbers stringify (a numeric join key is still a join key); bools
    /// and objects carry no join identity.
    fn note(&mut self, v: &Value) {
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return,
        };
        self.value_n += 1;
        if let Some(g) = value_grammar(&s) {
            *self.grammar_hits.entry(g).or_default() += 1;
        }
        if self.values.len() < DISTINCT_CAP {
            self.values.insert(s);
        }
    }

    /// The fraction of sampled values matching ANY known ID grammar. A real
    /// `aliases` array mixes CVEs and GHSAs, so demanding ≥90% of ONE
    /// grammar would disqualify the exact field this analysis exists for —
    /// "identifier-ish" is the property that qualifies a field; the shared
    /// values name the family on the edge ([`overlap_grammar`]).
    fn id_ratio(&self) -> f64 {
        if self.value_n == 0 {
            return 0.0;
        }
        self.grammar_hits.values().sum::<u64>() as f64 / self.value_n as f64
    }

    fn many(&self) -> bool {
        self.array_n > 0
    }
}

/// The grammar of an overlap: the family held by ≥90% of the SHARED values,
/// or `value`. Computed from the shared values themselves so a mixed-family
/// field (CVEs and GHSAs in one `aliases`) still yields a precise label.
fn overlap_grammar(shared: &BTreeSet<&String>) -> String {
    let mut tally: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut n = 0u64;
    for v in shared {
        n += 1;
        if let Some(g) = value_grammar(v) {
            *tally.entry(g).or_default() += 1;
        }
    }
    tally
        .iter()
        .max_by(|(a, an), (b, bn)| bn.cmp(an).then_with(|| a.cmp(b)))
        .filter(|(_, c)| **c * 10 >= n * 9)
        .map(|(g, _)| g.to_string())
        .unwrap_or_else(|| "value".to_string())
}

/// The ID grammars relation analysis knows. Vuln-corpus shaped on purpose
/// (this grew out of the rust-vulns pack) but none of them is vuln-only:
/// a CVE-shaped or sha-shaped value is an identifier wherever it appears.
fn grammars() -> &'static [(&'static str, Regex)] {
    static G: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    G.get_or_init(|| {
        vec![
            ("cve", Regex::new(r"(?i)^CVE-\d{4}-\d{4,}$").unwrap()),
            (
                "ghsa",
                Regex::new(r"^GHSA(-[23456789cfghjmpqrvwx]{4}){3}$").unwrap(),
            ),
            ("rustsec", Regex::new(r"^RUSTSEC-\d{4}-\d{4,}$").unwrap()),
            // 7..=40 lowercase hex: git short..full sha. {6} would admit
            // ordinary words-in-hex like "abc123" (the M1 trap).
            ("sha", Regex::new(r"^[0-9a-f]{7,40}$").unwrap()),
        ]
    })
    .as_slice()
}

/// Classify one value. Vuln-id shapes first, then [`entities::classify`]
/// for url/email/ip/uuid — reused, not re-implemented.
fn value_grammar(v: &str) -> Option<&'static str> {
    let t = v.trim();
    if t.is_empty() || t.len() > 512 {
        return None;
    }
    for (name, re) in grammars() {
        if re.is_match(t) {
            return Some(name);
        }
    }
    entities::classify(t).map(|e| e.as_str())
}

/// Analyze a sample of the merged records. `sample_cap` is the recipe's
/// `[suggest] sample`.
pub fn analyze(records: &[Map<String, Value>], sample_cap: usize) -> Suggestions {
    // stride sample over id-sorted records: every ⌈n/cap⌉-th one. Cluster
    // order (the caller's) must never reach the output.
    let mut order: Vec<usize> = (0..records.len()).collect();
    order.sort_by_key(|&i| {
        records[i]
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    });
    let step = records.len().div_ceil(sample_cap).max(1);
    let sample: Vec<&Map<String, Value>> =
        order.iter().step_by(step).map(|&i| &records[i]).collect();

    let mut views: HashMap<String, FieldView> = HashMap::new();
    for r in &sample {
        for (k, v) in r.iter() {
            views.entry(k.clone()).or_default().add(v);
        }
    }

    // FieldAcc map for infer_fields — the real verdicts, not a fork.
    let mut accs: HashMap<String, FieldAcc> = HashMap::new();
    for (k, v) in &views {
        accs.insert(k.clone(), v.acc.clone());
    }
    let fields = crate::infer::infer_fields_with_policy(&accs, sample.len() as u64, false, false);

    // Relations over non-envelope fields, in name order (pairs and output
    // both derive their order from it).
    let mut names: Vec<&String> = views
        .keys()
        .filter(|k| !RELATION_EXCLUDED.contains(&k.as_str()))
        .collect();
    names.sort();
    let mut relations = Vec::new();
    for (i, a) in names.iter().enumerate() {
        for b in &names[i + 1..] {
            let (va, vb) = (&views[*a], &views[*b]);
            if va.values.is_empty() || vb.values.is_empty() {
                continue;
            }
            let shared: BTreeSet<&String> = va.values.intersection(&vb.values).collect();
            if shared.is_empty() {
                continue;
            }
            let union = va.values.len() + vb.values.len() - shared.len();
            let jaccard = shared.len() as f64 / union as f64;
            // An identifier-ish pair (≥90% of values in known grammars —
            // families may differ, an `aliases` array mixes them) needs any
            // overlap at all to be worth a look; a shapeless pair must be at
            // least half identical, or it is two keyword fields that happen
            // to share a value.
            let both_ids = va.id_ratio() >= 0.9 && vb.id_ratio() >= 0.9;
            if !both_ids && jaccard < 0.5 {
                continue;
            }
            relations.push(Relation {
                left: (*a).clone(),
                right: (*b).clone(),
                grammar: overlap_grammar(&shared),
                jaccard,
                left_in_right: shared.len() as f64 / va.values.len() as f64,
                right_in_left: shared.len() as f64 / vb.values.len() as f64,
                cardinality: match (va.many(), vb.many()) {
                    (true, true) => "many-to-many",
                    (false, true) => "one-to-many",
                    (true, false) => "many-to-one",
                    (false, false) => "one-to-one",
                },
                overlap: shared
                    .iter()
                    .take(OVERLAP_EXAMPLES)
                    .map(|s| (*s).clone())
                    .collect(),
            });
        }
    }
    // strongest first for truncation, name-ordered for the files
    relations.sort_by(|x, y| {
        y.jaccard
            .partial_cmp(&x.jaccard)
            .unwrap()
            .then_with(|| x.left.cmp(&y.left))
            .then_with(|| x.right.cmp(&y.right))
    });
    let truncated = relations.len() > MAX_RELATIONS;
    relations.truncate(MAX_RELATIONS);
    relations.sort_by(|x, y| x.left.cmp(&y.left).then_with(|| x.right.cmp(&y.right)));

    Suggestions {
        total: records.len(),
        sampled: sample.len(),
        fields,
        relations,
        truncated,
    }
}

impl Suggestions {
    /// `mapping.suggested.json` — the full field verdicts plus a plain
    /// ES-compat properties map (`semantic_text` is XERJ's; ES reads it as
    /// `text`).
    pub fn mapping_doc(&self) -> Value {
        let mut props = BTreeMap::new();
        for f in &self.fields {
            let es_type = if f.es_type == "semantic_text" {
                "text".to_string()
            } else {
                f.es_type.clone()
            };
            props.insert(
                f.name.clone(),
                serde_json::json!({
                    "type": es_type,
                    "xerj_type": f.es_type,
                }),
            );
        }
        serde_json::json!({
            "note": SUGGEST_HEADER,
            "sampled_records": self.sampled,
            "total_records": self.total,
            "fields": self.fields,
            "es_mapping": { "properties": props },
        })
    }

    /// `suggestions.md` — the human-readable half; the recipe author reads
    /// this one.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Suggestions\n\n");
        out.push_str(SUGGEST_HEADER);
        out.push_str(&format!(
            ".\n\nSampled {} of {} record(s) (every \u{230c}n/sample\u{230d}-th, id-ordered).\n\n",
            self.sampled, self.total
        ));
        out.push_str("## Fields\n\n");
        out.push_str("| field | type | coverage | distinct | notes |\n");
        out.push_str("|---|---|---|---|---|\n");
        for f in &self.fields {
            let notes = if f.notes.is_empty() {
                String::new()
            } else {
                f.notes.join("; ")
            };
            out.push_str(&format!(
                "| `{}` | {} | {:.0}% | {}{} | {} |\n",
                f.name,
                f.es_type,
                f.coverage * 100.0,
                f.cardinality_est,
                if f.cardinality_overflow { "+" } else { "" },
                notes.replace('|', "\\|"),
            ));
        }
        out.push_str("\n## Possible joins / aliases\n\n");
        if self.relations.is_empty() {
            out.push_str("None found in the sample.\n");
        } else {
            for r in &self.relations {
                out.push_str(&format!(
                    "- `{}` \u{2194} `{}` \u{2014} grammar `{}`, jaccard {:.2} ({:.0}% of `{}` and \
                     {:.0}% of `{}` shared) \u{2014} {}\n",
                    r.left,
                    r.right,
                    r.grammar,
                    r.jaccard,
                    r.left_in_right * 100.0,
                    r.left,
                    r.right_in_left * 100.0,
                    r.right,
                    r.cardinality,
                ));
                if !r.overlap.is_empty() {
                    out.push_str(&format!(
                        "  - e.g. {}\n",
                        r.overlap
                            .iter()
                            .map(|v| format!("`{v}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            if self.truncated {
                out.push_str(&format!(
                    "\n(more than {MAX_RELATIONS} candidate relations existed; the strongest \
                     {} are shown)\n",
                    self.relations.len()
                ));
            }
        }
        out.push_str(
            "\nHow to use: edit the recipe \u{2014} a same-grammar alias pair wants an \
                      `[identity]` edge (or a derived `regex_extract`), a mis-typed field wants \
                      an explicit mapping. Nothing here is applied automatically.\n",
        );
        out
    }

    /// `relations.jsonl` — one JSON object per line, name-ordered.
    pub fn relations_jsonl(&self) -> String {
        let mut out = String::new();
        for r in &self.relations {
            out.push_str(&serde_json::to_string(r).expect("Relation serializes"));
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rec(id: &str, extra: &[(&str, Value)]) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("id".into(), json!(id));
        for (k, v) in extra {
            m.insert(k.to_string(), v.clone());
        }
        m
    }

    #[test]
    fn value_grammar_knows_the_id_families() {
        assert_eq!(value_grammar("CVE-2021-25900"), Some("cve"));
        assert_eq!(
            value_grammar("cve-2021-25900"),
            Some("cve"),
            "case-insensitive"
        );
        assert_eq!(value_grammar("GHSA-43w2-9j62-hq99"), Some("ghsa"));
        assert_eq!(value_grammar("RUSTSEC-2021-0003"), Some("rustsec"));
        assert_eq!(
            value_grammar("0e5f1d6c2ba9a3e8f7d4c1b0a987654321abcdef"),
            Some("sha")
        );
        assert_eq!(
            value_grammar("91a3e8f"),
            Some("sha"),
            "7 hex chars = short sha"
        );
        assert_eq!(value_grammar("https://x.example/y"), Some("url"));
        assert_eq!(value_grammar("a@b.example"), Some("email"));
        assert_eq!(
            value_grammar("741e7b6b-dbd2-4a7f-93a9-4ba50fb561d3"),
            Some("uuid")
        );
        assert_eq!(
            value_grammar("abc123"),
            None,
            "6 hex-ish chars is a word, not a sha"
        );
        assert_eq!(value_grammar("hello world"), None);
    }

    #[test]
    fn sampling_is_strided_and_input_order_independent() {
        let mut records: Vec<Map<String, Value>> =
            (0..10).map(|i| rec(&format!("X{i}"), &[])).collect();
        let a = analyze(&records, 4);
        assert_eq!(a.sampled, 4, "step 3 over 10 → indices 0,3,6,9");
        assert_eq!(a.total, 10);

        // cluster order must not reach the output: shuffle in, same stats out
        records.reverse();
        let b = analyze(&records, 4);
        assert_eq!(a.to_markdown(), b.to_markdown());
        assert_eq!(
            serde_json::to_string(&a.mapping_doc()).unwrap(),
            serde_json::to_string(&b.mapping_doc()).unwrap()
        );
    }

    #[test]
    fn field_verdicts_are_infer_verdicts() {
        let records: Vec<_> = (0..24)
            .map(|i| {
                rec(
                    &format!("R{i:02}"),
                    &[
                        ("modified", json!("2026-03-17T00:00:13Z")),
                        ("severity", json!(if i % 2 == 0 { "HIGH" } else { "LOW" })),
                        (
                            "body",
                            json!(
                                "The connection pool retries every failed handshake with \
                                   backoff. Each worker owns one socket and never shares it."
                            ),
                        ),
                        ("score", json!(i)),
                    ],
                )
            })
            .collect();
        let s = analyze(&records, 64);
        let by = |n: &str| {
            s.fields
                .iter()
                .find(|f| f.name == n)
                .unwrap_or_else(|| panic!("{n} missing: {:?}", s.fields))
                .es_type
                .clone()
        };
        assert_eq!(by("modified"), "date");
        assert_eq!(by("severity"), "keyword");
        assert_eq!(by("score"), "long");
        assert_eq!(by("body"), "semantic_text", "the natural-language election");

        // and the ES-compat view downgrades the XERJ-only type
        let doc = s.mapping_doc();
        assert_eq!(
            doc["es_mapping"]["properties"]["body"]["type"],
            json!("text")
        );
        assert_eq!(
            doc["es_mapping"]["properties"]["body"]["xerj_type"],
            json!("semantic_text")
        );
        assert_eq!(
            doc["es_mapping"]["properties"]["modified"]["type"],
            json!("date")
        );
    }

    #[test]
    fn relations_find_the_alias_pair_not_the_noise() {
        let records: Vec<_> = (0..20)
            .map(|i| {
                rec(
                    &format!("R{i:02}"),
                    &[
                        (
                            "aliases",
                            json!([format!("CVE-2021-{i:04}"), format!("GHSA-43w2-9j6{i}-hq99")]),
                        ),
                        ("cve_ids", json!([format!("CVE-2021-{i:04}")])),
                        ("a_text", json!(format!("unique prose number {i}"))),
                        ("b_text", json!(format!("other prose number {i}"))),
                        // envelope mechanics: source ⊂ sources BY CONSTRUCTION
                        ("source", json!("rustsec")),
                        ("sources", json!(["rustsec", "ghsa"])),
                    ],
                )
            })
            .collect();
        let s = analyze(&records, 64);

        let alias_edge = s
            .relations
            .iter()
            .find(|r| r.left == "aliases" && r.right == "cve_ids")
            .expect("the cve-shaped pair must be reported");
        assert_eq!(alias_edge.grammar, "cve");
        assert_eq!(alias_edge.cardinality, "many-to-many");
        assert!(
            (alias_edge.right_in_left - 1.0).abs() < 1e-9,
            "cve_ids ⊂ aliases"
        );

        // prose fields share no values → no relation; envelope keys excluded
        assert!(!s
            .relations
            .iter()
            .any(|r| r.left == "a_text" || r.right == "b_text"));
        assert!(!s.relations.iter().any(|r| {
            r.left == "source" || r.right == "source" || r.left == "sources" || r.right == "sources"
        }));
    }

    #[test]
    fn shapeless_fields_need_half_overlap() {
        let records: Vec<_> = (0..20)
            .map(|i| {
                rec(
                    &format!("R{i:02}"),
                    &[
                        ("left", json!(format!("pkg-{i}"))),
                        ("right", json!(format!("pkg-{}", i % 10))), // 50% of left
                        ("none", json!(format!("zz-{i}"))),
                    ],
                )
            })
            .collect();
        let s = analyze(&records, 64);
        let edge = s
            .relations
            .iter()
            .find(|r| r.left == "left" && r.right == "right")
            .expect("exactly-half overlap qualifies as a value join");
        assert_eq!(edge.grammar, "value");
        assert_eq!(edge.cardinality, "one-to-one");

        // and a field sharing nothing with anyone produces no edges at all
        assert!(!s
            .relations
            .iter()
            .any(|r| r.left == "none" || r.right == "none"));
    }

    #[test]
    fn markdown_carries_the_header_and_the_tables() {
        let records: Vec<_> = (0..4)
            .map(|i| {
                rec(
                    &format!("R{i}"),
                    &[
                        ("aliases", json!([format!("CVE-2021-{i:04}")])),
                        ("cve_ids", json!([format!("CVE-2021-{i:04}")])),
                    ],
                )
            })
            .collect();
        let s = analyze(&records, 64);
        let md = s.to_markdown();
        assert!(md.contains("Suggestions, not decisions"), "{md}");
        assert!(md.contains("| `aliases` |"), "{md}");
        assert!(md.contains("`CVE-2021-0000`"), "{md}");
        assert!(s.relations_jsonl().ends_with("}\n"));
    }
}
