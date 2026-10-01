"""Local-judge gate harness (issue #1060) — nDCG@10 of the shipped first stages
with and without the opt-in `judge` block, plus the judge's added latency.

Everything printed comes from a query against a live node; nothing is assumed.

Usage:
  XERJ_URL=http://localhost:<port> python3 judge_gate.py <dataset_dir> <index> quality [runs]
  XERJ_URL=http://localhost:<port> python3 judge_gate.py <dataset_dir> <index> latency

`dataset_dir` is an extracted BEIR dataset directory (corpus.jsonl is not read
here, only queries.jsonl and qrels/test.tsv). Quality mode runs each arm
`runs` times (default 3) with the query order shuffled per run, mirroring the
protocol of results/2026-09-20-rerank-full. Latency mode times the same
hybrid top-30 request with and without the judge block, both carrying `_source`
so the only difference is the stage.

Arms (quality):
  bm30           BM25 `multi_match` top-30, no judge          (first stage alone)
  bm30+judge     BM25 top-30, judged (query inferred)         (hosted-Jev shape)
  hy30           `hybrid` RRF top-30, no judge                (THE bar: 0.699/0.345)
  hy30+judge     `hybrid` RRF top-30, judged (`judge.query`)  (the gate arm)

The judge block carries NO `min_p` in the gate arms: it reorders and drops
nothing, which is the arm a quality gate must measure (a threshold can only
remove recall). `--min-p <p>` adds a pruned informational arm instead of
replacing these.
"""
import json, math, sys, time, random, urllib.request, urllib.error, collections, os

U = os.environ.get("XERJ_URL", "http://localhost:9410")
DS = sys.argv[1]
IDX = sys.argv[2]
MODE = sys.argv[3] if len(sys.argv) > 3 else "quality"
RUNS = int(sys.argv[4]) if len(sys.argv) > 4 else 3
MINP = None
if "--min-p" in sys.argv:
    MINP = float(sys.argv[sys.argv.index("--min-p") + 1])
SIZE = 30


def post(p, b):
    r = urllib.request.Request(U + p, data=json.dumps(b).encode(), method="POST",
                               headers={"content-type": "application/json"})
    try:
        return json.loads(urllib.request.urlopen(r, timeout=300).read())
    except urllib.error.HTTPError as e:
        return {"_err": e.code, "body": e.read().decode()[:300]}


queries = {}
for l in open(DS + "/queries.jsonl"):
    d = json.loads(l)
    queries[d["_id"]] = d["text"]
qrels = collections.defaultdict(dict)
for i, l in enumerate(open(DS + "/qrels/test.tsv")):
    if i == 0:
        continue
    q, d, s = l.split("\t")
    qrels[q][d] = int(s)


def ndcg10(ranked, rel):
    dcg = sum(rel.get(d, 0) / math.log2(i + 2) for i, d in enumerate(ranked[:10]))
    ideal = sorted(rel.values(), reverse=True)[:10]
    idcg = sum(g / math.log2(i + 2) for i, g in enumerate(ideal))
    return dcg / idcg if idcg else 0.0


def ids(res):
    if "_err" in res:
        raise SystemExit(f"request failed: {res}")
    return [h["_id"] for h in res.get("hits", {}).get("hits", [])]


BM = lambda q: {"multi_match": {"query": q, "fields": ["title", "text"]}}
SEM = lambda q: {"semantic": {"field": "body", "query": q}}
HYB = lambda q: {"hybrid": {"queries": [{"query": BM(q)}, {"query": SEM(q)}], "fusion": "rrf"}}


def bm30(q):
    return ids(post(f"/{IDX}/_search", {"size": SIZE, "_source": False, "query": BM(q)}))


def hy30(q):
    return ids(post(f"/{IDX}/_search", {"size": SIZE, "_source": False, "query": HYB(q)}))


def judged(q, first, min_p=None):
    judge = {"local": True}
    if first == "bm25":
        body_q = BM(q)          # multi_match is an inferable shape
    else:
        body_q = HYB(q)         # hybrid is not: judge.query is required
        judge["query"] = q
    if min_p is not None:
        judge["min_p"] = min_p
    r = post(f"/{IDX}/_search", {"size": SIZE, "query": body_q, "judge": judge})
    if "_err" in r:
        raise SystemExit(f"judged request failed: {r}")
    j = r.get("judged", {})
    if not j.get("applied"):
        raise SystemExit(f"judge not applied: {json.dumps(j)[:200]}")
    return (ids(r), j)


ARMS = {
    "bm30": lambda q: (bm30(q), None),
    "hy30": lambda q: (hy30(q), None),
    "bm30+judge": lambda q: judged(q, "bm25"),
    "hy30+judge": lambda q: judged(q, "hybrid"),
}
if os.environ.get("ARMS"):  # restrict to the named arms, e.g. ARMS=bm30,bm30+judge
    keep = set(os.environ["ARMS"].split(","))
    ARMS = {k: v for k, v in ARMS.items() if k in keep}
if MINP is not None:
    ARMS[f"hy30+judge min_p={MINP}"] = lambda q: judged(q, "hybrid", MINP)

