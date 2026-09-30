"""Load a BEIR corpus into an index with a CHOSEN default analyzer (gate #1059).

Identical to load.py's document pipeline (title/text mappings, `body` semantic_text
fed title+". "+text, bulk batches of 200) with one addition: the create body may
declare `settings.analysis.analyzer.default` — the exact surface PR #1070 ships
(`{"type": "stemmer"}`) and the exact surface PR #991 made honoured end-to-end.
That is the stemming-ON arm; omitting the setting reproduces the standard-analyzer
arm the rc.74 baseline (README table) was measured on.

    XERJ_URL=http://localhost:9620 python3 load_stem.py nfcorpus/corpus.jsonl nfcorpus-stem stemmer
    XERJ_URL=http://localhost:9620 python3 load_stem.py nfcorpus/corpus.jsonl nfcorpus-std standard
"""
import json, urllib.request, sys, os
U = os.environ.get("XERJ_URL", "http://localhost:9410")
def req(m, p, b=None, ct="application/json"):
    d = b if isinstance(b, (bytes, type(None))) else json.dumps(b).encode()
    r = urllib.request.Request(U + p, data=d, method=m, headers={"content-type": ct})
    try: return json.loads(urllib.request.urlopen(r, timeout=600).read())
    except urllib.error.HTTPError as e: return {"_err": e.code, "body": e.read().decode()[:400]}
DS = sys.argv[2]
ANALYZER = sys.argv[3] if len(sys.argv) > 3 else "standard"
body = {"mappings": {"properties": {
    "title": {"type": "text"}, "text": {"type": "text"},
    "body": {"type": "semantic_text"}}}}
if ANALYZER != "standard":
    # the #1070 surface, verbatim: settings.analysis.analyzer.default, create-time only
    body["settings"] = {"analysis": {"analyzer": {"default": {"type": ANALYZER}}}}
print(req("DELETE", "/" + DS))
print(req("PUT", "/" + DS, body))
docs = [json.loads(l) for l in open(sys.argv[1])]
B = 200
for i in range(0, len(docs), B):
    lines = []
    for d in docs[i:i + B]:
        lines.append(json.dumps({"index": {"_index": DS, "_id": d["_id"]}}))
        lines.append(json.dumps({"title": d["title"], "text": d["text"], "body": (d["title"] + ". " + d["text"])}))
    r = req("POST", "/_bulk", ("\n".join(lines) + "\n").encode(), "application/x-ndjson")
    if r.get("errors") or "_err" in r: print("bulk problem", str(r)[:300]); break
    if i % 1000 == 0: print("indexed", i, flush=True)
print(req("POST", "/" + DS + "/_refresh")); print(req("GET", "/" + DS + "/_count"))
