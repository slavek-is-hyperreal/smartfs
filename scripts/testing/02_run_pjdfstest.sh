#!/usr/bin/env bash
#
# 02_run_pjdfstest.sh — The Great SmartFS Test, Stage 2.
#
# Runs pjdfstest (https://github.com/pjd/pjdfstest) against a live SmartFS mount,
# with a plain-ext4 baseline run first so environment noise cannot be mistaken
# for a SmartFS defect.
#
# Failures are triaged against scripts/testing/pjdfstest-expected-failures.txt.
# Any failure NOT in that file fails the stage. Any entry in that file without a
# justification comment also fails the stage - an unexplained exclusion is how a
# suite quietly stops testing anything.
#
# Usage: sudo scripts/testing/02_run_pjdfstest.sh
# See:   docs/testing/the-great-smartfs-test.md section 3

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib_common.sh"

stage_begin "stage2-pjdfstest"
require_cmd git make cc prove perl awk
require_root   # pjdfstest needs root for chown / setuid / sticky-bit cases

RES="$(results_dir stage2-pjdfstest)"
PJD_DIR="${PJD_DIR:-${SMARTFS_REPO}/third_party/pjdfstest}"
EXPECTED_FAILURES="${SMARTFS_REPO}/scripts/testing/pjdfstest-expected-failures.txt"

# ── obtain and build the suite ──────────────────────────────────────────────
info "obtaining pjdfstest"
if [[ ! -d "$PJD_DIR/.git" ]]; then
  mkdir -p "$(dirname "$PJD_DIR")"
  git clone --depth 1 https://github.com/pjd/pjdfstest "$PJD_DIR" \
    || die "could not clone pjdfstest. No network access is a FAILURE of this stage,
       not a reason to skip it - vendor the suite into third_party/ and re-run."
  pass "cloned pjdfstest into $PJD_DIR"
else
  pass "pjdfstest already present at $PJD_DIR"
fi

if [[ ! -x "${PJD_DIR}/pjdfstest" ]]; then
  info "building pjdfstest"
  ( cd "$PJD_DIR" && autoreconf -ifs >/dev/null 2>&1 || true
    ./configure >"${RES}/configure.log" 2>&1 || true
    make pjdfstest >"${RES}/build.log" 2>&1 ) \
    || die "pjdfstest build failed; see ${RES}/build.log"
fi
[[ -x "${PJD_DIR}/pjdfstest" ]] || die "pjdfstest binary still missing after build"
pass "pjdfstest binary built"

# failure_ids <tap-file> -> stable "<suite>/<file>.t:<n>" identifiers, one per line.
#
# The obvious extraction — strip "not ok N - " and keep the description — does
# not work, and quietly. pjdfstest names every file it creates with a fresh
# random suffix, so a description reads
#
#   tried 'mkfifo pjdfstest_9a02…b201', expected 0, got EOPNOTSUPP
#
# and never repeats between runs. 132 of 184 entries looked like that, which
# means pjdfstest-expected-failures.txt could never match them and "NEW
# failures" was always ~everything. The plan's finish line — every failure
# classified, unjustified entries fail the stage — was unreachable as written.
#
# The test file plus the assertion number is stable: pjdfstest runs a fixed
# sequence, so rename/09.t:24 asserts the same thing every time.
failure_ids() {
  awk '
    match($0, /tests\/([a-z0-9_]+\/[0-9]+\.t)/, m) { cur = m[1]; next }
    /^not ok [0-9]+/ {
      if (cur != "") { n = $3; sub(/[^0-9].*$/, "", n); print cur ":" n }
    }
  ' "$1" | sort -u
}

# run_suite <label> <dir-to-test> -> writes <label>.tap, echoes "pass fail"
run_suite() {
  local label="$1" target="$2"
  local tapf="${RES}/${label}.tap"
  mkdir -p "${target}/pjd-run"
  ( cd "${target}/pjd-run" && prove -rv "${PJD_DIR}/tests" ) >"$tapf" 2>&1 || true
  rm -rf "${target}/pjd-run" 2>/dev/null || true

  local ok notok
  ok="$(grep -cE '^ok ' "$tapf" || true)"
  notok="$(grep -cE '^not ok ' "$tapf" || true)"
  printf '%s %s' "${ok:-0}" "${notok:-0}"
}

# ── baseline: plain ext4 ────────────────────────────────────────────────────
info "baseline run against plain ext4 at ${SMARTFS_BACKING_MOUNT}"
mountpoint -q "$SMARTFS_BACKING_MOUNT" \
  || die "${SMARTFS_BACKING_MOUNT} is not mounted; run 00_preflight_checks.sh first"

read -r EXT4_OK EXT4_FAIL <<<"$(run_suite ext4-baseline "$SMARTFS_BACKING_MOUNT")"
(( EXT4_OK > 0 )) \
  || die "the ext4 baseline produced zero passing tests - the harness itself is broken.
       Nothing measured against SmartFS afterwards would mean anything.
       See ${RES}/ext4-baseline.tap"
