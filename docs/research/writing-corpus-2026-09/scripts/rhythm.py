#!/usr/bin/env python3
"""Rhythm and reflex metrics: the tells that survive a slop-word blocklist.

Burstiness (stdev of sentence length) is the one that matters. Human prose
varies its beat; generated prose settles into one. The rest are constructions a
model reaches for because they sound resolved, not because they say anything.
"""
import glob, re, statistics, sys, json
from lxml import html as LH
sys.path.insert(0, ".")
from analyze_structure import body_of, BOILER, sentences, words

NEG_FLIP = re.compile(r"\b(is|are|isn't|aren't|was|were|it's|that's)\s+not\s+(just\s+)?\w", re.I)
NOT_BUT  = re.compile(r"\bnot\s+[^.;]{3,40}\s+but\s+", re.I)
TRIAD    = re.compile(r"\b\w+,\s+\w+,?\s+and\s+\w+\b")
OPENERS  = ("the ", "it ", "this ", "these ", "with ", "by ")

def rhythm(path):
    try: doc = LH.parse(path).getroot()
    except Exception: return None
    for b in doc.xpath(BOILER):
        p = b.getparent()
        if p is not None: p.remove(b)
    art = body_of(doc)
    if art is None: return None
    prose = "\n".join(p.text_content() for p in art.xpath(".//p") if words(p.text_content()) >= 5)
    s = sentences(prose)
    if len(s) < 12: return None
    L = [words(x) for x in s]
    n = len(prose.split())
    return {
        "path": path, "words": n, "sents": len(s),
        "wps_mean": round(statistics.mean(L), 1),
        "burstiness": round(statistics.pstdev(L), 1),                 # the headline
        "burst_ratio": round(statistics.pstdev(L) / max(statistics.mean(L), 1), 3),
        "same_opener_pct": round(100 * sum(1 for x in s if x.lower().startswith(OPENERS)) / len(s)),
        "negflip_per_1kw": round(1000 * len(NEG_FLIP.findall(prose)) / n, 2),
        "notbut_per_1kw": round(1000 * len(NOT_BUT.findall(prose)) / n, 2),
        "triad_per_1kw": round(1000 * len(TRIAD.findall(prose)) / n, 2),
        "colon_per_1kw": round(1000 * prose.count(":") / n, 2),
    }

KEYS = ["wps_mean","burstiness","burst_ratio","same_opener_pct","negflip_per_1kw",
        "notbut_per_1kw","triad_per_1kw","colon_per_1kw"]

def run(files, label):
    rows = [r for r in (rhythm(p) for p in files) if r]
    print(f"\n=== {label}  n={len(rows)}")
    print(f"{'metric':<20}{'p25':>9}{'median':>9}{'p75':>9}")
    out = {}
    for k in KEYS:
        v = sorted(r[k] for r in rows)
        g = lambda f: v[min(int(len(v)*f), len(v)-1)]
        out[k] = (g(.25), g(.5), g(.75))
        print(f"{k:<20}{g(.25):>9}{g(.5):>9}{g(.75):>9}")
    return rows, out

if __name__ == "__main__":
  ref, refq = run(sorted(glob.glob(__import__("os").path.expanduser("~/.xerj-write/corpora/*/raw/*.html"))),
                "REFERENCE")
  ours, oursq = run(sorted(glob.glob("/home/claude/ai/xerj/landing/**/*.html", recursive=True)), "XERJ SITE")

  print("\n=== OUR PAGES WITH THE FLATTEST RHYTHM (low burst_ratio = one beat, every sentence)")
  for r in sorted(ours, key=lambda r: r["burst_ratio"])[:12]:
    print(f"  {r['burst_ratio']:.3f}  (ref median {refq['burst_ratio'][1]:.3f})  "
          f"mean {r['wps_mean']:>4} sd {r['burstiness']:>4}  {r['words']:>5}w  {r['path']}")
  json.dump({"ref": refq, "ours": oursq, "ours_rows": ours}, open("rhythm.json","w"), indent=1)
