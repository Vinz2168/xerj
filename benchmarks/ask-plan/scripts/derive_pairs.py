#!/usr/bin/env python3
"""derive_pairs.py — build data/gold/pairs.jsonl for benchmarks/ask-plan.

STATUS: DESIGN, NOTHING MEASURED. This script produces evaluation *inputs*
(prompts and gold result sets), never results. Gold is derived from the
committed raw snapshots in data/raw/ by filtering the raw rows in Python —
no engine is run, at derivation time or ever, to produce gold. The
`query_equivalent` in each pair is the predicate the prompt names, written as
Elasticsearch-style DSL that the harness later EXECUTES as a fixture
self-check (its result set must reproduce gold doc_ids on a correct engine);
it is not itself gold and not an engine output.

python3 stdlib only. Deterministic: fixed template order, fixed value lists,
seeded RNG (SEED) used only to choose between equally-valid prompt phrasings.
Re-running on the same raw bytes produces byte-identical pairs.jsonl;
`--check` verifies exactly that and exits non-zero on drift.

Output — one JSON object per line:

  {"id": "...", "dataset": "...", "index_hint": "ax-...",
   "prompt": "...",
   "gold": {"index": "ax-...", "doc_ids": [...], "query_equivalent": {...}}}

  doc_ids           sorted, unique _ids of every raw row matching the
                    predicate (the loader assigns _id from the dataset's own
                    key column, so ids are data-derived, not row-order-derived)
  query_equivalent  the query object that would sit under "query" in a
                    _search body; leaves are term/range/bool only — the
                    shapes engine/crates/xerj-query/src/parser.rs accepts

Keeps a pair only when 1 <= len(gold doc_ids) <= MAX_GOLD (a gold of 0 or of
"everything" scores nothing interesting). If the raw snapshot ever changes
under a refresh, keep-counts change and the >= 200 assertion fails loudly
instead of silently shipping a thin gate.
"""
import argparse
import csv
import json
import pathlib
import random
import sys
from datetime import datetime, timezone

HERE = pathlib.Path(__file__).resolve().parent
DATA = HERE.parent / "data"
GOLD = DATA / "gold" / "pairs.jsonl"

SEED = 1056          # pinned; changing it changes prompt phrasings only
MIN_PAIRS = 200      # the #1056 gate: >= 200 (prompt, gold result set) pairs
MAX_GOLD = 1500      # bounded result sets: F1 is meaningful, size stays sane

rng = random.Random(SEED)


# ───────────────────────────── helpers ──────────────────────────────────────
def parse_iso(ts):
    """USGS/FDSN timestamps: 2026-09-01T01:29:11.420Z -> aware datetime."""
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def num(x):
    """Format a number for prompt text without pointless trailing zeros."""
    if isinstance(x, float) and x.is_integer():
        return str(int(x))
    return str(x)


def fnum(x):
    """Numeric bound as it should appear in the DSL (float/int)."""
    f = float(x)
    return int(f) if f.is_integer() else f


def pair(dataset, prompt, clauses, doc_ids):
    """Assemble one pair. clauses = list of DSL leaf clauses (ANDed)."""
    q = clauses[0] if len(clauses) == 1 else {"bool": {"filter": clauses}}
    return {
        "id": "",  # assigned sequentially per dataset once ordering is fixed
        "dataset": dataset,
        "index_hint": f"ax-{dataset}",
        "prompt": prompt,
        "gold": {
            "index": f"ax-{dataset}",
            "doc_ids": sorted(doc_ids),
            "query_equivalent": q,
        },
    }


def pick(*variants):
    """Deterministic phrasing choice among equally-correct prompt wordings."""
    return rng.choice(variants)


