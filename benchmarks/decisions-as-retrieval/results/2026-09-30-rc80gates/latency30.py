"""Gate #1057 latency AC: "p50 <= 50 ms for 30 questions on 8 CPU cores".

30 distinct questions per shape, single sequential client, wall-clock.
The node must already be booted under `taskset -c 0-7` (the runner records
that fact from its own command line, not from here). Shapes:
  - /_decide noul, no index (2 hypotheses) — the decide question
  - /v1/systemone noul (2 hypotheses)
  - /v1/systemone 5-option choice
  - /v1/systemone 77-option choice (the full Banking77 vocabulary)
Warm-up (3 requests) is excluded and reported as cold; p50/p99/mean are the
30 measured requests.
"""
import argparse, http.client, json, statistics, time

def post(c, path, body):
    c.request("POST", path, json.dumps(body).encode(),
              {"content-type": "application/json"})
    return json.loads(c.getresponse().read())

def bench(c, path, bodies, warm=3):
    first = None
    for i in range(warm):
        t0 = time.perf_counter(); post(c, path, bodies[i % len(bodies)])
        dt = (time.perf_counter()-t0)*1000.0
        if i == 0:
            first = dt
    out = []
    for b in bodies:
        t0 = time.perf_counter(); post(c, path, b)
        out.append((time.perf_counter()-t0)*1000.0)
    return out, first

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--es", default="http://localhost:9600")
    ap.add_argument("--native", default="http://localhost:9601")
    ap.add_argument("--data", default="/tmp/xerj-rc80gates-work/data")
    ap.add_argument("--out", default=".")
    args = ap.parse_args()
    def conn(base):
        h, p = base.split("//")[1].split(":")
        return http.client.HTTPConnection(h, int(p), timeout=300)
    es = conn(args.es)
    nat = conn(args.native)

    sms = json.load(open(f"{args.data}/sms_test.json"))[:30]
    labels = json.load(open(f"{args.data}/b77_labels.json"))
    q30 = [t for t, _ in sms]

    res = {}
    bodies = [{"question": q, "positive_label": "spam", "index": ""} for q in q30]
    s, cold = bench(es, "/_decide", bodies)
    res["decide_noul"] = {"desc": "POST /_decide, no index, 30 distinct SMS texts (2 hypotheses)",
                          "cold_ms": round(cold, 1), "p50_ms": round(statistics.median(s), 1),
                          "p99_ms": round(sorted(s)[29], 1), "mean_ms": round(statistics.fmean(s), 1)}

    bodies = [{"model": "xerj-decide-local-1", "state": {"message": q},
               "questions": {"spam": {"type": "noul", "instructions": "Judge `message` for spam"}}}
              for q in q30]
    s, cold = bench(nat, "/v1/systemone", bodies)
    res["systemone_noul"] = {"desc": "POST /v1/systemone, one noul (2 hypotheses), 30 distinct texts",
                             "cold_ms": round(cold, 1), "p50_ms": round(statistics.median(s), 1),
                             "p99_ms": round(sorted(s)[29], 1), "mean_ms": round(statistics.fmean(s), 1)}

    five = {"top_up_failed": "top up did not work", "card_arrival": "where is my card",
            "exchange_rate": "rate for euros", "cancel_transfer": "stop a payment",
            "balance_not_updating": "balance is stale"}
    bodies = [{"model": "xerj-decide-local-1", "state": {"message": q},
               "questions": {"intent": {"type": "choice",
                                        "instructions": "Which intent matches `message`?",
                                        "criteria": five}}} for q in q30]
    s, cold = bench(nat, "/v1/systemone", bodies)
    res["systemone_choice_5"] = {"desc": "POST /v1/systemone, 5-option choice, 30 distinct texts",
                                 "cold_ms": round(cold, 1), "p50_ms": round(statistics.median(s), 1),
                                 "p99_ms": round(sorted(s)[29], 1), "mean_ms": round(statistics.fmean(s), 1)}

    crit = {l: f"example of {l}" for l in labels}
    bodies = [{"model": "xerj-decide-local-1", "state": {"message": q},
               "questions": {"intent": {"type": "choice",
                                        "instructions": "Which intent matches `message`?",
                                        "criteria": crit}}} for q in q30]
    s, cold = bench(nat, "/v1/systemone", bodies)
    res["systemone_choice_77"] = {"desc": "POST /v1/systemone, 77-option choice (77 hypotheses), 30 distinct texts",
                                  "cold_ms": round(cold, 1), "p50_ms": round(statistics.median(s), 1),
                                  "p99_ms": round(sorted(s)[29], 1), "mean_ms": round(statistics.fmean(s), 1)}

    for k, v in res.items():
        print(f"{k:20s} cold {v['cold_ms']:7.1f}ms  p50 {v['p50_ms']:7.1f}ms  "
              f"p99 {v['p99_ms']:7.1f}ms  mean {v['mean_ms']:7.1f}ms  — {v['desc']}")
    with open(f"{args.out}/latency30.json", "w") as f:
        json.dump({"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "n": 30, "results": res}, f, indent=1)

if __name__ == "__main__":
    main()
