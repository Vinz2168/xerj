"""p50/p99 latency of the local decide tier against a booted node.

Measures `/_decide` (ES-compat port) and `/v1/systemone` (native port) with
the tier-2 local head armed, so the numbers are the ones an operator sees:
the first request pays the model load (mmap + graph build), everything after
pays the score.

Usage — the node must already be up (private port, throwaway data dir):

    xerj --insecure --port 9410 --data-dir ./tmpdata \
        --decide-mode local --decide-model-dir ../decide-model/artifact/xerj-decide-v1 &

    python3 scripts/eval_latency.py --es http://localhost:9410 --native http://localhost:9411

Stdlib only. Writes nothing unless --out.
"""
import argparse, json, time, urllib.request, urllib.error, statistics

def post(url, body, timeout=300):
    req = urllib.request.Request(
        url, data=json.dumps(body).encode(), method="POST",
        headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())

def get(url):
    with urllib.request.urlopen(urllib.request.Request(url), timeout=60) as r:
        return json.loads(r.read())

def pct(samples, p):
    if not samples:
        return 0.0
    ordered = sorted(samples)
    return ordered[min(len(ordered) - 1, int(round(p / 100.0 * (len(ordered) - 1))))]

def bench(url, body, n, warm=3):
    """Warm the lazy load first, then time n requests. Returns (ms list, first_ms)."""
    first = None
    for i in range(warm):
        t0 = time.perf_counter()
        r = post(url, body)
        dt = (time.perf_counter() - t0) * 1000.0
        if i == 0:
            first = dt
        if r.get("_err"):
            raise SystemExit(f"request failed: {r}")
    out = []
    for _ in range(n):
        t0 = time.perf_counter()
        post(url, body)
        out.append((time.perf_counter() - t0) * 1000.0)
    return out, first


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--es", default="http://localhost:9410", help="ES-compat base URL (/_decide)")
    ap.add_argument("--native", default="http://localhost:9411", help="native base URL (/v1/systemone)")
    ap.add_argument("--n", type=int, default=100)
    ap.add_argument("--labels", default=None, help="optional file: one label per line for the wide choice")
    ap.add_argument("--labels-csv", default=None,
                    help="optional Banking77-style csv (text,label); distinct labels, first-seen order")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    results = {"note": "local decide tier, tier-2 armed; single client, sequential, same machine",
               "n": args.n, "es": args.es, "native": args.native}

    # The node must say the local tier is armed.
    health = get(f"{args.es}/_cluster/health")
    print("cluster:", health.get("status"), health.get("version"))

    spam = "URGENT! You have won a £1000 prize, call 09001234567 to claim now"
    ham = "hey sorry i'll be late, start without me"

    # /_decide with no index → the local head answers the noul.
    body_noul = {"question": spam, "positive_label": "spam", "index": ""}
    samples, first = bench(f"{args.es}/_decide", body_noul, args.n)
    results["decide_noul"] = {
        "desc": "POST /_decide, no index, positive_label=spam (2 hypotheses)",
        "cold_ms": round(first, 1),
        "p50_ms": round(statistics.median(samples), 1),
        "p99_ms": round(pct(samples, 99), 1),
        "mean_ms": round(statistics.fmean(samples), 1),
    }

    # /v1/systemone: one noul question.
    so_noul = {"model": "xerj-decide-local-1", "state": {"message": spam},
               "questions": {"spam": {"type": "noul", "instructions": "Judge `message` for spam"}}}
    samples, first = bench(f"{args.native}/v1/systemone", so_noul, args.n)
    results["systemone_noul"] = {
        "desc": "POST /v1/systemone, one noul question (2 hypotheses)",
        "cold_ms": round(first, 1),
        "p50_ms": round(statistics.median(samples), 1),
        "p99_ms": round(pct(samples, 99), 1),
        "mean_ms": round(statistics.fmean(samples), 1),
    }

    # /v1/systemone: a 5-option choice (the realistic routing shape).
    five = {"top_up_failed": "top up did not work", "card_arrival": "where is my card",
            "exchange_rate": "rate for euros", "cancel_transfer": "stop a payment",
            "balance_not_updating": "balance is stale"}
    so_choice5 = {"model": "xerj-decide-local-1",
                  "state": {"message": "i topped up but the balance still shows the old amount"},
                  "questions": {"intent": {"type": "choice",
                                           "instructions": "Which intent matches `message`?",
                                           "criteria": five}}}
    samples, first = bench(f"{args.native}/v1/systemone", so_choice5, args.n)
    results["systemone_choice_5"] = {
        "desc": "POST /v1/systemone, one choice of 5 options (5 hypotheses)",
        "cold_ms": round(first, 1),
        "p50_ms": round(statistics.median(samples), 1),
        "p99_ms": round(pct(samples, 99), 1),
        "mean_ms": round(statistics.fmean(samples), 1),
    }

    # /v1/systemone: the whole 77-label Banking77 vocabulary in one choice —
    # the widest request the surface allows, and the honest ceiling of the
    # tier's per-request cost on CPU.
    labels = None
    if args.labels:
        labels = [l.strip() for l in open(args.labels) if l.strip()]
    elif args.labels_csv:
        import csv as _csv
        labels = []
        with open(args.labels_csv, newline="") as f:
            for row in _csv.DictReader(f):
                v = row.get("label") or row.get("category")
                if v and v not in labels:
                    labels.append(v)
    if labels:
        criteria = {l: f"example of {l}" for l in labels}
        so_choice77 = {"model": "xerj-decide-local-1",
                       "state": {"message": "i topped up but the balance still shows the old amount"},
                       "questions": {"intent": {"type": "choice",
                                                "instructions": "Which intent matches `message`?",
                                                "criteria": criteria}}}
        samples, first = bench(f"{args.native}/v1/systemone", so_choice77, max(10, args.n // 5))
        results["systemone_choice_77"] = {
            "desc": f"POST /v1/systemone, one choice of {len(criteria)} options",
            "cold_ms": round(first, 1),
            "p50_ms": round(statistics.median(samples), 1),
            "p99_ms": round(pct(samples, 99), 1),
            "mean_ms": round(statistics.fmean(samples), 1),
        }

    # Correctness spot-checks against the two obvious cases, so the latency
    # run also proves which tier answered.
    r = post(f"{args.es}/_decide", {"question": spam, "positive_label": "spam", "index": ""})
    results["spot"] = {
        "spam_noul": {"label": r.get("label"), "confidence": r.get("confidence"),
                      "tier": r.get("tier"), "model": r.get("model")},
        "ham_noul": (lambda h: {"label": h.get("label"), "confidence": h.get("confidence")})(
            post(f"{args.es}/_decide", {"question": ham, "positive_label": "spam", "index": ""})),
    }

    for key, block in results.items():
        if isinstance(block, dict) and "p50_ms" in block:
            print(f"{key:22s} {block['desc']}\n"
                  f"{'':22s} cold {block['cold_ms']:8.1f}ms   p50 {block['p50_ms']:7.1f}ms   "
                  f"p99 {block['p99_ms']:7.1f}ms   mean {block['mean_ms']:7.1f}ms")
    print("spot:", json.dumps(results["spot"]))
    if args.out:
        with open(args.out, "w") as f:
            json.dump(results, f, indent=1)
        print("wrote", args.out)

if __name__ == "__main__":
    main()