# ────────────────────────── dataset: usgs-earthquakes ───────────────────────
def usgs_pairs():
    rows = list(csv.DictReader(open(DATA / "raw" / "usgs-earthquakes" / "usgs-earthquakes.csv")))
    for r in rows:
        r["_id"] = r["id"]
        r["_mag"] = float(r["mag"])
        r["_depth"] = float(r["depth"])
        r["_t"] = parse_iso(r["time"])
    out = []

    def ids(pred):
        return [r["_id"] for r in rows if pred(r)]

    # magnitude thresholds
    for t in [2.75, 3.0, 3.25, 3.5, 3.75, 4.0, 4.25, 4.5, 5.0, 5.5, 6.0]:
        out.append(pair("usgs-earthquakes",
                        pick(f"all events with magnitude {num(t)} or greater",
                             f"events of magnitude {num(t)} and above"),
                        [{"range": {"mag": {"gte": fnum(t)}}}],
                        ids(lambda r, t=t: r["_mag"] >= t)))
    for t in [3.5, 4.0, 5.0]:
        out.append(pair("usgs-earthquakes",
                        f"events strictly stronger than magnitude {num(t)}",
                        [{"range": {"mag": {"gt": fnum(t)}}}],
                        ids(lambda r, t=t: r["_mag"] > t)))
    for a, b in [(3.0, 4.0), (3.5, 4.5), (4.0, 5.0)]:
        out.append(pair("usgs-earthquakes",
                        pick(f"events between magnitude {num(a)} and {num(b)}",
                             f"events with magnitude from {num(a)} to {num(b)}"),
                        [{"range": {"mag": {"gte": fnum(a), "lte": fnum(b)}}}],
                        ids(lambda r, a=a, b=b: a <= r["_mag"] <= b)))
    for t in [4.0, 4.5, 5.0]:
        out.append(pair("usgs-earthquakes",
                        f"events with magnitude exactly {num(t)}",
                        [{"term": {"mag": fnum(t)}}],
                        ids(lambda r, t=t: r["_mag"] == t)))

    # depth
    for t in [35, 50, 100, 150, 200, 250, 350, 400, 500]:
        out.append(pair("usgs-earthquakes",
                        pick(f"events deeper than {t} km",
                             f"quakes at depths greater than {t} kilometers"),
                        [{"range": {"depth": {"gt": t}}}],
                        ids(lambda r, t=t: r["_depth"] > t)))
    for t in [5, 10, 20, 35, 50]:
        out.append(pair("usgs-earthquakes",
                        f"events shallower than {t} km",
                        [{"range": {"depth": {"lt": t}}}],
                        ids(lambda r, t=t: r["_depth"] < t)))

    # keyword equality: magType and net, swept over every distinct value with a
    # bounded count in the snapshot (data-driven, hence deterministic; a value
    # absent from the window simply never appears)
    from collections import Counter
    for field in ("magType", "net"):
        for v, c in sorted(Counter(r[field] for r in rows).items()):
            if not (2 <= c <= MAX_GOLD):
                continue
            if field == "magType":
                pr = pick(f"events whose magnitude type is {v}",
                          f"events with magnitude type {v}")
            else:
                pr = pick(f"events reported by the {v} network",
                          f"events from the {v} seismic network")
            out.append(pair("usgs-earthquakes", pr,
                            [{"term": {field: v}}],
                            ids(lambda r, f=field, v=v: r[f] == v)))

    # day windows and multi-day windows (UTC)
    for d in range(1, 8):
        day = f"2026-09-{d:02d}"
        lo = datetime.fromisoformat(f"{day}T00:00:00+00:00")
        hi = datetime.fromisoformat(f"2026-09-{d + 1:02d}T00:00:00+00:00")
        out.append(pair("usgs-earthquakes",
                        pick(f"events on {day} (UTC)",
                             f"all events that happened on {day} UTC"),
                        [{"range": {"time": {"gte": f"{day}T00:00:00Z",
                                             "lt": f"2026-09-{d + 1:02d}T00:00:00Z"}}}],
                        ids(lambda r, lo=lo, hi=hi: lo <= r["_t"] < hi)))
    for a, b in [(1, 3), (3, 5), (5, 7)]:
        lo = datetime.fromisoformat(f"2026-09-{a:02d}T00:00:00+00:00")
        hi = datetime.fromisoformat(f"2026-09-{b:02d}T00:00:00+00:00")
        out.append(pair("usgs-earthquakes",
                        f"events from 2026-09-{a:02d} through 2026-09-{b:02d} UTC",
                        [{"range": {"time": {"gte": f"2026-09-{a:02d}T00:00:00Z",
                                             "lt": f"2026-09-{b:02d}T00:00:00Z"}}}],
                        ids(lambda r, lo=lo, hi=hi: lo <= r["_t"] < hi)))

    # combinations (AND of two predicates)
    combos = [
        ("events from the ak network deeper than 50 km",
         [{"term": {"net": "ak"}}, {"range": {"depth": {"gt": 50}}}],
         lambda r: r["net"] == "ak" and r["_depth"] > 50),
        ("us network events deeper than 300 km",
         [{"term": {"net": "us"}}, {"range": {"depth": {"gt": 300}}}],
         lambda r: r["net"] == "us" and r["_depth"] > 300),
        ("ml events with magnitude 4.0 or greater",
         [{"term": {"magType": "ml"}}, {"range": {"mag": {"gte": 4.0}}}],
         lambda r: r["magType"] == "ml" and r["_mag"] >= 4.0),
        ("mb events with magnitude 5.0 or greater",
         [{"term": {"magType": "mb"}}, {"range": {"mag": {"gte": 5.0}}}],
         lambda r: r["magType"] == "mb" and r["_mag"] >= 5.0),
        ("events on 2026-09-04 UTC with magnitude 4.0 or greater",
         [{"range": {"time": {"gte": "2026-09-04T00:00:00Z", "lt": "2026-09-05T00:00:00Z"}}},
          {"range": {"mag": {"gte": 4.0}}}],
         lambda r: r["_t"].strftime("%Y-%m-%d") == "2026-09-04" and r["_mag"] >= 4.0),
        ("ak network events with magnitude 3.5 or greater",
         [{"term": {"net": "ak"}}, {"range": {"mag": {"gte": 3.5}}}],
         lambda r: r["net"] == "ak" and r["_mag"] >= 3.5),
        ("deep events (over 400 km) with magnitude 4.5 or greater",
         [{"range": {"depth": {"gt": 400}}}, {"range": {"mag": {"gte": 4.5}}}],
         lambda r: r["_depth"] > 400 and r["_mag"] >= 4.5),
        ("mww events deeper than 100 km",
         [{"term": {"magType": "mww"}}, {"range": {"depth": {"gt": 100}}}],
         lambda r: r["magType"] == "mww" and r["_depth"] > 100),
        ("pr network events with magnitude 3.5 or greater",
         [{"term": {"net": "pr"}}, {"range": {"mag": {"gte": 3.5}}}],
         lambda r: r["net"] == "pr" and r["_mag"] >= 3.5),
        ("hv network events with magnitude 3.0 or greater",
         [{"term": {"net": "hv"}}, {"range": {"mag": {"gte": 3.0}}}],
         lambda r: r["net"] == "hv" and r["_mag"] >= 3.0),
        ("tx network events shallower than 10 km",
         [{"term": {"net": "tx"}}, {"range": {"depth": {"lt": 10}}}],
         lambda r: r["net"] == "tx" and r["_depth"] < 10),
        ("events during 2026-09-05 UTC with depth greater than 100 km",
         [{"range": {"time": {"gte": "2026-09-05T00:00:00Z", "lt": "2026-09-06T00:00:00Z"}}},
          {"range": {"depth": {"gt": 100}}}],
         lambda r: r["_t"].strftime("%Y-%m-%d") == "2026-09-05" and r["_depth"] > 100),
        ("mb events shallower than 35 km",
         [{"term": {"magType": "mb"}}, {"range": {"depth": {"lt": 35}}}],
         lambda r: r["magType"] == "mb" and r["_depth"] < 35),
    ]
    for prompt, clauses, pred in combos:
        out.append(pair("usgs-earthquakes", prompt, clauses, ids(pred)))

    return [p for p in out if 1 <= len(p["gold"]["doc_ids"]) <= MAX_GOLD]


