#!/usr/bin/env python3
"""Build a reference corpus of human-written product/engineering copy.

Mirrors the xerj-code reference-corpus discipline, for prose instead of code:

  * REFERENCE ONLY. Every page here is someone else's copyrighted marketing or
    engineering writing. It exists so we can read how good writers solve a
    problem before we write our own. Never paste a sentence from it into XERJ
    copy, the website, docs or a post. Same rule as the AGPL/GPL entries in
    corpus.json: read the approach, write your own words.
  * robots.txt is honoured per URL. Rate-limited to ~1 request/second per host.
  * Raw HTML is kept (that is the "raw source") and plain text is extracted
    beside it for indexing and retrieval.

Usage: fetch_corpus.py <source-group> [--limit N]
"""
import hashlib, json, os, re, sys, time, urllib.parse, urllib.request, urllib.robotparser
from lxml import html as LH, etree

ROOT = os.path.expanduser("~/.xerj-write/corpora")
UA = "XERJ-writing-corpus/1.0 (internal style reference, not republished; hello@xerj.org)"
DELAY = 1.0

# name, home, url substrings that mark an article, topic tags
SOURCES = {
 "ai-labs": [
   ("anthropic",  "https://www.anthropic.com",  ["/news/", "/research/", "/engineering/"], "launch,research"),
   ("openai",     "https://openai.com",         ["/index/", "/blog/"],                      "launch,research"),
   ("xai",        "https://x.ai",               ["/news/", "/blog/"],                       "launch"),
   ("typesafe",   "https://docs.typesafe.ai",   ["/", ],                                    "competitor,docs"),
   ("deepmind",   "https://deepmind.google",    ["/discover/blog/"],                        "research"),
   ("mistral",    "https://mistral.ai",         ["/news/"],                                 "launch"),
   ("cohere",     "https://cohere.com",         ["/blog/"],                                 "launch,product"),
   ("huggingface","https://huggingface.co",     ["/blog/"],                                 "research,howto"),
 ],
 "search": [
   ("meilisearch","https://www.meilisearch.com",["/blog/"],                                 "search,product"),
   ("typesense",  "https://typesense.org",      ["/docs/", "/blog/"],                       "search,docs"),
   ("algolia",    "https://www.algolia.com",    ["/blog/"],                                 "search,product"),
   ("elastic",    "https://www.elastic.co",     ["/blog/"],                                 "search,product"),
   ("qdrant",     "https://qdrant.tech",        ["/articles/", "/blog/"],                   "vector,search"),
   ("weaviate",   "https://weaviate.io",        ["/blog/"],                                 "vector,search"),
   ("clickhouse", "https://clickhouse.com",     ["/blog/"],                                 "database,benchmark"),
 ],
 "devtools": [
   ("cloudflare", "https://blog.cloudflare.com",["/"],                                      "engineering,launch"),
   ("fly",        "https://fly.io",             ["/blog/"],                                 "engineering,voice"),
   ("stripe",     "https://stripe.com",         ["/blog/"],                                 "product,engineering"),
   ("linear",     "https://linear.app",         ["/blog/", "/method/"],                     "product,voice"),
   ("vercel",     "https://vercel.com",         ["/blog/"],                                 "product,launch"),
   ("supabase",   "https://supabase.com",       ["/blog/"],                                 "product,launch"),
   ("planetscale","https://planetscale.com",    ["/blog/"],                                 "database,engineering"),
   ("sentry",     "https://sentry.io",          ["/blog/"],                                 "product,voice"),
   ("honeycomb",  "https://www.honeycomb.io",   ["/blog/"],                                 "observability"),
   ("tailwind",   "https://tailwindcss.com",    ["/blog/"],                                 "product,launch"),
   ("github",     "https://github.blog",        ["/"],                                      "engineering,launch"),
   ("grafana",    "https://grafana.com",        ["/blog/"],                                 "observability"),
   ("netlify",    "https://www.netlify.com",    ["/blog/"],                                 "product"),
   ("railway",    "https://blog.railway.com",   ["/p/"],                                    "product,voice"),
   ("neon",       "https://neon.tech",          ["/blog/"],                                 "database"),
   ("turso",      "https://turso.tech",         ["/blog/"],                                 "database"),
   ("modal",      "https://modal.com",          ["/blog/"],                                 "product,engineering"),
 ],
 "evidence": [
   ("jepsen",     "https://jepsen.io",            ["/analyses/"],                  "benchmark,evidence"),
   ("danluu",     "https://danluu.com",           ["/"],                           "evidence,essay"),
   ("brendangregg","https://www.brendangregg.com",["/blog/"],                      "performance,evidence"),
   ("marcbrooker","https://brooker.co.za",        ["/blog/"],                      "distributed,essay"),
   ("jvns",       "https://jvns.ca",              ["/blog/"],                      "explanatory,voice"),
   ("simonw",     "https://simonwillison.net",    ["/20"],                         "ai,explanatory"),
   ("tigerbeetle","https://tigerbeetle.com",      ["/blog/"],                      "database,engineering"),
   ("cockroach",  "https://www.cockroachlabs.com",["/blog/"],                      "database,engineering"),
   ("materialize","https://materialize.com",      ["/blog/"],                      "database,engineering"),
   ("incidentio", "https://incident.io",          ["/blog/"],                      "product,voice"),
   ("figma",      "https://www.figma.com",        ["/blog/"],                      "product,design"),
   ("notion",     "https://www.notion.com",       ["/blog/"],                      "product"),
   ("discord",    "https://discord.com",          ["/blog/"],                      "engineering"),
   ("dropbox",    "https://dropbox.tech",         ["/"],                           "engineering"),
   ("netflix",    "https://netflixtechblog.com",  ["/"],                           "engineering"),
   ("zed",        "https://zed.dev",              ["/blog/"],                      "product,engineering"),
   ("warp",       "https://www.warp.dev",         ["/blog/"],                      "product"),
   ("ghost",      "https://ghost.org",            ["/blog/"],                      "product,voice"),
 ],
 "essays": [
   ("paulgraham", "https://paulgraham.com",     ["/"],                                      "essay,clarity"),
   ("37signals",  "https://signalvnoise.com",   ["/"],                                      "essay,voice"),
   ("firstround", "https://review.firstround.com", ["/"],                                   "essay,product"),
 ],
}

