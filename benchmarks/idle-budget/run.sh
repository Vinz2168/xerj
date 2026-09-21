#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# idle-budget — issue #874's "one fixture, kept runnable".
#
#   "15 repos x ~30 datasets, boot, settle 120 s, measure CPU/wakeups/RSS via
#    /proc. Gate the budget in CI on ubuntu (coarse thresholds; the point is
#    catching O(N) regressions, not ±1%)."
#
# In order:
#   0. generates the corpus (gen_corpus.sh: 15 repo dirs x 30 dataset CSVs,
#      unique column names per dataset — the autoindex inference key)
#   1. boots a BASELINE node on an empty data dir and samples it over the
#      same settle window — the per-index RSS math subtracts this
#   2. boots the measurement node, loads ONE INDEX PER DATASET over the ES
#      wire with DEFAULT settings (no settings.index.xerj_ingest_shards
#      override — default behaviour is the point), POST /_flush, SIGTERM
#   3. re-boots on the cleanly-flushed corpus: boot-to-green is the boot
#      number, and the second boot's log must contain ZERO "replayed WAL
#      entries" lines (a clean flush replays nothing; O(corpus) WAL replay
#      after a clean flush is the regression class this catches)
#   4. settles SETTLE_SECS (default 120, the issue's number) and measures,
#      /proc only (perf_event_paranoid makes perf unavailable; ps/uptime are
#      absent on the shared box this was written on):
#        CPU      utime+stime delta over the window -> % of one core
#        wakeups  voluntary+nonvoluntary ctxt-switch delta -> per second
#        RSS      VmRSS at window end (VmHWM/RssAnon/RssFile recorded too)
#   5. gates coarsely — ::error:: annotations + exit 1 — and writes
#      results/result-<label>.json
#
# CPU and wakeups are DELTAS over the window, never spot samples: the runner
# is a shared 2-4 vCPU ubuntu-latest box and a spot sample is scheduler noise.
# RSS is inherently instantaneous; it is read at the END of the window, after
# the full settle. Thresholds are coarse on purpose (~2x the healthy
# measurement): a gate that flaps on ±1% gets disabled, and the regression it
# exists to catch (per-index timers, per-index polling, per-index retention)
# shows up as a multiple, not a percent.
#
# Environment:
#   XERJ_BIN     server binary (default engine/target/release/xerj)
#   PORT         force the base port (default: first free in 9610..9639).
#                +1 (rest) and +2 (grpc) are claimed too. NEVER :9200 — that
#                is the reference-coding server in this sandbox.
#   XERJ_DATA    throwaway data dir (default mktemp -d). Must not exist
#                non-empty, and must be on a real disk: on tmpfs the corpus
#                and index live in RAM and every RSS figure is wrong (same
#                refusal as benchmarks/mbox-ingest/run.sh).
#   REPOS / DATASETS   corpus shape (default 15 x 30 = 450 indices)
#   SETTLE_SECS  idle measurement window (default 120; CI keeps 120)
#   DRAIN_SECS   boot-tail drain before the window opens (default 5): boot
#                work has its own gate (boot-to-green), the idle window must
#                measure steady state
#   LABEL        result-file name (default: n<N>indices)
#   KEEP=1       keep the throwaway dir + logs
#   GATE_*       threshold overrides. Defaults are the measured-run values
#                from README.md — do not move one to make a run pass; if a
#                gate fails, the finding is the point.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)

XERJ_BIN=${XERJ_BIN:-$REPO/engine/target/release/xerj}
REPOS=${REPOS:-15}
DATASETS=${DATASETS:-30}
SETTLE_SECS=${SETTLE_SECS:-120}
DRAIN_SECS=${DRAIN_SECS:-5}
KEEP=${KEEP:-0}
RESULTS_DIR=${RESULTS_DIR:-$HERE/results}
N=$((REPOS * DATASETS))
LABEL=${LABEL:-n${N}indices}