# ────────────────────────── dataset: nasa-exoplanets ────────────────────────
def exo_pairs():
    rows = list(csv.DictReader(open(DATA / "raw" / "nasa-exoplanets" / "nasa-exoplanets.csv")))
    for r in rows:
        r["_id"] = r["pl_name"]
        r["_year"] = int(r["disc_year"])
        r["_rade"] = float(r["pl_rade"]) if r["pl_rade"] else None
        r["_per"] = float(r["pl_orbper"]) if r["pl_orbper"] else None
        r["_snum"] = int(r["sy_snum"])
        r["_pnum"] = int(r["sy_pnum"])
        r["_teff"] = float(r["st_teff"]) if r["st_teff"] else None
    out = []

    def ids(pred):
        return [r["_id"] for r in rows if pred(r)]

    # discovery method (term)
    methods = ["Transit", "Radial Velocity", "Microlensing", "Imaging",
               "Transit Timing Variations", "Eclipse Timing Variations"]
    for m in methods:
        out.append(pair("nasa-exoplanets",
                        pick(f"planets discovered by the {m} method".replace("by the Imaging", "by direct"),
                             f"planets found via {m}"),
                        [{"term": {"discoverymethod": m}}],
                        ids(lambda r, m=m: r["discoverymethod"] == m)))

    # discovery year (term / gte / lte) — every year present in the snapshot
    years = sorted({r["_year"] for r in rows})
    for y in years:
        out.append(pair("nasa-exoplanets",
                        pick(f"planets discovered in {y}",
                             f"planets first observed in {y}"),
                        [{"term": {"disc_year": y}}],
                        ids(lambda r, y=y: r["_year"] == y)))
    for y in [2018, 2020, 2022, 2024]:
        out.append(pair("nasa-exoplanets",
                        f"planets discovered in {y} or later",
                        [{"range": {"disc_year": {"gte": y}}}],
                        ids(lambda r, y=y: r["_year"] >= y)))
    for y in [1995, 2005, 2015]:
        out.append(pair("nasa-exoplanets",
                        f"planets discovered in {y} or earlier",
                        [{"range": {"disc_year": {"lte": y}}}],
                        ids(lambda r, y=y: r["_year"] <= y)))
    out.append(pair("nasa-exoplanets",
                    "planets discovered between 2010 and 2015 inclusive",
                    [{"range": {"disc_year": {"gte": 2010, "lte": 2015}}}],
                    ids(lambda r: 2010 <= r["_year"] <= 2015)))

    # system shape (sy_snum / sy_pnum)
    for n in [1, 2, 3]:
        word = {1: "single-star", 2: "binary", 3: "triple"}[n]
        out.append(pair("nasa-exoplanets",
                        pick(f"planets in {word} systems",
                             f"planets orbiting hosts with {n} star" + ("" if n == 1 else "s")),
                        [{"term": {"sy_snum": n}}],
                        ids(lambda r, n=n: r["_snum"] == n)))
    for n in [2, 3, 4, 5]:
        out.append(pair("nasa-exoplanets",
                        f"systems with at least {n} known planets",
                        [{"range": {"sy_pnum": {"gte": n}}}],
                        ids(lambda r, n=n: r["_pnum"] >= n)))
    for n in [1, 2, 3, 4]:
        out.append(pair("nasa-exoplanets",
                        f"systems with exactly {n} known planets",
                        [{"term": {"sy_pnum": n}}],
                        ids(lambda r, n=n: r["_pnum"] == n)))

    # radius / period / host temperature (numeric; blank values never match)
    for t in [4, 8, 12]:
        out.append(pair("nasa-exoplanets",
                        f"planets larger than {t} Earth radii",
                        [{"range": {"pl_rade": {"gt": t}}}],
                        ids(lambda r, t=t: r["_rade"] is not None and r["_rade"] > t)))
    for t in [1.5, 2]:
        out.append(pair("nasa-exoplanets",
                        f"planets smaller than {num(t)} Earth radii",
                        [{"range": {"pl_rade": {"lt": fnum(t)}}}],
                        ids(lambda r, t=t: r["_rade"] is not None and r["_rade"] < t)))
    for t in [1, 10, 100]:
        out.append(pair("nasa-exoplanets",
                        f"planets with orbital periods shorter than {t} days",
                        [{"range": {"pl_orbper": {"lt": t}}}],
                        ids(lambda r, t=t: r["_per"] is not None and r["_per"] < t)))
    for t in [100, 1000]:
        out.append(pair("nasa-exoplanets",
                        f"planets with orbital periods longer than {t} days",
                        [{"range": {"pl_orbper": {"gt": t}}}],
                        ids(lambda r, t=t: r["_per"] is not None and r["_per"] > t)))
    out.append(pair("nasa-exoplanets",
                    "planets with orbital periods between 10 and 100 days",
                    [{"range": {"pl_orbper": {"gte": 10, "lte": 100}}}],
                    ids(lambda r: r["_per"] is not None and 10 <= r["_per"] <= 100)))
    for t in [6000, 7500]:
        out.append(pair("nasa-exoplanets",
                        f"planets around host stars hotter than {t} K",
                        [{"range": {"st_teff": {"gt": t}}}],
                        ids(lambda r, t=t: r["_teff"] is not None and r["_teff"] > t)))

    # combinations
    combos = [
        ("transit planets discovered in 2020 or later",
         [{"term": {"discoverymethod": "Transit"}}, {"range": {"disc_year": {"gte": 2020}}}],
         lambda r: r["discoverymethod"] == "Transit" and r["_year"] >= 2020),
        ("radial-velocity planets larger than 8 Earth radii",
         [{"term": {"discoverymethod": "Radial Velocity"}}, {"range": {"pl_rade": {"gt": 8}}}],
         lambda r: r["discoverymethod"] == "Radial Velocity" and r["_rade"] is not None and r["_rade"] > 8),
        ("systems with at least 3 planets discovered in 2016",
         [{"range": {"sy_pnum": {"gte": 3}}}, {"term": {"disc_year": 2016}}],
         lambda r: r["_pnum"] >= 3 and r["_year"] == 2016),
        ("planets in binary systems found by imaging",
         [{"term": {"sy_snum": 2}}, {"term": {"discoverymethod": "Imaging"}}],
         lambda r: r["_snum"] == 2 and r["discoverymethod"] == "Imaging"),
        ("planets with periods under 10 days and radii under 2 Earth radii",
         [{"range": {"pl_orbper": {"lt": 10}}}, {"range": {"pl_rade": {"lt": 2}}}],
         lambda r: r["_per"] is not None and r["_per"] < 10 and r["_rade"] is not None and r["_rade"] < 2),
        ("planets discovered in 2016 or later in triple-star systems",
         [{"range": {"disc_year": {"gte": 2016}}}, {"term": {"sy_snum": 3}}],
         lambda r: r["_year"] >= 2016 and r["_snum"] == 3),
        ("microlensing planets discovered before 2010",
         [{"term": {"discoverymethod": "Microlensing"}}, {"range": {"disc_year": {"lt": 2010}}}],
         lambda r: r["discoverymethod"] == "Microlensing" and r["_year"] < 2010),
        ("transit planets with periods shorter than 3 days",
         [{"term": {"discoverymethod": "Transit"}}, {"range": {"pl_orbper": {"lt": 3}}}],
         lambda r: r["discoverymethod"] == "Transit" and r["_per"] is not None and r["_per"] < 3),
        ("planets around host stars hotter than 6000 K discovered in 2020 or later",
         [{"range": {"st_teff": {"gt": 6000}}}, {"range": {"disc_year": {"gte": 2020}}}],
         lambda r: r["_teff"] is not None and r["_teff"] > 6000 and r["_year"] >= 2020),
        ("planets in multi-planet systems discovered by transit",
         [{"range": {"sy_pnum": {"gte": 2}}}, {"term": {"discoverymethod": "Transit"}}],
         lambda r: r["_pnum"] >= 2 and r["discoverymethod"] == "Transit"),
        ("planets larger than 10 Earth radii with periods longer than 100 days",
         [{"range": {"pl_rade": {"gt": 10}}}, {"range": {"pl_orbper": {"gt": 100}}}],
         lambda r: r["_rade"] is not None and r["_rade"] > 10 and r["_per"] is not None and r["_per"] > 100),
        ("planets discovered in 2021 or later in binary systems",
         [{"range": {"disc_year": {"gte": 2021}}}, {"term": {"sy_snum": 2}}],
         lambda r: r["_year"] >= 2021 and r["_snum"] == 2),
    ]
    for prompt, clauses, pred in combos:
        out.append(pair("nasa-exoplanets", prompt, clauses, ids(pred)))

    return [p for p in out if 1 <= len(p["gold"]["doc_ids"]) <= MAX_GOLD]


