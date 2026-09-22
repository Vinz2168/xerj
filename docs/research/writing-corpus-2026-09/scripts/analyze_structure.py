#!/usr/bin/env python3
"""How are the best posts actually built?

Runs on the RAW HTML, not the extracted text, because everything structural —
headings, lists, code, links, images, tables — is exactly what extraction drops.
Per post it measures the things a writer can control, then reports the
distribution so we can aim at a range rather than at somebody's opinion.

Usage: analyze_structure.py [--group G] [--json out.json]
"""
import glob, json, os, re, statistics, sys, collections
from lxml import html as LH

ROOT = os.path.expanduser("~/.xerj-write/corpora")

BOILER = ("//script|//style|//noscript|//svg|//nav|//header|//footer|//form|"
          "//aside|//*[contains(@class,'nav')]|//*[contains(@class,'sidebar')]|"
          "//*[contains(@class,'related')]|//*[contains(@class,'newsletter')]|"
          "//*[contains(@class,'cookie')]|//*[contains(@class,'subscribe')]")


def body_of(doc):
    """The article node: prefer semantic tags, else the container with the most <p> text."""
    for xp in ("//article", "//main", "//*[@role='main']",
               "//*[contains(@class,'post-content')]", "//*[contains(@class,'entry-content')]",
               "//*[contains(@class,'article-body')]", "//*[contains(@class,'markdown')]"):
        n = doc.xpath(xp)
        if n and len(" ".join(p.text_content() for p in n[0].xpath(".//p")).split()) > 150:
            return n[0]
    best, best_w = None, 0
    for cand in doc.xpath("//div|//section|//body"):
        w = len(" ".join(p.text_content() for p in cand.xpath("./p|./div/p")).split())
        if w > best_w:
            best, best_w = cand, w
    return best if best_w > 150 else None


def words(s):
    return len(s.split())


def sentences(t):
    return [s.strip() for s in re.split(r"(?<=[.!?])\s+", t)
            if len(s.split()) >= 3 and re.search(r"[a-z]{3}", s)]


def analyse(path):
    try:
        doc = LH.parse(path).getroot()
    except Exception:
        return None
    for bad in doc.xpath(BOILER):
        par = bad.getparent()
        if par is not None:
            par.remove(bad)
    art = body_of(doc)
    if art is None:
        return None

    title = (doc.xpath("//h1//text()") or doc.xpath("//title/text()") or [""])[0].strip()
    # crumbs, bylines and eyebrows are page furniture, not prose
    SKIP = ("crumb", "byline", "eyebrow", "kicker", "meta", "caption", "tag")
    ps = [p for p in art.xpath(".//p")
          if words(p.text_content()) >= 5
          and not any(k in (p.get("class") or "").lower() for k in SKIP)]
    prose = "\n".join(p.text_content() for p in ps)
    n = words(prose)
    if n < 200:
        return None

    h2 = [h.text_content().strip() for h in art.xpath(".//h2")]
    h3 = [h.text_content().strip() for h in art.xpath(".//h3")]
    heads = h2 + h3
    pre = art.xpath(".//pre")
    tables = art.xpath(".//table")
    imgs = art.xpath(".//img|.//figure|.//picture")
    lis = art.xpath(".//li")
    lists = art.xpath(".//ul|.//ol")
    links = art.xpath(".//a[@href]")
    quotes = art.xpath(".//blockquote")

    pw = [words(p.text_content()) for p in ps]
    sents = sentences(prose)
    wps = [words(s) for s in sents] or [0]
    lead = pw[0] if pw else 0

    # numbers: where do they land? intro (first 150 words) vs the rest
    num_re = r"(?<![\w-])\d[\d.,]*%?"
    intro = " ".join(prose.split()[:150])
    rest = " ".join(prose.split()[150:])
    low = prose.lower()

    # words between headings = how long a reader goes without a landmark
    sec_words = round(n / (len(heads) + 1))

    return {
        "path": os.path.basename(path),
        "source": os.path.basename(path).split("-")[0],
        "title_words": words(title),
        "title_is_question": title.strip().endswith("?"),
        "words": n,
        "read_min": round(n / 230, 1),
        "paras": len(ps),
        "para_median_words": statistics.median(pw) if pw else 0,
        "para_p90_words": sorted(pw)[int(len(pw) * .9)] if pw else 0,
        "lead_words": lead,
        "h2": len(h2),
        "h3": len(h3),
        "heads": len(heads),
        "words_per_section": sec_words,
        "head_question_pct": round(100 * sum(1 for h in heads if h.endswith("?")) / max(len(heads), 1)),
        "head_median_words": statistics.median([words(h) for h in heads]) if heads else 0,
        "code_blocks": len(pre),
        "tables": len(tables),
        "images": len(imgs),
        "lists": len(lists),
        "list_items": len(lis),
        "quotes": len(quotes),
        "links_per_1kw": round(1000 * len(links) / n, 1),
        "nums_per_100w": round(100 * len(re.findall(num_re, prose)) / n, 2),
        "nums_in_lead_150": len(re.findall(num_re, intro)),
        "nums_per_100w_body": round(100 * len(re.findall(num_re, rest)) / max(words(rest), 1), 2),
        "wps_median": statistics.median(wps),
        "wps_p90": sorted(wps)[int(len(wps) * .9)],
        "pct_sent_over_30": round(100 * sum(1 for w in wps if w > 30) / len(wps), 1),
        "pct_sent_under_12": round(100 * sum(1 for w in wps if w < 12) / len(wps), 1),
        "we_per_1kw": round(1000 * len(re.findall(r"\b(we|our|us)\b", low)) / n, 1),
        "you_per_1kw": round(1000 * len(re.findall(r"\b(you|your)\b", low)) / n, 1),
        "i_per_1kw": round(1000 * len(re.findall(r"\bi\b|\bmy\b", low)) / n, 1),
        "emdash_per_1kw": round(1000 * prose.count("—") / n, 1),
        "has_code": len(pre) > 0,
        "has_chart": len(imgs) > 0 or len(tables) > 0,
    }


