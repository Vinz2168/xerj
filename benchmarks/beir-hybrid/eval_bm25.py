"""BM25-only nDCG@10 + zero-hit count for the #1059 stemming gate.

Same query, same metric, same qrels handling as eval.py's bm25 arm (default-OR
`multi_match` on title+text, size=100, graded nDCG@10) so the numbers are
comparable with the README's rc.74 baseline row (0.6572 SciFact / 0.3016
NFCorpus). Prints a summary line and writes one raw TSV line per test query.

    XERJ_URL=http://localhost:9620 python3 eval_bm25.py nfcorpus nfcorpus-stem out.tsv
"""
import json, math, sys, time, urllib.request, collections, os
U = os.environ.get("XERJ_URL", "http://localhost:9410")
ROOT = sys.argv[1]          # dataset dir (queries.jsonl, qrels/test.tsv, corpus.jsonl)
IDX = sys.argv[2]           # index name on the node
OUT = sys.argv[3]           # per-query TSV destination
def post(p, b):
    r = urllib.request.Request(U + p, data=json.dumps(b).encode(), method="POST",
                               headers={"content-type": "application/json"})
    try: return json.loads(urllib.request.urlopen(r, timeout=120).read())
    except urllib.error.HTTPError as e: return {"_err": e.code, "body": e.read().decode()[:300]}
queries = {}
for l in open(f"{ROOT}/queries.jsonl"):
    d = json.loads(l); queries[d["_id"]] = d["text"]
qrels = collections.defaultdict(dict)
for i, l in enumerate(open(f"{ROOT}/qrels/test.tsv")):
    if i == 0: continue
    q, d, s = l.split("\t"); qrels[q][d] = int(s)
def ndcg10(ranked, rel):
    dcg = sum(rel.get(d, 0) / math.log2(i + 2) for i, d in enumerate(ranked[:10]))
    ideal = sorted(rel.values(), reverse=True)[:10]
    idcg = sum(g / math.log2(i + 2) for i, g in enumerate(ideal))
    return dcg / idcg if idcg else 0.0
def bm25(q, n=100):
    r = post(f"/{IDX}/_search", {"size": n, "_source": False,
           "query": {"multi_match": {"query": q, "fields": ["title", "text"]}}})
    if "_err" in r: raise SystemExit(f"search failed: {r}")
    return [h["_id"] for h in r.get("hits", {}).get("hits", [])]
count = post(f"/{IDX}/_count", {}).get("count")
print(f"index={IDX}  queries={len(qrels)}  docs={count}")
scores, zero, lat = [], [], []
with open(OUT, "w") as f:
    f.write("qid\tnhits\tnDCG@10\tquery\n")
    for qid, rel in qrels.items():
        t0 = time.time(); ranked = bm25(queries[qid]); lat.append(time.time() - t0)
        if not ranked: zero.append((qid, queries[qid]))
        s = ndcg10(ranked, rel); scores.append(s)
        f.write(f"{qid}\t{len(ranked)}\t{s:.6f}\t{queries[qid].replace(chr(9), ' ')}\n")
lat.sort()
print(f"bm25-only nDCG@10={sum(scores) / len(scores):.4f}  zero-hit={len(zero):3d}  "
      f"p50={lat[len(lat) // 2] * 1000:6.1f}ms  p95={lat[int(len(lat) * .95)] * 1000:6.1f}ms")
for qid, q in sorted(zero): print(f"  ZERO\t{qid}\t{q}")