# ───────────────────────────── dataset: gapminder ───────────────────────────
def gap_pairs():
    rows = list(csv.DictReader(open(DATA / "raw" / "gapminder" / "gapminder.tsv"), delimiter="\t"))
    for r in rows:
        r["_id"] = f"{r['country']}-{r['year']}"
        r["_year"] = int(r["year"])
        r["_life"] = float(r["lifeExp"])
        r["_pop"] = int(r["pop"])
        r["_gdp"] = float(r["gdpPercap"])
    out = []

    def ids(pred):
        return [r["_id"] for r in rows if pred(r)]

    # continent (term)
    for c in ["Africa", "Americas", "Asia", "Europe", "Oceania"]:
        out.append(pair("gapminder",
                        pick(f"all records for countries in {c}",
                             f"every record from {c}"),
                        [{"term": {"continent": c}}],
                        ids(lambda r, c=c: r["continent"] == c)))

    # year (term / gte / lte)
    for y in range(1952, 2008, 5):
        out.append(pair("gapminder",
                        f"all records from {y}",
                        [{"term": {"year": y}}],
                        ids(lambda r, y=y: r["_year"] == y)))
    for y in [1982, 1997, 2002]:
        out.append(pair("gapminder",
                        f"all records from {y} onward",
                        [{"range": {"year": {"gte": y}}}],
                        ids(lambda r, y=y: r["_year"] >= y)))
    out.append(pair("gapminder",
                    "all records up to and including 1982",
                    [{"range": {"year": {"lte": 1982}}}],
                    ids(lambda r: r["_year"] <= 1982)))

    # country (term on the country key)
    for c in ["Japan", "Brazil", "France", "Kenya", "Canada", "Egypt",
              "United States", "China", "India", "Germany", "Mexico", "Australia"]:
        out.append(pair("gapminder",
                        f"all records for {c}",
                        [{"term": {"country": c}}],
                        ids(lambda r, c=c: r["country"] == c)))

    # life expectancy at chosen years (thresholds chosen for set-size spread)
    for y in [1952, 1967, 1982, 1997, 2007]:
        for t in [40, 60, 70]:
            out.append(pair("gapminder",
                            pick(f"countries with life expectancy over {t} in {y}",
                                 f"in {y}, countries where life expectancy exceeded {t} years"),
                            [{"term": {"year": y}}, {"range": {"lifeExp": {"gt": t}}}],
                            ids(lambda r, y=y, t=t: r["_year"] == y and r["_life"] > t)))
    for y, t in [(1997, 75), (2007, 75), (2007, 80)]:
        out.append(pair("gapminder",
                        f"countries with life expectancy over {t} in {y}",
                        [{"term": {"year": y}}, {"range": {"lifeExp": {"gt": t}}}],
                        ids(lambda r, y=y, t=t: r["_year"] == y and r["_life"] > t)))
    for y in [1952, 2007]:
        out.append(pair("gapminder",
                        f"in {y}, countries where life expectancy was below 45 years",
                        [{"term": {"year": y}}, {"range": {"lifeExp": {"lt": 45}}}],
                        ids(lambda r, y=y: r["_year"] == y and r["_life"] < 45)))
    out.append(pair("gapminder",
                    "countries with life expectancy between 60 and 70 in 2007",
                    [{"term": {"year": 2007}}, {"range": {"lifeExp": {"gte": 60, "lte": 70}}}],
                    ids(lambda r: r["_year"] == 2007 and 60 <= r["_life"] <= 70)))

    # population
    for y, t in [(2007, 5_000_000), (2007, 10_000_000), (2007, 50_000_000), (2007, 100_000_000), (2007, 300_000_000),
                 (1982, 10_000_000), (1982, 100_000_000), (1997, 100_000_000), (1967, 50_000_000)]:
        out.append(pair("gapminder",
                        f"countries with population above {t} in {y}",
                        [{"term": {"year": y}}, {"range": {"pop": {"gt": t}}}],
                        ids(lambda r, y=y, t=t: r["_year"] == y and r["_pop"] > t)))

    # GDP per capita
    for y, t in [(2007, 5000), (2007, 10000), (2007, 20000), (2007, 40000),
                 (1992, 10000), (1982, 5000), (1977, 5000)]:
        out.append(pair("gapminder",
                        f"countries with GDP per capita above {t} in {y}",
                        [{"term": {"year": y}}, {"range": {"gdpPercap": {"gt": t}}}],
                        ids(lambda r, y=y, t=t: r["_year"] == y and r["_gdp"] > t)))
    out.append(pair("gapminder",
                    "countries with GDP per capita below 1000 in 1952",
                    [{"term": {"year": 1952}}, {"range": {"gdpPercap": {"lt": 1000}}}],
                    ids(lambda r: r["_year"] == 1952 and r["_gdp"] < 1000)))

    # combinations across two non-year axes
    combos = [
        ("European countries with life expectancy over 78 in 2007",
         [{"term": {"continent": "Europe"}}, {"term": {"year": 2007}}, {"range": {"lifeExp": {"gt": 78}}}],
         lambda r: r["continent"] == "Europe" and r["_year"] == 2007 and r["_life"] > 78),
        ("African countries with life expectancy over 60 in 2007",
         [{"term": {"continent": "Africa"}}, {"term": {"year": 2007}}, {"range": {"lifeExp": {"gt": 60}}}],
         lambda r: r["continent"] == "Africa" and r["_year"] == 2007 and r["_life"] > 60),
        ("Asian countries with population above 100 million in 2007",
         [{"term": {"continent": "Asia"}}, {"term": {"year": 2007}}, {"range": {"pop": {"gt": 100_000_000}}}],
         lambda r: r["continent"] == "Asia" and r["_year"] == 2007 and r["_pop"] > 100_000_000),
        ("European records from 1982",
         [{"term": {"continent": "Europe"}}, {"term": {"year": 1982}}],
         lambda r: r["continent"] == "Europe" and r["_year"] == 1982),
        ("countries in the Americas with GDP per capita above 20000 in 2007",
         [{"term": {"continent": "Americas"}}, {"term": {"year": 2007}}, {"range": {"gdpPercap": {"gt": 20000}}}],
         lambda r: r["continent"] == "Americas" and r["_year"] == 2007 and r["_gdp"] > 20000),
        ("Oceania records with life expectancy below 70",
         [{"term": {"continent": "Oceania"}}, {"range": {"lifeExp": {"lt": 70}}}],
         lambda r: r["continent"] == "Oceania" and r["_life"] < 70),
        ("Asian records from 1952",
         [{"term": {"continent": "Asia"}}, {"term": {"year": 1952}}],
         lambda r: r["continent"] == "Asia" and r["_year"] == 1952),
        ("African records from 1997",
         [{"term": {"continent": "Africa"}}, {"term": {"year": 1997}}],
         lambda r: r["continent"] == "Africa" and r["_year"] == 1997),
        ("countries in the Americas from 2007 with population above 10 million",
         [{"term": {"continent": "Americas"}}, {"term": {"year": 2007}}, {"range": {"pop": {"gt": 10_000_000}}}],
         lambda r: r["continent"] == "Americas" and r["_year"] == 2007 and r["_pop"] > 10_000_000),
    ]
    for prompt, clauses, pred in combos:
        out.append(pair("gapminder", prompt, clauses, ids(pred)))

    return [p for p in out if 1 <= len(p["gold"]["doc_ids"]) <= MAX_GOLD]


