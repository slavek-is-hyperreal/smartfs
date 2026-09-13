#!/usr/bin/env bash
#
# run-great-smartfs-test.sh — one-command driver for The Great SmartFS Test.
#
# This is a WRAPPER, not one of the plan's scripts. It sets the environment up
# the way docs/testing/the-great-smartfs-test.md assumes, runs 00 through 04 in
# order, and collects the artifacts. It deliberately does NOT touch anything
# under scripts/testing/ — §7.3 of the plan says a script modified to make a
# stage pass is itself the finding, and that applies to this file too.
#
# Disk layout it enforces (the project owner's decision):
#   /dev/sda3 -> SmartFS only: the mount and the blob stores, nothing else.
#   ZFS pool  -> everything incidental: build output, tmp, daemon logs, results.
#
# Usage:  sudo scripts/run-great-smartfs-test.sh [options]
#
#   --skip-apt            do not install the xfstests build dependencies
#   --with-xfstests-run   also run ./check-smartfs -g generic (HOURS; off by default)
#   --stages "0 1 2"      run only these stages (default: 0 1 2 3 4)
#
# Environment passthrough: STOCHASTIC_ROUNDS (default 25, the plan's figure)
# lowers or raises Stage 4's random-kill loop. Anything below 25 is reduced
# coverage and is recorded as such in the run manifest.

set -uo pipefail
export LC_ALL=C

# ── must be root, and must know who called ─────────────────────────────────
if [[ "$(id -u)" -ne 0 ]]; then
  echo "run-great-smartfs-test: must run as root (stages 1-4 mount, chown and useradd)." >&2
  echo "  sudo scripts/run-great-smartfs-test.sh" >&2
  exit 1
fi
if [[ -z "${SUDO_USER:-}" || "$SUDO_USER" == "root" ]]; then
  echo "run-great-smartfs-test: run this via sudo from your normal account, not as root directly." >&2
  echo "  the script needs SUDO_USER to find your cargo toolchain and to hand files back afterwards." >&2
  exit 1
fi

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CALLER="$SUDO_USER"
CALLER_HOME="$(getent passwd "$CALLER" | cut -d: -f6)"

INSTALL_APT=1
RUN_XFSTESTS=0
STAGES="0 1 2 3 4"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-apt)          INSTALL_APT=0; shift ;;
    --with-xfstests-run) RUN_XFSTESTS=1; shift ;;
    --stages)            STAGES="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

C_RED=$'\033[31m'; C_GRN=$'\033[32m'; C_YEL=$'\033[33m'; C_BLD=$'\033[1m'; C_OFF=$'\033[0m'
say()  { printf '\n%s══ %s ══%s\n' "$C_BLD" "$*" "$C_OFF"; }
info() { printf '   %s\n' "$*"; }
warn() { printf '%s   ! %s%s\n' "$C_YEL" "$*" "$C_OFF"; }
bad()  { printf '%s   ✗ %s%s\n' "$C_RED" "$*" "$C_OFF" >&2; }

# ── safety interlock of our own, before the plan's ─────────────────────────
# The plan's preflight has its own checks; this one exists so we never even
# reach them pointed at the wrong disk. sdb/sdc hold the real ZFS pool and are
# never touched, directly or indirectly.
say "safety interlock"
BACKING_DEV="/dev/sda3"
EXPECT_LABEL="smartfs-test"
ACTUAL_LABEL="$(blkid -o value -s LABEL "$BACKING_DEV" 2>/dev/null || true)"
if [[ "$ACTUAL_LABEL" != "$EXPECT_LABEL" ]]; then
  bad "SAFETY STOP: ${BACKING_DEV} has label '${ACTUAL_LABEL:-<none>}', expected '${EXPECT_LABEL}'."
  bad "Later stages destroy data on that device. Refusing to guess. Check: lsblk -o NAME,LABEL,FSTYPE,SIZE,MOUNTPOINT"
  exit 1