NUMERIC = ["words", "read_min", "title_words", "paras", "para_median_words", "para_p90_words",
           "lead_words", "h2", "h3", "heads", "words_per_section", "head_median_words",
           "code_blocks", "tables", "images", "lists", "list_items", "quotes",
           "links_per_1kw", "nums_per_100w", "nums_in_lead_150", "nums_per_100w_body",
           "wps_median", "wps_p90", "pct_sent_over_30", "pct_sent_under_12",
           "we_per_1kw", "you_per_1kw", "i_per_1kw", "emdash_per_1kw"]


def qs(vals):
    v = sorted(vals)
    g = lambda f: v[min(int(len(v) * f), len(v) - 1)]
    return g(.10), g(.25), g(.50), g(.75), g(.90)


def table(name, rows, keys=NUMERIC):
    print(f"\n=== {name}  (n={len(rows)} posts, {sum(r['words'] for r in rows):,} words)")
    print(f"{'metric':<22}{'p10':>9}{'p25':>9}{'median':>9}{'p75':>9}{'p90':>9}")
    out = {}
    for k in keys:
        a, b, c, d, e = qs([r[k] for r in rows])
        out[k] = (a, b, c, d, e)
        f = lambda x: f"{x:.1f}" if isinstance(x, float) else str(x)
        print(f"{k:<22}{f(a):>9}{f(b):>9}{f(c):>9}{f(d):>9}{f(e):>9}")
    return out


if __name__ == "__main__":
    if "--dir" in sys.argv:
        files = sorted(glob.glob(sys.argv[sys.argv.index("--dir") + 1] + "/**/*.html", recursive=True))
    else:
        group = sys.argv[sys.argv.index("--group") + 1] if "--group" in sys.argv else "*"
        files = sorted(glob.glob(f"{ROOT}/{group}/raw/*.html"))
    rows = [r for r in (analyse(p) for p in files) if r]
    print(f"parsed {len(rows)} of {len(files)} raw pages as articles")

    allq = table("ALL REFERENCE POSTS", rows)

    by_src = collections.defaultdict(list)
    for r in rows:
        by_src[r["source"]].append(r)

    print(f"\n=== PER SOURCE (median)")
    hdr = ["words", "heads", "words_per_section", "para_median_words", "wps_median",
           "nums_per_100w", "links_per_1kw", "code_blocks", "images", "we_per_1kw", "you_per_1kw"]
    print(f"{'source':<15}{'n':>4}" + "".join(f"{h[:11]:>12}" for h in hdr))
    for s, rs in sorted(by_src.items(), key=lambda kv: -len(kv[1])):
        if len(rs) < 8:
            continue
        line = f"{s:<15}{len(rs):>4}"
        for h in hdr:
            v = statistics.median(r[h] for r in rs)
            line += f"{v:>12.1f}" if isinstance(v, float) else f"{v:>12}"
        print(line)

    print("\n=== SHAPE OF A POST")
    for lo, hi, label in [(0, 600, "short (<600w)"), (600, 1200, "standard (600-1200w)"),
                          (1200, 2500, "long (1200-2500w)"), (2500, 10**9, "deep (>2500w)")]:
        g = [r for r in rows if lo <= r["words"] < hi]
        if not g:
            continue
        print(f"  {label:<22} {len(g):>4} posts ({100*len(g)/len(rows):4.1f}%)  "
              f"median {statistics.median(r['heads'] for r in g):>4.0f} headings, "
              f"{statistics.median(r['words_per_section'] for r in g):>4.0f} words/section, "
              f"{100*sum(1 for r in g if r['has_code'])/len(g):4.0f}% carry code, "
              f"{100*sum(1 for r in g if r['images'])/len(g):4.0f}% carry an image")

    print("\n=== EVIDENCE DENSITY: where the numbers sit")
    withn = [r for r in rows if r["nums_per_100w"] > 0]
    print(f"  {100*len(withn)/len(rows):.0f}% of posts contain any number at all")
    lead_heavy = [r for r in rows if r["nums_in_lead_150"] >= 3]
    print(f"  {100*len(lead_heavy)/len(rows):.0f}% put 3+ numbers in the first 150 words")
    print(f"  median numbers in the first 150 words: {statistics.median(r['nums_in_lead_150'] for r in rows):.0f}")
    print(f"  median nums/100w overall {statistics.median(r['nums_per_100w'] for r in rows):.2f}, "
          f"body-only {statistics.median(r['nums_per_100w_body'] for r in rows):.2f}")

    if "--json" in sys.argv:
        out = sys.argv[sys.argv.index("--json") + 1]
        json.dump({"posts": rows, "quantiles": {k: list(v) for k, v in allq.items()}},
                  open(out, "w"), indent=1)
        print(f"\nwrote {out}")