def fetch(url, timeout=30):
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "text/html,application/xml"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()

def robots_for(home):
    rp = urllib.robotparser.RobotFileParser()
    rp.set_url(urllib.parse.urljoin(home, "/robots.txt"))
    sitemaps = []
    try:
        raw = fetch(urllib.parse.urljoin(home, "/robots.txt")).decode("utf-8", "replace")
        rp.parse(raw.splitlines())
        sitemaps = [l.split(":", 1)[1].strip() for l in raw.splitlines()
                    if l.lower().startswith("sitemap:")]
    except Exception as e:
        print(f"    robots.txt unreadable ({e}); assuming allowed, still rate-limited")
        rp = None
    return rp, sitemaps

def urls_from_sitemap(sm_url, depth=0, seen=None, budget=None):
    """Walk a sitemap or sitemap index, returning page URLs."""
    if seen is None: seen = set()
    if depth > 2 or sm_url in seen: return []
    seen.add(sm_url)
    try:
        raw = fetch(sm_url)
    except Exception:
        return []
    time.sleep(DELAY)
    try:
        root = etree.fromstring(raw)
    except Exception:
        return []
    ns = {"s": "http://www.sitemaps.org/schemas/sitemap/0.9"}
    out = []
    for sm in root.findall(".//s:sitemap/s:loc", ns):
        if budget and len(out) > budget: break
        out += urls_from_sitemap(sm.text.strip(), depth + 1, seen, budget)
    for u in root.findall(".//s:url/s:loc", ns):
        out.append(u.text.strip())
    return out

