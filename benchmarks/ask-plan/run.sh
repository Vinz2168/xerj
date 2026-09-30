#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# ask-plan — the #1056 gate harness. SKELETON: POST /_ask does not exist in
# the engine yet; this script runs everything that can honestly run today
# (fixture load + fixture self-check) and stops with measured:false when it
# finds the endpoint absent. It never fabricates a result. Raw output goes to
# results/<date>-<label>/.
#
# The gate this harness exists to enforce (ALL of it DESIGN — zero measured
# numbers ship in this directory; see README.md):
#   1. >= 200 (prompt, gold result set) pairs over public tabular datasets —
#      data/gold/pairs.jsonl, derived from the committed raw snapshots by
#      scripts/derive_pairs.py, never from an engine run
#   2. result-set macro-F1 >= 0.9 over those pairs, where per-pair F1 compares
#      the doc-id set of the DSL POST /_ask returned against gold doc_ids;
#      zero invalid DSL out (every returned query must execute)
#   3. agent harness (case-study method: 16 runs per arm, real `claude -p`
#      token counts): output tokens per solved structured-query task <= 50%
#      of agent-written DSL at equal solve rate — scripts/agent_arm.py,
#      guarded, unrun
#
# In order:
#   0. verifies its own inputs: raw snapshots match provenance.json sha256,
#      pairs.jsonl passes schema + raw-data + DSL checks and is byte-identical
#      to what derive_pairs.py produces from the committed snapshots
#   1. boots a node on a THROWAWAY data dir (refuses a non-empty dir and a
#      RAM-backed dir) and a PRIVATE port (never :9200 — the reference-coding
#      server in this sandbox)
#   2. loads ax-usgs-earthquakes / ax-nasa-exoplanets / ax-gapminder over the
#      ES wire, _id from each dataset's own key column
#   3. self-check: executes every pair's query_equivalent and requires its
#      hit-id set to equal gold doc_ids — a fixture test, not an engine score
#   4. asks: POST /_ask per pair (DETERMINISM_RUNS passes each, all must be
#      identical — the #940 lesson); 404/405/501 stops the run with
#      measured:false and exit 0
#
# Environment:
#   XERJ_BIN           server binary (default engine/target/release/xerj)
#   PORT               force the base port (default: first free in 9640..9669;
#                      +1/+2 are claimed too). NEVER :9200.
#   XERJ_DATA          throwaway data dir (default mktemp -d); must not exist
#                      non-empty
#   DETERMINISM_RUNS   passes per pair in the ask arm (default 3 — the issue's
#                      "3 runs identical")
#   LABEL              results subdirectory label (default: skeleton)
#   KEEP=1             keep the throwaway dir + logs
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)

XERJ_BIN=${XERJ_BIN:-$HERE/../../engine/target/release/xerj}
DETERMINISM_RUNS=${DETERMINISM_RUNS:-3}
KEEP=${KEEP:-0}
LABEL=${LABEL:-skeleton}
RESULTS_DIR=${RESULTS_DIR:-$HERE/results}
DATE=$(date -u +%Y-%m-%d)
RUN_DIR="$RESULTS_DIR/$DATE-$LABEL"

[ -x "$XERJ_BIN" ] || { echo "no server binary at $XERJ_BIN — build one:"; \
  echo "  cd engine && cargo build --release -p xerj-server"; exit 2; }
command -v curl >/dev/null || { echo "curl is required"; exit 2; }
command -v python3 >/dev/null || { echo "python3 (stdlib) is required"; exit 2; }

# ── throwaway data dir: fresh, empty ────────────────────────────────────────
if [ -n "${XERJ_DATA:-}" ]; then
  DATA=$XERJ_DATA
  if [ -e "$DATA" ] && [ -n "$(ls -A "$DATA" 2>/dev/null)" ]; then
    echo "refusing: XERJ_DATA=$DATA exists and is not empty (the fixture treats its data dir as throwaway; point it at a fresh path)"
    exit 2
  fi
  mkdir -p "$DATA"
else
  DATA=$(mktemp -d "${TMPDIR:-/tmp}/ask-plan.XXXXXX")
fi

# ── private port (never :9200): first free base in 9640..9669 ───────────────
port_free() {
  python3 - "$1" <<'PY'
import socket, sys
base = int(sys.argv[1])
for p in (base, base + 1, base + 2):
    s = socket.socket()
    try:
        s.bind(("127.0.0.1", p))
    except OSError:
        sys.exit(1)
    finally:
        s.close()
PY
}
if [ -n "${PORT:-}" ]; then
  # :9200 is the reference-coding server on dev boxes — refuse it by name.
  [ "$PORT" = "9200" ] && { echo "refusing: PORT=9200 is reserved (reference-coding server); pick a private port"; exit 2; }
  port_free "$PORT" || { echo "refusing: PORT=$PORT (or +1/+2) is busy"; exit 2; }
