#!/usr/bin/env python3
"""Stage 1 (paid) of the #1063 rc.80 final-form gate: PAIR-LEVEL FiQA rows.

The 2026-09-20 baseline (benchmarks/beir-hybrid/results/2026-09-20-rerank-full-fiqa,
produced by its raw_bench_full.py) collapsed the Jev probabilities into 10 binned
aggregates and kept only per-query nDCG — the raw (query, doc, p, gold) rows were
never written. This script re-runs the measurement at pair level with the SAME
request shape (model jev-1.13.0, one noul per shortlist document, the identical
instruction string, document = title + ". " + text[:1400]), so the pair-level
result is comparable to the baseline's curve.

What it does, in order:
1. BM25 top-30 shortlists from the local node (deterministic engine arm) —
   byte-identical request to the baseline harness.
2. Harness validation BEFORE any paid call: BM25 nDCG@10 over those shortlists
   must reproduce the baseline's 0.2382 (which itself matched BEIR's published
   0.236). Aborts otherwise unless --force.
3. One provider call per query (<= HARD_CALL_CAP paid calls in total, enforced),
   resumable: pairs already in the output JSONL are not re-called.
4. Every pair is written raw: qid, docid, shortlist rank, p_raw (the noul
   probability), gold (1 iff qrels score >= 1 — the baseline's binarisation),
   and the graded qrels score for reference.

Cost gauge (from the baseline's own usage): ~7.8k input tokens per call at
$0.042/Mtok input-only => 648 calls ~= $0.21.

Usage:
  export TYPESAFE_API_KEY=...   # never printed, never written
  python3 pairlevel_run.py --dataset-dir /path/to/fiqa --out results/<label>
"""

import argparse
import collections
import json
import math
import os
import sys
import time
import urllib.error
import urllib.request

API = "https://api.typesafe.ai/v1/systemone"
MODEL = "jev-1.13.0"
WINDOW = 30
HARD_CALL_CAP = 50_000          # the task's paid-call ceiling, enforced below
COST_PER_MTOK_IN = 0.042        # the rate the baseline runs were costed at
BASELINE_BM25_NDCG10 = 0.2382   # harness validation target (fiqa-full-run.json)
BASELINE_BM25_TOL = 0.002
INSTRUCTION = (
    "Does `document` contain information that answers the query in the state? "
    "Judge only whether `document` is relevant to the query, not whether it is "
    "well written."
)


def node_search(url, idx, body):
    r = urllib.request.Request(
        url + f"/{idx}/_search",
        data=json.dumps(body).encode(),
        method="POST",
        headers={"content-type": "application/json"},
    )
    return json.loads(urllib.request.urlopen(r, timeout=120).read())


def jev_fixed(key, query, docs):
    """The baseline's fixed request shape, verbatim."""
    questions = {}
    for i, (t, x) in docs.items():
        questions[f"d{i}"] = {
            "type": "noul",
            "instructions": {"question": INSTRUCTION, "document": (t + ". " + x)[:1400]},
        }
    body = {"state": query, "model": MODEL, "questions": questions}
    r = urllib.request.Request(
        API,
        data=json.dumps(body).encode(),
        method="POST",
        headers={"content-type": "application/json", "authorization": f"Bearer {key}"},
    )
    try:
        return json.loads(urllib.request.urlopen(r, timeout=180).read())
    except urllib.error.HTTPError as e:
        return {"_err": e.code, "body": e.read().decode()[:300]}