# ── gate thresholds (coarse CI lines; rationale + measured healthy values in
#    README.md). CPU/RSS/boot are the issue's own budget lines; wakeups is the
#    established /proc proxy for timer churn (#334, #871). Values are ~2x the
#    measured healthy state (see README) — loose enough for a shared 2-4 vCPU
#    runner, tight enough that the regression classes they exist for (per-index
#    timers, per-index polling, per-index retention, O(corpus) WAL replay) trip
#    them by multiples.
GATE_CPU_PERCENT=${GATE_CPU_PERCENT:-0.5}          # < 0.5 % of one core, any N
GATE_WAKEUPS_PER_S=${GATE_WAKEUPS_PER_S:-100}      # O(1)/s process-wide, any N
GATE_RSS_PER_INDEX_KB=${GATE_RSS_PER_INDEX_KB:-204.8}  # <= 0.2 MB per idle index
GATE_BOOT_MS=${GATE_BOOT_MS:-10000}                # boot-to-green ceiling, N indices

[ -x "$XERJ_BIN" ] || { echo "no server binary at $XERJ_BIN — build one:"; \
  echo "  cd engine && cargo build --release -p xerj-server"; exit 2; }
command -v curl >/dev/null || { echo "curl is required"; exit 2; }
command -v python3 >/dev/null || { echo "python3 (stdlib) is required"; exit 2; }
mkdir -p "$RESULTS_DIR"

# ── throwaway data dir: fresh, on a real disk ───────────────────────────────
if [ -n "${XERJ_DATA:-}" ]; then
  DATA=$XERJ_DATA
  if [ -e "$DATA" ] && [ -n "$(ls -A "$DATA" 2>/dev/null)" ]; then
    echo "refusing: XERJ_DATA=$DATA exists and is not empty (the fixture treats its data dir as throwaway; point it at a fresh path)"
    exit 2
  fi
  mkdir -p "$DATA"
else
  DATA=$(mktemp -d "${TMPDIR:-/tmp}/idle-budget.XXXXXX")
fi
if command -v findmnt >/dev/null; then
  case "$(findmnt -no FSTYPE -T "$DATA" 2>/dev/null || true)" in
    tmpfs|ramfs) echo "refusing: $DATA is RAM-backed — RSS figures would be wrong; set XERJ_DATA to a dir on a real disk"; exit 2 ;;
  esac
fi

# ── private port (never :9200): first free base in 9610..9639 ───────────────
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
  # :9200 is the reference-coding server on dev boxes — refuse it by name, not
  # only because it is usually busy.
  [ "$PORT" = "9200" ] && { echo "refusing: PORT=9200 is reserved (reference-coding server); pick a private port"; exit 2; }
  port_free "$PORT" || { echo "refusing: PORT=$PORT (or +1/+2) is busy"; exit 2; }
else
  PORT=""
  for p in $(seq 9610 9639); do
    if port_free "$p"; then PORT=$p; break; fi
  done
  [ -n "$PORT" ] || { echo "no free port in 9610..9639"; exit 2; }
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