def extract(raw_html, url):
    try:
        doc = LH.fromstring(raw_html)
    except Exception:
        return None, None
    for bad in doc.xpath("//script|//style|//nav|//header|//footer|//noscript|//form|//aside"):
        bad.getparent().remove(bad)
    title = (doc.xpath("//title/text()") or [""])[0].strip()
    node = None
    for xp in ("//article", "//main", "//*[contains(@class,'post')]", "//*[contains(@class,'content')]", "//body"):
        n = doc.xpath(xp)
        if n:
            node = n[0]; break
    if node is None: return title, None
    text = node.text_content()
    text = re.sub(r"[ \t\xa0]+", " ", text)
    text = re.sub(r"\n\s*\n\s*\n+", "\n\n", text).strip()
    return title, text

def run(group, limit_per_source):
    srcs = SOURCES[group]
    base = os.path.join(ROOT, group)
    os.makedirs(os.path.join(base, "raw"), exist_ok=True)
    os.makedirs(os.path.join(base, "text"), exist_ok=True)
    manifest_path = os.path.join(base, "corpus.json")
    manifest = json.load(open(manifest_path)) if os.path.exists(manifest_path) else {
        "corpus": group, "usage": "REFERENCE ONLY — copyrighted; read the approach, never copy the words",
        "sources": {}}
    for name, home, filters, topics in srcs:
        done = manifest["sources"].get(name, {}).get("pages", 0)
        if done >= limit_per_source:
            print(f"  {name}: already {done} pages, skipping"); continue
        print(f"  {name}: {home}", flush=True)
        rp, sitemaps = robots_for(home)
        if not sitemaps:
            sitemaps = [urllib.parse.urljoin(home, "/sitemap.xml"),
                        urllib.parse.urljoin(home, "/sitemap_index.xml")]
        cand = []
        for sm in sitemaps[:4]:
            cand += urls_from_sitemap(sm, budget=limit_per_source * 4)
            if len(cand) > limit_per_source * 6: break
        host = urllib.parse.urlparse(home).netloc
        keep = [u for u in dict.fromkeys(cand)
                if urllib.parse.urlparse(u).netloc.endswith(host.replace("www.", ""))
                and any(f in u for f in filters)]
        if rp: keep = [u for u in keep if rp.can_fetch(UA, u)]
        keep = keep[:limit_per_source]
        print(f"    {len(cand)} sitemap urls -> {len(keep)} in scope", flush=True)
        nraw = ntext = 0
        for i, u in enumerate(keep):
            h = hashlib.sha1(u.encode()).hexdigest()[:16]
            rawp = os.path.join(base, "raw", f"{name}-{h}.html")
            txtp = os.path.join(base, "text", f"{name}-{h}.md")
            if os.path.exists(txtp): continue
            try:
                raw = fetch(u)
            except Exception:
                time.sleep(DELAY); continue
            time.sleep(DELAY)
            open(rawp, "wb").write(raw); nraw += len(raw)
            title, text = extract(raw, u)
            if text and len(text) > 400:
                meta = (f"---\nsource: {name}\nurl: {u}\ntitle: {title!r}\ntopics: {topics}\n"
                        f"fetched: {time.strftime('%Y-%m-%d')}\nusage: reference-only\n---\n\n")
                open(txtp, "w").write(meta + text); ntext += len(text)
            if (i + 1) % 25 == 0:
                print(f"    {i+1}/{len(keep)}  raw {nraw/1e6:.1f}MB text {ntext/1e6:.1f}MB", flush=True)
        manifest["sources"][name] = {"home": home, "topics": topics, "pages": len(keep),
                                     "raw_bytes": nraw, "text_bytes": ntext,
                                     "licence": "all rights reserved by publisher; reference only"}
        json.dump(manifest, open(manifest_path, "w"), indent=1)
        print(f"    {name}: {len(keep)} pages, raw {nraw/1e6:.1f}MB, text {ntext/1e6:.1f}MB", flush=True)

if __name__ == "__main__":
    group = sys.argv[1]
    limit = int(sys.argv[sys.argv.index("--limit") + 1]) if "--limit" in sys.argv else 150
    run(group, limit)
