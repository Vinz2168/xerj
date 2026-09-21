# idle budget — measured on 450 idle indices

Issue #874's "one fixture, kept runnable":

> 15 repos x ~30 datasets, boot, settle 120 s, measure CPU/wakeups/RSS via
> /proc. Gate the budget in CI on ubuntu (coarse thresholds; the point is
> catching O(N) regressions, not ±1%).

What [`run.sh`](./run.sh) does, the numbers that came out, and — the reason
this fixture exists — the idle-CPU regression it caught on its very first run
against the then-current tree, with the fix that followed. Everything below is
from real runs on this box; result files as produced by the runs are in
[`results/`](./results/).

**Units.** Memory figures are `/proc` kB (1024-byte units — `VmRSS` and
friends are reported in kB by the kernel); "0.2 MB per index" in the issue is
taken at the issue's word as 204.8 kB. CPU is **% of one core** =
(utime+stime delta over the window) / CLK_TCK / window seconds, summed over
all threads — 100 % = one full core, 3.1 % ≈ the box's 32 threads at 1 %.
Wakeups are context switches per second, voluntary + nonvoluntary, summed
across `/proc/<pid>/task/*/status`. The three are never mixed into one number.

## Machine

| | |
|---|---|
| CPU | AMD Ryzen AI MAX+ 395 w/ Radeon 8060S — 32 threads (16 cores × 2) |
| RAM | 119 GiB usable |
| Disk | NVMe, ext4; the throwaway data dir lives on it, NOT on tmpfs (`run.sh` refuses a RAM-backed dir) |
| OS | Linux 7.0.0-27-generic (Ubuntu), x86_64 |
| XERJ | v1.0.0-rc.77 + branch `bench/issue-874-idle-budget-gate`, `cargo build --release -p xerj-server` |
| Node | throwaway data dir, private port in 9610..9639, `--insecure` (TLS+auth off), `--embed-mode lexical` (the default feature-hashing embedder — not neural), no config file — `Config::default()` flush/merge/sampler behaviour is the thing measured |
| Box load | **shared**: other agents were building and running on it during every window (load average is recorded inside each result file: 38–45 in the run of record, up to 63 in the before-fix artifact). Idle-CPU and wakeup figures are process-wide counters, so a busy neighbour inflates *nonvoluntary* switches (scheduler preemption) but not the process's own CPU time; RSS is unaffected. |

## What the fixture measures

Four arms, in order ([`run.sh`](./run.sh)):

0. **corpus** — [`gen_corpus.sh`](./gen_corpus.sh) writes 15 repo dirs × 30
   dataset CSVs (3 docs each, unique column names per dataset).
1. **baseline** — a node on an *empty* data dir, same 120 s window. The
   per-index RSS line subtracts this node's VmRSS; without it the divisor
   would include one runtime's worth of allocations.
2. **load** — boot, create one index per dataset over the ES wire with
   default settings (no shard override — default behaviour is the point),
   `_bulk` 3 docs each, `POST /_flush`, SIGTERM (graceful, flushes on the way
   out).
3. **restart + measure** — boot again on the cleanly-flushed corpus: this
   boot-to-green is the boot number, and the restart log must contain **zero**
   `replayed WAL entries` lines (a clean flush replays nothing). Then 5 s of
   boot-tail drain and the 120 s measurement window, `/proc` only
   (`perf_event_paranoid` makes perf unavailable; `ps`/`uptime` are absent
   here). CPU and wakeups are **deltas over the window**, never spot samples;
   RSS is read at window end.

One environmental normalization, applied to the load node only: the sandbox
these numbers were taken on keeps its disk at ~96 % used, above the node's
default 95 % disk flood-stage watermark, which would 429 the loader for
reasons that have nothing to do with the idle budget. `run.sh` raises the
watermark to 99 % on its own throwaway node via the engine's documented
runtime override (`PUT _cluster/settings` on
`cluster.routing.allocation.disk.watermark.flood_stage`), waits out the
resource sampler's apply/release ticks, and probes a real document write
before loading. On CI (disk nowhere near 95 %) the override is a no-op.
`KEEP=1` preserves the throwaway dir and both arms' boot logs.

## Measured

The run of record (fixed binary, all gates green, 2026-09-21,
[`results/result-n450indices.json`](./results/result-n450indices.json)):

