#!/bin/sh
# Fetch the three source files the harness trains on, then verify them.
# Sources are exactly benchmarks/decisions-as-retrieval/load.py's (issue #1064
# keeps the two benchmarks on the same rows so their numbers are comparable).
#
# Banking77:  PolyAI-LDN/task-specific-datasets, banking_data/ — the repo's
#             LICENSE is Creative Commons Attribution 4.0 International.
# SMS spam:   justmarkham/pycon-2016-tutorial's mirror of the UCI SMS Spam
#             Collection (Almeida & Hidalgo 2012, DOI 10.24432/C5CC84),
#             CC BY 4.0 per the UCI dataset page.
set -eu
dir="${1:-data}"
mkdir -p "$dir"

fetch() { # fetch URL dest
    if [ -s "$2" ]; then
        echo "have $2"
    else
        echo "get  $1"
        curl -fsSL --retry 3 "$1" -o "$2"
    fi
}

fetch "https://raw.githubusercontent.com/PolyAI-LDN/task-specific-datasets/master/banking_data/train.csv" "$dir/b77_train.csv"
fetch "https://raw.githubusercontent.com/PolyAI-LDN/task-specific-datasets/master/banking_data/test.csv"  "$dir/b77_test.csv"
fetch "https://raw.githubusercontent.com/justmarkham/pycon-2016-tutorial/master/data/sms.tsv"             "$dir/sms.tsv"

# Digests are pinned in src/data.rs; verify-data fails loudly if either side
# has moved. Run from benchmarks/decide-model.
cargo run --release -- verify-data --data-dir "$dir"
