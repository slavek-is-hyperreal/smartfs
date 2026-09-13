#!/usr/bin/env bash
#
# 05_perf_diagnostics.sh — The Great SmartFS Test, diagnostics (NOT a gate).
#
# Records timings. It does not assert thresholds and it does not fail on a slow
# number, deliberately: a performance threshold inside a correctness suite
# creates pressure to trade correctness for speed, and on a shared machine the
# numbers are noisy enough that such a gate would mostly measure what else is
# running. What it DOES fail on is its own inability to measure — an unusable
# mount or a missing daemon is still a FAIL, per the Fail-Loud Rule.
#
# Why this exists: ADR-58 rewrote the write path for latency and nobody ever
# measured it. The headline number below is close() latency, which is exactly
# what that ADR moved — release() used to wait on a Postgres transaction and now
# returns once a marker is durable. If ADR-58 did not pay for itself, this is
# where it shows.
#
# Output: test-results/stage5-perf/metrics.json, plus a diff against the
# committed baseline at scripts/testing/perf-baseline.json when one exists.
# Refresh the baseline deliberately, never automatically:
#     cp test-results/stage5-perf/metrics.json scripts/testing/perf-baseline.json
#
# Usage: sudo scripts/testing/05_perf_diagnostics.sh
# See:   docs/testing/the-great-smartfs-test.md

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib_common.sh"

stage_begin "stage5-perf"
require_cmd psql python3 dd
require_root
require_db "$SMARTFS_DB_URL"

RES="$(results_dir stage5-perf)"
BASELINE="${SMARTFS_REPO}/scripts/testing/perf-baseline.json"
SAMPLES="${PERF_SAMPLES:-200}"

# Debug level on the write-phase target only: the daemon logs one fixed-format
# line per committed write, which section 1b aggregates. A single close()
# latency number says which step is slow only by accident; six say it directly.
export RUST_LOG="${RUST_LOG:-info,smartfs::write_phases=debug}"

trap stop_daemon EXIT
start_daemon "$SMARTFS_MOUNT" "$SMARTFS_STORE_PATH" "$SMARTFS_DB_URL" --no-semantic
require_live_mount "$SMARTFS_MOUNT"

WORK="${SMARTFS_MOUNT}/perf-$$"
mkdir -p "$WORK" || die "mkdir on the mount failed; cannot measure anything"

# ns_now / elapsed_ms keep the arithmetic in the shell rather than spawning a
# process per sample, which would dominate the very latency being measured.
ns_now() { date +%s%N; }

declare -A METRIC

# ── 1. close() latency: the number ADR-58 set out to change ────────────────
info "1  close() latency over ${SAMPLES} small writes"
LAT_FILE="${RES}/close-latency-ns.txt"
: > "$LAT_FILE"
for i in $(seq 1 "$SAMPLES"); do
  t0="$(ns_now)"
  printf 'perf sample %s padding-to-something-realistic-%s' "$i" "$RANDOM" > "${WORK}/lat-${i}.txt"
  t1="$(ns_now)"
  echo $(( t1 - t0 )) >> "$LAT_FILE"
done
read -r LAT_P50 LAT_P95 LAT_P99 LAT_MEAN <<<"$(
  python3 - "$LAT_FILE" <<'PY'
import sys, statistics
v = sorted(int(x) for x in open(sys.argv[1]) if x.strip())
def pct(p): return v[min(len(v) - 1, int(len(v) * p))]
print(pct(.50) / 1e6, pct(.95) / 1e6, pct(.99) / 1e6, statistics.mean(v) / 1e6)
PY
)"
METRIC[close_latency_p50_ms]="$LAT_P50"
METRIC[close_latency_p95_ms]="$LAT_P95"
METRIC[close_latency_p99_ms]="$LAT_P99"
METRIC[close_latency_mean_ms]="$LAT_MEAN"
pass "close() latency recorded: p50=${LAT_P50}ms p95=${LAT_P95}ms p99=${LAT_P99}ms"

# ── 1b. where that latency actually goes ───────────────────────────────────
info "1b where the write path spends its time"
PHASE_LOG="${RESULTS_ROOT}/${_STAGE_NAME}/smartfsd.log"
if grep -q "phases_us" "$PHASE_LOG" 2>/dev/null; then
  # Median per phase, so one slow outlier cannot dominate the picture.
  while IFS='=' read -r phase value; do
    [[ -z "$phase" ]] && continue
    METRIC["write_phase_${phase}_ms"]="$value"
    log "  ${phase}: ${value} ms (median)"
  done < <(
    grep -o 'phases_us .*' "$PHASE_LOG" | python3 -c '
import sys, statistics
cols = {}
for line in sys.stdin:
    for field in line.split()[1:]:
        if "=" not in field:
            continue
        k, v = field.split("=", 1)
        if v.isdigit():
            cols.setdefault(k, []).append(int(v))
for k, vals in cols.items():
    print(f"{k}={round(statistics.median(vals) / 1000, 3)}")
'
  )
  pass "write path broken down into phases"
else
  note "no phase lines in ${PHASE_LOG}; the daemon may predate the instrumentation"
fi