fi
for crit in / /home; do
  src="$(findmnt -n -o SOURCE "$crit" 2>/dev/null | head -n1 || true)"
  if [[ -n "$src" && "$src" == "$BACKING_DEV" ]]; then
    bad "SAFETY STOP: ${BACKING_DEV} also backs ${crit}. Refusing to continue."
    exit 1
  fi
done
info "${BACKING_DEV} carries label '${EXPECT_LABEL}' and backs neither / nor /home"
info "ZFS pool devices (sdb/sdc) are not referenced by this run"

# ── layout ─────────────────────────────────────────────────────────────────
export SMARTFS_REPO="$REPO"
export SMARTFS_BACKING_DEV="$BACKING_DEV"
export SMARTFS_BACKING_MOUNT="/mnt/smartfs-test-backing"
export SMARTFS_MOUNT="/mnt/smartfs-test"
export SMARTFS_STORE_PATH="${SMARTFS_BACKING_MOUNT}/blobs"
export SMARTFS_DB_URL="${SMARTFS_DB_URL:-postgres://postgres:postgres@172.17.0.2:5432/smartfs}"
export RESULTS_ROOT="${REPO}/test-results"
export PG_CONTAINER="${PG_CONTAINER:-smartfs-test-db}"
export STOCHASTIC_ROUNDS="${STOCHASTIC_ROUNDS:-25}"

# Incidental storage on the pool, never on the test partition.
POOL_SCRATCH="${REPO}/test-results/_scratch"
export TMPDIR="${POOL_SCRATCH}/tmp"
XFS_LOG_DIR="${POOL_SCRATCH}/xfs-logs"

# Root's PATH is reset by sudoers secure_path, so cargo has to be found.
export CARGO_HOME="${CALLER_HOME}/.cargo"
export RUSTUP_HOME="${CALLER_HOME}/.rustup"
export PATH="${CARGO_HOME}/bin:${PATH}"

say "layout"
info "repo            ${REPO}"
info "SmartFS mount   ${SMARTFS_MOUNT}          (FUSE)"
info "blob store      ${SMARTFS_STORE_PATH}     (sda3 — SmartFS data only)"
info "results         ${RESULTS_ROOT}           (pool)"
info "tmp             ${TMPDIR}                 (pool)"
info "daemon logs     ${XFS_LOG_DIR}            (pool)"
info "cargo           $(command -v cargo || echo 'NOT FOUND')"
command -v cargo >/dev/null || { bad "cargo not on PATH even after adding ${CARGO_HOME}/bin"; exit 1; }

mkdir -p "$TMPDIR" "$XFS_LOG_DIR" "$RESULTS_ROOT" "$SMARTFS_STORE_PATH" "$SMARTFS_MOUNT"
chmod 1777 "$TMPDIR"

# xfstests' check-smartfs wrapper hardcodes /var/log/smartfs-xfs-*.log, and /
# has ~6 GB free. Symlinks move that traffic to the pool without touching the
# generated wrapper: >> follows a symlink and creates the target.
for role in test scratch; do
  ln -sfn "${XFS_LOG_DIR}/${role}.log" "/var/log/smartfs-xfs-${role}.log"
done
info "redirected /var/log/smartfs-xfs-*.log onto the pool (root fs has $(df -h / | awk 'NR==2{print $4}') free)"

# ── clean slate ────────────────────────────────────────────────────────────
say "clearing leftovers from any previous run"
pkill -x smartfsd 2>/dev/null && { info "killed a stray smartfsd"; sleep 2; } || true
for mp in "$SMARTFS_MOUNT" /mnt/smartfs-crash /mnt/smartfs-xfs-test /mnt/smartfs-xfs-scratch; do
  fusermount -u -z "$mp" 2>/dev/null || umount -l "$mp" 2>/dev/null || true
done
if [[ -n "$(ls -A "$SMARTFS_MOUNT" 2>/dev/null)" ]]; then
  bad "${SMARTFS_MOUNT} is not empty and is not a mountpoint. Inspect it by hand; refusing to delete."
  ls -la "$SMARTFS_MOUNT" >&2
  exit 1
