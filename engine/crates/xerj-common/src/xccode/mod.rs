//! # xccode — the reference-coding semantics shared by `xerj code`, the
//! `xerj corpus` lifecycle, and the `xerj_code_search` MCP tool
//!
//! Until issue #977 the reference-coding loop lived in three wrapper scripts
//! (`tools/xerj-code/scripts/xc.py`, `xc-index.sh`, `xc-corpus.sh`). They are
//! ported here as ONE pure implementation so the CLI and the MCP tool serve
//! byte-identical prose from the same renderer, and so every contract — the
//! 30-day staleness refusal, the restricted-licence warning, the
//! not-loaded-here diagnosis, the exit-code triangle — has exactly one home
//! and one test suite.
//!
//! Layout (all persistent paths parameterised by `root`; tests inject
//! tempdirs — that is test hygiene, NOT a layout change):
//!
//! * [`state`] — the `state/<corpus>.json` ledger: read, staleness,
//!   incomplete-coverage warning, atomic write
//! * [`licence`] — restricted-licence tuple + the clone-time detector
//! * [`manifest`] — `corpora/<corpus>/corpus.json` read/write + hub manifests
//! * [`pathgate`] — the hard gate every manifest-derived path passes before
//!   anything destructive runs inside it
//! * [`fields`] — mapping-resolved `multi_match` fields + semantic-capable
//!   index discovery
//! * [`passage`] — symbol-passage / best-window / file-head selection
//! * [`render`] — the one output renderer (prose + MEATL)
//!
//! HTTP is a 3-method shell ([`XcHttp`]) so the pure core stays free of any
//! client dependency: `xerj-autoindex` implements it over its blocking `Es`
//! client, `xerj-mcp` over reqwest. No fusion or query logic lives in either
//! — hybrid retrieval emits the engine's native top-level `hybrid` query and
//! lets the server fuse.

pub mod fields;
pub mod licence;
pub mod manifest;
pub mod passage;
pub mod pathgate;
pub mod render;
pub mod state;

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

/// Corpora older than this are refused, not served: a stale index returns
/// code that no longer exists, with false confidence (`STALE_DAYS` in the
/// original `xc.py`).
pub const STALE_DAYS: i64 = 30;

/// Reciprocal Rank Fusion constant (Cormack, Clarke & Buettcher, SIGIR 2009).
/// k=60 is the paper's value and the universal default. It is NOT tuned here.
/// Fusion itself is SERVER-SIDE (native `hybrid` query, #943); this constant
/// only names k in the request we emit and in the arms-ran note.
pub const RRF_K: u64 = 60;

/// The retrieval arm. Default is [`Mode::Bm25`] — measured 12/12 top-3 across
/// both standing corpora (bm25 beat hybrid and semantic on the combined
/// metric; see `tools/xerj-code/SKILL.md`'s retrieval-mode table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Bm25,
    Semantic,
    Hybrid,
}

impl Mode {
    /// Parse the `--mode` / MCP `mode` argument. Unknown values are usage
    /// errors (exit 2), never a silent default.
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "bm25" => Some(Mode::Bm25),
            "semantic" => Some(Mode::Semantic),
            "hybrid" => Some(Mode::Hybrid),
            _ => None,
        }
    }
}

/// Everything one `xerj code` invocation (or `xerj_code_search` tool call)
/// needs. Defaults mirror `xc.py` exactly: k=5, bm25, full=800, symbols on.
#[derive(Debug, Clone)]
pub struct CodeParams {
    pub corpus: String,
    pub query: String,
    pub k: usize,
    pub lang: Option<String>,
    pub mode: Mode,
    /// Max chars of each passage; 0 = file-head mode (first 400 chars,
    /// labelled as the file head — retrieving the definition IS the feature,
    /// issue #368, so this is a cap, never an opt-out).
    pub full: usize,
    pub no_symbol: bool,
    pub stale_ok: bool,
    pub meatl: bool,
    /// CLI `--json`: raw server response on stdout. MCP never sets this.
    pub as_json: bool,
    /// MCP `licence_policy:"strict"`: restricted-licence hits keep
    /// provenance + licence + warning but lose the passage text. The CLI
    /// stays `false` (warn) for `xc.py` parity.
    pub strict_licence: bool,
}

impl CodeParams {
    pub fn new(corpus: &str, query: &str) -> Self {
        CodeParams {
            corpus: corpus.to_string(),
            query: query.to_string(),
            k: 5,
            lang: None,
            mode: Mode::Bm25,
            full: 800,
            no_symbol: false,
            stale_ok: false,
            meatl: false,
            as_json: false,
            strict_licence: false,
        }
    }
}

/// The one HTTP shell the pure core needs. Implemented over the blocking `Es`
/// client in `xerj-autoindex` and over reqwest in `xerj-mcp`; fakes in tests.
///
/// `cat_indices_json` maps HTTP 404 to `Ok(vec![])` — a wildcard that matches
/// no index means "none", not an error — while every other failure is `Err`:
/// a node that cannot answer must never be reported as "0 live indices".
pub trait XcHttp {
    /// `GET /{pattern}/_mapping`. `Err` carries a human-readable reason.
    fn get_mapping(&self, pattern: &str) -> Result<Value, String>;
    /// `GET /_cat/indices/{pattern}?format=json&h=index` as index names.
    /// `Err` = unreachable/ambiguous (never a silent zero).
    fn cat_indices_json(&self, pattern: &str) -> Result<Vec<String>, String>;
    /// `POST /{index}/_search`. `Err` carries status + reason or transport.
    fn search(&self, index: &str, body: &Value) -> Result<Value, String>;
}

