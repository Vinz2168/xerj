#!/usr/bin/env python3
"""xerj_map gate #1055 — unknown-field 400s from MCP-issued DSL.

Two arms over the SAME corpus and the SAME 30 query intents
(`queries.json`, this directory):

  arm A  no-map  — the agent guesses field names (guess_rule in queries.json)
  arm B  with-map — field choices are derived mechanically from the live
                   `xerj_map` MCP tool response (map_policy in queries.json)

Both arms issue exactly the DSL the MCP tools proxy (xerj_search →
POST /{index}/_search {query,sort}; xerj_semantic_search →
{"query":{"semantic":{field,query}}}; xerj_vector_search → top-level knn;
xerj_hybrid_search → {"query":{"hybrid":{queries:[...]}}}) against a real
node over the ES wire, and every request + response is written raw under
results/<label>/arm{A,B}/<intent>.json.

The counted failure is the HTTP 400 whose error names a field the index does
not have or has with the wrong type ("No mapping found for [...]" on sort,
"[knn] query field [...] is not a vector field", "semantic query on field
[...]: it is not a `semantic_text` field"). Silent zero-hit misses
(term/match/range/exists on a wrong field — HTTP 200, no error) are counted
separately: they are the OTHER thing xerj_map fixes, not this gate's metric.

Usage:
  run.py --node http://127.0.0.1:9660 --xerj-bin /path/to/xerj \
         --prefix xc-map-gate-v --label 2026-09-30-rc78gates
"""

import argparse
import json
import os
import subprocess
import sys
import urllib.request
import urllib.error

HERE = os.path.dirname(os.path.abspath(__file__))

SORTABLE = {"keyword", "long", "date"}