qids = list(qrels.keys())
if os.environ.get("LIMIT"):  # harness-validation subset; NOT a measurement
    qids = qids[: int(os.environ["LIMIT"])]
print(f"index={IDX} queries={len(qids)} mode={MODE} size={SIZE} url={U}")
print(f"docs={post('/' + IDX + '/_count', {}).get('count')}")

if MODE == "quality":
    summary = {}
    per_query = {}
    for run in range(1, RUNS + 1):
        order = qids[:]
        random.Random(1000 + run).shuffle(order)
        for name, fn in ARMS.items():
            scores, empties, took, kept, dropped = [], 0, [], [], []
            for qid in order:
                t0 = time.time()
                ranked, j = fn(queries[qid])
                dt = time.time() - t0
                if not ranked:
                    empties += 1
                scores.append(ndcg10(ranked, qrels[qid]))
                if j:
                    took.append(j.get("took_ms", 0))
                    kept.append(j.get("kept", 0))
                    dropped.append(j.get("dropped", 0))
                per_query.setdefault((name, qid), []).append((ndcg10(ranked, qrels[qid]), dt))
            m = sum(scores) / len(scores)
            summary.setdefault(name, []).append(m)
            took_s = sorted(took)
            line = f"run {run} {name:24s} nDCG@10={m:.4f} empty={empties:3d}"
            if took:
                line += (f" judge_took_ms_p50={took_s[len(took_s) // 2]} "
                         f"kept_avg={sum(kept) / len(kept):.1f} dropped_avg={sum(dropped) / len(dropped):.1f}")
            print(line, flush=True)
    print("\n== per-arm means over runs, spread, and head-to-head vs hy30 ==")
    results = {}
    for name, means in summary.items():
        spread = max(means) - min(means)
        results[name] = {"runs": means, "mean": sum(means) / len(means), "spread": spread}
        print(f"{name:26s} runs={' '.join(f'{m:.4f}' for m in means)}  mean={sum(means)/len(means):.4f}  spread={spread:.4f}")
    # head-to-head per query (run-mean per query), hy30 vs hy30+judge
    for a, b in (("hy30", "hy30+judge"), ("bm30", "bm30+judge")):
        if not all((a, qid) in per_query and (b, qid) in per_query for qid in qids):
            print(f"{b} vs {a}: skipped (an arm was not run — ARMS filter?)")
            continue
        w = l = t = 0
        deltas = []
        for qid in qids:
            va = sum(v for v, _ in per_query[(a, qid)]) / RUNS
            vb = sum(v for v, _ in per_query[(b, qid)]) / RUNS
            if vb > va:
                w += 1
            elif vb < va:
                l += 1
            else:
                t += 1
            deltas.append(vb - va)
        print(f"{b} vs {a}: W/L/T = {w}/{l}/{t}  mean per-query delta={sum(deltas)/len(deltas):+.4f}")
elif MODE == "latency":
    # Both arms identical except the judge block: same query, same size, both
    # with _source rendered (the judge refuses _source:false with no fields).
    FIRST = os.environ.get("FIRST", "hybrid")  # "hybrid" or "bm25"
    FIRSTQ = HYB if FIRST == "hybrid" else BM
    JUDGE_Q = lambda q: ({"local": True, "query": q} if FIRST == "hybrid"
                         else {"local": True})

    def base(q):
        t0 = time.time()
        post(f"/{IDX}/_search", {"size": SIZE, "query": FIRSTQ(q)})
        return time.time() - t0

    def jud(q):
        t0 = time.time()
        r = post(f"/{IDX}/_search", {"size": SIZE, "query": FIRSTQ(q),
                                     "judge": JUDGE_Q(q)})
        dt = time.time() - t0
        if "_err" in r or not r.get("judged", {}).get("applied"):
            raise SystemExit(f"judged request failed: {json.dumps(r)[:200]}")
        return dt, r["judged"].get("took_ms", 0)

    warm = qids[:30]
    for qid in warm:
        base(queries[qid]); jud(queries[qid])
    b, j, took = [], [], []
    for qid in qids:
        b.append(base(queries[qid]))
        dt, ms = jud(queries[qid])
        j.append(dt)
        took.append(ms)
    for name, arr in (("base wall s", b), ("judged wall s", j), ("judge took_ms", took)):
        arr.sort()
        p = lambda q: arr[min(int(len(arr) * q), len(arr) - 1)]
        print(f"{name:16s} p50={p(0.5) * (1000 if 'wall' in name else 1):7.1f} "
              f"p95={p(0.95) * (1000 if 'wall' in name else 1):7.1f}")
    bd, jd = sorted(b), sorted(j)
    p50d = (jd[len(jd) // 2] - bd[len(bd) // 2]) * 1000
    print(f"added p50 (judged wall - base wall, per-set p50s) = {p50d:.1f} ms")
    dq = sorted(jx - bx for bx, jx in zip(b, j))
    print(f"added p50 (per-query delta distribution)          = {dq[len(dq)//2]*1000:.1f} ms")
