#!/usr/bin/env python3
"""What does a real landing hero contain? Measured over the reference homepages."""
import glob, os, re, statistics, sys
from lxml import html as LH

CTA_WORDS = re.compile(r"^(get |start|try |sign |book |talk |contact|request|download|"
                       r"install|deploy|create|build|read the docs|docs|see |view |learn |"
                       r"explore|watch|join|buy|pricing|free)", re.I)

def analyse(path):
    try: doc = LH.parse(path).getroot()
    except Exception: return None
    for b in doc.xpath("//script|//style|//noscript|//svg"):
        p=b.getparent()
        if p is not None: p.remove(b)
    h1s = doc.xpath("//h1")
    if not h1s: return None
    h1 = " ".join(h1s[0].text_content().split())
    if not h1 or len(h1.split()) > 40: return None
    # everything structurally after the h1, within its section/parent chain
    par = h1s[0].getparent()
    for _ in range(3):
        if par is None: break
        txt = " ".join(par.text_content().split())
        if len(txt.split()) > len(h1.split()) + 8: break
        par = par.getparent()
    hero = " ".join(par.text_content().split()) if par is not None else h1
    sub = hero[len(h1):].strip() if hero.startswith(h1) else hero
    sub_words = len(sub.split())
    # calls to action anywhere in the top of the document
    links = doc.xpath("//a")[:60] + doc.xpath("//button")[:20]
    ctas = [" ".join(a.text_content().split()) for a in links]
    ctas = [c for c in ctas if c and len(c.split()) <= 5 and CTA_WORDS.match(c)]
    return {
        "site": os.path.basename(path)[:-5],
        "h1": h1,
        "h1_words": len(h1.split()),
        "h1_chars": len(h1),
        "h1_has_number": bool(re.search(r"\d", h1)),
        "h1_sentences": h1.count(".") + h1.count("!") or 1,
        "sub_words": min(sub_words, 200),
        "ctas": len(dict.fromkeys(ctas)),
        "cta_examples": list(dict.fromkeys(ctas))[:3],
        "h2s": len(doc.xpath("//h2")),
        "hero_has_code": bool(doc.xpath("//h1/following::pre[position()<3]|//h1/following::code[position()<3]")),
    }

rows=[r for r in (analyse(p) for p in sorted(glob.glob(os.path.expanduser("~/.xerj-write/landing/*.html")))) if r]
print(f"{len(rows)} homepages with a usable <h1>\n")
def q(k):
    v=sorted(r[k] for r in rows); g=lambda f: v[min(int(len(v)*f),len(v)-1)]
    return g(.25), g(.5), g(.75)
for k in ("h1_words","h1_chars","sub_words","ctas","h2s"):
    a,b,c=q(k); print(f"  {k:<12} p25 {a:>5}   median {b:>5}   p75 {c:>5}")
print(f"\n  h1 contains a number:  {100*sum(r['h1_has_number'] for r in rows)/len(rows):.0f}%")
print(f"  h1 is >1 sentence:     {100*sum(r['h1_sentences']>1 for r in rows)/len(rows):.0f}%")
print(f"  code/pre near the h1:  {100*sum(r['hero_has_code'] for r in rows)/len(rows):.0f}%")
print("\n=== EVERY HEADLINE (the thing we are competing with)\n")
for r in sorted(rows, key=lambda r: r["h1_words"]):
    print(f"  {r['h1_words']:>2}w  [{r['site']:<14}] {r['h1'][:96]}")
print("\n=== CTA labels used")
import collections
c=collections.Counter(x.lower() for r in rows for x in r["cta_examples"])
print("  " + " · ".join(f"{k} ({n})" for k,n in c.most_common(14)))