# ── node lifecycle (a direct child of this shell; the EXIT trap owns it) ────
node_start() { # $1 = data dir, $2 = log file; sets SERVER_PID
  mkdir -p "$1"
  # --insecure => TLS+auth off only; --port => es_compat/rest/grpc = P/P+1/P+2;
  # no -c => Config::default() — default flush/merge/sampler behaviour is what
  # this fixture measures. Lexical embedder, explicitly (the default
  # feature-hashing embedder, never called neural).
  #
  # THP + decay pins: jemalloc follows the kernel's THP mode by default. On a
  # kernel with transparent_hugepage=always (GitHub runners), every index's
  # small boot allocations land in distinct 2 MB extents and each pins a full
  # hugepage — measured 2075 kB anon per idle index on the runner vs 299 kB
  # on a madvise host, i.e. a ~2 MB/index kernel page-granularity artifact
  # that would fail a 0.2 MB/index budget even for a perfect engine. Pinning
  # thp:never for the MEASURED process removes the artifact. The decay pins
  # (dirty/muzzy -> 0) make both arms read true in-use memory instead of
  # whatever freed pages the allocator happens to be retaining at sample
  # time — without them the baseline arm alone wobbled 31-76 MB across runs
  # on this host, which is a 1.5x swing in the per-index subtraction.
  #
  # BOTH spellings are set: the engine's allocator is tikv-jemalloc-sys,
  # which builds jemalloc with --with-jemalloc-prefix=_rjem_, and a prefixed
  # build reads <PREFIX>MALLOC_CONF (_RJEM_MALLOC_CONF), NOT MALLOC_CONF.
  # The unprefixed MALLOC_CONF was set alone first and silently did nothing
  # — the second CI run measured the same 1835 kB/idx as the first, with the
  # pin printed in the log.  Verified on this box: MALLOC_CONF=bogus_opt:1
  # boots with no allocator comment, _RJEM_MALLOC_CONF=bogus_opt:1 boots with
  # "<jemalloc>: Invalid conf pair: bogus_opt:1".  The unprefixed spelling
  # stays for the day the engine links an unprefixed jemalloc.
  CONF="thp:never,dirty_decay_ms:0,muzzy_decay_ms:0"
  MALLOC_CONF="$CONF" _RJEM_MALLOC_CONF="$CONF" \
    "$XERJ_BIN" --insecure --port "$PORT" --data-dir "$1" \
    --embed-mode lexical > "$2" 2>&1 &
  SERVER_PID=$!
  sleep 0.4
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "node failed to start; log:"; tail -20 "$2"; exit 1
  fi
  # A rejected pair means the measurement would run uncontrolled — the exact
  # failure that cost two CI runs.  jemalloc writes the complaint to stderr
  # at allocator init, well inside the 0.4 s above.
  if grep -q 'Invalid conf pair' "$2"; then
    echo "::error::allocator rejected the MALLOC_CONF pin — refusing to measure uncontrolled"
    grep 'jemalloc' "$2" || true
    exit 1
  fi
}

wait_green() { # $1 = log for context; sets BOOT_MS (process start -> health green)
  local t0 t1 body
  t0=$(date +%s.%N)
  for _ in $(seq 1 300); do
    body=$(curl -s -m 2 "$URL/_cluster/health" 2>/dev/null || true)
    case "$body" in
      *'"status":"green"'*)
        t1=$(date +%s.%N)
        BOOT_MS=$(python3 -c "print(int(($t1 - $t0) * 1000))")
        # Never write into somebody else's node: a curl that succeeds while our
        # process failed to bind means SOMEBODY ELSE answered. Same check as
        # mbox-ingest/run.sh, run after green because the bind happens during
        # startup, not at exec.
        local listener
        listener=$(ss -ltnp 2>/dev/null | grep -E ":$PORT\b" | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2 || true)
        [ "$listener" = "$SERVER_PID" ] || { echo "refusing: :$PORT is held by pid ${listener:-?}, not by the node this script started ($SERVER_PID)"; exit 2; }
        return 0 ;;
    esac
    kill -0 "$SERVER_PID" 2>/dev/null || { echo "node died while waiting for green; log:"; tail -20 "$1"; exit 1; }
    sleep 0.2
  done
  echo "node never reached green within 60 s; log:"; tail -20 "$1"; exit 1
}

node_stop() { # graceful: SIGTERM runs flush_all_force on the way out
  kill -TERM "$SERVER_PID" 2>/dev/null || true
  for _ in $(seq 1 200); do kill -0 "$SERVER_PID" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
}