def ndcg10(ranked, rel):
    dcg = sum(rel.get(d, 0) / math.log2(i + 2) for i, d in enumerate(ranked[:10]))
    ideal = sorted(rel.values(), reverse=True)[:10]
    idcg = sum(g / math.log2(i + 2) for i, g in enumerate(ideal))
    return dcg / idcg if idcg else 0.0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dataset-dir", required=True, help="dir containing queries.jsonl and qrels/test.tsv")
    ap.add_argument("--index", default="fiqa")
    ap.add_argument("--xerj-url", default=os.environ.get("XERJ_URL", "http://localhost:9680"))
    ap.add_argument("--out", required=True, help="output dir (created); pairs.jsonl + shortlists.json + call log land here")
    ap.add_argument("--force", action="store_true", help="skip the BM25 validation abort")
    ap.add_argument("--limit", type=int, default=None, help="debug: only first N queries")
    args = ap.parse_args()

    key = os.environ.get("TYPESAFE_API_KEY")
    if not key:
        print("TYPESAFE_API_KEY not set", file=sys.stderr)
        return 2

    ds = args.dataset_dir
    queries = {}
    with open(os.path.join(ds, "queries.jsonl")) as f:
        for l in f:
            d = json.loads(l)
            queries[d["_id"]] = d["text"]
    qrels = collections.defaultdict(dict)
    with open(os.path.join(ds, "qrels", "test.tsv")) as f:
        for i, l in enumerate(f):
            if i == 0:
                continue
            q, d, s = l.split("\t")
            qrels[q][d] = int(s)

    pilot = list(qrels.items())
    # --limit caps the PAID calls only; shortlists and validation are engine-only
    # and always cover every judged query, so the 0.2382 check is meaningful.
    call_list = pilot[: args.limit] if args.limit else pilot
    os.makedirs(args.out, exist_ok=True)
    pairs_path = os.path.join(args.out, "pairs.jsonl")
    log_path = os.path.join(args.out, "provider-calls.log")
    short_path = os.path.join(args.out, "shortlists.json")

    # -- resume: pairs already written are not re-called (no double spend) --
    done = set()
    if os.path.exists(pairs_path):
        with open(pairs_path) as f:
            for l in f:
                done.add(json.loads(l)["qid"])

    # -- 1. shortlists (deterministic engine arm) --
    t0 = time.time()
    shorts = {}
    for qid, _ in pilot:
        res = node_search(
            args.xerj_url,
            args.index,
            {
                "query": {"multi_match": {"query": queries[qid], "fields": ["title", "text"]}},
                "size": WINDOW,
                "_source": ["title", "text"],
            },
        )
        shorts[qid] = [
            (h["_id"], h["_source"].get("title", ""), h["_source"].get("text", ""))
            for h in res["hits"]["hits"]
        ]
    empty = [q for q in shorts if not shorts[q]]
    bm = {qid: ndcg10([d[0] for d in shorts[qid]], rel) for qid, rel in pilot}
    mb = sum(bm.values()) / len(bm)
    print(
        f"shortlists: {len(pilot)} queries ({len(empty)} empty), "
        f"{sum(len(v) for v in shorts.values())} candidate docs, "
        f"bm25 nDCG@10 = {mb:.4f}  [{time.time() - t0:.0f}s]",
        flush=True,
    )

    # -- 2. validation before any paid call --
    if abs(mb - BASELINE_BM25_NDCG10) > BASELINE_BM25_TOL and not args.force:
        print(
            f"HARNESS VALIDATION FAILED: bm25 nDCG@10 {mb:.4f} vs baseline "
            f"{BASELINE_BM25_NDCG10} (tol {BASELINE_BM25_TOL}); refusing to spend. "
            "Re-check the node/index/dataset.",
            file=sys.stderr,
        )
        return 1

    with open(short_path, "w") as f:
        json.dump({q: [d[0] for d in v] for q, v in shorts.items()}, f, indent=1)

    todo = [q for q, _ in call_list if q not in done and shorts[q]]
    paid_calls = 0
    if paid_calls + len(todo) > HARD_CALL_CAP:
        print(
            f"BLOCKED: method needs {len(todo)} paid calls, cap is {HARD_CALL_CAP}.",
            file=sys.stderr,
        )
        return 3
    print(f"{len(todo)} queries to call ({len(done)} already on disk)", flush=True)

    tok_in = tok_out = 0
    walls = []
    fails = 0
    log = open(log_path, "a")
    pf = open(pairs_path, "a")
    t00 = time.time()
    for n, (qid, rel) in enumerate(call_list):
        if qid in done or not shorts[qid]:
            continue
        docs = {i: (t, x) for i, (idd, t, x) in enumerate(shorts[qid])}
        r = None
        for attempt in range(3):
            if paid_calls >= HARD_CALL_CAP:
                print(f"cap hit at {paid_calls} paid calls; stopping (resume later)", file=sys.stderr)
                pf.close()
                log.close()
                return 3
            ta = time.time()
            r = jev_fixed(key, queries[qid], docs)
            walls.append(time.time() - ta)
            paid_calls += 1
            if "_err" not in r and "answers" in r:
                break
            print(f"  attempt {attempt + 1} FAIL {qid}: {str(r)[:150]}", flush=True)
            time.sleep(5)
        if r is None or "_err" in r or "answers" not in r:
            fails += 1
            log.write(json.dumps({"qid": qid, "err": str(r)[:200]}) + "\n")
            log.flush()
            continue
        u = r.get("usage") or {}
        tok_in += u.get("input_tokens") or 0
        tok_out += u.get("output_tokens") or 0
        log.write(
            json.dumps(
                {
                    "qid": qid,
                    "in_tok": u.get("input_tokens"),
                    "out_tok": u.get("output_tokens"),
                    "wall_s": round(walls[-1], 2),
                    "answers": len(r["answers"]),
                }
            )
            + "\n"
        )
        log.flush()
        answered = 0
        for k, v in r["answers"].items():
            if not (isinstance(v, dict) and v.get("noul") is not None):
                continue
            i = int(k[1:])
            docid = shorts[qid][i][0]
            graded = rel.get(docid, 0)
            pf.write(
                json.dumps(
                    {
                        "qid": qid,
                        "docid": docid,
                        "rank": i,
                        "p_raw": v["noul"],
                        "gold": 1 if graded >= 1 else 0,
                        "graded": graded,
                    }
                )
                + "\n"
            )
            answered += 1
        pf.flush()
        if answered != len(shorts[qid]):
            print(f"  note {qid}: {answered}/{len(shorts[qid])} docs answered", flush=True)
        if (n + 1) % 50 == 0:
            el = time.time() - t00
            print(f"  {n + 1}/{len(call_list)} paid_calls={paid_calls} {el:.0f}s", flush=True)

    pf.close()
    log.close()
    walls.sort()
    cost = tok_in * COST_PER_MTOK_IN / 1e6
    summary = {
        "engine_arm": "bm25 top-30 multi_match title+text (deterministic)",
        "queries": len(pilot),
        "empty_bm25": len(empty),
        "bm25_ndcg10": round(mb, 4),
        "paid_calls": paid_calls,
        "fails": fails,
        "input_tokens": tok_in,
        "output_tokens": tok_out,
        "cost_usd_input_only_at_0.042_per_Mtok": round(cost, 4),
        "wall_s_p50": round(walls[len(walls) // 2], 2) if walls else None,
        "total_wall_min": round((time.time() - t00) / 60, 1),
    }
    with open(os.path.join(args.out, "stage1-summary.json"), "w") as f:
        json.dump(summary, f, indent=1)
    print(json.dumps(summary, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
