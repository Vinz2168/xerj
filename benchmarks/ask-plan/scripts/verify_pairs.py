#!/usr/bin/env python3
"""verify_pairs.py — static checks on data/gold/pairs.jsonl. Exit 1 on any.

STATUS: DESIGN, NOTHING MEASURED. This verifies evaluation *inputs*: schema,
count, derivability from the raw data, and DSL shapes. It runs no engine and
scores nothing. Use it in CI next to derive_pairs.py --check.

Checks:
  1. JSONL parses; every line has exactly the keys
     {id, dataset, index_hint, prompt, gold:{index, doc_ids, query_equivalent}}.
  2. Pair count >= MIN_PAIRS (the #1056 gate needs >= 200).
  3. ids unique; index_hint == gold.index == "ax-<dataset>"; datasets known.
  4. doc_ids sorted, unique, non-empty, and EVERY id exists in the raw
     snapshot's id space (usgs `id`, exoplanet `pl_name`, gapminder
     "<country>-<year>") — gold that references rows not in the raw data is
     the classic silent fixture rot.
  5. query_equivalent uses only leaves xerj-query parses (term / range /
     bool.filter), with ranges carrying at least one bound.
  6. derive_pairs.py --check passes (file is byte-identical to what the
     committed raw snapshots produce — no hand-edits, no stale gold).
"""
import csv
import json
import pathlib
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
GOLD = HERE.parent / "data" / "gold" / "pairs.jsonl"
RAW = HERE.parent / "data" / "raw"
MIN_PAIRS = 200

ALLOWED_LEAVES = {"term", "range", "bool"}


def raw_ids(dataset):
    if dataset == "usgs-earthquakes":
        return {r["id"] for r in csv.DictReader(open(RAW / dataset / "usgs-earthquakes.csv"))}
    if dataset == "nasa-exoplanets":
        return {r["pl_name"] for r in csv.DictReader(open(RAW / dataset / "nasa-exoplanets.csv"))}
    if dataset == "gapminder":
        return {f"{r['country']}-{r['year']}"
                for r in csv.DictReader(open(RAW / dataset / "gapminder.tsv"), delimiter="\t")}
    raise SystemExit(f"unknown dataset {dataset}")


def check_query(q, where):
    if not isinstance(q, dict) or len(q) != 1:
        raise AssertionError(f"{where}: query_equivalent must be a single-key object, got {q!r}")
    leaf = next(iter(q))
    if leaf not in ALLOWED_LEAVES:
        raise AssertionError(f"{where}: unsupported leaf {leaf!r} (allowed: {sorted(ALLOWED_LEAVES)})")
    body = q[leaf]
    if leaf == "bool":
        if set(body) - {"filter"}:
            raise AssertionError(f"{where}: only bool.filter is used by this fixture, got {sorted(body)}")
        for sub in body["filter"]:
            check_query(sub, where)
    elif leaf == "range":
        for field, bounds in body.items():
            if not set(bounds) & {"gt", "gte", "lt", "lte"}:
                raise AssertionError(f"{where}: range on {field} has no bound")
    else:  # term
        for field, value in body.items():
            if not isinstance(value, (str, int, float, bool)):
                raise AssertionError(f"{where}: term on {field} has non-scalar value {value!r}")


def main():
    id_spaces = {d: raw_ids(d) for d in ("usgs-earthquakes", "nasa-exoplanets", "gapminder")}
    seen_ids = set()
    n = 0
    for line in GOLD.read_text().splitlines():
        n += 1
        where = f"line {n}"
        p = json.loads(line)
        assert set(p) == {"id", "dataset", "index_hint", "prompt", "gold"}, f"{where}: keys {sorted(p)}"
        g = p["gold"]
        assert set(g) == {"index", "doc_ids", "query_equivalent"}, f"{where}: gold keys {sorted(g)}"
        assert p["id"] not in seen_ids, f"{where}: duplicate id {p['id']}"
        seen_ids.add(p["id"])
        assert p["dataset"] in id_spaces, f"{where}: unknown dataset"
        assert p["index_hint"] == g["index"] == f"ax-{p['dataset']}", f"{where}: index mismatch"
        assert isinstance(p["prompt"], str) and p["prompt"].strip(), f"{where}: empty prompt"
        docs = g["doc_ids"]
        assert docs and docs == sorted(docs) and len(docs) == len(set(docs)), \
            f"{where}: doc_ids must be non-empty, sorted, unique"
        unknown = set(docs) - id_spaces[p["dataset"]]
        assert not unknown, f"{where}: {len(unknown)} doc_ids not in raw data, e.g. {sorted(unknown)[:3]}"
        check_query(g["query_equivalent"], where)

    print(f"ok: {n} pairs pass schema + raw-data + DSL checks")
    if n < MIN_PAIRS:
        print(f"::error::ask-plan: {n} pairs < the {MIN_PAIRS}-pair gate")
        return 1

    r = subprocess.run([sys.executable, str(HERE / "derive_pairs.py"), "--check"],
                       capture_output=True, text=True)
    print(r.stdout.strip())
    if r.returncode != 0:
        print(r.stderr.strip())
        return 1
    print("verified inputs only — no engine was run, nothing was measured")
    return 0


if __name__ == "__main__":
    sys.exit(main())
