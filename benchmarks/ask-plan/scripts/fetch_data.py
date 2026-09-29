#!/usr/bin/env python3
"""fetch_data.py — (re)acquire the raw public datasets for benchmarks/ask-plan.

STATUS: DESIGN, NOTHING MEASURED. Fetching data is not a measurement; this
script records provenance (URL, sha256, size, retrieval date) and nothing else.

python3 stdlib only. Three modes:

  python3 scripts/fetch_data.py --check     default: verify the committed
                                            snapshots against provenance.json
                                            (sha256). Reports drift loudly but
                                            exits 0 — drift is EXPECTED (the
                                            exoplanet composite table refreshes
                                            monthly; USGS revises historical
                                            windows) and the committed snapshot
                                            stays canonical for gold derivation.
  python3 scripts/fetch_data.py --refresh   re-download into data/raw/<name>/,
                                            update provenance.json, and print a
                                            reminder to re-run derive_pairs.py.
  python3 scripts/fetch_data.py --dry-run   print the pinned URLs and exit.

The committed snapshots are the canonical input to scripts/derive_pairs.py;
--refresh intentionally changes the bytes under gold derivation and must be
followed by regenerating and re-reviewing data/gold/pairs.jsonl.
"""
import argparse
import datetime
import hashlib
import json
import pathlib
import sys
import urllib.request

HERE = pathlib.Path(__file__).resolve().parent
DATA = HERE.parent / "data"

# Pinned sources. Licence evidence for each lives in data/raw/<name>/LICENCE.
SOURCES = {
    "usgs-earthquakes": (
        "https://earthquake.usgs.gov/fdsnws/event/1/query?format=csv"
        "&starttime=2026-09-01&endtime=2026-09-08&minmagnitude=2.5&orderby=time-asc"
    ),
    "nasa-exoplanets": (
        "https://exoplanetarchive.ipac.caltech.edu/TAP/sync?query=SELECT%20TOP%203000%20"
        "pl_name,hostname,sy_snum,sy_pnum,discoverymethod,disc_year,pl_orbper,pl_rade,"
        "pl_bmasse,pl_eqt,st_teff,st_rad,st_mass,ra,dec%20FROM%20pscomppars%20"
        "ORDER%20BY%20pl_name%20ASC&format=csv"
    ),
    "gapminder": (
        "https://raw.githubusercontent.com/jennybc/gapminder/master/inst/extdata/gapminder.tsv"
    ),
}

FILENAMES = {
    "usgs-earthquakes": "usgs-earthquakes.csv",
    "nasa-exoplanets": "nasa-exoplanets.csv",
    "gapminder": "gapminder.tsv",
}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def download(url, dest):
    req = urllib.request.Request(url, headers={"User-Agent": "xerj-ask-plan-fetch/1.0"})
    with urllib.request.urlopen(req, timeout=180) as resp:
        body = resp.read()
    dest.write_bytes(body)
    return len(body)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--check", action="store_true", help="verify committed sha256 (default)")
    g.add_argument("--refresh", action="store_true", help="re-download and update provenance")
    g.add_argument("--dry-run", action="store_true", help="print pinned URLs")
    args = ap.parse_args()

    now = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    for name, url in SOURCES.items():
        d = DATA / "raw" / name
        f = d / FILENAMES[name]
        meta = d / "provenance.json"
        if args.dry_run:
            print(f"{name}: {url}")
            continue
        if args.refresh:
            d.mkdir(parents=True, exist_ok=True)
            try:
                size = download(url, f)
            except Exception as e:  # noqa: BLE001 — report and keep going
                print(f"REFRESH FAILED {name}: {e}")
                continue
            record = {
                "dataset": name,
                "url": url,
                "file": f.name,
                "sha256": sha256(f),
                "bytes": size,
                "fetched_utc": now,
                "licence": f"LICENCE (in this directory)",
            }
            meta.write_text(json.dumps(record, indent=1) + "\n")
            print(f"refreshed {name}: {size} bytes sha256={record['sha256'][:16]}…")
            print(f"  REMINDER: re-run scripts/derive_pairs.py and re-review data/gold/")
            continue
        # --check (default)
        if not f.exists() or not meta.exists():
            print(f"MISSING {name}: {f} or {meta} absent")
            sys.exit(2)
        rec = json.loads(meta.read_text())
        actual, recorded = sha256(f), rec["sha256"]
        if actual == recorded:
            print(f"ok       {name} sha256={actual[:16]}… (as fetched {rec['fetched_utc']})")
        else:
            print(f"DRIFT    {name}: committed={actual[:16]}… provenance={recorded[:16]}…")
            print("  upstream moved since the snapshot was taken; the COMMITTED bytes")
            print("  remain canonical for gold derivation. --refresh to adopt the new copy.")
    if not args.dry_run and not args.refresh:
        print("(drift, if any, is informational; nothing was measured)")


if __name__ == "__main__":
    main()
