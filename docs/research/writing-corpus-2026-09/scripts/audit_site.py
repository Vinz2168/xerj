#!/usr/bin/env python3
"""Audit the site for unreadable prose, AI-slop phrasing, and off-product claims.

Reports evidence with file:line so every finding can be checked, not argued about.
"""
import os, re, sys, json, collections
from lxml import html as LH

ROOT = sys.argv[1] if len(sys.argv) > 1 else "landing"

# Phrases that mark machine-sounding marketing prose. Each is a real tell, not a style opinion:
# they are the phrases that survive because a model likes them, not because they say anything.
SLOP = [
 "delve", "seamless", "seamlessly", "robust", "leverage", "leveraging", "unlock", "unlocks",
 "empower", "empowers", "in today's", "game-chang", "revolutioniz", "cutting-edge", "state-of-the-art",
 "harness the power", "elevate", "streamline", "streamlined", "effortless", "effortlessly",
 "unparalleled", "unlike anything", "paradigm", "synerg", "holistic", "best-in-class",
 "world-class", "next-generation", "supercharge", "turbocharge", "blazing", "lightning-fast",
 "at scale, effortlessly", "the future of", "reimagine", "transformative", "journey",
 "dive deep", "deep dive into", "it's not just", "whether you're", "that's where", "enter the",
 "imagine a world", "say goodbye", "look no further", "rest assured", "needless to say",
 "we're thrilled", "we're excited to announce", "proud to announce", "ushering in",
]
# Vague claims with no number attached — the project's own honesty rules forbid these.
VAGUE = ["blazing fast", "incredibly fast", "super fast", "massively", "dramatically faster",
         "orders of magnitude" , "virtually instant", "near-instant", "enterprise-grade",
         "military-grade", "bank-grade", "infinitely", "unlimited scale", "any scale"]

def sentences(t):
    return [s.strip() for s in re.split(r"(?<=[.!?])\s+", t) if s.strip()]

def visible_text(path):
    try:
        doc = LH.parse(path).getroot()
    except Exception:
        return ""
    for bad in doc.xpath("//script|//style|//noscript|//svg"):
        bad.getparent().remove(bad)
    return re.sub(r"[ \t\xa0]+", " ", doc.text_content())

def audit(root):
    pages, slop_hits, vague_hits, long_s, imgs = [], [], [], [], []
    for dirpath, _, files in os.walk(root):
        for f in sorted(files):
            p = os.path.join(dirpath, f)
            if f.endswith((".png", ".jpg", ".jpeg", ".webp", ".gif")):
                imgs.append((os.path.getsize(p), p)); continue
            if not f.endswith((".html", ".md", ".txt")): continue
            raw = open(p, encoding="utf-8", errors="replace").read()
            text = visible_text(p) if f.endswith(".html") else raw
            words = len(text.split())
            ss = sentences(text)
            longs = [s for s in ss if len(s.split()) > 40]
            avg = sum(len(s.split()) for s in ss) / max(len(ss), 1)
            pages.append((words, round(avg, 1), len(longs), p))
            low = raw.lower()
            for ph in SLOP:
                for m in re.finditer(r"\b" + re.escape(ph), low):
                    line = raw[:m.start()].count("\n") + 1
                    slop_hits.append((p, line, ph))
            for ph in VAGUE:
                for m in re.finditer(r"\b" + re.escape(ph), low):
                    line = raw[:m.start()].count("\n") + 1
                    vague_hits.append((p, line, ph))
            long_s += [(p, s[:150]) for s in longs]
    return pages, slop_hits, vague_hits, long_s, imgs

pages, slop, vague, longs, imgs = audit(ROOT)
print(f"=== {ROOT}: {len(pages)} text pages, {len(imgs)} images\n")

print("PAGES BY LENGTH (a landing page over ~1,200 words is not being read)")
for w, avg, nl, p in sorted(pages, reverse=True)[:12]:
    print(f"  {w:6} words  avg sentence {avg:5.1f}  {nl:3} over-40-word sentences  {p}")

print(f"\nREADABILITY: {sum(1 for w,a,n,p in pages if a>25)} pages with avg sentence >25 words "
      f"(plain-English target is 15-20); {len(longs)} sentences over 40 words")
for p, s in longs[:6]:
    print(f"  {p}\n     {s}...")

c = collections.Counter(ph for _, _, ph in slop)
print(f"\nSLOP PHRASES: {len(slop)} hits across {len(set(p for p,_,_ in slop))} files")
for ph, n in c.most_common(18):
    ex = next((f"{p}:{l}" for p, l, q in slop if q == ph), "")
    print(f"  {n:4}x {ph:28} e.g. {ex}")

cv = collections.Counter(ph for _, _, ph in vague)
print(f"\nUNQUANTIFIED CLAIMS: {len(vague)} hits (the project's own rule: every number traces to a run)")
for ph, n in cv.most_common(10):
    ex = next((f"{p}:{l}" for p, l, q in vague if q == ph), "")
    print(f"  {n:4}x {ph:28} e.g. {ex}")

print(f"\nIMAGES: {len(imgs)}, total {sum(s for s,_ in imgs)/1e6:.1f}MB")
for s, p in sorted(imgs, reverse=True)[:8]:
    print(f"  {s/1024:7.0f}KB  {p}")
big = [1 for s, _ in imgs if s > 300_000]
print(f"  {len(big)} images over 300KB — each one is a second of load on a phone")
