#!/usr/bin/env python3
"""agent_arm.py — the #1056 agent-harness comparison, case-study method. DESIGN ONLY.

STATUS: DESIGN, NOTHING MEASURED — this script has never been run and ships no
results. The issue's third gate line reads: "agent harness (case-study method:
16 runs per arm, real `claude -p` token counts) shows output tokens per solved
structured-query task <= 50% of agent-written DSL at equal solve rate." The
prior case study that defines the method measured 9,982 vs 26,477 output tokens
(docs/case-studies/reference-coding/CASE_STUDY.md, 8 tasks x 2 trials = 16 runs
per arm, tokens read from `claude -p --output-format json` usage, never
estimated). Those numbers are context for the method, NOT results of this
harness, and are not about /_ask.

Two arms, same 8 tasks (seeded pick from pairs.jsonl, 2-3 per dataset), 2
trials each = 16 runs per arm:

  direct   the agent gets the index mapping and the task prompt, and writes
           Elasticsearch query DSL itself. The harness executes exactly the
           JSON the agent emitted.
  ask      the agent gets the same mapping and task, plus the knowledge that
           POST /_ask exists (curl in the shell), and must return the DSL the
           endpoint produced.

Solved := returned DSL executes (HTTP 200) AND result-set F1 >= 0.9 vs gold.
Metric := median real output tokens per SOLVED task (and totals), read from the
`usage` field of `claude -p --output-format json`. Gate := ask-arm tokens <=
50% of direct-arm at equal solve count.

Refuses to run unless:
  - the `claude` CLI is on PATH (no CLI -> no run; the script does not fall
    back to estimating tokens, because an estimate is exactly what the
    case-study method forbids), and
  - CONFIRM_AGENT_RUN=1 is set (32 real model runs cost real money).

Usage:
  XERJ_URL=http://127.0.0.1:9640 CONFIRM_AGENT_RUN=1 \
    python3 scripts/agent_arm.py --out results/<date>-agent
"""
import argparse
import datetime
import json
import os
import pathlib
import random
import shutil
import subprocess
import sys
import urllib.request

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from ask_arm import U, pairs, search_ids, f1  # noqa: E402 — shared fixture code

SEED = 1056
TASKS_PER_ARM = 8
TRIALS = 2
SOLVE_F1 = 0.9

MAPPING_HINT = {
    "usgs-earthquakes": "fields: time (date), place (keyword), mag (double), depth (double), magType (keyword), net (keyword), nst (long), rms (double), type (keyword), status (keyword)",
    "nasa-exoplanets": "fields: hostname (keyword), sy_snum (long), sy_pnum (long), discoverymethod (keyword), disc_year (long), pl_orbper (double, days), pl_rade (double, Earth radii), pl_bmasse (double), st_teff (double, K), ra (double), dec (double)",
    "gapminder": "fields: country (keyword), continent (keyword), year (long), lifeExp (double), pop (long), gdpPercap (double)",
}

DIRECT_PROMPT = """You are working against an Elasticsearch-8-compatible search server at {url}.

Index "{index}" ({dataset}) — {mapping}.

Task: {prompt}

Reply with ONLY the JSON body of a _search request (an object with a "query"
key). The body will be executed verbatim; nothing else you write is executed.
"""

ASK_PROMPT = """You are working against an Elasticsearch-8-compatible search server at {url}.

Index "{index}" ({dataset}) — {mapping}.

The server implements POST {url}/_ask with body {{"index": "<index or pattern>",
"prompt": "<natural language>"}}; it returns {{"query": <validated query DSL>,
"plan": [...], "confidence": <number>, "indices": [...]}} — the DSL is already
validated server-side.

Task: {prompt}

Use POST /_ask (curl is available) and reply with ONLY the JSON _search body
you would send next (the "query" the endpoint returned). The body will be
executed verbatim; nothing else you write is executed.
"""


def pick_tasks():
    ps = pairs()
    rng = random.Random(SEED)
    by_ds = {}
    for p in ps:
        by_ds.setdefault(p["dataset"], []).append(p)
    tasks = []
    for ds in sorted(by_ds):
        tasks.extend(rng.sample(by_ds[ds], min(3, len(by_ds[ds]))))
    return tasks[:TASKS_PER_ARM]