fi

# ── build dependencies for xfstests (Stage 3) ──────────────────────────────
if [[ " $STAGES " == *" 3 "* && $INSTALL_APT -eq 1 ]]; then
  say "xfstests build dependencies"
  APT_PKGS=(libtool-bin uuid-dev libattr1-dev libacl1-dev libaio-dev libgdbm-dev
            xfslibs-dev e2fsprogs attr acl libssl-dev lz4 autoconf automake)
  MISSING=()
  for pkg in "${APT_PKGS[@]}"; do
    dpkg -s "$pkg" >/dev/null 2>&1 || MISSING+=("$pkg")
  done
  if (( ${#MISSING[@]} == 0 )); then
    info "all present, nothing to install"
  else
    warn "installing ${#MISSING[@]} package(s): ${MISSING[*]}"
    warn "(pass --skip-apt to do this yourself instead)"
    apt-get update -qq && apt-get install -y -qq "${MISSING[@]}" \
      || warn "apt install reported an error; Stage 3 may fail on missing deps"
  fi
fi

# ── pre-build as the caller, so root's cargo run is a no-op ────────────────
say "pre-build as ${CALLER} (keeps root from writing into target/)"
runuser -u "$CALLER" -- env PATH="$PATH" CARGO_HOME="$CARGO_HOME" RUSTUP_HOME="$RUSTUP_HOME" \
  "${REPO}/scripts/build.sh" 2>&1 | tail -4

# ── run the stages ─────────────────────────────────────────────────────────
chmod +x "${REPO}"/scripts/testing/*.sh
declare -A EXIT_CODE
STARTED_AT="$(date -Is)"

run_stage() {
  local num="$1" script="$2" label="$3"
  [[ " $STAGES " == *" $num "* ]] || { info "stage $num not selected, skipping"; return 0; }
  say "STAGE ${num} — ${label}"
  local log="${RESULTS_ROOT}/${num}-console.log"
  info "log: $log"
  set -o pipefail
  "${REPO}/scripts/testing/${script}" 2>&1 | tee "$log"
  local rc=${PIPESTATUS[0]}
  EXIT_CODE[$num]=$rc
  if (( rc == 0 )); then
    printf '%s   STAGE %s: exit 0%s\n' "$C_GRN" "$num" "$C_OFF"
  else
    printf '%s   STAGE %s: exit %s%s\n' "$C_RED" "$num" "$rc" "$C_OFF"
  fi
  return $rc
}

run_stage 0 00_preflight_checks.sh "preflight"
if [[ " $STAGES " == *" 0 "* && "${EXIT_CODE[0]}" -ne 0 ]]; then
  say "STOPPED"
  bad "Preflight failed (exit ${EXIT_CODE[0]}). Per §7.2 nothing below it may run."
  bad "Read ${RESULTS_ROOT}/0-console.log — it names exactly what was missing."
  bad "Do NOT work around it: an unmet precondition is the finding, not an obstacle."
  chown -R "$CALLER:$CALLER" "$RESULTS_ROOT" 2>/dev/null || true
  exit "${EXIT_CODE[0]}"
fi

# Stages 1-4 are independent of each other; a failure in one does not
# invalidate the next, so all of them run and every exit code is recorded.
run_stage 1 01_smoke_test.sh                  "smoke test (B-04 probe)"
run_stage 2 02_run_pjdfstest.sh               "pjdfstest (POSIX semantics)"
run_stage 3 03_setup_check_smartfs_xfstests.sh "xfstests scaffold"
run_stage 4 04_crash_consistency_test.sh      "crash consistency"

# ── optional: the multi-hour generic/ run ──────────────────────────────────
if (( RUN_XFSTESTS )) && [[ -x "${REPO}/third_party/xfstests/check-smartfs" ]]; then
  say "xfstests generic/ — this takes hours"
  ( cd "${REPO}/third_party/xfstests" && ./check-smartfs -g generic ) \
    2>&1 | tee "${RESULTS_ROOT}/3-generic-run.log"
  EXIT_CODE[3g]=${PIPESTATUS[0]}
fi

# ── manifest ───────────────────────────────────────────────────────────────
say "writing run manifest"
MANIFEST="${RESULTS_ROOT}/RUN-MANIFEST.txt"
{
  echo "The Great SmartFS Test — run manifest"
  echo "started            : ${STARTED_AT}"
  echo "finished           : $(date -Is)"
  echo "host               : $(uname -srm)  $(hostname)"
  echo "invoked by         : ${CALLER}"
  echo "repo commit        : $(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "repo dirty         : $(git -C "$REPO" status --porcelain 2>/dev/null | wc -l) file(s)"
  echo "smartfsd version   : $("${REPO}/target/debug/smartfsd" --version 2>/dev/null || echo 'not built')"
  echo "backing device     : ${SMARTFS_BACKING_DEV} (label ${EXPECT_LABEL})"
  echo "free on sda3       : $(df -h "$SMARTFS_BACKING_MOUNT" | awk 'NR==2{print $4}')"
  echo "free on /          : $(df -h / | awk 'NR==2{print $4}')"
  echo "PG_CONTAINER       : ${PG_CONTAINER}  (K12 enabled)"
  echo "STOCHASTIC_ROUNDS  : ${STOCHASTIC_ROUNDS}$( (( STOCHASTIC_ROUNDS < 25 )) && echo '   *** BELOW the plan figure of 25 — REDUCED COVERAGE ***')"
  echo "crash instrumentation: $("${REPO}/target/debug/smartfsd" --crash-points >/dev/null 2>&1 && echo 'present' || echo 'ABSENT (Stage 0b not implemented; Stage 4 stochastic-only by design)')"
  echo
  echo "stage exit codes (0 = pass, anything else = FAIL):"
  for k in 0 1 2 3 4 3g; do
    [[ -v EXIT_CODE[$k] ]] && printf '  stage %-3s : %s\n' "$k" "${EXIT_CODE[$k]}"
  done
  echo
  echo "Known-expected failures for this build, per docs/testing/the-great-smartfs-test.md:"
  echo "  stage 1 — smartfs-mcp has no [[bin]], so the B-04 probe dies before reaching a"
  echo "            verdict. Outcome is UNREACHABLE, not CONFIRMED (see plan section 1.1)."
  echo "  stage 4 — Stage 0b was deliberately not implemented, so K1-K11 are reported as a"
  echo "            coverage gap and only the stochastic phase runs. That is the designed"
  echo "            loud behaviour, not a malfunction."
} > "$MANIFEST"
cat "$MANIFEST"

# ── hand everything back ───────────────────────────────────────────────────
say "returning file ownership to ${CALLER}"
for d in "${REPO}/target" "${REPO}/third_party" "$RESULTS_ROOT" "$CARGO_HOME"; do
  [[ -e "$d" ]] && chown -R "$CALLER:$CALLER" "$d" 2>/dev/null || true
done
info "done"

say "SUMMARY"
FAILED=0
for k in 0 1 2 3 4 3g; do
  if [[ -v EXIT_CODE[$k] ]]; then
    if (( ${EXIT_CODE[$k]} == 0 )); then
      printf '%s  stage %-3s PASS%s\n' "$C_GRN" "$k" "$C_OFF"
    else
      printf '%s  stage %-3s FAIL (exit %s)%s\n' "$C_RED" "$k" "${EXIT_CODE[$k]}" "$C_OFF"
      FAILED=1
    fi
  fi
done
echo
info "artifacts: ${RESULTS_ROOT}"
info "manifest : ${MANIFEST}"
if (( ! RUN_XFSTESTS )) && [[ -x "${REPO}/third_party/xfstests/check-smartfs" ]]; then
  echo
  info "Stage 3 only scaffolded. The actual suite is hours and was not run:"
  info "  cd ${REPO}/third_party/xfstests && sudo ./check-smartfs -g generic"
  info "  (or re-run this script with --with-xfstests-run)"
fi
exit $FAILED
