#!/usr/bin/env bash
# lib_common.sh — shared helpers for The Great SmartFS Test.
#
# Sourced by every 0X_*.sh script. Defines the fail-loud contract:
#   * a missing precondition is a FAIL, never a skip
#   * "couldn't connect" is a FAIL, never a warning
#   * no function here may return success without having actually checked something
#
# See docs/testing/the-great-smartfs-test.md

set -euo pipefail
export LC_ALL=C

# ── Configuration (override via environment) ────────────────────────────────
SMARTFS_REPO="${SMARTFS_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
SMARTFS_MOUNT="${SMARTFS_MOUNT:-/mnt/smartfs-test}"
SMARTFS_BACKING_MOUNT="${SMARTFS_BACKING_MOUNT:-/mnt/smartfs-test-backing}"
SMARTFS_BACKING_DEV="${SMARTFS_BACKING_DEV:-/dev/sda3}"
SMARTFS_BACKING_LABEL="${SMARTFS_BACKING_LABEL:-smartfs-test}"
SMARTFS_STORE_PATH="${SMARTFS_STORE_PATH:-${SMARTFS_BACKING_MOUNT}/blobs}"
SMARTFS_DB_URL="${SMARTFS_DB_URL:-postgres://postgres:postgres@172.17.0.2:5432/smartfs}"
SMARTFS_DAEMON_BIN="${SMARTFS_DAEMON_BIN:-${SMARTFS_REPO}/target/debug/smartfsd}"
SMARTFS_CLI_BIN="${SMARTFS_CLI_BIN:-${SMARTFS_REPO}/target/debug/smartfs-cli}"
SMARTFS_MCP_BIN="${SMARTFS_MCP_BIN:-${SMARTFS_REPO}/target/debug/smartfs-mcp}"
RESULTS_ROOT="${RESULTS_ROOT:-${SMARTFS_REPO}/test-results}"

# ── Output ──────────────────────────────────────────────────────────────────
if [[ -t 1 ]]; then
  C_RED=$'\033[31m'; C_GRN=$'\033[32m'; C_YEL=$'\033[33m'
  C_BLD=$'\033[1m';  C_OFF=$'\033[0m'
else
  C_RED=''; C_GRN=''; C_YEL=''; C_BLD=''; C_OFF=''
fi

_STAGE_NAME="${_STAGE_NAME:-unnamed-stage}"
_CHECKS_RUN=0
_CHECKS_FAILED=0

stage_begin() {
  _STAGE_NAME="$1"
  _CHECKS_RUN=0
  _CHECKS_FAILED=0
  printf '\n%s=== %s ===%s\n' "$C_BLD" "$_STAGE_NAME" "$C_OFF"
}

log()  { printf '     %s\n' "$*"; }
info() { printf '%s---%s %s\n' "$C_BLD" "$C_OFF" "$*"; }

pass() {
  _CHECKS_RUN=$((_CHECKS_RUN + 1))
  printf '%s[PASS]%s %s\n' "$C_GRN" "$C_OFF" "$*"
}

# fail() records a failure and keeps going, so one run reports every problem.
fail() {
  _CHECKS_RUN=$((_CHECKS_RUN + 1))
  _CHECKS_FAILED=$((_CHECKS_FAILED + 1))
  printf '%s[FAIL]%s %s\n' "$C_RED" "$C_OFF" "$*" >&2
}

# die() is for a precondition so broken that continuing would produce
# meaningless results. Always exits non-zero. Never call this "skip".
die() {
  printf '%s[FATAL]%s %s\n' "$C_RED" "$C_OFF" "$*" >&2
  printf '%s[FATAL]%s stage %s aborted; this is a FAILURE, not a skip.\n' \
    "$C_RED" "$C_OFF" "$_STAGE_NAME" >&2
  exit 1
}

# note() is for context only. It must never be used to soften a failure.
note() { printf '%s[note]%s %s\n' "$C_YEL" "$C_OFF" "$*"; }

stage_end() {
  printf '\n%s--- %s summary: %d checks, %d failed ---%s\n' \
    "$C_BLD" "$_STAGE_NAME" "$_CHECKS_RUN" "$_CHECKS_FAILED" "$C_OFF"
  if (( _CHECKS_RUN == 0 )); then
    printf '%s[FAIL]%s %s executed ZERO checks. A stage that asserts nothing is a\n' \
      "$C_RED" "$C_OFF" "$_STAGE_NAME" >&2
    printf '       failed stage, not a passing one (this is the exact anti-pattern in\n' >&2
    printf '       dispatch_tests.rs that hid B-04). Exiting non-zero.\n' >&2
    exit 1
  fi
  if (( _CHECKS_FAILED > 0 )); then
    printf '%s%s: FAIL%s\n' "$C_RED" "$_STAGE_NAME" "$C_OFF" >&2
    exit 1
  fi
  printf '%s%s: PASS%s\n' "$C_GRN" "$_STAGE_NAME" "$C_OFF"
}

# ── Tooling ─────────────────────────────────────────────────────────────────
require_cmd() {
  local c
  for c in "$@"; do
    command -v "$c" >/dev/null 2>&1 || die "required command not found: $c"
  done
}

require_root() {
  [[ "$(id -u)" -eq 0 ]] || die "$_STAGE_NAME must run as root (needed for mount/chown tests)"
}

# ── Postgres ────────────────────────────────────────────────────────────────
# psql_q <sql> [db_url] -> tuples-only, unaligned output. Connection failure is fatal.
psql_q() {
  local sql="$1" url="${2:-$SMARTFS_DB_URL}"
  psql "$url" -qtAX -v ON_ERROR_STOP=1 -c "$sql" 2>&1 \
    || die "psql query failed against ${url%%\?*}: ${sql:0:120}"
}

