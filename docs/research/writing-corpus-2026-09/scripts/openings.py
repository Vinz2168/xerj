#!/usr/bin/env python3
"""How do the best posts open? Classify the first sentence of each article body."""
import glob, os, re, sys, collections, random
from lxml import html as LH
sys.path.insert(0, ".")
from analyze_structure import body_of, BOILER, words

def first_para(path):
    try: doc = LH.parse(path).getroot()
    except Exception: return None
    for b in doc.xpath(BOILER):
        p = b.getparent()
        if p is not None: p.remove(b)
    art = body_of(doc)
    if art is None: return None
    for p in art.xpath(".//p"):
        t = " ".join(p.text_content().split())
        if words(t) >= 8: return t
    return None

def classify(t):
    s = re.split(r"(?<=[.!?])\s", t)[0]
    low = s.lower()
    if s.rstrip().endswith("?"):                                    return "question"
    if re.match(r"^(today|we('re| are)|i('m| am)? ?(thrilled|excited|happy)|announcing|starting today)", low): return "announcement"
    if re.search(r"\b(last (week|month|year)|in 20\d\d|a few (weeks|months)|one (morning|night)|when i|i was|at \d)", low): return "scene / when-this-happened"
    if re.match(r"^(if you|when you|you('ve| have| are| want| need)|imagine|suppose|say you)", low):  return "second person / you"
    if re.search(r"\d", s) and words(s) < 30:                       return "number up front"
    if re.match(r"^\w[\w \-]{0,40}\bis\b", low) and words(s) < 25:  return "definition"
    if re.search(r"\b(problem|hard|difficult|fails?|broke|wrong|slow|painful|struggle|doesn't|can't)\b", low): return "problem statement"
    return "other / straight into it"

files = sorted(glob.glob(os.path.expanduser("~/.xerj-write/corpora/*/raw/*.html")))
rows = [(p, first_para(p)) for p in files]
rows = [(p, t) for p, t in rows if t]
c = collections.Counter(classify(t) for _, t in rows)
print(f"opening move, n={len(rows)} posts\n")
for k, n in c.most_common():
    print(f"  {100*n/len(rows):5.1f}%  {k}")

print(f"\nlead paragraph length: ", end="")
L = sorted(words(t) for _, t in rows)
print(f"p25 {L[len(L)//4]}w  median {L[len(L)//2]}w  p75 {L[3*len(L)//4]}w  "
      f"({100*sum(1 for x in L if x<=40)/len(L):.0f}% are <=40 words)")

print("\n=== SAMPLES (short, evidence-led openings — the pattern we want)\n")
rnd = random.Random(7)
cand = [(p, t) for p, t in rows if words(t) <= 45 and re.search(r"\d", t)]
for p, t in rnd.sample(cand, 10):
    print(f"  [{os.path.basename(p).split('-')[0]}] {t}\n")
