#!/usr/bin/env bash
# Generate the issue #874 idle-budget corpus: 15 "repos" x ~30 "datasets" of
# a few small docs each — the synthetic equivalent of the corpus the at-rest
# budgets were measured on (autoindex over 15 reference repos => ~464 small
# indices + system indices; see docs/measurements/autoindex-watch-2026-09-19.md).
#
# Shape: one directory per repo, one CSV per dataset, UNIQUE column names per
# dataset — the same dataset-inference key .github/scripts/autoindex-fd-smoke.sh
# uses (a distinct schema => a distinct inferred dataset). run.sh turns each
# CSV into one index over the ES wire.
#
#   benchmarks/idle-budget/gen_corpus.sh <out-dir> [repos] [datasets]
#   env REPOS= / DATASETS= work too; positional args win.
set -euo pipefail
OUT=${1:?usage: gen_corpus.sh <out-dir> [repos] [datasets]}
REPOS=${2:-${REPOS:-15}}
DATASETS=${3:-${DATASETS:-30}}

mkdir -p "$OUT"
n=0
for r in $(seq 1 "$REPOS"); do
  rd=$(printf 'repo-%02d' "$r")
  mkdir -p "$OUT/$rd"
  for d in $(seq 1 "$DATASETS"); do
    # Global counter across the whole corpus: column names never repeat, so
    # every dataset has a distinct schema (and run.sh's per-dataset docs with
    # them). 3 data rows = "a few small docs".
    i=$((n + 1))
    printf 'k_%d_id,k_%d_name,k_%d_val\n1,alpha-%d,10\n2,beta-%d,20\n3,gamma-%d,30\n' \
      "$i" "$i" "$i" "$i" "$i" "$i" > "$OUT/$rd/$(printf 'ds_%03d.csv' "$d")"
    n=$i
  done
done
echo "generated $n datasets under $OUT ($REPOS repos x $DATASETS datasets, 3 docs each)"
