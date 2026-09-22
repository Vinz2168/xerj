#!/usr/bin/env python3
"""Measure our copy against the reference corpus on the same dimensions.

The point is not to imitate anyone's voice. It is to find where our pages sit
outside the range that every market leader writes inside — those are the places
a reader stalls.
"""
import os, re, sys, glob, statistics
from lxml import html as LH

def prose_from_html(path):
    doc = LH.parse(path).getroot()
    for bad in doc.xpath("//script|//style|//noscript|//svg|//nav|//footer|//header|//code|//pre|//table"):
        par = bad.getparent()
        if par is not None: par.remove(bad)
    # only real paragraphs — headings and nav are not prose
    ps = [p.text_content() for p in doc.xpath("//p")]
    return "\n".join(ps)

def prose_from_md(path):
    t = open(path, encoding="utf-8", errors="replace").read()
    t = re.sub(r"^---.*?---", "", t, flags=re.S)          # front matter
    t = re.sub(r"```.*?```", " ", t, flags=re.S)           # code blocks
    return t

def metrics(text):
    text = re.sub(r"[ \t\xa0]+", " ", text)
    sents = [s.strip() for s in re.split(r"(?<=[.!?])\s+", text)
             if len(s.split()) >= 4 and re.search(r"[a-z]{3}", s)]
    if len(sents) < 5: return None
    words = text.split()
    n = len(words)
    wps = [len(s.split()) for s in sents]
    digits = len(re.findall(r"(?<![\w-])\d[\d.,]*", text))
    return {
        "words": n,
        "wps_median": statistics.median(wps),
        "wps_mean": round(statistics.mean(wps), 1),
        "pct_over_30": round(100 * sum(1 for w in wps if w > 30) / len(wps), 1),
        "nums_per_100w": round(100 * digits / n, 2),
        "emdash_per_1kw": round(1000 * text.count("—") / n, 1),
        "parens_per_1kw": round(1000 * text.count("(") / n, 1),
        "semicolon_per_1kw": round(1000 * text.count(";") / n, 1),
    }

def gather(paths, reader):
    out = []
    for p in paths:
        try:
            m = metrics(reader(p))
        except Exception:
            continue
        if m: out.append((p, m))
    return out

KEYS = ["wps_median", "wps_mean", "pct_over_30", "nums_per_100w", "emdash_per_1kw",
        "parens_per_1kw", "semicolon_per_1kw"]

def summarise(name, rows):
    print(f"\n=== {name}: {len(rows)} documents, {sum(r[1]['words'] for r in rows):,} words")
    print(f"{'metric':<20}{'p25':>9}{'median':>9}{'p75':>9}")
    agg = {}
    for k in KEYS:
        vals = sorted(r[1][k] for r in rows)
        q = lambda f: vals[min(int(len(vals) * f), len(vals) - 1)]
        agg[k] = (q(.25), q(.5), q(.75))
        print(f"{k:<20}{q(.25):>9}{q(.5):>9}{q(.75):>9}")
    return agg

if __name__ != "__main__":
    import sys as _s; _s.exit if False else None

if __name__ == "__main__":
  corpus = gather(sorted(glob.glob(os.path.expanduser("~/.xerj-write/corpora/*/text/*.md"))), prose_from_md)
  ours_paths = [p for p in sorted(glob.glob(sys.argv[1] + "/**/*.html", recursive=True))
              if "/answers/" not in p or True]
  ours = gather(ours_paths, prose_from_html)
  ref = summarise("REFERENCE CORPUS (market leaders)", corpus)
  mine = summarise("XERJ SITE", ours)

  print("\n=== PAGES FURTHEST OUTSIDE THE REFERENCE RANGE")
  print("(flag = metric above the corpus p75; these are where a reader stalls)\n")
  scored = []
  for p, m in ours:
    if m["words"] < 150: continue
    flags = []
    for k in KEYS:
        if m[k] > ref[k][2] * 1.15:
            flags.append(f"{k} {m[k]} (ref p75 {ref[k][2]})")
    if flags: scored.append((len(flags), m["words"], p, flags))
  for nf, w, p, flags in sorted(scored, reverse=True)[:14]:
    print(f"  {p}  ({w} words)")
    for f in flags: print(f"      {f}")