| | baseline node (N=0) | 450 indices | gate | verdict |
|---|---|---|---|---|
| idle CPU (% of one core, 120 s window) | 0.10 | **0.175** | < 0.5 | ok |
| wakeups/s (vol + nonvol, process-wide) | 26.0 | **26.6** (24.2 + 2.4) | < 100 | ok |
| VmRSS (kB) | 76 240 | **158 944** (anon 134 704 + file 24 240; HWM 173 028) | — | — |
| per-index idle RSS (kB) | — | **183.8** = (158 944 − 76 240) / 450 | ≤ 204.8 | ok |
| boot-to-green (ms) | 184 (empty) | **215** on the cleanly-flushed corpus | < 10 000 | ok |
| WAL replay lines after clean flush | — | **0** | 0 | ok |
| threads / fds | 340 / 15 | **336 / 15** | — | — |

N-independence is the story the CPU and wakeup rows tell: 450 indices cost
0.075 pp more CPU and 0.6 wakeups/s more than an empty node — both within
window noise on a box whose load average was 38–45 during the runs (recorded
in the result file). Per-thread attribution at N=450: `xerj-memtable-s`
0.083 %, `xerj-mem-sample` 0.058 %, `xerj-rt` 0.008 % — no thread class
scales with index count.

### What the first run caught: an O(N) idle metrics loop

The fixture's first full run against the then-current tree (same binary
version, before the fix; result preserved as
[`results/result-n450-with-metrics-gauge-loop.json`](./results/result-n450-with-metrics-gauge-loop.json))
**failed the CPU gate**:

| 450 idle indices | CPU (% of one core) | wakeups/s | per-index RSS (kB) |
|---|---|---|---|
| background gauge loop present (first run) | **1.12** (0.78 in an earlier window; load 40→63 during the runs) | 46.1 (24.9 vol + 21.2 nonvol) | 185.3 |
| per-thread attribution | `xerj-rt` 0.92, `xerj-memtable-s` 0.10, `xerj-mem-sample` 0.08 | | |
| loop disabled, binary otherwise identical (A/B, 4 windows) | **0.15–0.18** | 26.3–26.9 | — |
| loop deleted (the committed fix; run of record above) | **0.175** | 26.6 | 183.8 |

Root cause: `run_metrics_gauge_loop` — a 10 s ticker that fed the Prometheus
gauges (`xerj_doc_count`, `xerj_segment_count`, `xerj_wal_size_bytes`,
`xerj_memory_usage_bytes`) — walked **every index's WAL subtree** on every
tick: `read_dir` + one `metadata()` per WAL shard. At 450 indices × ~16
default shards that is ~7 400 `stat()` calls every 10 s on an async runtime
worker, and it scales with index count: 0.7–0.9 % of one core at N=450 idle,
against the issue's < 0.5 % budget line, with essentially all of it in the
`xerj-rt` (tokio) threads. The A/B row is the causal proof: same binary, loop
disabled by an env kill-switch, four consecutive 120 s windows.

The fix (this branch): the loop is **deleted**; `/v1/metrics` refreshes the
gauges at scrape time — the only moment they are observable — with the WAL
subtree walk on the blocking pool
(`engine/crates/xerj-api/src/es_compat.rs`, `refresh_metric_gauges`;
wired in `engine/crates/xerj-api/src/native.rs`). A scrape now costs what the
loop's tick used to, and idle time between scrapes costs nothing. Pinned by
`engine/crates/xerj-api/tests/metrics_gauges_refresh_at_scrape_time.rs`
(fail-before shape: with the loop gone and nothing refreshing at scrape time,
`xerj_doc_count` reads 0 on a node holding documents).

This is what the issue meant by "the point is catching O(N) regressions": a
per-index background walk is invisible at N=5 in a unit test and 2× the budget
at N=450.

### Thresholds and why