def run_claude(prompt):
    """One `claude -p --output-format json` run; returns (text, usage dict)."""
    proc = subprocess.run(
        ["claude", "-p", "--output-format", "json", prompt],
        capture_output=True, text=True, timeout=900)
    if proc.returncode != 0:
        return None, {"error": f"exit {proc.returncode}: {proc.stderr[:300]}"}
    try:
        out = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return None, {"error": f"unparseable output: {proc.stdout[:300]}"}
    return out.get("result", ""), out.get("usage", {})


def execute(text, index):
    """Extract a JSON object from the agent reply and execute it verbatim."""
    text = text.strip()
    a, b = text.find("{"), text.rfind("}")
    if a < 0 or b <= a:
        return None, {"error": "no JSON object in reply"}
    try:
        body = json.loads(text[a:b + 1])
    except json.JSONDecodeError as e:
        return None, {"error": f"bad JSON: {e}"}
    q = body.get("query") if isinstance(body, dict) else None
    if q is None:
        return None, {"error": "body has no query key"}
    r = urllib.request.Request(f"{U}/{index}/_search",
                               data=json.dumps({"query": q, "size": 10000}).encode(),
                               method="POST", headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=120) as resp:
            hits = json.loads(resp.read().decode())
        return {h["_id"] for h in hits["hits"]["hits"]}, {}
    except urllib.error.HTTPError as e:
        return None, {"error": f"engine {e.code}: {e.read().decode(errors='replace')[:200]}"}


def main():
    ap = argparse.ArgumentParser(description="agent-harness comparison (DESIGN, unrun)")
    ap.add_argument("--out", required=True, help="run directory, e.g. results/2026-xx-xx-agent")
    args = ap.parse_args()

    if not shutil.which("claude"):
        print("REFUSED: the `claude` CLI is not on PATH. The case-study method reads REAL")
        print("token counts from `claude -p --output-format json`; without the CLI there is")
        print("no honest run, and this script will not estimate tokens in its place.")
        return 2
    if os.environ.get("CONFIRM_AGENT_RUN") != "1":
        print("REFUSED: set CONFIRM_AGENT_RUN=1 — this spends real model budget")
        print(f"({TASKS_PER_ARM} tasks x {TRIALS} trials x 2 arms = "
              f"{TASKS_PER_ARM * TRIALS * 2} real claude runs)")
        return 2

    tasks = pick_tasks()
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    runs = []
    for arm, template in [("direct", DIRECT_PROMPT), ("ask", ASK_PROMPT)]:
        for t in tasks:
            for trial in range(1, TRIALS + 1):
                prompt = template.format(url=U, index=t["gold"]["index"],
                                         dataset=t["dataset"],
                                         mapping=MAPPING_HINT[t["dataset"]],
                                         prompt=t["prompt"])
                text, usage = run_claude(prompt)
                ids, err = (None, usage.get("error")) if text is None else execute(text, t["gold"]["index"])
                solved = ids is not None and f1(ids, set(t["gold"]["doc_ids"])) >= SOLVE_F1
                rec = {"arm": arm, "trial": trial, "pair": t["id"],
                       "dataset": t["dataset"], "prompt": t["prompt"],
                       "reply": text, "usage": usage, "solved": solved,
                       "error": err}
                runs.append(rec)
                print(f"{arm:6s} trial {trial} {t['id']:24s} solved={solved} "
                      f"out_tokens={usage.get('output_tokens')}")

    (out / "agent-raw.jsonl").write_text("".join(json.dumps(r) + "\n" for r in runs))
    summary = {}
    for arm in ("direct", "ask"):
        rs = [r for r in runs if r["arm"] == arm]
        solved = [r for r in rs if r["solved"]]
        toks = sorted(r["usage"].get("output_tokens", 0) for r in solved)
        summary[arm] = {
            "runs": len(rs), "solved": len(solved),
            "median_output_tokens_solved": toks[len(toks) // 2] if toks else None,
            "total_output_tokens_solved": sum(toks),
        }
    summary["measured"] = True
    summary["note"] = "DESIGN-first run; README gate table updates only after review"
    (out / "agent-summary.json").write_text(json.dumps(summary, indent=1) + "\n")
    print(json.dumps(summary, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