# ── /proc sampling over a window (deltas, never spot samples) ───────────────
# Wakeups are summed across /proc/<pid>/task/*/status, NOT read from
# /proc/<pid>/status: the process-level counters describe the MAIN thread
# only, and the main thread of this server parks after boot and never wakes
# (every timer lives on a worker) — the process-level delta reads 0/s on a
# node that is demonstrably ticking. Per-thread CPU (utime+stime grouped by
# comm) rides along so a failing CPU line names its offender instead of
# leaving a bare percentage.
sample_window() { # $1 = pid, $2 = seconds, $3 = out json; echoes the json
  python3 - "$1" "$2" "$3" <<'PY'
import json, os, sys, time
pid, secs, out = int(sys.argv[1]), float(sys.argv[2]), sys.argv[3]

def sample():
    stat = open(f"/proc/{pid}/stat").read()
    rest = stat.rsplit(")", 1)[1].split()      # after comm; [11],[12] = utime,stime
    st = {}
    for line in open(f"/proc/{pid}/status"):
        k, _, v = line.partition(":")
        st[k] = v.split()
    def num(key): return int(st[key][0])
    def cnt(path):
        try: return len(os.listdir(f"/proc/{pid}/{path}"))
        except OSError: return -1
    # AnonHugePages (smaps_rollup): THP-backed anon RSS, straight from the
    # kernel. Under thp:never it reads ~0 even on a THP=always host; a dead
    # pin shows up here as hundreds of MB. Printed as evidence, not gated —
    # the per-index RSS gate is the gate; this line is the one-grep diagnosis.
    try:
        thp_kb = next(int(l.split()[1]) for l in open(f"/proc/{pid}/smaps_rollup")
                      if l.startswith("AnonHugePages:"))
    except (OSError, StopIteration, IndexError, ValueError):
        thp_kb = -1
    vol = non = 0
    tasks = {}
    try: tids = os.listdir(f"/proc/{pid}/task")
    except OSError: tids = []
    for tid in tids:
        try:
            tstat = open(f"/proc/{pid}/task/{tid}/stat").read()
            trest = tstat.rsplit(")", 1)[1].split()
            comm = tstat.split(" (", 1)[1].rsplit(")", 1)[0]
            tasks[tid] = (comm, int(trest[11]) + int(trest[12]))
            tstatus = {
                line.partition(":")[0]: line.partition(":")[2].split()
                for line in open(f"/proc/{pid}/task/{tid}/status")
            }
            vol += int(tstatus["voluntary_ctxt_switches"][0])
            non += int(tstatus["nonvoluntary_ctxt_switches"][0])
        except (OSError, KeyError, IndexError, ValueError):
            pass
    return dict(t=time.monotonic(), utime=int(rest[11]), stime=int(rest[12]),
                vol=vol, non=non, tasks=tasks,
                rss_kb=num("VmRSS"), hwm_kb=num("VmHWM"),
                rss_anon_kb=num("RssAnon"), rss_file_kb=num("RssFile"),
                anon_hugepages_kb=thp_kb,
                threads=cnt("task"), fds=cnt("fd"))

a = sample(); time.sleep(secs); b = sample()
elapsed = b["t"] - a["t"]
clk = os.sysconf("SC_CLK_TCK")
ticks = (b["utime"] - a["utime"]) + (b["stime"] - a["stime"])
# max(0, …): a thread exiting between the two passes drops its counters from
# the second sum, which would otherwise print a negative rate (seen as the
# CI run's "nonvoluntary -0.0/s").
vol = max(0, b["vol"] - a["vol"]); non = max(0, b["non"] - a["non"])
per_comm = {}
for tid, (comm, ticks_then) in a["tasks"].items():
    if tid in b["tasks"]:
        d = b["tasks"][tid][1] - ticks_then
        if d > 0:
            name = b["tasks"][tid][0]
            c = per_comm.setdefault(name, [0, 0])
            c[0] += d; c[1] += 1
top_threads = [
    dict(comm=k, cpu_ticks=v[0], threads=v[1],
         cpu_percent_of_one_core=round(100.0 * v[0] / clk / elapsed, 3))
    for k, v in sorted(per_comm.items(), key=lambda kv: -kv[1][0])[:10]
]
res = dict(
    pid=pid, window_s=round(elapsed, 1), clk_tck=clk, cpu_ticks=ticks,
    cpu_percent_of_one_core=round(100.0 * ticks / clk / elapsed, 4),
    wakeups_per_s=round((vol + non) / elapsed, 1),
    voluntary_per_s=round(vol / elapsed, 1),
    nonvoluntary_per_s=round(non / elapsed, 1),
    rss_kb=b["rss_kb"], hwm_kb=b["hwm_kb"],
    rss_anon_kb=b["rss_anon_kb"], rss_file_kb=b["rss_file_kb"],
    anon_hugepages_kb=b["anon_hugepages_kb"],
    threads=b["threads"], fds=b["fds"],
    per_thread_cpu=top_threads,
    loadavg_start=open("/proc/loadavg").read().split()[:3],
)
json.dump(res, open(out, "w"), indent=1)
print(json.dumps(res))
PY
}

