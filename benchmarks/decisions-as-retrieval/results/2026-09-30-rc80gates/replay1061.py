"""Gate #1061: the decision-cache flywheel replay on Banking77.

  "Banking77 replay, after 2,000 cached hosted answers, >=80% of later
   traffic answered by history at >=0.8 confidence with accuracy >=0.97."

Tier 3 (hosted) is not built, so the replay is run twice:

  arm a — the ladder as shipped: the first 2,000 test rows are fired through
     /v1/systemone (choice of the 77 intents); every tier-2 answer the node
     produces is cached by the flywheel itself ([decisions] index). "Later
     traffic" = the remaining 1,080 test rows. If fewer than 2,000 answers
     got cached (history started answering before the cache filled), the
     script tops up from the TRAIN rows until 2,000 cached docs exist, then
     measures the same 1,080 test rows (arm a').

  arm b — the gate's premise instantiated: 2,000 gold-labelled answers
     (what a hosted model at 100% accuracy would have cached) written in the
     write-back's own document shape (text/label in the configured fields +
     p/source/ts, source="hosted"), then the same 1,080 test rows measured.

Reported per arm: requests fired, tier split, cached doc count, share of
later traffic answered by history at >=0.8 confidence, that share's accuracy,
and overall accuracy.
"""
import argparse, csv, http.client, json, os, time

class ES:
    def __init__(self, base):
        h, p = base.split("//")[1].split(":")
        self.c = http.client.HTTPConnection(h, int(p), timeout=300)
    def req(self, method, path, body=None, ct="application/json"):
        data = None if body is None else (
            body if isinstance(body, bytes) else json.dumps(body).encode())
        for _ in range(3):
            try:
                self.c.request(method, path, data, {"content-type": ct})
                r = self.c.getresponse()
                return r.status, json.loads(r.read() or b"{}")
            except Exception:
                self.c.close()
                h, p = self.c.host, self.c.port
                self.c = http.client.HTTPConnection(h, p, timeout=300)
        raise SystemExit("ES request kept failing: " + path)

class Native:
    def __init__(self, base):
        h, p = base.split("//")[1].split(":")
        self.c = http.client.HTTPConnection(h, int(p), timeout=300)
    def systemone(self, body):
        for _ in range(3):
            try:
                self.c.request("POST", "/v1/systemone", json.dumps(body).encode(),
                               {"content-type": "application/json"})
                return json.loads(self.c.getresponse().read())
            except Exception:
                self.c.close()
                h, p = self.c.host, self.c.port
                self.c = http.client.HTTPConnection(h, p, timeout=300)
        raise SystemExit("systemone kept failing")

def ask(nat, criteria, text):
    return nat.systemone({
        "model": "xerj-decide-local-1", "state": {"message": text},
        "questions": {"intent": {"type": "choice",
                                 "instructions": "Which intent matches `message`?",
                                 "criteria": criteria}}})

def measure_rows(rows, nat, es, index, out_rows, tag):
    """Fire rows, record tier/conf/correct. Returns per-row dicts."""
    recs = []
    for i, (text, gold) in enumerate(rows):
        r = ask(nat, CRITERIA, text)
        ev = r.get("decisions", {}).get("evidence", {}).get("intent", {})
        pred = r["answers"]["intent"]["choice"]
        conf = r["answers"]["intent"]["confidence"]
        tier = ev.get("tier")
        recs.append({"tier": tier, "conf": conf, "correct": int(pred == gold),
                     "gold": gold, "pred": pred})
        out_rows.write(json.dumps({"arm": tag, "i": i, "tier": tier, "conf": conf,
                                   "gold": gold, "pred": pred}) + "\n")
        if (i+1) % 100 == 0:
            es.req("POST", f"/{index}/_refresh", {})
            print(f"    {tag} {i+1}/{len(rows)}", flush=True)
    es.req("POST", f"/{index}/_refresh", {})
    return recs

def gate_numbers(recs):
    n = len(recs)
    hist = [r for r in recs if r["tier"] == "history"]
    hist_hi = [r for r in hist if r["conf"] >= 0.8]
    local = [r for r in recs if r["tier"] == "local"]
    def acc(rs): return round(sum(r["correct"] for r in rs)/max(len(rs), 1), 4)
    return {
        "later_traffic_n": n,
        "history_share": round(len(hist)/n, 4),
        "history_at>=0.8_share": round(len(hist_hi)/n, 4),
        "history_at>=0.8_accuracy": acc(hist_hi),
        "history_all_accuracy": acc(hist),
        "local_share": round(len(local)/n, 4),
        "local_accuracy": acc(local),
        "overall_accuracy": acc(recs),
        "gate": "PASS" if len(hist_hi)/n >= 0.80 and acc(hist_hi) >= 0.97 else "FAIL",
    }

def fresh_index(es, index):
    es.req("DELETE", f"/{index}")
    st, _ = es.req("PUT", f"/{index}", {"mappings": {"properties": {
        "text": {"type": "text"}, "label": {"type": "keyword"}}}})
    assert st in (200, 201), f"create {index}"

def count(es, index):
    es.req("POST", f"/{index}/_refresh", {})
    st, r = es.req("GET", f"/{index}/_count")
    return r.get("count", -1)

