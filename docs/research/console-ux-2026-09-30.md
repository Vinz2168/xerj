# Console UX review — what a user sees after indexing a mixed corpus

Date: 2026-09-30 · Reviewer: UX gate pass (task "UX review: what users see after
indexing a mixed corpus") · Binary reviewed: `xerj v1.0.0-rc.78` (release binary
built from main @ `a179e1ef3`; this worktree is the same commit, so the code read
and the binary booted are one snapshot) · Scope: `/_xerj-console` (SPA +
`xerj-console-api`) as shipped, after one `xerj autoindex` run over a mixed
folder. No engine code changed for this review.

## Method note — no browser

There is no browser in this sandbox. Every screen below is reconstructed the
only honest way available:

1. **The served SPA** — `GET /_xerj-console/` returns the real `index.html`
   (26 lines, CSP `script-src 'self'`, boots `src/boot.js` → auth guard →
   `app.js`). All assets were fetched from the running server
   (`/_xerj-console/src/app.js` etc., 200s, correct content types).
2. **The API calls the SPA makes** — read out of `xerj-ux/src/**` and then
   curled verbatim with an operator session cookie: `/me`,
   `/data-sources/connections[...]/indices[...]/fields|search`, `/graph/brains`,
   `/graph/{brain}/overview|ego|edges/_search`, `/prefs`, `/dashboards`,
   `/_cat/indices`, `/_cluster/stats`.
3. **The repo's own renderers** — where the render code is pure
   (`ux/corpus-render.js`, `data/catalog.js`, `data/second-brain-api.js`), it
   was executed under node against the live payloads, so what is quoted below is
   the SPA's own output for this corpus, not a paraphrase.

The operator session itself was obtained by completing the real first-launch
bootstrap (magic link from the server's stderr banner → passkey enrolment with a
software P-256 authenticator speaking the actual WebAuthn ceremony — the server
verified the attestation). That journey is itself a finding (gap 7).

## The corpus indexed

Built at `/tmp/uxcorpus` — an operations team's working folder, mixed kinds:

| kind | files | source records |
|---|---|---|
| mbox mailboxes (`incidents`, `vendors`, `misc`) | 3 | 90 messages |
| loose `.eml` (phishing reports) | 12 | 12 |
| CSV tables (incidents, vendor spend, deploy log, assets) | 4 | 220 rows |
| JSON records (NDJSON: RFCs, service catalog, audit events) | 3 | 54 |
| markdown notes (40 wikilinked, `notes/ops` + `notes/design`) | 40 | 40 |
| source files (rust, python, js, go, ts, sql, sh) | 10 | 10 |
| **total** | **72 files / 84 KB** | **426** |

`xerj autoindex /tmp/uxcorpus --url http://127.0.0.1:9712` (node booted
`--insecure --embed-mode lexical --port 9712`, throwaway data dir):

```
done in 2.2s — 8 datasets, 512 records live, 0 duplicate aliases, 0 junk records
  ax-docs 231 · ax-tables-deploy-log 81 · ax-tables-incidents 61 · ax-tables-vendor-spend 46
  ax-tables-assets 36 · ax-records-rfcs 26 · ax-records-audit-events 21 · ax-records-service-catalog 10
graph: 290 edges → .xerj-memory-uxcorpus-edges (samedir@2 63, sequence@2 41, sharedterm@1 68, wikilink@2 118)
```

## Walk — every view/mode the SPA has

Sections (top nav, `dashboards/registry.js`): **Corpus · Discover · Reader ·
Dashboards · Alerts · Data · Users · Settings**. Dashboards splits into groups
AI (ai-overview, rag-quality, vector-index, agent-memory, **second-brain**),
Logs (logs-overview, anomaly-detect, ingest-pipeline), Infra (system). Each of
AI/Logs gates its nav entry on a live probe (`requiresLive`); after this run
none of those index classes exist, so **Dashboards renders exactly two entries:
Second Brain and System**.

### CORPUS (the landing view — `#/`)