echo "== idle-budget fixture =="
echo "  binary     $XERJ_BIN ($("$XERJ_BIN" --version | head -1))"
echo "  port       $PORT (private; +1 rest, +2 grpc)"
echo "  data dir   $DATA (throwaway)"
echo "  corpus     $REPOS repos x $DATASETS datasets = $N indices x 3 docs"
echo "  window     ${SETTLE_SECS}s settle (+${DRAIN_SECS}s boot-tail drain)"
echo "  host       $(uname -srm), nproc=$(nproc 2>/dev/null || echo '?'), load1=$(cut -d' ' -f1 /proc/loadavg)"
echo

# ── 0. corpus ───────────────────────────────────────────────────────────────
bash "$HERE/gen_corpus.sh" "$DATA/corpus" "$REPOS" "$DATASETS" >&2

# ── 1. baseline arm: empty node, same window (per-index RSS subtracts it) ───
node_start "$DATA/node-base" "$DATA/base-boot.log"
wait_green "$DATA/base-boot.log"
BASE_BOOT_MS=$BOOT_MS
sleep "$DRAIN_SECS"
echo "== baseline arm (0 user indices): ${SETTLE_SECS}s window =="
sample_window "$SERVER_PID" "$SETTLE_SECS" "$DATA/base-sample.json" > /dev/null
node_stop

# ── 2. load arm: one index per dataset, default settings ───────────────────
node_start "$DATA/node-n" "$DATA/n-boot1.log"
wait_green "$DATA/n-boot1.log"
EMPTY_BOOT_MS=$BOOT_MS
# Environmental normalization, not a behaviour change: the node's own disk
# flood-stage watermark (95% used) blocks writes with 429 when the throwaway
# dir lives on a nearly-full shared disk — it gates WRITE AVAILABILITY and has
# nothing to do with the idle budget this fixture measures. Raise it to 99%
# on this throwaway node (the runtime override the engine itself documents for
# exactly this unblock) so a full sandbox cannot fail the run spuriously.
curl -s -XPUT "$URL/_cluster/settings" -H 'content-type: application/json' \
  -d '{"persistent":{"cluster.routing.allocation.disk.watermark.flood_stage":"99%"}}' \
  -o /dev/null -w "flood-stage override PUT: %{http_code}\n" || true
curl -s -XPUT "$URL/_all/_settings" -H 'content-type: application/json' \
  -d '{"index.blocks.read_only_allow_delete": null}' \
  -o /dev/null -w "index-block clear PUT: %{http_code}\n" || true