CRITERIA = None  # set in main

def main():
    global CRITERIA
    ap = argparse.ArgumentParser()
    ap.add_argument("--es", default="http://localhost:9604")
    ap.add_argument("--native", default="http://localhost:9605")
    ap.add_argument("--data", default="/tmp/xerj-rc80gates-work/data")
    ap.add_argument("--out", default=".")
    ap.add_argument("--arms", default="a,b")
    ap.add_argument("--cache-target", type=int, default=2000)
    args = ap.parse_args()
    es, nat = ES(args.es), Native(args.native)
    index = "decisions"
    labels = json.load(open(f"{args.data}/b77_labels.json"))
    CRITERIA = {l: f"example of {l}" for l in labels}
    test = json.load(open(f"{args.data}/b77_test.json"))
    # eval.py's shuffle (seed 11): the test file is LABEL-SORTED (40 rows per
    # label), so an unshuffled head/tail split measures label ordering, not
    # the flywheel — the tail's labels would be absent from the cached head
    # and accuracy would be 0 by construction (that first run is kept in
    # replay_rows.jsonl.history-sorted-run for the record).
    import random
    random.Random(11).shuffle(test)
    train = [(r["text"], r["category"]) for r in csv.DictReader(open(f"{args.data}/b77_train.csv"))]
    out_rows = open(os.path.join(args.out, "replay_rows.jsonl"), "a")
    results = {}

    if "a" in args.arms:
        print("== arm a: live flywheel — first 2,000 test rows fired through the ladder", flush=True)
        fresh_index(es, index)
        phase1, cached_from_p1 = [], 0
        t0 = time.time()
        for i, (text, gold) in enumerate(test[:args.cache_target]):
            r = ask(nat, CRITERIA, text)
            ev = r.get("decisions", {}).get("evidence", {}).get("intent", {})
            pred = r["answers"]["intent"]["choice"]
            phase1.append({"tier": ev.get("tier"), "conf": r["answers"]["intent"]["confidence"],
                           "correct": int(pred == gold)})
            out_rows.write(json.dumps({"arm": "a-fill", "i": i, "tier": ev.get("tier"),
                                       "conf": r["answers"]["intent"]["confidence"],
                                       "gold": gold, "pred": pred}) + "\n")
            if (i+1) % 10 == 0:
                es.req("POST", f"/{index}/_refresh", {})
            if (i+1) % 250 == 0:
                print(f"  fill {i+1}/{args.cache_target}  cached={count(es, index)}", flush=True)
        cached = count(es, index)
        cached_from_p1 = cached
        local_p1 = sum(1 for r in phase1 if r["tier"] == "local")
        fill = {"requests": len(phase1), "tier_local": local_p1,
                "tier_history": len(phase1) - local_p1,
                "cached_docs_after_fill": cached,
                "fill_local_accuracy": round(sum(r["correct"] for r in phase1 if r["tier"] == "local")/max(local_p1, 1), 4),
                "fill_overall_accuracy": round(sum(r["correct"] for r in phase1)/len(phase1), 4),
                "s": round(time.time()-t0, 1)}
        results["arm_a_fill"] = fill
        print(json.dumps(fill), flush=True)

        # The freeze, quantified: the last fill request that the local tier
        # answered (i.e. that was cached) — after it, history answered every
        # remaining fill request from the frozen cache and nothing new was
        # cached. No top-up: firing more traffic cannot add docs once history
        # has support for everything, which is the finding.
        last_local = max((i for i, r in enumerate(phase1) if r["tier"] == "local"), default=-1)
        fill["last_cached_at_fill_request"] = last_local
        fill["gate_premise_2000_cached"] = bool(cached >= args.cache_target)

        recs = measure_rows(test[args.cache_target:], nat, es, index, out_rows, "a-measure")
        results["arm_a_gate"] = gate_numbers(recs)
        results["arm_a_gate"]["cached_docs_at_measure"] = cached
        print(json.dumps(results["arm_a_gate"]), flush=True)

    if "b" in args.arms:
        print("== arm b: 2,000 gold 'hosted' answers cached, then later traffic", flush=True)
        fresh_index(es, index)
        lines = []
        for i, (text, gold) in enumerate(test[:args.cache_target]):
            doc = {"text": text, "label": gold, "p": 0.99, "source": "hosted",
                   "ts": "2026-09-30T00:00:00.000Z"}
            lines.append(json.dumps({"index": {"_index": index, "_id": f"g{i}"}}))
            lines.append(json.dumps(doc))
        st, r = es.req("POST", "/_bulk", ("\n".join(lines)+"\n").encode(), "application/x-ndjson")
        assert st in (200, 201) and not r.get("errors"), f"bulk: {r}"
        cached = count(es, index)
        results["arm_b_seed"] = {"gold_docs_cached": cached}
        recs = measure_rows(test[args.cache_target:], nat, es, index, out_rows, "b-measure")
        results["arm_b_gate"] = gate_numbers(recs)
        print(json.dumps(results["arm_b_gate"]), flush=True)

    out_rows.close()
    with open(os.path.join(args.out, "replay_summary.json"), "a") as f:
        f.write(json.dumps({"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "results": results},
                           indent=1) + "\n")

if __name__ == "__main__":
    main()