Data: `POST /_xerj-console/api/v1/data-sources/connections/built-in/indices/autoindex-catalog/search`
with `{query:{term:{doc_kind:"dataset"}}}` → 8 hits, one per dataset. The card
for `ax-docs` (rendered by the repo's own `renderCorpusCard` under node):

```
DATASET  ax-docs
231 records · 65 files · 55 KB · last run
formats · last run: code, eml, mbox, txt-prose
email_date: 2025-11-03 → 2025-11-08
semantic_text field: body
fields (8 of 25): body/semantic_text, email_from_address/keyword, email_message_id/keyword,
  email_to/keyword, email_to_address/keyword, email_date/date, email_from/keyword, language/keyword
  (each with 2 example values in a hover tooltip)
[OPEN IN READER] [BROWSE IN DISCOVER]
TRY A QUERY · FROM THIS DATASET'S OWN CATALOG ENTRY:
  TERM email_message_id=08943a91ccd0.0@northwind-example.com
  MATCH Certificate   HYBRID Certificate
```

Scene meta: `8 DATASETS · 512 RECORDS`. The seven table/record datasets get the
same treatment (formats, time span, "no semantic_text field in the last run",
fields with types, one TERM sample each). Empty state (verified by code,
`corpus-render.js`): one command, `xerj brain <folder>` — never sample data.

**What it requires the user to know**: nothing — this is the one screen that
answers "what is indexed here" unattended. It is also the only screen that
surfaces the autoindex catalog at all.

### DISCOVER (`#/discover`)

Search over one index at a time; `*` resolves to the first user index and says
so (`* SEARCHES ONE INDEX AT A TIME — SHOWING ax-docs`). Query types:
match / term / range / prefix / phrase / semantic / hybrid. The schema roles
derived live from the mapping for `ax-docs`:

```json
{"textField":"body","semanticField":"body","dateField":"email_date","isEmail":true,
 "searchFields":["body","email_subject","title","code","defs"],
 "keywordFields":["email_from","email_from_address","email_to", ...]}
```

A hybrid query as the console builds it (`data/search-body.js`) ran through the
console proxy: `phishing payroll` → **28 hits, 13 ms**, top hit the phishing
report emails. The caption is honest about the default embedder ("with the
default embedder that is lexical feature hashing, not a neural model").

**Requires**: knowing a query language exists and which of 7 types to pick;
facet fields are auto-picked (first 3 keyword fields).

### READER (`#/reader?index=…&id=…`)

Record view: title, body, metadata table, attachments, sibling records of the
same file, plus **the knowledge graph around the record** (`GET /_graph/{brain}/ego`).
For note `notes/ops/runbook-index.md`:

```
edges: 4
  wikilink  - [[tiered-storage-layout]]
  wikilink  - [[search-relevance-review]]
  wikilink  - [[console-auth-bootstrap]]
  sequence  section 0 opens notes/ops/runbook-index.md
```

**Requires**: arriving from a Corpus card or a `xerj brain` link; nothing
advertises the Reader from the CLI side.

### SECOND BRAIN (`#/second-brain` — "knowledge base mode"; see note below)

`GET /_xerj-console/api/v1/graph/brains` → `[{"name":"uxcorpus",
"nodes_index":"ax-docs,…,ax-tables-vendor-spend"}]`. Overview:

```json
{"nodes":{"total":512},"edges":{"total":290,"live":290,"invalidated":0},
 "types":[{"type":"wikilink","live":118},{"type":"shared_term","live":68},
          {"type":"same_dir","live":63},{"type":"sequence","live":41}],
 "detectors":[{"detector":"wikilink@2","live":118}, ...], "embedder":"lexical-feature-hash"}
```

Node stats (`nodes/_search`): `csv 224 · mbox 93 · txt-prose 82 · jsonl 57 ·
code 32 · eml 24`. Crossings: **5 of 290 edges cross file formats** (md→md 264,
eml→eml 11 …). Default ego lands on the top hub: 69 edges, 38 neighbours.

### DATA (`#/data`)

Live via the console facade (`GET …/connections` → 1 connection `built-in`;
`…/indices` → the 9 real user indices with correct docs counts;
`…/indices/ax-docs/fields` → the real 25-field mapping). But:

- **Bytes read `0.0 B` for every index** — `data-sources.js` maps the facade
  response with `bytes: 0 // bytes not surfaced by phase-3 facade yet`.
- The **CLUSTERS panel shows four clusters; three are fabricated**
  (PROD-US 840M docs yellow, PROD-EU 512M, STAGING 22M — `MOCK_CLUSTERS` kept
  "so the selector still has multi-cluster shape"), and the LOCAL row keeps its
  mock totals (`6 idx · 1M docs · v0.1.0`) because the console-connections path
  overrides only url/status/name/kind. Clicking PROD-US (`data-mg-cluster`)
  swaps the index table to `MOCK_INDICES['prod-us']` — ten invented indices
  with billions of docs.
- FIELDS table columns CARDINALITY and RATIO render `—` and `0%` (nulls from the
  facade; `(null*100).toFixed(0)` prints `0%`).

### ALERTS (`#/alerts`), USERS (`#/users`), SYSTEM (`#/dashboards/system`)

All three render **fabricated numbers on every boot**. Alerts: `7 ACTIVE FIRES ·
3 SILENCED · 48 RULES DEFINED` with a caption saying so ("Illustrative sample
data — not yet wired to your engine's alert rules"). Users: `128 active users ·
8 roles · 42 API keys · 86 sessions`. System: sample host metrics (captioned).
The engine actually holds `.xerj_alert_rules` and `.xerj_alert_fires` — **both
empty (0 docs)** — and the console has real endpoints for passkeys and API
tokens (`/auth/passkeys`, `/auth/api-tokens`) that the Users screen ignores.

### SETTINGS

Local dashboard management (rename/reorder/hide/clone, reset). Real and honest.

## The user gate, answered

**(a) How large is the corpus — docs, bytes, per index?**
**Visible, with gaps.** Corpus home: per-dataset `records · files · bytes`
(231 records / 65 files / 55 KB …) and scene meta `8 DATASETS · 512 RECORDS`.
NOT visible anywhere: total corpus bytes or files summed; the DATA section — the
screen whose caption says "what the engine actually has" — shows `0.0 B` per
index (facade gap) under a fabricated `1M docs` LOCAL cluster header. The number
that would settle it exists in `/_cluster/stats` (914 docs, 897 KB store across
24 indices) and in `_cat/indices` (`store.size`), but the session-authenticated
facade path surfaces neither.

**(b) Which data is there — indices, fields, types, coverage, examples?**
**Mostly visible — and the weakest part is that the terminal beats the GUI.**
The Corpus cards show indices, formats, time span, semantic field, up to 8
fields with types, and 2 example values (hover tooltip only). The autoindex
catalog actually carries **coverage (null%) and cardinality per field**, and
`xerj autoindex map` prints them in markdown:

```
| `email_date` | date (rfc3339) | — | 40 | 39% | `2025-11-03T09:00:00Z`, … |
| `language`   | keyword        | — | 6  | 86% | `go`, `python`, `rust` |
```

No console surface shows null% or cardinality anywhere (DATA's FIELDS table
prints `—` and `0%` placeholders). Field examples are invisible without hover,
and cards cap at 8 of 25 fields.

**(c) What XERJ can do next — search, ask, decide, watch, map…?**
**Partial, and the gaps are the product's whole second act.** The console
advertises: search (Discover, 7 query types incl. semantic/hybrid), read
(Reader), graph (Second Brain), dashboards. It never mentions — not in nav,
captions, or any panel: **decide/ask** (`/_decide`, `/v1/systemone` answered on
this node with a real config error, i.e. the ladder exists), **watch**
(`xerj autoindex --watch` change feed; the CLI has a full object-watch budget
surface), **share** (`xerj share` — a human-facing feature, CLI-only), **map**
(`xerj autoindex map`, richer than any console screen), **def/passage** search
for code (the catalog's own sample queries carry code-aware bodies the console
never offers), **MCP** (13 agent tools). Worse, the two sections that do name
capabilities — Alerts and Users — describe them with invented numbers.

## The agents-first asymmetry (facts agents get, console humans don't)

`xerj mcp --help` proxies **thirteen tools**: xerj_search, xerj_semantic_search,
xerj_vector_search, xerj_hybrid_search, xerj_memory_store, xerj_memory_recall,
xerj_brain_ego, xerj_brain_link, xerj_brain_unlink, xerj_brain_overview,
xerj_code_search, **xerj_map**, **xerj_plan**.

| Fact / capability | Agent path | Console human |
|---|---|---|
| Full field table: type, semantic, cardinality, null%, 3 examples | `xerj_map` / `autoindex map` markdown | 8 fields, 2 hover examples, no cardinality/null% |
| Query planning over the corpus | `xerj_plan` | — |
| Graph writes (assert / remove a link) | `xerj_brain_link` / `unlink` | graph is read-only in the console |
| Memory store/recall | `xerj_memory_store/recall` | — |
| Code-aware search (defs/passage) | `xerj_code_search` | Discover has no code mode |
| Decide ladder | `/_decide`, `/v1/systemone` | — |
| Watch a folder for changes | `autoindex --watch` | — (Alerts shows mock fires instead) |
| Read-only share for a human | `xerj share` | share *viewing* exists (`/_xerj-console/share`); creating one is CLI-only |

(Also: the bare-server `--help` still says MCP "exposes 11 tools" —
`engine/crates/xerj-server/src/main.rs:332` — while `xerj mcp --help` lists
thirteen. Stale count, worth a one-line fix.)

## The "knowledge base mode" note

What users call knowledge-base mode is the **SECOND BRAIN** dashboard — the
thing `xerj brain <folder>` opens in a browser (`#/second-brain?brain=<name>`).
It is the *relationship layer* over the corpus: a bounded map, a belief-time
scrubber, the evidence ledger, detector stats, crossings, plus an honest
"what this view did not show" panel. It feels weird for three concrete reasons,
all visible in this run:

1. **Its frame is epistemics, not knowledge.** Headers say "WHAT YOUR NOTES
   BELIEVE · EVERY LINK SHOWS ITS EVIDENCE", "BELIEVED AT THIS MOMENT",
   "RETIRED · KEPT FOR REPLAY", "WHAT TAUGHT THIS BRAIN". A user who indexed a
   work folder is asking "show me my stuff", not "what do my notes believe".
2. **For an ordinary mixed corpus the graph is mostly plumbing.** 285 of 290
   edges stay inside one format (md→md 264); the detectors that fired hardest
   are `same_dir` (63) and `sequence` (41) — file adjacency, not meaning. The
   wikilinks a person actually wrote (118) sit in the same undifferentiated
   ledger as "section 0 opens notes/ops/runbook-index.md".
3. **Two doors, two frames.** `xerj brain` (a top-level product command that
   opens the browser) lands on a dashboard nested under Dashboards → AI group,
   beside gated telemetry dashboards — while the documents themselves live in
   Corpus/Reader. Nothing on the Second Brain page links back to the corpus
   cards, and nothing on the corpus cards mentions the brain's name
   (`uxcorpus`) even though `graph/brains` reports it maps all 8 indices.

The pending "simplified knowledge view" task should aim at exactly this seam: a
document-first browse of what the brain holds (the node stats already know:
csv 224 · mbox 93 · txt-prose 82 · jsonl 57 · code 32 · eml 24) with the graph
as a secondary, human-worded panel.

## Ranked gaps

1. **No capability surface.** After indexing, no console screen answers "what
   can I do with this now?" — decide, ask, watch, share, map, plan, MCP are all
   invisible (gate c). The empty-state's one command (`xerj brain <folder>`) is
   the only forward pointer the console ever gives.
2. **ALERTS and USERS show fabricated numbers on every console** (7 fires / 48
   rules / 128 users) while the engine's real state is empty and readable
   (`.xerj_alert_rules`, `.xerj_alert_fires`, `/auth/passkeys`,
   `/auth/api-tokens`). An honest-claims risk sitting in the shipped UI.
3. **DATA undercuts (a) and (b)**: `0.0 B` sizes on the facade path, a LOCAL
   cluster row with mock `6 idx · 1M docs`, and three entirely fake clusters
   (PROD-US/EU/STAGING) that are clickable into fake index tables.
4. **Coverage (null%) and cardinality are known to the catalog and printed by
   `autoindex map`, but appear nowhere in the console** — the terminal answers
   "which data is there" better than the GUI.
5. **Field examples are hover-only and capped at 8 per card**; the catalog's
   examples are the fastest way to understand a dataset and are nearly invisible.
6. **No door from CLI to console**: `xerj autoindex`'s "next:" prints curl and
   `autoindex map`, never the `/_xerj-console` URL; only `xerj brain` opens the
   browser. The bootstrap URL lives in the server's stderr banner alone.
7. **First-run console auth is a hard ceremony on dev nodes**: a node booted
   `--insecure` (no TLS, no API keys) still requires the full WebAuthn passkey
   bootstrap; there is no dev bypass and the setup link is only in stderr.
   (This review had to implement a software authenticator to walk the console.)
8. **No whole-corpus totals** (bytes, files) on any screen; scene meta counts
   datasets and records only.
9. **Graph is read-only for humans** while agents can link/unlink; a person
   cannot correct a bad edge from the UI.
10. **Stale capability text**: server `--help` says MCP exposes 11 tools; the
    MCP server lists 13 (main.rs:332).

## Repro

```sh
xerj --insecure --embed-mode lexical --port 9712 -d /tmp/uxdata-console &
xerj autoindex /tmp/uxcorpus --url http://127.0.0.1:9712
# console: http://127.0.0.1:9712/_xerj-console  (setup link in the server's stderr)
curl -s -H "Cookie: xerj_session=…" \
  http://127.0.0.1:9712/_xerj-console/api/v1/data-sources/connections/built-in/indices
```
