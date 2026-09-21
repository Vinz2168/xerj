"""THE ACCEPTANCE GATE: pip-installed jev-reranker, UNMODIFIED, pointed at a
XERJ node.

    TYPESAFE_ENDPOINT=http://localhost:<native-rest-port>/v1/systemone
    TYPESAFE_API_KEY=<the node's admin key>

Ranks the fixed gate documents (gate-docs.json: 5 spam, 5 ham) through BOTH
client entry points and asserts two things per phase:

1. WIRE: the call completes with no ResponseValidationError — answers keyed
   exactly, noul numeric in 0..1, model echoed as a non-empty string, usage
   non-negative ints. The client enforces this; surviving it IS the gate.
2. SEMANTICS: the local vote separates the classes — mean noul over spam
   documents strictly exceeds mean noul over ham documents by >= 0.3.

Phase 1 is the constructor default (listwise, compact instruction). Phase 2
is `rerank_relevance()` — the documented relevance preset, which since client
0.1.2 ships its rubric OBJECT in state and references it in backticks; the
node must acknowledge the rubric without embedding its prose in the vote.

The query stays neutral (`triage inbox unsolicited correspondence`, every
term absent from the corpus): the client's own question references `query`
in backticks, so the query is named payload and joins the vote by design.
For a classification history that adds noise — which is why the query is
neutral, and why phase 3 asserts survival of a deliberately spammy query
("winner claim free prize congratulations") under the same threshold.

Run: python3 gate_run.py
"""
import json
from importlib.metadata import version

import jev_reranker
from jev_reranker import JevReranker

docs = json.load(open("gate-docs.json"))
texts = [d["text"] for d in docs]
labels = [d["label"] for d in docs]
QUERY = "triage inbox unsolicited correspondence"
SPAMMY = "winner claim free prize congratulations"


def phase(name, scores):
    print(f"-- {name}")
    for lab, txt, s in zip(labels, texts, scores):
        print(f"  {s:.4f}  {lab:4}  {txt[:70]}")
    assert len(scores) == len(texts)
    assert all(isinstance(s, float) and 0.0 <= s <= 1.0 for s in scores), scores
    spam = [s for s, l in zip(scores, labels) if l == "spam"]
    ham = [s for s, l in zip(scores, labels) if l == "ham"]
    ms, mh = sum(spam) / len(spam), sum(ham) / len(ham)
    print(f"  mean noul  spam={ms:.4f}  ham={mh:.4f}  gap={ms - mh:.4f}")
    assert ms - mh >= 0.3, f"vote did not separate: {ms:.3f} vs {mh:.3f}"


def rank(query):
    result = rr.rerank(query, texts, detail=True)
    by_index = {r["document_index"]: r["score"] for r in result["results"]}
    return [by_index[i] for i in range(len(texts))], result


print(f"client: jev-reranker {version('jev-reranker')} (pip, unmodified)")

rr = JevReranker()  # reads TYPESAFE_ENDPOINT / TYPESAFE_API_KEY from the env
scores, result = rank(QUERY)
print("client resolved model:", result["detail"]["resolved_models"])
phase("constructor default (listwise), neutral query", scores)

# The documented preset: rubric object in state, referenced in backticks.
relevance = rr.relevance_rerank(QUERY, texts, threshold=0.0, return_documents=False)
scores = [s for _, s in sorted(
    (r["document_index"], r["score"]) for r in relevance["results"])]
phase("rerank_relevance() preset (rubric object), neutral query", scores)

scores, _ = rank(SPAMMY)
phase("constructor default, deliberately spammy query", scores)

print("GATE PASSED: unmodified jev-reranker ranked from the XERJ node")
