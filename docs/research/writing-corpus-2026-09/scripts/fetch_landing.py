#!/usr/bin/env python3
"""Fetch the HOMEPAGE of each reference company, not their blog posts.

A landing page is a different artefact from an article: the question is not
"how long is a paragraph" but "how many words does the headline carry, what
sits directly under it, how many calls to action are above the fold, and how
many sections does the page have". Those are the numbers the hero rebuild needs.

REFERENCE ONLY, same rule as the article corpus: read the approach, write our
own words and our own markup.
"""
import hashlib, json, os, time, urllib.parse, urllib.request, urllib.robotparser

ROOT = os.path.expanduser("~/.xerj-write/landing")
UA = "XERJ-writing-corpus/1.0 (internal style reference, not republished; hello@xerj.org)"
DELAY = 1.0

HOMES = [
    "https://www.anthropic.com", "https://openai.com", "https://x.ai",
    "https://mistral.ai", "https://cohere.com", "https://www.typesafe.ai",
    "https://clickhouse.com", "https://www.elastic.co", "https://qdrant.tech",
    "https://weaviate.io", "https://www.meilisearch.com", "https://typesense.org",
    "https://www.algolia.com", "https://www.pinecone.io",
    "https://fly.io", "https://stripe.com", "https://linear.app", "https://vercel.com",
    "https://supabase.com", "https://planetscale.com", "https://sentry.io",
    "https://www.honeycomb.io", "https://tailwindcss.com", "https://grafana.com",
    "https://neon.tech", "https://turso.tech", "https://modal.com", "https://railway.com",
    "https://www.cloudflare.com", "https://zed.dev", "https://www.warp.dev",
    "https://tigerbeetle.com", "https://www.cockroachlabs.com", "https://materialize.com",
    "https://incident.io", "https://www.figma.com", "https://www.notion.com",
    "https://resend.com", "https://clerk.com", "https://upstash.com",
    "https://www.docker.com", "https://redis.io", "https://duckdb.org",
    "https://www.sourcegraph.com", "https://sourcegraph.com",
]


def fetch(url, timeout=30):
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "text/html"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def allowed(home, url):
    rp = urllib.robotparser.RobotFileParser()
    try:
        raw = fetch(urllib.parse.urljoin(home, "/robots.txt")).decode("utf-8", "replace")
        rp.parse(raw.splitlines())
        return rp.can_fetch(UA, url)
    except Exception:
        return True


if __name__ == "__main__":
    os.makedirs(ROOT, exist_ok=True)
    manifest = {}
    for home in HOMES:
        name = urllib.parse.urlparse(home).netloc.replace("www.", "").split(".")[0]
        out = os.path.join(ROOT, f"{name}.html")
        if os.path.exists(out):
            print(f"  {name}: have it"); continue
        try:
            if not allowed(home, home):
                print(f"  {name}: robots.txt says no"); continue
            time.sleep(DELAY)
            raw = fetch(home)
            open(out, "wb").write(raw)
            manifest[name] = {"home": home, "bytes": len(raw)}
            print(f"  {name}: {len(raw)/1000:.0f} KB", flush=True)
        except Exception as e:
            print(f"  {name}: {type(e).__name__} {e}")
        time.sleep(DELAY)
    json.dump({"usage": "REFERENCE ONLY — copyrighted; read the approach, never copy the words",
               "pages": manifest}, open(os.path.join(ROOT, "manifest.json"), "w"), indent=1)
    print(f"\n{len([f for f in os.listdir(ROOT) if f.endswith('.html')])} homepages in {ROOT}")