# ── 2. queue drain: how far behind Postgres runs ───────────────────────────
info "2  time for the pending queue to drain after the burst"
# Deliberately NOT lib_common's quiesce_queue: that one records a [FAIL] when
# the queue will not drain, which is right for a correctness stage and wrong
# here. A slow drain is the measurement, not a verdict. This stage waits far
# longer and reports what it saw.
DRAIN_Q="${SMARTFS_STORE_PATH}/pending/queue"
DRAIN_T0="$(ns_now)"
DRAIN_WAITED=0
while (( DRAIN_WAITED < 1200 )); do            # up to 60s at 50ms
  [[ ! -d "$DRAIN_Q" || -z "$(ls -A "$DRAIN_Q" 2>/dev/null)" ]] && break
  sleep 0.05; DRAIN_WAITED=$((DRAIN_WAITED + 1))
done
DRAIN_MS="$(python3 -c "print(round(($(ns_now) - $DRAIN_T0) / 1e6, 1))")"
METRIC[queue_drain_ms]="$DRAIN_MS"
if (( DRAIN_WAITED >= 1200 )); then
  METRIC[queue_drained_fully]=0
  note "queue still had $(ls -A "$DRAIN_Q" 2>/dev/null | wc -l) marker(s) after 60s — recorded, not failed"
else
  METRIC[queue_drained_fully]=1
fi
pass "queue drain after ${SAMPLES} writes: ${DRAIN_MS}ms"

# ── 3. sequential throughput, compressible and incompressible ──────────────
# Both, because ADR-59's whole premise is that general-purpose compression is
# a waste on already-compressed payloads. This is where that shows up.
measure_throughput() {
  local label="$1" src="$2" mb="$3" t0 t1
  t0="$(ns_now)"
  cp "$src" "${WORK}/tp-${label}.bin"
  sync
  t1="$(ns_now)"
  python3 -c "print(round($mb / ((($t1 - $t0)) / 1e9), 2))"
}
DD_MB=64
dd if=/dev/zero    of="${TMPDIR:-/tmp}/perf-zero.bin"   bs=1M count=$DD_MB status=none
dd if=/dev/urandom of="${TMPDIR:-/tmp}/perf-random.bin" bs=1M count=$DD_MB status=none
METRIC[write_compressible_mbps]="$(measure_throughput compressible "${TMPDIR:-/tmp}/perf-zero.bin" $DD_MB)"
METRIC[write_incompressible_mbps]="$(measure_throughput incompressible "${TMPDIR:-/tmp}/perf-random.bin" $DD_MB)"
pass "write throughput: compressible ${METRIC[write_compressible_mbps]} MB/s, incompressible ${METRIC[write_incompressible_mbps]} MB/s"

# ── 4. read-back throughput (cold-ish: the buffer is gone after release) ───
READ_T0="$(ns_now)"
cat "${WORK}/tp-incompressible.bin" > /dev/null
METRIC[read_incompressible_mbps]="$(python3 -c "print(round($DD_MB / ((($(ns_now)) - $READ_T0) / 1e9), 2))")"
pass "read throughput: ${METRIC[read_incompressible_mbps]} MB/s"

# ── 5. metadata rate ───────────────────────────────────────────────────────
info "5  metadata operations"
META_T0="$(ns_now)"
for i in $(seq 1 100); do mkdir -p "${WORK}/d-${i}"; done
METRIC[mkdir_per_sec]="$(python3 -c "print(round(100 / ((($(ns_now)) - $META_T0) / 1e9), 1))")"
STAT_T0="$(ns_now)"
for i in $(seq 1 100); do stat "${WORK}/lat-${i}.txt" > /dev/null; done
METRIC[stat_per_sec]="$(python3 -c "print(round(100 / ((($(ns_now)) - $STAT_T0) / 1e9), 1))")"
pass "metadata: ${METRIC[mkdir_per_sec]} mkdir/s, ${METRIC[stat_per_sec]} stat/s"

# ── emit ───────────────────────────────────────────────────────────────────
{
  echo "{"
  echo "  \"recorded_at\": \"$(date -Is)\","
  echo "  \"commit\": \"$(git -C "$SMARTFS_REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)\","
  echo "  \"samples\": ${SAMPLES},"
  first=1
  for k in "${!METRIC[@]}"; do
    (( first )) || echo ","
    first=0
    printf '  "%s": %s' "$k" "${METRIC[$k]}"
  done
  echo
  echo "}"
} > "${RES}/metrics.json"
cat "${RES}/metrics.json"

# ── compare against the committed baseline, report only ────────────────────
if [[ -f "$BASELINE" ]]; then
  info "comparison against ${BASELINE}"
  python3 - "$BASELINE" "${RES}/metrics.json" <<'PY'
import json, sys
base = json.load(open(sys.argv[1])); now = json.load(open(sys.argv[2]))
# For latency and drain time lower is better; for the rest higher is better.
lower_is_better = ("latency", "drain")
for k in sorted(set(base) & set(now)):
    if not isinstance(base[k], (int, float)):
        continue
    b, n = float(base[k]), float(now[k])
    if b == 0:
        continue
    delta = (n - b) / b * 100
    better = (n < b) if any(t in k for t in lower_is_better) else (n > b)
    mark = "improved" if better else "REGRESSED"
    if abs(delta) < 10:
        mark = "flat"
    print(f"  {k:34} {b:>10.2f} -> {n:>10.2f}  ({delta:+.1f}%)  {mark}")
PY
  note "differences above are reported, never enforced — this stage is diagnostics"
else
  note "no baseline at ${BASELINE}; to adopt this run as the baseline, copy"
  note "  cp ${RES}/metrics.json ${BASELINE}"
fi

rm -rf "$WORK" "${TMPDIR:-/tmp}/perf-zero.bin" "${TMPDIR:-/tmp}/perf-random.bin" 2>/dev/null || true
info "artifacts written to ${RES}"
stage_end