# psql_try <sql> [db_url] -> like psql_q but returns non-zero instead of dying.
# Use only where the *failure itself* is the thing being asserted.
psql_try() {
  local sql="$1" url="${2:-$SMARTFS_DB_URL}"
  psql "$url" -qtAX -v ON_ERROR_STOP=1 -c "$sql" 2>&1
}

require_db() {
  local url="${1:-$SMARTFS_DB_URL}"
  require_cmd psql
  local out
  out="$(psql "$url" -qtAX -c 'SELECT 1' 2>&1)" \
    || die "cannot reach Postgres at ${url%%\?*}: $out
       This is a FAILURE. Do not continue and do not report this stage as skipped."
  [[ "$(printf '%s' "$out" | tr -d '[:space:]')" == "1" ]] \
    || die "Postgres reachable but 'SELECT 1' returned unexpected output: $out"
}

# ── Daemon lifecycle ────────────────────────────────────────────────────────
DAEMON_PID=""
DAEMON_READY_FILE=""

# start_daemon <mountpoint> <store_path> <db_url> [extra smartfsd args...]
start_daemon() {
  local mp="$1" store="$2" url="$3"; shift 3
  [[ -x "$SMARTFS_DAEMON_BIN" ]] \
    || die "no daemon binary at $SMARTFS_DAEMON_BIN — Stage 0 is not done.
       See docs/testing/the-great-smartfs-test.md section 1. smartfs-fuse is a
       library crate with no main.rs and no [[bin]]; nothing in this workspace
       can mount SmartFS until smartfsd exists."

  DAEMON_MOUNT="$mp"
  DAEMON_READY_FILE="$(mktemp -u "${TMPDIR:-/tmp}/smartfsd-ready.XXXXXX")"
  local logf="${RESULTS_ROOT}/${_STAGE_NAME}/smartfsd.log"
  mkdir -p "$(dirname "$logf")"

  "$SMARTFS_DAEMON_BIN" \
    --mountpoint "$mp" \
    --database-url "$url" \
    --store-path "$store" \
    --ready-file "$DAEMON_READY_FILE" \
    --foreground "$@" >>"$logf" 2>&1 &
  DAEMON_PID=$!

  local waited=0
  while (( waited < 30 )); do
    if [[ -f "$DAEMON_READY_FILE" ]]; then
      pass "smartfsd ready (pid $DAEMON_PID, mount $mp)"
      log "  $(cat "$DAEMON_READY_FILE" 2>/dev/null | tr '\n' ' ')"
      return 0
    fi
    kill -0 "$DAEMON_PID" 2>/dev/null \
      || die "smartfsd exited during startup (exit $(wait "$DAEMON_PID" 2>/dev/null; echo $?)); see $logf"
    sleep 1; waited=$((waited + 1))
  done
  die "smartfsd did not become ready within 30s; see $logf"
}

stop_daemon() {
  [[ -n "$DAEMON_PID" ]] || return 0
  kill -TERM "$DAEMON_PID" 2>/dev/null || true
  local waited=0
  while kill -0 "$DAEMON_PID" 2>/dev/null && (( waited < 15 )); do
    sleep 1; waited=$((waited + 1))
  done
  kill -0 "$DAEMON_PID" 2>/dev/null && kill -9 "$DAEMON_PID" 2>/dev/null || true
  wait "$DAEMON_PID" 2>/dev/null || true
  DAEMON_PID=""
  [[ -n "$DAEMON_READY_FILE" ]] && rm -f "$DAEMON_READY_FILE"
  # AutoUnmount should handle this; be certain anyway (lazy unmount clears broken endpoints).
  local active_mp="${DAEMON_MOUNT:-$SMARTFS_MOUNT}"
  fusermount -u -z "$active_mp" 2>/dev/null || umount -l "$active_mp" 2>/dev/null || true
  if [[ "$active_mp" != "$SMARTFS_MOUNT" ]]; then
    fusermount -u -z "$SMARTFS_MOUNT" 2>/dev/null || umount -l "$SMARTFS_MOUNT" 2>/dev/null || true
  fi
  return 0
}

quiesce_queue() {
  local store="${1:-$SMARTFS_STORE_PATH}"
  local q="${store}/pending/queue"
  local waited=0
  while (( waited < 100 )); do
    if [[ ! -d "$q" ]] || [[ -z "$(ls -A "$q" 2>/dev/null)" ]]; then
      return 0
    fi
    sleep 0.05
    waited=$((waited + 1))
  done
  # A queue that will not drain is a real failure of the system under test, not
  # a timing inconvenience. Returning non-zero here used to trip the caller's
  # `set -e` and abort the stage with no [FAIL] line and no explanation, which
  # turned a meaningful result into an unexplained exit. Record it and let the
  # stage carry on to its summary.
  fail "pending queue did not quiesce within 5s at ${q}
          $(ls -1 "$q" 2>/dev/null | head -3 | sed 's/^/            /')
          The drain is stuck or too slow; SQL assertions after this point are
          reading a database that is behind the filesystem."
  return 0
}

require_live_mount() {
  local mp="${1:-$SMARTFS_MOUNT}"
  mountpoint -q "$mp" \
    || die "$mp is not a mountpoint. The filesystem under test is not running.
       This is a FAILURE, not a skip."
  local probe="${mp}/.smartfs-liveness-$$"
  echo probe > "$probe" 2>/dev/null \
    || die "$mp is mounted but not writable — mount is not serving"
  rm -f "$probe" 2>/dev/null || true
  pass "live SmartFS mount at $mp is serving I/O"
}

sha256_of() { sha256sum "$1" | awk '{print $1}'; }

results_dir() {
  local d="${RESULTS_ROOT}/$1"
  mkdir -p "$d"
  printf '%s' "$d"
}