else
  PORT=""
  for p in $(seq 9640 9669); do
    if port_free "$p"; then PORT=$p; break; fi
  done
  [ -n "$PORT" ] || { echo "no free port in 9640..9669"; exit 2; }
fi
URL="http://127.0.0.1:$PORT"

SERVER_PID=""
cleanup() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    for _ in $(seq 1 100); do kill -0 "$SERVER_PID" 2>/dev/null || break; sleep 0.1; done
    kill -9 "$SERVER_PID" 2>/dev/null || true
  fi
  if [ "${KEEP:-0}" = 1 ]; then
    echo "KEEP=1 — data dir and logs kept at $DATA"
  else
    rm -rf "$DATA"
  fi
}
trap cleanup EXIT

echo "== ask-plan harness (skeleton — #1056) =="
echo "  binary     $XERJ_BIN ($("$XERJ_BIN" --version | head -1))"
echo "  port       $PORT (private; +1 rest, +2 grpc)"
echo "  data dir   $DATA (throwaway)"
echo "  run dir    $RUN_DIR"
echo "  host       $(uname -srm), nproc=$(nproc 2>/dev/null || echo '?')"
echo

# ── 0. verify the fixture's own inputs (no engine involved) ────────────────
python3 "$HERE/scripts/fetch_data.py" --check
python3 "$HERE/scripts/verify_pairs.py"
echo

# ── 1. boot the node ────────────────────────────────────────────────────────
mkdir -p "$DATA/node"
# --insecure => TLS+auth off only; lexical embedder explicitly (the default
# feature-hashing embedder, never called neural).
"$XERJ_BIN" --insecure --port "$PORT" --data-dir "$DATA/node" \
  --embed-mode lexical > "$DATA/boot.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 300); do
  body=$(curl -s -m 2 "$URL/_cluster/health" 2>/dev/null || true)
  case "$body" in
    *'"status":"green"'*)
      # never write into somebody else's node: a curl that succeeds while our
      # process failed to bind means SOMEBODY ELSE answered (idle-budget rule).
      # ss -ltnp carries pids on CI runners but prints nothing on some hosts
      # (this sandbox among them), so fall back to the node's own boot log,
      # which prints the address it bound.
      listener=$(ss -ltnp 2>/dev/null | grep -E ":$PORT\b" | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2 || true)
      if [ -n "$listener" ] && [ "$listener" != "$SERVER_PID" ]; then
        echo "refusing: :$PORT is held by pid $listener, not by this node ($SERVER_PID)"; exit 2
      fi
      if [ -z "$listener" ]; then
        grep -q "127.0.0.1:$PORT" "$DATA/boot.log" || { echo "refusing: this node's log shows no bind to 127.0.0.1:$PORT — somebody else answered"; exit 2; }
      fi
      break ;;
  esac
  kill -0 "$SERVER_PID" 2>/dev/null || { echo "node died while waiting for green; log:"; tail -20 "$DATA/boot.log"; exit 1; }
  sleep 0.2
done
[ -n "${body:-}" ] || { echo "node never reached green; log:"; tail -20 "$DATA/boot.log"; exit 1; }
echo "node green on $URL"

# ── 2. load the fixture (three ax-* indices, deterministic _ids) ───────────
XERJ_URL="$URL" python3 "$HERE/scripts/ask_arm.py" load
curl -fs -XPOST "$URL/_refresh" >/dev/null 2>&1 || curl -fs -XPOST "$URL/_all/_refresh" >/dev/null || true

# ── 3. fixture self-check: query_equivalents must reproduce gold sets ──────
mkdir -p "$RUN_DIR"
XERJ_URL="$URL" python3 "$HERE/scripts/ask_arm.py" selfcheck --out "$RUN_DIR" | tee "$RUN_DIR/selfcheck.txt"

# ── 4. the /_ask arm ────────────────────────────────────────────────────────
XERJ_URL="$URL" python3 "$HERE/scripts/ask_arm.py" ask --out "$RUN_DIR" \
  --determinism "$DETERMINISM_RUNS" | tee "$RUN_DIR/ask.txt"

if [ -f "$RUN_DIR/status.json" ]; then
  echo
  echo "== POST /_ask not implemented (issue #1056 open) =="
  echo "   harness is ready and waiting; nothing was measured and nothing was"
  echo "   fabricated. Re-run against a build with POST /_ask to populate"
  echo "   $RUN_DIR/summary.json and the README gate table."
  exit 0
fi

# ── 5. the latency arm (p50 ≤ 300 ms gate; wall-clock, see latency_arm.py) ──
# One warmup + N measured passes over the same 230 prompts, on the same node
# the ask arm just used. Writes latency-raw.jsonl + latency-summary.json into
# the SAME run dir; refuses to fabricate when /_ask is absent (it isn't here).
XERJ_URL="$URL" python3 "$HERE/scripts/latency_arm.py" --out "$RUN_DIR" \
  --passes "$DETERMINISM_RUNS" | tee "$RUN_DIR/latency.txt"