def http_json(url, body=None, method=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method or ("POST" if data else "GET"),
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, {"raw": raw}


def mcp_xerj_map(xerj_bin, node):
    """Call the real xerj_map tool over MCP stdio, exactly as an agent would."""
    p = subprocess.Popen(
        [xerj_bin, "mcp", "--url", node, "--disable-feedback"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    def send(obj):
        p.stdin.write(json.dumps(obj) + "\n"); p.stdin.flush()
    def read():
        line = p.stdout.readline()
        return json.loads(line) if line.strip() else None
    send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
          "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                     "clientInfo": {"name": "map-gate", "version": "1"}}})
    init = read()
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    send({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
          "params": {"name": "xerj_map", "arguments": {}}})
    resp = read()
    p.terminate()
    text = resp["result"]["content"][0]["text"]
    return init, text, json.loads(text)


def classify(status, body, fields_used):
    """Classify one response against the gate's failure definition."""
    out = {"http_status": status, "fields_used": fields_used}
    err = body.get("error") if isinstance(body, dict) else None
    reason = ""
    if err:
        reason = str(err.get("reason", ""))
        for rc in err.get("root_cause", []) or []:
            reason += " | " + str(rc.get("reason", ""))
    out["error_reason"] = reason[:400]
    if status == 400:
        reasons = []
        if "No mapping found for [" in reason:
            reasons.append("sort-unknown-field")
        if "is not a vector field" in reason:
            reasons.append("knn-non-vector-field")
        if "it is not a `semantic_text` field" in reason:
            reasons.append("semantic-non-semantic-field")
        out["unknown_field_400"] = bool(reasons)
        out["failure_kind"] = reasons or ["other-400"]
        # attribution: the error must name one of the fields this arm used
        out["error_names_used_field"] = any(f in reason for f in fields_used if f)
    else:
        out["unknown_field_400"] = False
        out["failure_kind"] = []
        hits = body.get("hits", {}).get("total", {}).get("value") if isinstance(body, dict) else None
        out["hits"] = hits
        hints = body.get("hints", []) if isinstance(body, dict) else []
        codes = [h.get("code") for h in hints]
        out["zero_hit_unknown_field_hint"] = ("unknown_field" in codes)
        out["hint_reason"] = next((h.get("reason", "")[:200] for h in hints
                                   if h.get("code") == "unknown_field"), "")
    return out


def entry_for(map_data, index_name):
    for e in map_data.get("indexes", []):
        if e.get("index") == index_name:
            return e
    return None


def pick_by_candidates(entry, candidates, types=None):
    """First candidate present in the map entry (case-insensitive) with a
    compatible es_type; returns (exact_name, action)."""
    fields = {f["name"]: f for f in entry.get("fields", [])}
    lower = {n.lower(): n for n in fields}
    for cand in candidates:
        exact = lower.get(cand.lower())
        if exact is None:
            continue
        if types is None or fields[exact].get("es_type") in types:
            action = "map-exact-name" if exact == cand else "map-case-corrected"
            return exact, action
    return None, "no-candidate-in-map"


def best_of_type(entry, es_type):
    fs = [f for f in entry.get("fields", []) if f.get("es_type") == es_type]
    if not fs:
        return None
    return max(fs, key=lambda f: f.get("coverage") or 0.0)["name"]


def semantic_policy(entry):
    """(endpoint_body, action, fields_used) for a semantic-shaped need."""
    sem = best_of_type(entry, "semantic_text")
    if sem:
        return "semantic", sem, "map-semantic-field"
    txt = best_of_type(entry, "text")
    if txt:
        return "match", txt, "map-no-semantic-fell-back-to-match"
    return "match_all", None, "map-no-semantic-no-text-fell-back-to-match_all"


def build_arm_a(intent):
    """The no-map request: the guessed field, in the tool's wire shape."""
    t, form = intent["tool"], intent["form"]
    if form == "sort":
        return {"query": {"match_all": {}}, "sort": [{intent["guess_field"]: "asc"}], "size": 5}, \
               [intent["guess_field"]]
    if form == "semantic":
        return {"query": {"semantic": {"field": intent["guess_field"],
                                       "query": intent["query_text"]}}, "size": 5}, \
               [intent["guess_field"]]
    if form == "knn":
        return {"knn": {"field": intent["guess_field"],
                        "query_vector": [0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8],
                        "k": 3}}, \
               [intent["guess_field"]]
    if form == "hybrid":
        return {"query": {"hybrid": {"queries": [
                    {"query": {"match": {"content": intent["lexical_text"]}}},
                    {"query": {"knn": {"field": intent["vector_leg_guess_field"],
                                       "query_vector": [0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8],
                                       "k": 3}}}],
                "fusion": "rrf"}}, "size": 5}, \
               ["content", intent["vector_leg_guess_field"]]
    if form == "term":
        return {"query": {"term": {intent["guess_field"]: intent["value"]}}, "size": 5}, \
               [intent["guess_field"]]
    if form == "range":
        return {"query": {"range": {intent["guess_field"]: intent["range"]}}, "size": 5}, \
               [intent["guess_field"]]
    if form == "exists":
        return {"query": {"exists": {"field": intent["guess_field"]}}, "size": 5}, \
               [intent["guess_field"]]
    raise ValueError(form)


def build_arm_b(intent, entry):
    """The with-map request: field choices derived from the xerj_map entry."""
    t, form = intent["tool"], intent["form"]
    actions, fields = [], []
    if form == "sort":
        name, action = pick_by_candidates(entry, intent.get("map_candidates", []), SORTABLE)
        if name:
            body = {"query": {"match_all": {}}, "sort": [{name: "asc"}], "size": 5}
            fields = [name]
        else:  # the map shows no such field: the agent keeps the search, drops the sort
            body = {"query": {"match_all": {}}, "size": 5}
            action = "dropped-sort:" + action
        actions.append(action)
        return body, fields, actions
    if form in ("semantic", "knn"):
        if form == "knn":
            name, action = pick_by_candidates(entry, intent.get("map_candidates", []),
                                              {"dense_vector"})
            if name:
                body = {"knn": {"field": name,
                                "query_vector": [0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8],
                                "k": 3}}
                return body, [name], [action]
        kind, name, action = semantic_policy(entry)
        if kind == "semantic":
            body = {"query": {"semantic": {"field": name, "query": intent["query_text"]}}, "size": 5}
        elif kind == "match":
            body = {"query": {"match": {name: intent["query_text"]}}, "size": 5}
        else:
            body = {"query": {"match_all": {}}, "size": 5}
        return body, ([name] if name else []), [action]
    if form == "hybrid":
        txt = best_of_type(entry, "text") or "code"
        kind, name, action = semantic_policy(entry)
        if kind == "semantic":
            leg = {"query": {"semantic": {"field": name, "query": intent["vector_text"]}}}
            fields = [txt, name]
        elif kind == "match":
            leg = {"query": {"match": {name: intent["vector_text"]}}}
            fields = [txt, name]
        else:
            leg = None
            fields = [txt]
        queries = [{"query": {"match": {txt: intent["lexical_text"]}}}] + ([leg] if leg else [])
        return {"query": {"hybrid": {"queries": queries, "fusion": "rrf"}}, "size": 5}, \
               fields, ["hybrid-legs:" + action]
    if form in ("term", "range", "exists"):
        name, action = pick_by_candidates(entry, intent.get("map_candidates", []))
        if name is None:  # map shows no such field: the agent drops the unexpressable filter
            actions.append("dropped-filter:" + action)
            return {"query": {"match_all": {}}, "size": 5}, [], actions
        actions.append(action)
        if form == "term":
            body = {"query": {"term": {name: intent["value"]}}, "size": 5}
        elif form == "range":
            body = {"query": {"range": {name: intent["range"]}}, "size": 5}
        else:
            body = {"query": {"exists": {"field": name}}, "size": 5}
        return body, [name], actions
    raise ValueError(form)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--node", default="http://127.0.0.1:9660")
    ap.add_argument("--xerj-bin", required=True)
    ap.add_argument("--prefix", default="xc-map-gate-v", help="corpus index prefix on the node")
    ap.add_argument("--label", required=True)
    args = ap.parse_args()

    outdir = os.path.join(HERE, "results", args.label)
    for sub in ("armA", "armB"):
        os.makedirs(os.path.join(outdir, sub), exist_ok=True)

    # ── preflight ──────────────────────────────────────────────────────────
    status, root = http_json(args.node + "/")
    version = root.get("version", {}).get("number")
    status, cat = http_json(args.node + "/_cat/indices?format=json")
    live = {r["index"]: int(r["docs.count"]) for r in cat}
    bin_version = subprocess.run([args.xerj_bin, "--version"], capture_output=True,
                                 text=True).stdout.strip()

    qs = json.load(open(os.path.join(HERE, "queries.json")))
    targets = {}
    for it in qs["intents"]:
        # corpus indexes are <prefix>-b<stamp>-<dataset>; the stamp differs per
        # build, so resolve by prefix + exact dataset suffix and demand uniqueness
        exact = [i for i in live
                 if i.startswith(args.prefix + "-") and i.endswith("-" + it["target"])]
        targets[it["target"]] = exact[0]
        assert len(exact) == 1, f"target {it['target']} resolved to {exact}"

    # ── the real xerj_map call, as an agent issues it ──────────────────────
    init, map_text, map_data = mcp_xerj_map(args.xerj_bin, args.node)
    open(os.path.join(outdir, "xerj-map-response.txt"), "w").write(map_text)
    json.dump(map_data, open(os.path.join(outdir, "xerj-map-response.json"), "w"), indent=1)

    header = {
        "gate": qs["gate"],
        "harness_git": subprocess.run(["git", "rev-parse", "HEAD"], cwd=HERE,
                                      capture_output=True, text=True).stdout.strip(),
        "binary": bin_version,
        "mcp_server": init["result"]["serverInfo"],
        "node": args.node, "node_es_version": version,
        "corpus_prefix": args.prefix,
        "indexes": {t: n for t, n in targets.items()},
        "map_matched": map_data.get("matched"),
        "map_bytes": len(map_text),
        "intents": len(qs["intents"]),
    }
    json.dump(header, open(os.path.join(outdir, "run-header.json"), "w"), indent=1)

    # ── both arms, sequentially, same node, same corpus state ─────────────
    rows = []
    counts = {arm: {"issued": 0, "unknown_field_400": 0, "other_400": 0,
                    "silent_zero_hit_with_hint": 0, "silent_zero_hit_no_hint": 0,
                    "ok_with_hits": 0} for arm in ("A", "B")}
    for it in qs["intents"]:
        idx = targets[it["target"]]
        entry = entry_for(map_data, idx)
        assert entry is not None, f"no xerj_map entry for {idx}"
        for arm, builder in (("A", build_arm_a), ("B", build_arm_b)):
            if arm == "A":
                body, fields = builder(it)
                actions = ["guess"]
            else:
                body, fields, actions = builder(it, entry)
            st, resp = http_json(f"{args.node}/{idx}/_search", body)
            cls = classify(st, resp, fields)
            rec = {"intent": it["id"], "arm": arm, "tool": it["tool"], "form": it["form"],
                   "target_index": idx, "intent_text": it.get("intent", ""),
                   "guess_field": it.get("guess_field") or it.get("vector_leg_guess_field"),
                   "control": it.get("control"),
                   "actions": actions, "request": body, **cls,
                   "map_shows": [f["name"] for f in entry.get("fields", [])]}
            json.dump(rec, open(os.path.join(outdir, f"arm{arm}", it["id"] + ".json"), "w"),
                      indent=1)
            c = counts[arm]
            c["issued"] += 1
            if cls["unknown_field_400"]:
                c["unknown_field_400"] += 1
            elif st == 400:
                c["other_400"] += 1
            elif cls.get("hits") == 0 and cls.get("zero_hit_unknown_field_hint"):
                c["silent_zero_hit_with_hint"] += 1
            elif cls.get("hits") == 0:
                c["silent_zero_hit_no_hint"] += 1
            else:
                c["ok_with_hits"] += 1
            rows.append(rec)

    a, b = counts["A"], counts["B"]
    reduction = (100.0 * (a["unknown_field_400"] - b["unknown_field_400"]) /
                 a["unknown_field_400"]) if a["unknown_field_400"] else None
    summary = {
        "gate": qs["gate"],
        "threshold": ">= 80% reduction in unknown-field 400s, arm A (no-map) vs arm B (with-map)",
        "armA_no_map": a, "armB_with_map": b,
        "reduction_pct": reduction,
        "verdict": ("PASS" if reduction is not None and reduction >= 80.0 else
                    "FAIL" if reduction is not None else "UNDEFINED"),
        "note": ("arm B's unknown_field_400 count is the gate's numerator; silent zero-hit "
                 "misses (HTTP 200, wrong field) are a separate, non-gate metric counted here "
                 "for honesty — xerj_map fixes those too but the gate text names 400s"),
    }
    json.dump(summary, open(os.path.join(outdir, "summary.json"), "w"), indent=1)
    with open(os.path.join(outdir, "results.tsv"), "w") as f:
        f.write("arm\tintent\tform\tstatus\tunknown_field_400\thits\tactions\tguess\n")
        for r in rows:
            f.write("\t".join(str(x) for x in (
                r["arm"], r["intent"], r["form"], r["http_status"],
                r["unknown_field_400"], r.get("hits", ""), ",".join(r["actions"]),
                r["guess_field"])) + "\n")
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