# The override is applied by the node's resource sampler at the END of its
# first tick, and the flood-stage latch releases on the tick AFTER that. A
# first tick that lands after this PUT still engages the block with the
# pre-override default for ~one tick (100 ms) — so first let a couple of
# sampler periods pass, then probe with a DOCUMENT write (index creation does
# not consult the disk block; a doc write is the gated operation) until the
# node accepts one, and delete the probe index.
sleep 2
PROBE_ATTEMPTS=0
for _ in $(seq 1 300); do
  PROBE_CODE=$(curl -s -XPUT "$URL/idle-budget-write-probe/_doc/1" \
    -H 'content-type: application/json' -d '{"probe":true}' -o /dev/null -w '%{http_code}' 2>/dev/null || true)
  if [ "$PROBE_CODE" = 201 ] || [ "$PROBE_CODE" = 200 ]; then
    echo "write probe accepted after $PROBE_ATTEMPTS refused attempt(s)"
    curl -fs -XDELETE "$URL/idle-budget-write-probe" >/dev/null 2>&1 || true
    break
  fi
  PROBE_ATTEMPTS=$((PROBE_ATTEMPTS + 1))
  if [ $((PROBE_ATTEMPTS % 25)) = 0 ]; then echo "  write probe still refused ($PROBE_ATTEMPTS x $PROBE_CODE)"; fi
  sleep 0.1
done
echo "== loading $N indices over the ES wire (default settings) =="
python3 - "$URL" "$DATA/corpus" <<'PY' >&2
import csv, json, os, sys, urllib.request
base, corpus = sys.argv[1], sys.argv[2]
def req(method, path, body=None, ctype="application/json"):
    r = urllib.request.Request(base + path, data=body.encode() if body else None, method=method)
    if body: r.add_header("content-type", ctype)
    try:
        with urllib.request.urlopen(r, timeout=60) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")[:600]
        print(f"{method} {path} -> {e.code}: {detail}", file=sys.stderr)
        raise
created = docs = 0
for root, dirs, files in os.walk(corpus):
    dirs.sort()
    for f in sorted(files):
        if not f.endswith(".csv"): continue
        repo = os.path.basename(root); ds = f[:-4]
        idx = f"idle-{repo}-{ds}"
        req("PUT", f"/{idx}")
        lines = []
        with open(os.path.join(root, f), newline="") as fh:
            reader = csv.reader(fh)
            header = next(reader)
            for row in reader:
                lines.append(json.dumps({"index": {"_index": idx}}))
                lines.append(json.dumps(dict(zip(header, row))))
        resp = req("POST", "/_bulk", "\n".join(lines) + "\n", "application/x-ndjson")
        if resp.get("errors"):
            print(f"BULK ERRORS on {idx}: {json.dumps(resp)[:300]}", file=sys.stderr)
            sys.exit(3)
        created += 1; docs += len(lines) // 2
        if created % 100 == 0: print(f"  …{created} indices, {docs} docs")
print(f"loaded {created} indices ({docs} docs)")
PY
# The engine's _cat/indices ignores ?h=index and always returns the full
# table (health status first, index name third column) — count on $3.
count_idle_indices() { curl -s "$URL/_cat/indices" | awk '$3 ~ /^idle-/ {n++} END {print n+0}'; }
LOADED=$(count_idle_indices)
[ "$LOADED" = "$N" ] || { echo "::error::expected $N idle-* indices after load, got $LOADED"; exit 1; }
# flush_all: every memtable -> segment + WAL checkpoint; 200 means durable.
curl -fs -XPOST "$URL/_flush" >/dev/null || { echo "::error::POST /_flush failed"; exit 1; }
echo "flushed all indices; letting the 5 s merge debounce finish"
sleep 10
node_stop

