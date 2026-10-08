#!/usr/bin/env python3
"""Regenerate the xerj-blogposts corpus records from landing/blog/*.html.

The corpus source of truth is the PUBLISHED post (the HTML that xerj.org
serves). This script derives one JSON record per post — id, title,
description, published date, and the readable prose of <main> — into
posts/<slug>.json beside this script, in a deterministic order so the pack
build is reproducible and `--check` can gate CI on drift between the site
and the corpus.

Stdlib only (html.parser): the extraction is deliberately naive — headings
and paragraphs as plain lines. The corpus exists so an agent writing a new
post can retrieve how the house voice says things, not to re-render HTML.

Usage:
  gen_posts.py            regenerate posts/*.json
  gen_posts.py --check    exit 1 with a diff if in-tree records are stale
"""

import sys
import json
import re
from html.parser import HTMLParser
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent  # tools/packs/xerj-blogposts -> repo root
BLOG = REPO / "landing" / "blog"
POSTS = HERE / "posts"

DROP_INTO = {"script", "style", "nav", "footer", "noscript", "svg"}
BLOCK = {"p", "h1", "h2", "h3", "h4", "li", "pre", "blockquote", "tr", "td", "th", "figcaption"}
SKIP_TEXT = {"a", "span", "strong", "em", "b", "i", "code", "small", "sup", "sub", "time", "button"}


class Prose(HTMLParser):
    """Accumulate the readable text of <main>, dropping chrome."""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.parts: list[str] = []
        self.drop_depth = 0
        self.in_main = 0
        self.heading = None

    def handle_starttag(self, tag, attrs):
        if tag == "main":
            self.in_main += 1
        elif self.in_main:
            if tag in DROP_INTO and tag != "nav" or tag == "nav":
                if tag in DROP_INTO:
                    self.drop_depth += 1
            if tag in ("h1", "h2", "h3", "h4"):
                self.heading = tag

    def handle_endtag(self, tag):
        if tag == "main" and self.in_main:
            self.in_main -= 1
        elif self.in_main:
            if tag in DROP_INTO and self.drop_depth:
                self.drop_depth -= 1
            if tag in ("h1", "h2", "h3", "h4"):
                self.heading = None
            if tag in BLOCK:
                self.parts.append("\n")

    def handle_data(self, data):
        if not self.in_main or self.drop_depth:
            return
        text = " ".join(data.split())
        if not text:
            return
        prefix = ""
        if self.heading == "h1":
            prefix = "# "
        elif self.heading in ("h2", "h3"):
            prefix = "## "
        elif self.heading == "h4":
            prefix = "### "
        self.parts.append(prefix + text + " ")


def extract(html: str) -> tuple[str, str]:
    p = Prose()
    p.feed(html)
    raw = "".join(p.parts)
    raw = re.sub(r"[ \t]+", " ", raw)
    raw = re.sub(r"\n\s*\n+", "\n\n", raw)
    return raw.strip()


def meta(html: str, name: str) -> str:
    m = re.search(
        r'<meta[^>]+(?:name|property)=["\']' + re.escape(name) + r'["\'][^>]+content=["\']([^"\']*)["\']',
        html,
    )
    if not m:  # attribute order can flip
        m = re.search(
            r'<meta[^>]+content=["\']([^"\']*)["\'][^>]+(?:name|property)=["\']' + re.escape(name) + r'["\']',
            html,
        )
    return (m.group(1) if m else "").strip()


def main() -> int:
    check = "--check" in sys.argv
    POSTS.mkdir(exist_ok=True)
    slugs = sorted(p.stem for p in BLOG.glob("*.html") if p.name != "index.html")
    if not slugs:
        print(f"no posts under {BLOG}", file=sys.stderr)
        return 1
    drift = []
    for slug in slugs:
        html = (BLOG / f"{slug}.html").read_text(encoding="utf-8")
        title = re.search(r"<title>(.*?)</title>", html, re.S)
        published = re.search(r'datePublished"?\s*:\s*"(\d{4}-\d{2}-\d{2})', html)
        if not published:
            published = re.search(r"PUBLISHED\s*(\d{4}-\d{2}-\d{2})", html)
        record = {
            "id": f"blog-{slug}",
            "title": (title.group(1).strip() if title else slug),
            "description": meta(html, "description") or meta(html, "og:description"),
            "published": published.group(1) if published else "",
            "url": f"https://xerj.org/blog/{slug}",
            "body": extract(html),
        }
        out = POSTS / f"{slug}.json"
        text = json.dumps(record, indent=2, ensure_ascii=False) + "\n"
        if check:
            if not out.exists() or out.read_text(encoding="utf-8") != text:
                drift.append(out.name)
        else:
            out.write_text(text, encoding="utf-8")
    if check:
        stale = sorted(p.name for p in POSTS.glob("*.json") if p.stem not in slugs)
        if drift or stale:
            for n in drift:
                print(f"stale vs landing/blog: posts/{n}")
            for n in stale:
                print(f"no source post for: posts/{n}")
            print("run: python3 tools/packs/xerj-blogposts/gen_posts.py")
            return 1
        print(f"OK: {len(slugs)} post records match landing/blog")
        return 0
    print(f"wrote {len(slugs)} records to {POSTS}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