pass "ext4 baseline: ${EXT4_OK} ok, ${EXT4_FAIL} not ok"

# Tests that already fail on plain ext4 are environment problems; record their
# names so they can be excluded from the SmartFS comparison.
failure_ids "${RES}/ext4-baseline.tap" > "${RES}/ext4-known-bad.txt" || true

# ── SmartFS run ─────────────────────────────────────────────────────────────
trap stop_daemon EXIT
start_daemon "$SMARTFS_MOUNT" "$SMARTFS_STORE_PATH" "$SMARTFS_DB_URL" --allow-other
require_live_mount "$SMARTFS_MOUNT"

info "pjdfstest run against SmartFS at ${SMARTFS_MOUNT}"
read -r FS_OK FS_FAIL <<<"$(run_suite smartfs "$SMARTFS_MOUNT")"
(( FS_OK + FS_FAIL > 0 )) \
  || die "pjdfstest produced no results at all against SmartFS. Zero executed tests is a
       FAILURE, never a pass. See ${RES}/smartfs.tap"
pass "SmartFS run completed: ${FS_OK} ok, ${FS_FAIL} not ok"

failure_ids "${RES}/smartfs.tap" > "${RES}/smartfs-failures.txt" || true

# Keep the human-readable diagnostics alongside the ids, so triage does not
# require re-reading the raw TAP to find out what a given id actually did.
grep -E '^not ok ' "${RES}/smartfs.tap" > "${RES}/smartfs-failures-verbose.txt" || true

# ── triage ──────────────────────────────────────────────────────────────────
info "triage"

if [[ ! -f "$EXPECTED_FAILURES" ]]; then
  cat > "$EXPECTED_FAILURES" <<'EOF'
# pjdfstest expected failures for SmartFS.
#
# Format:  <suite>/<file>.t:<n><TAB># <category>: <one-line justification>
#          e.g.  link/00.t:12\t# LIMITATION: hardlinks unsupported, see ADR-59 point 8
#
# Identified by test file and assertion number, not by the failure text:
# pjdfstest embeds a fresh random filename in every message, so text never
# repeats between runs. See failure_ids() in 02_run_pjdfstest.sh.
# Category is exactly one of:
#     LIMITATION   - intentional, documented SmartFS MVP limitation
#     FUSE         - inherent to FUSE, outside SmartFS's control
#
# A "real SmartFS bug" NEVER belongs in this file. File it as a bug instead.
# An entry without a justification comment fails Stage 2 on purpose.
EOF
  note "created an empty ${EXPECTED_FAILURES}; every failure below is currently unclassified"
fi

# An entry with no justification is itself a finding.
UNJUSTIFIED="$(grep -vE '^\s*(#|$)' "$EXPECTED_FAILURES" | grep -vE '#\s*(LIMITATION|FUSE):' || true)"
if [[ -n "$UNJUSTIFIED" ]]; then
  fail "expected-failures entries without a LIMITATION:/FUSE: justification:
$(printf '%s\n' "$UNJUSTIFIED" | sed 's/^/          /')"
else
  pass "every expected-failures entry carries a justification"
fi

grep -vE '^\s*(#|$)' "$EXPECTED_FAILURES" | sed -E 's/[[:space:]]*#.*$//' \
  | sed -E 's/[[:space:]]+$//' | sort -u > "${RES}/expected.txt" || true

comm -23 "${RES}/smartfs-failures.txt" "${RES}/expected.txt" > "${RES}/unexpected-raw.txt" || true
comm -23 "${RES}/unexpected-raw.txt" "${RES}/ext4-known-bad.txt" > "${RES}/unexpected.txt" || true

NEW_FAILS="$(wc -l < "${RES}/unexpected.txt" | tr -d ' ')"
STALE="$(comm -13 "${RES}/smartfs-failures.txt" "${RES}/expected.txt" | wc -l | tr -d ' ')"

{
  echo "pjdfstest / SmartFS"
  echo "ext4 baseline : ${EXT4_OK} ok, ${EXT4_FAIL} not ok"
  echo "smartfs       : ${FS_OK} ok, ${FS_FAIL} not ok"
  echo "expected fails: $(wc -l < "${RES}/expected.txt" | tr -d ' ')"
  echo "ext4-known-bad excluded: $(wc -l < "${RES}/ext4-known-bad.txt" | tr -d ' ')"
  echo "NEW failures  : ${NEW_FAILS}"
  echo "stale entries : ${STALE}"
} | tee "${RES}/summary.txt"

if (( NEW_FAILS > 0 )); then
  fail "${NEW_FAILS} unexpected pjdfstest failures (not in expected-failures, not failing on ext4):
$(sed 's/^/          /' "${RES}/unexpected.txt")
          Triage each into: real bug / LIMITATION / FUSE, then either fix it or add it
          to ${EXPECTED_FAILURES} WITH a justification."
else
  pass "no unexpected pjdfstest failures"
fi

if (( STALE > 0 )); then
  note "${STALE} expected-failures entries no longer fail; prune them so the file keeps meaning something"
fi

info "artifacts written to ${RES}"
stage_end