# ── 3. restart on the cleanly-flushed corpus: boot + WAL-replay gates ──────
node_start "$DATA/node-n" "$DATA/n-boot2.log"
wait_green "$DATA/n-boot2.log"
N_BOOT_MS=$BOOT_MS
REPLAY_LINES=$(grep -c 'replayed WAL entries' "$DATA/n-boot2.log" || true)
COUNTED=$(count_idle_indices)
# one doc probe: the corpus is real docs that survived the restart, not empty dirs
PROBE=$(python3 -c "
import json, urllib.request
j = json.load(urllib.request.urlopen('$URL/idle-repo-01-ds_001/_search?size=0', timeout=10))
print(j['hits']['total']['value'] if isinstance(j['hits']['total'], dict) else j['hits']['total'])
")
[ "$PROBE" = 3 ] || { echo "::error::doc probe returned $PROBE hits on idle-repo-01-ds_001, expected 3 (corpus did not survive the flush+restart)"; exit 1; }

# ── 4. the measurement window ───────────────────────────────────────────────
sleep "$DRAIN_SECS"
echo "== measurement arm ($COUNTED indices): ${SETTLE_SECS}s window =="
sample_window "$SERVER_PID" "$SETTLE_SECS" "$DATA/n-sample.json" > /dev/null
node_stop

# ── 5. math + gate + results ────────────────────────────────────────────────
BASE_JSON="$DATA/base-sample.json" N_JSON="$DATA/n-sample.json" \
N_INDICES="$COUNTED" EXPECTED_INDICES="$N" BASE_BOOT_MS="$BASE_BOOT_MS" \
EMPTY_BOOT_MS="$EMPTY_BOOT_MS" N_BOOT_MS="$N_BOOT_MS" REPLAY_LINES="$REPLAY_LINES" \
PROBE_HITS="$PROBE" GATE_CPU_PERCENT="$GATE_CPU_PERCENT" \
GATE_WAKEUPS_PER_S="$GATE_WAKEUPS_PER_S" GATE_RSS_PER_INDEX_KB="$GATE_RSS_PER_INDEX_KB" \
GATE_BOOT_MS="$GATE_BOOT_MS" LABEL="$LABEL" RESULTS_DIR="$RESULTS_DIR" \
BIN_VERSION="$("$XERJ_BIN" --version | head -1)" BIN_PATH="$(readlink -f "$XERJ_BIN")" \
CORPUS_SHAPE="${REPOS}x${DATASETS}" PORT="$PORT" \
python3 - <<'PY'
import json, os, sys

env = os.environ
base = json.load(open(env["BASE_JSON"]))
n = json.load(open(env["N_JSON"]))
N = int(env["N_INDICES"]); expected = int(env["EXPECTED_INDICES"])

per_index_kb = (n["rss_kb"] - base["rss_kb"]) / N
out = dict(
    label=env["LABEL"], date_utc=__import__("datetime").datetime.now(__import__("datetime").timezone.utc).isoformat(timespec="seconds"),
    binary=dict(version=env["BIN_VERSION"], path=env["BIN_PATH"]),
    corpus=dict(shape=env["CORPUS_SHAPE"], indices=N, docs_per_index=3),
    port=int(env["PORT"]),
    window_s=n["window_s"], settle_target_s=float(os.environ.get("SETTLE_SECS", 120)),
    boot_ms=dict(empty_node=int(env["EMPTY_BOOT_MS"]), baseline_arm=int(env["BASE_BOOT_MS"]), n_indices=int(env["N_BOOT_MS"])),
    wal_replay_lines_after_clean_flush=int(env["REPLAY_LINES"]),
    doc_probe_hits=int(env["PROBE_HITS"]),
    baseline=base, measurement=n,
    per_index_rss_kb=round(per_index_kb, 1),
)
path = os.path.join(env["RESULTS_DIR"], f"result-{env['LABEL']}.json")
json.dump(out, open(path, "w"), indent=1)

print()
print("== measured ==")
try:
    _thp = next(l.strip() for l in open("/sys/kernel/mm/transparent_hugepage/enabled") if "[" in l)
except (OSError, StopIteration):
    _thp = "?"
print(f"  kernel THP mode {_thp}, allocator MALLOC_CONF=thp:never,decay=0 (page-granularity artifact removed; in-use memory)")
print(f"  boot-to-green, empty node        {out['boot_ms']['empty_node']:>8} ms")
print(f"  boot-to-green, {N:>4} indices         {out['boot_ms']['n_indices']:>8} ms   (cleanly-flushed corpus)")
print(f"  WAL replay lines after flush      {out['wal_replay_lines_after_clean_flush']:>8}      (0 = nothing to replay)")
print(f"  idle CPU, {N:>4} indices            {n['cpu_percent_of_one_core']:>8} % of one core  (window {n['window_s']}s)")
print(f"  idle CPU, baseline node           {base['cpu_percent_of_one_core']:>8} % of one core")
print(f"  wakeups/s, {N:>4} indices           {n['wakeups_per_s']:>8} /s   (voluntary {n['voluntary_per_s']}/s + nonvoluntary {n['nonvoluntary_per_s']}/s)")
print(f"  wakeups/s, baseline node          {base['wakeups_per_s']:>8} /s")
print(f"  VmRSS, {N:>4} indices              {n['rss_kb']:>8} kB   (VmHWM {n['hwm_kb']} kB; anon {n['rss_anon_kb']} kB + file {n['rss_file_kb']} kB)")
print(f"  VmRSS, baseline node              {base['rss_kb']:>8} kB")
print(f"  AnonHugePages, {N:>4} indices     {n['anon_hugepages_kb']:>8} kB   (~0 = the thp:never pin is live at the kernel, any THP mode)")
print(f"  AnonHugePages, baseline node      {base['anon_hugepages_kb']:>8} kB")
print(f"  per-index idle RSS                {per_index_kb:>8.1f} kB   (({n['rss_kb']} - {base['rss_kb']}) / {N})")
print(f"  threads {n['threads']}, fds {n['fds']}, loadavg {n['loadavg_start']}")
print(f"  results: {path}")

# ── the gate: coarse lines, ::error:: + FAIL, exit code ────────────────────
print()
print("== gate (coarse thresholds; the point is catching O(N) regressions, not ±1%) ==")
FAIL = 0
def gate(claimed, measured, ok, detail=""):
    global FAIL
    verdict = "ok" if ok else "FAIL"
    if not ok:
        FAIL = 1
        print(f"::error::idle-budget: {claimed} — measured {measured}{detail}")
    print(f"  {'ok  ' if ok else 'FAIL'}  CLAIMED  {claimed}")
    print(f"        MEASURED {measured}{detail}")

gate(f"idle CPU < {env['GATE_CPU_PERCENT']} % of one core (any N)",
     f"{n['cpu_percent_of_one_core']} % over {n['window_s']}s",
     n["cpu_percent_of_one_core"] < float(env["GATE_CPU_PERCENT"]))
gate(f"wakeups < {env['GATE_WAKEUPS_PER_S']}/s process-wide (any N)",
     f"{n['wakeups_per_s']}/s (vol {n['voluntary_per_s']} + nonvol {n['nonvoluntary_per_s']})",
     n["wakeups_per_s"] < float(env["GATE_WAKEUPS_PER_S"]))
gate(f"per-index idle RSS <= {env['GATE_RSS_PER_INDEX_KB']} kB (0.2 MB; total vs N)",
     f"{per_index_kb:.1f} kB = ({n['rss_kb']} - {base['rss_kb']}) / {N}",
     per_index_kb <= float(env["GATE_RSS_PER_INDEX_KB"]))
gate(f"boot-to-green < {env['GATE_BOOT_MS']} ms on a cleanly-flushed {N}-index corpus",
     f"{out['boot_ms']['n_indices']} ms (empty node boots in {out['boot_ms']['empty_node']} ms)",
     out["boot_ms"]["n_indices"] < int(env["GATE_BOOT_MS"]))
gate("no O(corpus) WAL replay after a clean flush",
     f"{out['wal_replay_lines_after_clean_flush']} 'replayed WAL entries' lines in the restart log",
     out["wal_replay_lines_after_clean_flush"] == 0)
if N != expected:
    print(f"::error::idle-budget: expected {expected} indices, fixture counted {N}")
    print(f"  FAIL  CLAIMED  {expected} indices")
    FAIL = 1
else:
    print(f"  ok   CLAIMED  {expected} indices")
    print(f"        MEASURED {N} (doc probe: {out['doc_probe_hits']} hits on one index)")

print()
if FAIL:
    print("IDLE-BUDGET GATE FAILED — see the ::error:: lines above")
    sys.exit(1)
print("IDLE-BUDGET GATE PASSED")
PY