# ─────────────────────────────── assemble ───────────────────────────────────
def build():
    built = []
    for name, fn in [("usgs-earthquakes", usgs_pairs),
                     ("nasa-exoplanets", exo_pairs),
                     ("gapminder", gap_pairs)]:
        pairs = fn()
        for i, p in enumerate(pairs, 1):
            p["id"] = f"{name}-{i:03d}"
        built.extend(pairs)
    return built


def render(pairs):
    return "".join(json.dumps(p, sort_keys=False) + "\n" for p in pairs)


def main():
    ap = argparse.ArgumentParser(description="derive gold pairs from raw snapshots")
    ap.add_argument("--check", action="store_true",
                    help="regenerate in memory and compare to the committed file")
    args = ap.parse_args()

    pairs = build()
    counts = {}
    for p in pairs:
        counts[p["dataset"]] = counts.get(p["dataset"], 0) + 1

    if args.check:
        on_disk = GOLD.read_text() if GOLD.exists() else ""
        if render(pairs) == on_disk:
            print(f"ok: {GOLD} is byte-identical to the derivation ({len(pairs)} pairs)")
            return 0
        print("DRIFT: pairs.jsonl differs from what derive_pairs.py produces from the")
        print("current raw snapshots. Regenerate (run me without --check) and re-review.")
        return 1

    total = len(pairs)
    print(f"derived {total} pairs: " + ", ".join(f"{k}={v}" for k, v in sorted(counts.items())))
    if total < MIN_PAIRS:
        print(f"::error::ask-plan: {total} pairs < the {MIN_PAIRS}-pair gate — refusing to write")
        return 1
    GOLD.parent.mkdir(parents=True, exist_ok=True)
    GOLD.write_text(render(pairs))
    sizes = sorted(len(p["gold"]["doc_ids"]) for p in pairs)
    print(f"wrote {GOLD}")
    print(f"gold set sizes: min={sizes[0]} median={sizes[len(sizes) // 2]} max={sizes[-1]}")
    print("gold derived from raw rows only; no engine was run to produce it")
    return 0


if __name__ == "__main__":
    sys.exit(main())