| gate | value | measured healthy | rationale |
|---|---|---|---|
| idle CPU | < 0.5 % of one core | 0.175 % at N=450 (0.10 baseline) | the issue's own line. ~3× above the fixed measurement, loose enough for a shared 2–4 vCPU runner, tight enough that the 0.9–1.1 % loop above fails it |
| wakeups | < 100/s process-wide | 26.6/s at N=450 (26.0 baseline) | "O(1)/s process-wide" made numeric. ~2× the worst healthy window seen (46/s under load-63 neighbours; ~26/s on the fixed binary). The #871-class regression (~115/s timer churn at 464 indices) fails it; a `GATE_WAKEUPS_PER_S=100` that flaps would get disabled, so it is set where only real per-index timers land |
| per-index RSS | ≤ 204.8 kB | 183.8 kB at N=450 | the issue's acceptance line, taken literally (0.2 MB = 204.8 kB in /proc units). 32 threads × default sharding is the *large* configuration — a 2–4 vCPU CI runner shards less |
| boot-to-green | < 10 000 ms | 215 ms at N=450 (184 empty) | ~45× the measured boot; the regression class is O(corpus) WAL replay, which lands in the tens of seconds, not the hundreds of ms |
| WAL replay after clean flush | 0 lines | 0 | a clean `POST /_flush` + SIGTERM checkpoint means nothing to replay; any `replayed WAL entries` line is the bug |

Overrides (`GATE_CPU_PERCENT`, `GATE_WAKEUPS_PER_S`,
`GATE_RSS_PER_INDEX_KB`, `GATE_BOOT_MS`) exist for local experiments; the
defaults are the CI contract. If a gate fails, the finding is the point — do
not move the line to make a run pass.

## Reproduce

```sh
cd engine && cargo build --release -p xerj-server && cd ..
XERJ_BIN=$PWD/engine/target/release/xerj bash benchmarks/idle-budget/run.sh
# smaller / faster variants:
REPOS=5  DATASETS=30 SETTLE_SECS=60 LABEL=n150 bash benchmarks/idle-budget/run.sh   # N-slope point
REPOS=15 DATASETS=30 SETTLE_SECS=20 LABEL=smoke bash benchmarks/idle-budget/run.sh  # ~3 min smoke
```

The script picks the first free base port in 9610..9639 (never :9200), refuses
a non-empty or RAM-backed data dir, and writes `results/result-<LABEL>.json`.
Expected tail of a green run:

```
== measured ==
  boot-to-green, empty node             184 ms
  boot-to-green,  450 indices              215 ms   (cleanly-flushed corpus)
  WAL replay lines after flush             0      (0 = nothing to replay)
  idle CPU,  450 indices               0.175 % of one core  (window 120.0s)
  idle CPU, baseline node                0.1 % of one core
  wakeups/s,  450 indices               26.6 /s   (voluntary 24.2/s + nonvoluntary 2.4/s)
  wakeups/s, baseline node              26.0 /s
  VmRSS,  450 indices                158944 kB   (VmHWM 173028 kB; anon 134704 kB + file 24240 kB)
  VmRSS, baseline node                 76240 kB
  per-index idle RSS                   183.8 kB   ((158944 - 76240) / 450)
  threads 336, fds 15, loadavg ['37.78', '39.23', '40.32']

== gate (coarse thresholds; the point is catching O(N) regressions, not ±1%) ==
  ok    CLAIMED  idle CPU < 0.5 % of one core (any N)
        MEASURED 0.175 % over 120.0s
  ok    CLAIMED  wakeups < 100/s process-wide (any N)
        MEASURED 26.6/s (vol 24.2 + nonvol 2.4)
  ok    CLAIMED  per-index idle RSS <= 204.8 kB (0.2 MB; total vs N)
        MEASURED 183.8 kB = (158944 - 76240) / 450
  ok    CLAIMED  boot-to-green < 10000 ms on a cleanly-flushed 450-index corpus
        MEASURED 215 ms (empty node boots in 184 ms)
  ok    CLAIMED  no O(corpus) WAL replay after a clean flush
        MEASURED 0 'replayed WAL entries' lines in the restart log
  ok   CLAIMED  450 indices
        MEASURED 450 (doc probe: 3 hits on one index)

IDLE-BUDGET GATE PASSED
```

CI: the `idle-budget` job in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)
builds `xerj-server` with the shared `ci-test` profile cache and runs the
fixture with its defaults (450 indices, 120 s settle, the thresholds above) on
`ubuntu-latest`, `timeout-minutes: 30` — the fixture itself is ~8 min measured.

## Result files

- [`results/result-n450indices.json`](./results/result-n450indices.json) — run of record, fixed binary, all gates green.
- [`results/result-n450-with-metrics-gauge-loop.json`](./results/result-n450-with-metrics-gauge-loop.json) — the failing first run (CPU 1.12 % > 0.5), kept as the before-artifact of the metrics-loop fix.

Each carries the binary version+path, corpus shape, boot times for all three
boots, both sample windows (including per-thread CPU attribution and the load
average at window start), and the computed per-index RSS.
