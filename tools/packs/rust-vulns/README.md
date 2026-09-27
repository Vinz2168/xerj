# rust-vulns — the showcase corpus pack

Rust vulnerability advisories, **identity-resolved across sources**: the same
vulnerability that arrives as `RUSTSEC-2021-0003`, `GHSA-43w2-9j62-hq99` and
`CVE-2021-25900` is one record, carrying every field any source knew — and
the curated vulnerable-function paths no export carries.

Built with `xerj corpus build` from this directory's
[recipe.toml](./recipe.toml). Every number below is measured from the actual
build ([pack-stats.json](./pack-stats.json), `provenance: measured`) and a
test pins README against stats so the two cannot drift apart.

| What | Count |
|---|---|
| Source records harvested (envelopes) | 4107 |
| Records after identity resolution | 1950 |
| …merged from both sources | 1221 |
| …canonical id RUSTSEC / GHSA / MAL | 1221 / 711 / 18 |
| Records with affected-function paths | 270 (663 distinct paths) |
| Records withdrawn (kept, flagged `is_withdrawn`) | 124 |
| Records with a CVE alias | 992 |
| Packages covered | 1813 |
| Records JSONL bytes | 10140789 |
| Cold build (network fetch + harvest + pack) | ~34 s |

## Why this pack exists

Each source alone is incomplete:

- **osv.dev's crates.io export** (CC0, 2,856 records) never merges aliases —
  the same vulnerability sits there as separate GHSA and RUSTSEC records
  sharing a CVE — and carries **zero** affected-functions data.
- **RustSec's advisory-db** (CC0, 1,251 native advisories) is the only
  source of the hand-curated `affected.functions` paths, CVSS vectors,
  categories, keywords and full advisory prose — but has no GHSA-only
  records.

The recipe's union-find over `id` + every `aliases` element closes the gap:
4,107 envelopes collapse to 1,950 records. Sibling filings union their
function lists — e.g. one 2026 CVE filed against three `libcrux-*` crates
becomes one record carrying all three crates' function paths.

GHSA is deliberately **not** a direct source: its crates.io records are
already inside the osv.dev export under CC0, and the only fields a direct
`github/advisory-database` clone would add live in `database_specific`,
which the tool never reads (upstream marks it internal-use-only). HackerOne
content is unredistributable under its ToS and is out of scope — the pack
author owns what goes in a pack.

## Build, consume, query

```sh
# build (or incrementally refresh) — fetches both sources, dedups, packs
xerj corpus build rust-vulns --recipe tools/packs/rust-vulns/recipe.toml
# pack lands at $XERJ_CODE_HOME/builds/rust-vulns/pack/rust-vulns/

# consume: install as a reference-coding corpus
xerj corpus add rust-vulns --from ~/.xerj-code/builds/rust-vulns/pack/rust-vulns

# query it — the envelope targets both ES-compat search and `xerj code`
xerj code rust-vulns "smallvec insert_many buffer overflow"
```

The pack ships `suggestions.md` / `mapping.suggested.json` /
`relations.jsonl` — field-type verdicts and join candidates computed from a
sample. Suggestions, not decisions: the index infers the real mapping.

## Fields worth knowing

| Field | What it carries |
|---|---|
| `id` / `aliases` | canonical advisory id and every alias (CVE/GHSA/RUSTSEC) unioned across sources |
| `cve_ids` | derived: CVE references extracted from id + aliases |
| `affected_functions` | RustSec's curated vulnerable-function paths — **unique to this pack** |
| `defs` | affected_functions joined — feeds `xerj code`'s symbol search |
| `patched_versions` / `unaffected_versions` | RustSec version requirements |
| `introduced_versions` / `fixed_versions` | osv.dev range events, unioned |
| `severity` / `severity_scores` | CVSS vectors |
| `categories` / `keywords` | RustSec facets |
| `is_withdrawn` / `informational` | lifecycle flags — withdrawn records stay, flagged |
| `licence` / `sources` / `origin` | provenance per record |

## Honest limits

- **Freshness is whatever the last build fetched.** Rebuild to refresh;
  a scheduled build with a freshness badge is the next milestone.
- **Not a security product.** This is a searchable corpus of advisories for
  agents and humans; it is not a scanner and makes no completeness claim
  beyond "what these two sources carried at build time".
- Withdrawn (124) and informational advisories are kept and flagged, not
  dropped — filtering is the consumer's decision.
- v1 ships checksums (SHA256SUMS verifies on `corpus add`); signing lands
  with the publishing milestone.