/// Everything a caller needs after the pipeline ran. The CLI prints `text`
/// (stdout, or stderr when [`Self::to_stderr`] — refusals are diagnostics)
/// and exits with [`Self::exit`]; MCP returns `text` as the tool payload with
/// [`Self::is_error`]. Warnings ride stderr on the CLI and the TOP of the
/// tool text on MCP — MCP has no stderr channel, so warnings are content.
#[derive(Debug, Clone)]
pub struct CodeOutcome {
    pub text: String,
    pub warnings: Vec<String>,
    /// 0 = hits, 1 = no match (a miss is an answer), 2 = usage / stale /
    /// transport, 3 = corpus in state/ but 0 live indices on this node.
    pub exit: i32,
    /// MCP `isError`: true exactly for the action-needed states (2 and 3).
    /// A no-match is `false` with the fall-back prose.
    pub is_error: bool,
    /// Raw server response when `as_json` was requested (CLI-only flag).
    pub json: Option<Value>,
    pub to_stderr: bool,
}

impl CodeOutcome {
    fn refused(text: String, exit: i32) -> Self {
        CodeOutcome {
            text,
            warnings: Vec::new(),
            exit,
            is_error: true,
            json: None,
            to_stderr: true,
        }
    }
}

/// The whole query pipeline, pure: ledger → staleness → live-count → mapping
/// discovery → query → passage selection → licence map → render.
///
/// `url` is the resolved node URL (for diagnostics); `stale_hint` is the
/// override spelling the caller's surface accepts — "`--stale-ok`" on the
/// CLI, "`stale_ok:true`" on MCP — so the one refusal text names the fix the
/// caller can actually type.
pub fn run_code_query(
    root: &Path,
    http: &impl XcHttp,
    url: &str,
    params: &CodeParams,
    stale_hint: &str,
) -> CodeOutcome {
    let corpus = params.corpus.clone();
    let query = params.query.clone();

    // 1. The ledger. Not indexed is a usage-class refusal naming the command
    //    that fixes it (exit 2 — the caller's driver script should stop).
    let st = match state::load_state(root, &corpus) {
        Ok(st) => st,
        Err(msg) => return CodeOutcome::refused(msg, 2),
    };

    // 2. Staleness: refuse BEFORE any query. A stale index returns code that
    //    no longer exists; --stale-ok / stale_ok:true is the only override
    //    (no env bypass — a wrapper must not be able to de-fang this).
    let age_days = match state::check_fresh(&st, params.stale_ok, stale_hint) {
        Ok(age) => age,
        Err(refusal) => return CodeOutcome::refused(refusal, 2),
    };

    let prefix = state::query_prefix(&st);
    // Legacy state (indexed before builds existed) names no verified build,
    // so the query widens to the whole `xc-<corpus>` namespace — and whatever
    // stale generations live there are mixed into the answers (#1136). Say
    // so at the point it happens; stderr, because stdout is the parsed
    // hit list. `corpus index <name> --fresh` pins one verified build.
    if st.index_prefix.is_none() {
        eprintln!(
            "xerj code: notice: corpus '{corpus}' has no verified build recorded — querying \
             the whole xc-{corpus} namespace; `xerj corpus index {corpus} --fresh` pins one \
             verified build"
        );
    }

    // 3. Not-loaded-here: in state/ yet 0 live indices on THIS node. A
    //    distinct, actionable diagnosis (exit 3) — collapsing it into
    //    "no match" is the state-ledger trust trap this guard closes.
    //    An unreachable node is reported honestly (exit 2) by the search
    //    path below, never as "0 live indices".
    match http.cat_indices_json(&format!("{prefix}*")) {
        Ok(indices) if indices.is_empty() => {
            return CodeOutcome::refused(state::not_loaded_message(&st, &prefix, url), 3)
        }
        // Unreachable/ambiguous: fall through; the search itself will report
        // the transport failure honestly.
        _ => {}
    }

    let mut warnings = Vec::new();
    if let Some(w) = state::incomplete_coverage(&st) {
        warnings.push(w);
    }

    // 4. Mapping: one read serves both field resolution and capable-set
    //    discovery. Unreadable is data, not an error (bm25 degrades to the
    //    full field list; hybrid degrades to BM25-only).
    let pattern = format!("{prefix}*");
    let mapping = http.get_mapping(&format!("/{pattern}/_mapping")).ok();
    let fields = fields::resolve_fields(mapping.as_ref());

    let mut note: Option<String> = None;
    let mut rrf_scores = false;

    // #1137: fetch deeper than the page so the per-file cap has candidates
    // to draw on; the page is cut back to `params.k` after diversification.
    let fetch = diversity_fetch(params.k);

    let (mut resp, hits) = match params.mode {
        Mode::Bm25 => match http.search(&pattern, &bm25_body(&query, fetch, &params.lang, &fields))
        {
            Ok(r) => (r.clone(), hit_list(&r)),
            Err(e) => return CodeOutcome::refused(format!("search failed: {e}"), 2),
        },
        Mode::Semantic => {
            // Standalone semantic is fatal on mapping/search failure: it has
            // no BM25 result to keep.
            let mapping = match http.get_mapping(&format!("/{pattern}/_mapping")) {
                Ok(m) => m,
                Err(e) => {
                    return CodeOutcome::refused(
                        format!("semantic mapping lookup failed at {url}: {e}"),
                        2,
                    )
                }
            };
            let (capable, total) = fields::semantic_capable(Some(&mapping));
            if capable.is_empty() {
                // No semantic arm is possible: do NOT post the query. An
                // empty comma-join is an empty index segment, which the
                // server reads as `/_search` — every index on the node,
                // corpus or not. The empty answer with its note is the
                // honest result.
                note = Some(format!(
                    "no index under '{pattern}' maps `body` as semantic_text"
                ));
                (Value::Null, Vec::new())
            } else {
                note = Some(format!(
                    "vector only over {} of {total} index(es)",
                    capable.len()
                ));
                let body = semantic_body(&query, fetch, &params.lang);
                match http.search(&capable.join(","), &body) {
                    Ok(r) => (r.clone(), hit_list(&r)),
                    Err(e) => return CodeOutcome::refused(format!("search failed: {e}"), 2),
                }
            }
        }
        Mode::Hybrid => {
            let (capable, total) = fields::semantic_capable(mapping.as_ref());
            if capable.is_empty() {
                // No vector arm possible: degrade, and SAY so.
                note = Some(format!(
                    "BM25 only — no usable semantic_text mapping for `body` could be \
                     discovered under '{pattern}'"
                ));
                match http.search(&pattern, &bm25_body(&query, fetch, &params.lang, &fields)) {
                    Ok(r) => (r.clone(), hit_list(&r)),
                    Err(e) => return CodeOutcome::refused(format!("search failed: {e}"), 2),
                }
            } else {
                // BM25 preflight: the lexical embedder returns confident
                // neighbours for ANY input, so a BM25 miss is the corpus's
                // only honest "no" — vector hits must not launder it.
                match http.search(&pattern, &bm25_body(&query, 1, &params.lang, &fields)) {
                    Ok(pre) if hit_list(&pre).is_empty() => {
                        note = Some(
                            "no lexical match in this corpus — vector nearest-neighbours \
                             are not evidence of a match, so this is reported as a miss"
                                .to_string(),
                        );
                        (Value::Null, Vec::new())
                    }
                    Ok(_) => {
                        // The engine's native top-level `hybrid` (RRF k=60) —
                        // no client-side fusion. Aimed at the comma-joined
                        // CAPABLE set when only some indices map `body` as
                        // semantic_text: a semantic leg posted at a wildcard
                        // covering plain-text indices 400s the WHOLE request.
                        let target = if capable.len() == total {
                            pattern.clone()
                        } else {
                            capable.join(",")
                        };
                        let body = hybrid_body(&query, fetch, &params.lang, &fields);
                        match http.search(&target, &body) {
                            Ok(r) => {
                                rrf_scores = true;
                                let mut n = format!(
                                    "hybrid RRF(k={RRF_K}) — BM25 over {total} index(es), \
                                     vector over {} of {total}",
                                    capable.len()
                                );
                                let lexical_only: Vec<String> = if capable.len() < total {
                                    mapping
                                        .as_ref()
                                        .and_then(Value::as_object)
                                        .map(|obj| {
                                            obj.keys()
                                                .filter(|k| !capable.contains(k))
                                                .cloned()
                                                .collect()
                                        })
                                        .unwrap_or_default()
                                } else {
                                    Vec::new()
                                };
                                if lexical_only.is_empty() {
                                    note = Some(n);
                                    (r.clone(), hit_list(&r))
                                } else {
                                    // #1146: the lexical-only indices still
                                    // owe the BM25 leg. The engine fuses PER
                                    // INDEX and merges indices by `_score`,
                                    // so the same `hybrid` with its BM25 leg
                                    // alone returns RRF scores on the same
                                    // scale — merging the two responses by
                                    // score is the engine's own cross-index
                                    // step, not a second fusion.
                                    let lex_body =
                                        hybrid_bm25_leg_body(&query, fetch, &params.lang, &fields);
                                    match http.search(&lexical_only.join(","), &lex_body) {
                                        Ok(lex) => {
                                            n.push_str(&format!(
                                                "; lexical-only (BM25 leg only, merged by \
                                                 score): {}",
                                                lexical_only.join(", ")
                                            ));
                                            note = Some(n);
                                            let merged = merge_by_score(r, &lex, fetch);
                                            let hits = hit_list(&merged);
                                            (merged, hits)
                                        }
                                        Err(e) => {
                                            return CodeOutcome::refused(
                                                format!("search failed: {e}"),
                                                2,
                                            )
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                // The vector arm failed, not the corpus:
                                // degrade to plain BM25, never abort.
                                note = Some(format!(
                                    "BM25 only — vector search failed or returned no hits \
                                     from the {} semantic_text index(es)",
                                    capable.len()
                                ));
                                match http.search(
                                    &pattern,
                                    &bm25_body(&query, fetch, &params.lang, &fields),
                                ) {
                                    Ok(r) => (r.clone(), hit_list(&r)),
                                    Err(e) => {
                                        return CodeOutcome::refused(
                                            format!("search failed: {e}"),
                                            2,
                                        )
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => return CodeOutcome::refused(format!("search failed: {e}"), 2),
                }
            }
        }
    };

    // 5. #1137: cut the overfetched window back to the page, capping records
    //    per source file, and patch the diversified page into the response so
    //    `--json` consumers see the SAME list the prose renderer does (the
    //    G7 grader is one). A page that shrank says so in the note.
    let (hits, dropped) = diversify(hits, params.k);
    if dropped > 0 {
        let cap = format!(
            "top-k diversified: ≤{MAX_PER_FILE} records per source file \
             ({dropped} same-file neighbours skipped)"
        );
        note = Some(match note.take() {
            Some(n) => format!("{n}; {cap}"),
            None => cap,
        });
    }
    if params.as_json {
        if let Some(hits_arr) = resp.pointer_mut("/hits/hits").and_then(Value::as_array_mut) {
            *hits_arr = hits.clone();
        }
        return CodeOutcome {
            text: String::new(),
            warnings,
            exit: if hits.is_empty() { 1 } else { 0 },
            is_error: false,
            json: Some(resp),
            to_stderr: false,
        };
    }

    if hits.is_empty() {
        // Say so explicitly. A silent miss makes the next agent re-run the
        // same dead query; this is the line that stops the loop.
        return CodeOutcome {
            text: render::no_match_text(&corpus, &query, params.meatl),
            warnings,
            exit: 1,
            is_error: false,
            json: None,
            to_stderr: false,
        };
    }

    // 6. Licence map: ALWAYS from the corpus's own corpus.json — never
    //    re-derived at query time, never from prose.
    let licences = manifest::licence_map(root, &corpus);

    let text = render::render(
        &hits,
        &licences,
        &render::RenderOpts {
            corpus: &corpus,
            query: &query,
            full: params.full,
            no_symbol: params.no_symbol,
            meatl: params.meatl,
            rrf_scores,
            age_days,
            strict_licence: params.strict_licence,
        },
        note.as_deref(),
    );

    CodeOutcome {
        text,
        warnings,
        exit: 0,
        is_error: false,
        json: None,
        to_stderr: false,
    }
}

/// Hits of a search response, missing containers tolerated as empty.
pub(crate) fn hit_list(resp: &Value) -> Vec<Value> {
    resp.pointer("/hits/hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Top-k diversity cap (#1137): at most this many records per source FILE in
/// a result page. The autoindex chunker (`SECTION_CHARS` = 2 KB, 200-char
/// overlap) turns one lexically dominant file into a wall of near-identical
/// neighbours that fills the whole top-k — measured 3–4 of 5 slots on the
/// zalando and otel G7 suites while the expected file never ranked.
/// Reference coding wants five FILES more than five passages of one.
pub(crate) const MAX_PER_FILE: usize = 2;

/// Fetch depth that gives the cap candidates to draw on: 10×k, bounded so a
/// k=50 page does not become a 500-hit fetch, and never below k itself.
pub(crate) fn diversity_fetch(k: usize) -> usize {
    k.saturating_mul(10).min(200).max(k)
}

/// Cap records per `_source.ax_file` at [`MAX_PER_FILE`], truncate to `want`,
/// preserving rank order. Returns the page and how many same-file neighbours
/// the cap skipped while filling it. A hit with no `ax_file` is never grouped
/// (missing provenance must not cost a slot).
pub(crate) fn diversify(hits: Vec<Value>, want: usize) -> (Vec<Value>, usize) {
    // Keys are owned: a borrowed &str would pin every hit it came from and
    // forbid the move into `out` below.
    let mut per_file: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::with_capacity(want.min(hits.len()));
    let mut dropped = 0usize;
    for h in hits {
        let key = h
            .pointer("/_source/ax_file")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(f) = key {
            let n = per_file.entry(f).or_insert(0);
            if *n >= MAX_PER_FILE {
                dropped += 1;
                continue;
            }
            *n += 1;
        }
        out.push(h);
        if out.len() == want {
            break;
        }
    }
    (out, dropped)
}

/// The measured BM25 body: flat `multi_match` over mapping-resolved fields
/// (`FIELDS`'s flat weights were measured, not chosen — 12/12 top-3). NEVER
/// a highlight block (issue #177: highlight changes ranking on this engine),
/// and no `_source` projection on the corpus path — passage selection needs
/// `body` and `symbols`.
pub(crate) fn bm25_body(query: &str, k: usize, lang: &Option<String>, fields: &[String]) -> Value {
    let mut must = vec![serde_json::json!({
        "multi_match": { "query": query, "fields": fields }
    })];
    if let Some(lg) = lang {
        must.push(serde_json::json!({ "match": { "language": lg } }));
    }
    serde_json::json!({ "size": k, "query": { "bool": { "must": must } } })
}

/// The vector-only body. Aimed ONLY at indices whose `body` is semantic_text
/// (a semantic query against plain text 400s the whole wildcard).
///
/// The clause carries `k` = the fetch window: without it the parser cuts the
/// vector pool to 10 whatever `size` says (#1145).
pub(crate) fn semantic_body(query: &str, k: usize, lang: &Option<String>) -> Value {
    let q = serde_json::json!({ "semantic": { "field": "body", "query": query, "k": k } });
    // `--lang` rides in `filter`: the engine sends a bool to the vector path
    // only when the semantic clause is its sole must/should clause (#1148).
    let query = match lang {
        Some(lg) => serde_json::json!({
            "bool": { "must": [q], "filter": [{ "match": { "language": lg } }] }
        }),
        None => q,
    };
    serde_json::json!({ "size": k, "query": query })
}

/// ONE native top-level `hybrid` request: BM25 clause + semantic clause,
/// fused server-side by RRF(k=60). Hybrid anywhere else is a 400 (#943), so
/// this shape is the whole contract — and there is deliberately NO
/// client-side fusion left to drift from the engine's.
pub(crate) fn hybrid_body(
    query: &str,
    k: usize,
    lang: &Option<String>,
    fields: &[String],
) -> Value {
    // `k` on the semantic clause: the vector leg's pool is cut to its own
    // `k` (parser default 10), not to `size`, so without it the #1137
    // overfetch widens the BM25 leg alone (#1145).
    let sem = with_lang(
        serde_json::json!({ "semantic": { "field": "body", "query": query, "k": k } }),
        lang,
    );
    hybrid_request(k, vec![bm25_leg(query, lang, fields), sem])
}

/// [`hybrid_body`] with its BM25 leg alone, for the lexical-only indices of
/// a mixed corpus (#1146). Still a native `hybrid`, so the engine stamps the
/// same per-index RRF score (`1/(k+rank)`) the full request does and the two
/// responses merge by `_score` exactly as the engine merges indices.
pub(crate) fn hybrid_bm25_leg_body(
    query: &str,
    k: usize,
    lang: &Option<String>,
    fields: &[String],
) -> Value {
    hybrid_request(k, vec![bm25_leg(query, lang, fields)])
}

fn bm25_leg(query: &str, lang: &Option<String>, fields: &[String]) -> Value {
    with_lang(
        serde_json::json!({ "multi_match": { "query": query, "fields": fields } }),
        lang,
    )
}

/// `--lang` must constrain BOTH legs (xc.py semantics): a language filter
/// on BM25 only lets the vector leg surface docs the user filtered out.
/// It is a `filter`, not a second `must`: a semantic clause beside a
/// sibling in `must` falls through to the lexical path and matches
/// nothing (#1148). Both legs take the same shape.
fn with_lang(q: Value, lang: &Option<String>) -> Value {
    match lang {
        Some(lg) => serde_json::json!({
            "bool": { "must": [q], "filter": [{ "match": { "language": lg } }] }
        }),
        None => q,
    }
}

fn hybrid_request(k: usize, legs: Vec<Value>) -> Value {
    let queries: Vec<Value> = legs
        .into_iter()
        .map(|q| serde_json::json!({ "query": q }))
        .collect();
    serde_json::json!({
        "size": k,
        "query": { "hybrid": {
            "queries": queries,
            "fusion": { "method": "rrf", "k": RRF_K }
        }}
    })
}

/// Merge two search responses the way the engine merges the indices of one
/// multi-index search: `_score` descending, stable — so each response keeps
/// its own rank order on a tie — with `_index` breaking cross-response ties
/// so the page does not depend on which request was sent first. Cut to
/// `size`; `hits.total` is the sum of both; the rest is `first`'s.
pub(crate) fn merge_by_score(mut first: Value, second: &Value, size: usize) -> Value {
    let mut hits = hit_list(&first);
    hits.extend(hit_list(second));
    let score = |h: &Value| h.get("_score").and_then(Value::as_f64).unwrap_or(f64::MIN);
    let index = |h: &Value| {
        h.get("_index")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    hits.sort_by(|a, b| {
        score(b)
            .total_cmp(&score(a))
            .then_with(|| index(a).cmp(&index(b)))
    });
    hits.truncate(size);

    let total_of = |r: &Value| r.pointer("/hits/total/value").and_then(Value::as_u64);
    let gte = |r: &Value| r.pointer("/hits/total/relation").and_then(Value::as_str) == Some("gte");
    let total = match (total_of(&first), total_of(second)) {
        (Some(a), Some(b)) => Some(serde_json::json!({
            "value": a + b,
            "relation": if gte(&first) || gte(second) { "gte" } else { "eq" }
        })),
        _ => None,
    };
    let max_score = hits.first().and_then(|h| h.get("_score")).cloned();
    if let Some(h) = first.get_mut("hits").and_then(Value::as_object_mut) {
        h.insert("hits".into(), Value::Array(hits));
        if let Some(t) = total {
            h.insert("total".into(), t);
        }
        if let Some(m) = max_score {
            h.insert("max_score".into(), m);
        }
    }
    first
}

/// `xerj corpus list` needs per-corpus live counts without the full pipeline;
/// this is the honest tri-state the ledger listing shares with `require_loaded`.
pub fn live_count_summary(http: &impl XcHttp, prefix: &str) -> Result<usize, String> {
    http.cat_indices_json(&format!("{prefix}*"))
        .map(|v| v.len())
}

/// Licence map for a corpus, exposed for `xerj corpus list`'s review.use line.
pub fn corpus_review_uses(root: &Path, corpus: &str) -> HashMap<String, String> {
    manifest::read_corpus_manifest(&root.join("corpora").join(corpus).join("corpus.json"))
        .map(|m| {
            m.repos
                .iter()
                .filter_map(|r| {
                    r.review
                        .as_ref()
                        .and_then(|v| v.get("use"))
                        .and_then(Value::as_str)
                        .map(|u| (r.repo.clone(), u.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake node: records every request, answers from canned data.
    struct FakeHttp {
        mapping: Value,
        hits: Vec<Value>,
        // (index, body) per search, in order — pins the EXACT wire shape.
        searches: std::sync::Mutex<Vec<(String, Value)>>,
        live: Vec<String>,
        fail_search: bool,
        // Per-target answers (#1146); a target not listed gets `hits`.
        hits_by_target: HashMap<String, Vec<Value>>,
    }

    impl FakeHttp {
        fn new() -> Self {
            FakeHttp {
                mapping: serde_json::json!({
                    "xc-kv-b1-000": { "mappings": { "properties": {
                        "body": { "type": "text" }, "defs": { "type": "text" },
                        "title": { "type": "text" }
                    }}}
                }),
                hits: vec![serde_json::json!({
                    "_score": 4.2,
                    "_source": {
                        "ax_path": "valkey/src/networking.c", "line": 1460,
                        "body": "void addReplyNull(client *c) {\n    addReplyProto(c, \"$-1\\r\\n\", 5);\n}\n",
                        "symbols": [ { "name": "addReplyNull", "kind": "function", "line": 1 } ]
                    }
                })],
                searches: std::sync::Mutex::new(Vec::new()),
                live: vec!["xc-kv-b1-000".to_string()],
                fail_search: false,
                hits_by_target: HashMap::new(),
            }
        }
    }

    impl XcHttp for FakeHttp {
        fn get_mapping(&self, pattern: &str) -> Result<Value, String> {
            // The real adapter hands `path` to `Es::get_json`, which sends it
            // verbatim after `base` — a path without the leading `/` is a
            // malformed URL, not a 404. Assert the shape here so a regression
            // in the caller fails the tests instead of silently passing.
            assert!(
                pattern.starts_with('/'),
                "get_mapping path must be request-shaped, got {pattern:?}"
            );
            Ok(self.mapping.clone())
        }
        fn cat_indices_json(&self, _pattern: &str) -> Result<Vec<String>, String> {
            Ok(self.live.clone())
        }
        fn search(&self, index: &str, body: &Value) -> Result<Value, String> {
            self.searches
                .lock()
                .unwrap()
                .push((index.to_string(), body.clone()));
            if self.fail_search {
                return Err("transport: connection refused".to_string());
            }
            let hits = self.hits_by_target.get(index).unwrap_or(&self.hits);
            Ok(serde_json::json!({ "hits": { "hits": hits } }))
        }
    }

    fn root_with_state() -> std::path::PathBuf {
        let root = tempfile::tempdir().unwrap().keep();
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::write(
            root.join("state/kv.json"),
            format!(
                "{{\"corpus\":\"kv\",\"indexed_at\":\"{}\",\"prefix\":\"xc-kv\",\"url\":\"u\",\
                 \"autoindex_exit\":0,\"salvaged\":false,\"build\":\"b1\",\
                 \"index_prefix\":\"xc-kv-b1\",\"state_dir\":\"/tmp/s\"}}",
                chrono::Utc::now().format(state::STAMP_FORMAT)
            ),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("corpora/kv/valkey")).unwrap();
        std::fs::write(
            root.join("corpora/kv/corpus.json"),
            "{\"corpus\":\"kv\",\"cloned_at\":\"t\",\"repos\":[{\"repo\":\"valkey\",\"url\":\"u\",\
             \"licence\":\"BSD-3-Clause\",\"sha\":\"s\"}]}",
        )
        .unwrap();
        root
    }

    #[test]
    fn bm25_query_uses_the_star_direct_glob_and_never_highlights() {
        let root = root_with_state();
        let http = FakeHttp::new();
        let out = run_code_query(
            &root,
            &http,
            "http://localhost:9200",
            &CodeParams::new("kv", "addReplyNull"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 0);
        let (idx, body) = http.searches.lock().unwrap()[0].clone();
        assert_eq!(idx, "xc-kv-b1*", "STAR-direct glob, not the dash form");
        assert_eq!(
            body,
            serde_json::json!({
                "size": 50,
                "query": { "bool": { "must": [
                    { "multi_match": { "query": "addReplyNull",
                        "fields": ["body", "defs", "title"] } }
                ]}}
            }),
            "mapping-resolved fields (defs_expanded dropped); no highlight, no _source \
             projection; size is the #1137 diversity fetch (10x k=5), cut back to k after"
        );
        assert!(out
            .text
            .contains("─── valkey/src/networking.c:1460  (score 4.20, BSD-3-Clause)"));
        assert!(out.text.contains("function addReplyNull @ line 1"));
        assert!(out.text.contains("Cite file:line"));
        assert!(!out.is_error);
        // No field-report nudge marker was ever part of this port (dropped
        // 2026-09-18): the prose must not ask the agent to report back.
        assert!(!out.text.to_lowercase().contains("report back"));
    }

    #[test]
    fn restricted_licence_warns_under_the_passage_end_to_end() {
        let root = root_with_state();
        std::fs::write(
            root.join("corpora/kv/corpus.json"),
            "{\"corpus\":\"kv\",\"repos\":[{\"repo\":\"valkey\",\"url\":\"u\",\"licence\":\"AGPL\"}]}",
        )
        .unwrap();
        let http = FakeHttp::new();
        let out = run_code_query(
            &root,
            &http,
            "u",
            &CodeParams::new("kv", "q"),
            "`--stale-ok`",
        );
        assert!(out
            .text
            .contains("!! AGPL: adapt the APPROACH, do not copy the code"));
    }

    #[test]
    fn not_in_state_not_loaded_and_no_match_are_three_different_answers() {
        let root = root_with_state();

        // Not in the ledger at all: usage-class refusal naming the fix (exit 2).
        let out = run_code_query(
            &root,
            &FakeHttp::new(),
            "u",
            &CodeParams::new("ghost", "q"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 2);
        assert!(out.text.contains("corpus 'ghost' is not indexed"));
        assert!(out.text.contains("xerj corpus index ghost"));

        // In the ledger, 0 live indices: the DISTINCT exit-3 diagnosis.
        let mut http = FakeHttp::new();
        http.live = vec![];
        let out = run_code_query(
            &root,
            &http,
            "http://localhost:9200",
            &CodeParams::new("kv", "q"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 3, "not-loaded is its own exit code");
        assert!(out.text.contains("This is NOT a 'no match'"));

        // Loaded, zero hits: exit 1 with the verbatim fall-back prose.
        let mut http = FakeHttp::new();
        http.hits = vec![];
        let out = run_code_query(
            &root,
            &http,
            "u",
            &CodeParams::new("kv", "xyzzy plugh"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 1);
        assert!(!out.is_error, "a miss is an answer, not an MCP error");
        assert!(
            out.text.contains(
                "The corpus is likely wrong for this task — fall back to normal work rather \
                 than forcing a bad match."
            ),
            "verbatim fall-back prose: {}",
            out.text
        );

        // --json with zero hits: still exit 1, empty array on stdout.
        let mut p = CodeParams::new("kv", "xyzzy plugh");
        p.as_json = true;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert_eq!(out.exit, 1);
        assert!(out.json.is_some());
    }

    #[test]
    fn a_stale_index_is_refused_and_stale_ok_is_the_only_override() {
        let root = root_with_state();
        let old = (chrono::Utc::now() - chrono::Duration::days(31))
            .format(state::STAMP_FORMAT)
            .to_string();
        std::fs::write(
            root.join("state/kv.json"),
            format!(
                "{{\"corpus\":\"kv\",\"indexed_at\":\"{old}\",\"prefix\":\"xc-kv\",\
                 \"autoindex_exit\":0}}"
            ),
        )
        .unwrap();
        let out = run_code_query(
            &root,
            &FakeHttp::new(),
            "u",
            &CodeParams::new("kv", "q"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 2);
        assert!(out.text.ends_with("or pass `--stale-ok`."));
        assert!(out.is_error);

        let mut p = CodeParams::new("kv", "q");
        p.stale_ok = true;
        let out = run_code_query(&root, &FakeHttp::new(), "u", &p, "`--stale-ok`");
        assert_eq!(out.exit, 0);
    }

    #[test]
    fn hybrid_is_one_native_top_level_request_with_capable_targeting() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.mapping = serde_json::json!({
            "xc-kv-b1-000": { "mappings": { "properties": { "body": { "type": "semantic_text" } } } },
            "xc-kv-b1-001": { "mappings": { "properties": { "body": { "type": "text" } } } }
        });
        let mut p = CodeParams::new("kv", "addReplyNull");
        p.mode = Mode::Hybrid;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert_eq!(out.exit, 0);

        let reqs = http.searches.lock().unwrap();
        // [0] = BM25 preflight over the wildcard; [1] = the fused request,
        // aimed ONLY at the capable index (the plain-text sibling would 400
        // the whole wildcard).
        assert_eq!(reqs[0].0, "xc-kv-b1*");
        assert_eq!(reqs[1].0, "xc-kv-b1-000");
        let hybrid = &reqs[1].1;
        assert!(
            hybrid.pointer("/query/hybrid").is_some(),
            "ONE native top-level hybrid: {hybrid}"
        );
        assert_eq!(
            hybrid.pointer("/query/hybrid/fusion").unwrap(),
            &serde_json::json!({ "method": "rrf", "k": 60 })
        );
        assert_eq!(
            hybrid
                .pointer("/query/hybrid/queries")
                .unwrap()
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        // #1137: the hybrid leg overfetches like every other mode (10x k=5).
        assert_eq!(hybrid.pointer("/size"), Some(&serde_json::json!(50)));
        // The arms-ran note says BOTH arms and the lexical-only exclusion.
        assert!(
            out.text
                .contains("[hybrid RRF(k=60) — BM25 over 2 index(es), vector over 1 of 2"),
            "{}",
            out.text
        );
        assert!(
            out.text.contains("rrf 4.20"),
            "hybrid scores render as rrf: {}",
            out.text
        );
    }

    /// #1146: with a mixed mapping the fused request may only go to the
    /// semantic_text indices (a semantic leg 400s on plain text), but the
    /// lexical-only indices must still answer the BM25 leg — before this fix
    /// hybrid returned 1 hit where `--mode bm25` returned 20, while the note
    /// claimed "BM25 over 2 index(es)".
    #[test]
    fn hybrid_keeps_lexical_only_indices_in_the_bm25_leg() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.mapping = serde_json::json!({
            "xc-kv-b1-000": { "mappings": { "properties": { "body": { "type": "semantic_text" } } } },
            "xc-kv-b1-001": { "mappings": { "properties": { "body": { "type": "text" } } } }
        });
        // Per-index RRF scores as the engine stamps them: 1/(60+rank).
        let hit = |idx: &str, file: &str, score: f64| {
            serde_json::json!({ "_index": idx, "_score": score,
                "_source": { "ax_path": file, "ax_file": file, "body": "retry handler" } })
        };
        http.hits_by_target.insert(
            "xc-kv-b1-000".into(),
            vec![hit("xc-kv-b1-000", "corpus.json", 2.0 / 61.0)],
        );
        http.hits_by_target.insert(
            "xc-kv-b1-001".into(),
            vec![
                hit("xc-kv-b1-001", "m1.py", 1.0 / 61.0),
                hit("xc-kv-b1-001", "m2.py", 1.0 / 62.0),
            ],
        );
        let mut p = CodeParams::new("kv", "retry handler");
        p.mode = Mode::Hybrid;
        p.as_json = true;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert_eq!(out.exit, 0);

        let reqs = http.searches.lock().unwrap().clone();
        // [0] preflight, [1] fused over the capable set, [2] the BM25 leg
        // alone over the lexical-only set — still a native `hybrid` so its
        // scores are RRF on the same scale as [1]'s.
        assert_eq!(reqs.len(), 3, "{reqs:?}");
        assert_eq!(reqs[1].0, "xc-kv-b1-000");
        assert_eq!(reqs[2].0, "xc-kv-b1-001");
        let lex = &reqs[2].1;
        let legs = lex
            .pointer("/query/hybrid/queries")
            .and_then(Value::as_array);
        assert_eq!(legs.map(Vec::len), Some(1), "BM25 leg only: {lex}");
        assert!(legs.unwrap()[0].pointer("/query/multi_match").is_some());
        assert_eq!(
            lex.pointer("/query/hybrid/fusion"),
            reqs[1].1.pointer("/query/hybrid/fusion"),
            "same fusion as the capable request"
        );
        assert_eq!(lex.pointer("/size"), Some(&serde_json::json!(50)));

        // Merged by _score descending, the engine's own cross-index order.
        let files: Vec<&str> = out.json.as_ref().unwrap()["hits"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["_source"]["ax_file"].as_str().unwrap())
            .collect();
        assert_eq!(files, ["corpus.json", "m1.py", "m2.py"]);

        // The prose path says which leg ran where.
        p.as_json = false;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert!(
            out.text.contains(
                "BM25 over 2 index(es), vector over 1 of 2; lexical-only (BM25 leg only, \
                 merged by score): xc-kv-b1-001"
            ),
            "{}",
            out.text
        );
    }

    /// #1158: a raw-JSON corpus (ghsa-db advisories) maps none of
    /// body/defs/title/text. The multi_match must go to the corpus's OWN
    /// text-typed fields — the old `["body"]` floor is a field no index
    /// maps, which collapses a multi-token query to zero hits — and the
    /// hit must render a real passage from its longest string field, not
    /// an empty one.
    #[test]
    fn a_raw_json_corpus_queries_its_own_text_fields_and_renders_a_passage() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.mapping = serde_json::json!({
            "xc-kv-b1-000": { "mappings": { "properties": {
                "id": { "type": "keyword" },
                "summary": { "type": "text" },
                "details": { "type": "text" },
                "ax_path": { "type": "keyword" }
            }}}
        });
        http.hits = vec![serde_json::json!({
            "_score": 5.1,
            "_source": {
                "ax_path": "ghsa-db/advisories/GHSA-9j.json",
                "id": "GHSA-9j",
                "summary": "lodash command injection",
                "details": "Applications using lodash are vulnerable to command \
                            injection via the template function."
            }
        })];
        let out = run_code_query(
            &root,
            &http,
            "u",
            &CodeParams::new("kv", "command injection"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 0, "{}", out.text);
        let reqs = http.searches.lock().unwrap().clone();
        assert!(!reqs.is_empty());
        assert_eq!(
            reqs[0].1.pointer("/query/bool/must/0/multi_match/fields"),
            Some(&serde_json::json!(["details", "summary"])),
            "the corpus's own text fields, sorted — not the silent-zero body floor",
        );
        assert!(
            out.text.contains("vulnerable to command injection"),
            "a real passage from the longest string field: {}",
            out.text
        );
    }

    /// #1145: a `semantic` clause without `k` is cut to the parser default
    /// (10), so the vector leg must carry the same window as `size` — else
    /// `--mode semantic -k 20` returns 10 hits and hybrid fuses BM25@50 with
    /// vector@10 (the #1137 overfetch widened the BM25 leg alone).
    #[test]
    fn the_vector_leg_carries_the_fetch_window_as_k() {
        let sem = semantic_body("q", 50, &None);
        assert_eq!(sem.pointer("/size"), Some(&serde_json::json!(50)));
        assert_eq!(
            sem.pointer("/query/semantic/k"),
            Some(&serde_json::json!(50))
        );

        let fields = vec!["body".to_string()];
        let hy = hybrid_body("q", 50, &None, &fields);
        assert_eq!(
            hy.pointer("/query/hybrid/queries/1/query/semantic/k"),
            Some(&serde_json::json!(50)),
            "{hy}"
        );
    }

    /// #1148: the engine dispatches a bool to the vector path only when the
    /// semantic clause is its ONE must/should clause (bool.filter is merged
    /// into the semantic filter); a `match` beside it in `must` fell through
    /// to the lexical path and answered 200 with zero hits. `--lang` must
    /// therefore ride in `filter`, on both hybrid legs.
    #[test]
    fn lang_is_a_filter_so_the_semantic_clause_stays_alone_in_must() {
        let lang = Some("rust".to_string());
        let want_filter = serde_json::json!([{ "match": { "language": "rust" } }]);

        let sem = semantic_body("q", 50, &lang);
        let must = sem.pointer("/query/bool/must").and_then(Value::as_array);
        assert_eq!(must.map(Vec::len), Some(1), "{sem}");
        assert!(must.unwrap()[0].get("semantic").is_some(), "{sem}");
        assert_eq!(sem.pointer("/query/bool/filter"), Some(&want_filter));

        let fields = vec!["body".to_string()];
        let hy = hybrid_body("q", 50, &lang, &fields);
        for (leg, clause) in [(0, "multi_match"), (1, "semantic")] {
            let q = hy
                .pointer(&format!("/query/hybrid/queries/{leg}/query"))
                .unwrap();
            let must = q.pointer("/bool/must").and_then(Value::as_array);
            assert_eq!(must.map(Vec::len), Some(1), "leg {leg}: {q}");
            assert!(must.unwrap()[0].get(clause).is_some(), "leg {leg}: {q}");
            assert_eq!(q.pointer("/bool/filter"), Some(&want_filter), "leg {leg}");
        }
    }

    #[test]
    fn hybrid_without_a_capable_index_degrades_to_bm25_and_says_so() {
        let root = root_with_state();
        let http = FakeHttp::new(); // mapping has plain text only
        let mut p = CodeParams::new("kv", "q");
        p.mode = Mode::Hybrid;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert_eq!(out.exit, 0);
        assert!(
            out.text.contains("[BM25 only —"),
            "degradation is SAID: {}",
            out.text
        );
        assert_eq!(
            http.searches.lock().unwrap().len(),
            1,
            "no fused request was sent"
        );
    }

    #[test]
    fn hybrid_reports_an_honest_miss_when_the_bm25_preflight_is_empty() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.mapping = serde_json::json!({
            "xc-kv-b1-000": { "mappings": { "properties": { "body": { "type": "semantic_text" } } } }
        });
        http.hits = vec![]; // preflight AND any vector hit: nothing
        let mut p = CodeParams::new("kv", "xyzzy plugh");
        p.mode = Mode::Hybrid;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert_eq!(
            out.exit, 1,
            "vector confidence must not launder a lexical miss"
        );
        assert!(
            out.text.contains("fall back to normal work"),
            "{}",
            out.text
        );
    }

    #[test]
    fn transport_failure_is_exit_2_never_fake_zero_indices() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.fail_search = true;
        let out = run_code_query(
            &root,
            &http,
            "u",
            &CodeParams::new("kv", "q"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 2);
        assert!(out.text.contains("search failed"), "{}", out.text);
    }

    /// #1137: one dominant file's adjacent 2 KB chunks must not fill the
    /// whole page. 8 hits from `compatibility` + 3 from other files, k=5:
    /// at most 2 compatibility records survive, the page fills with the
    /// next-ranked files, and the note says the cap ran.
    #[test]
    fn one_file_may_not_fill_the_page() {
        let mk = |file: &str, i: usize| {
            serde_json::json!({
                "_score": 10.0 - i as f64,
                "_source": { "ax_file": file, "ax_path": file,
                             "body": format!("passage {i} of {file}") }
            })
        };
        let mut hits: Vec<Value> = (0..8).map(|i| mk("compatibility", i)).collect();
        hits.push(mk("pagination", 8));
        hits.push(mk("security", 9));
        hits.push(mk("http-headers", 10));

        let (page, dropped) = diversify(hits, 5);
        let files: Vec<&str> = page
            .iter()
            .map(|h| {
                h.pointer("/_source/ax_file")
                    .and_then(Value::as_str)
                    .unwrap()
            })
            .collect();
        assert_eq!(
            files,
            vec![
                "compatibility",
                "compatibility",
                "pagination",
                "security",
                "http-headers"
            ]
        );
        assert_eq!(
            dropped, 6,
            "8 compatibility hits -> 2 kept, 6 skipped, rank order kept"
        );
    }

    /// A page that cannot fill is returned SHORT, not padded back up with the
    /// very neighbours the cap exists to suppress.
    #[test]
    fn a_single_files_wall_yields_a_short_page() {
        let hits: Vec<Value> = (0..10)
            .map(|i| {
                serde_json::json!({
                    "_score": 10.0 - i as f64,
                    "_source": { "ax_file": "spec.md", "body": format!("chunk {i}") }
                })
            })
            .collect();
        let (page, dropped) = diversify(hits, 5);
        assert_eq!(page.len(), 2);
        assert_eq!(dropped, 8);
    }

    /// Missing provenance is not a reason to drop a hit: hits without
    /// `ax_file` never group together.
    #[test]
    fn hits_without_ax_file_never_group() {
        let hits: Vec<Value> = (0..4)
            .map(|i| serde_json::json!({ "_score": 4.0 - i as f64, "_source": { "body": "b" } }))
            .collect();
        let (page, dropped) = diversify(hits, 5);
        assert_eq!(page.len(), 4, "no ax_file -> no cap applies");
        assert_eq!(dropped, 0);
    }

    /// 10x k, capped at 200, never below k.
    #[test]
    fn fetch_depth_bounds() {
        assert_eq!(diversity_fetch(5), 50);
        assert_eq!(diversity_fetch(50), 200);
        assert_eq!(diversity_fetch(1), 10);
    }
}
