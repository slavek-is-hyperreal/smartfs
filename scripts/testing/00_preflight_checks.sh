#!/usr/bin/env bash
#
# 00_preflight_checks.sh — The Great SmartFS Test, prerequisites.
#
# Verifies the scratch partition, the Postgres container, and the build BEFORE
# any stage runs. Exits non-zero and loudly on any failure. There is no
# "skipped" outcome anywhere in this file, by design.
#
# THIS SCRIPT IS DESTRUCTIVE-ADJACENT: it confirms which partition later stages
# are allowed to destroy. It refuses to continue if that identification is
# ambiguous in any way.
#
# Usage: sudo scripts/testing/00_preflight_checks.sh
# See:   docs/testing/the-great-smartfs-test.md section 7.1

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib_common.sh"

stage_begin "stage0-preflight"

require_cmd blkid findmnt lsblk psql cargo sha256sum awk

# ── 1. The scratch partition ────────────────────────────────────────────────
info "1/3  scratch partition (${SMARTFS_BACKING_DEV}, label '${SMARTFS_BACKING_LABEL}')"

[[ -b "$SMARTFS_BACKING_DEV" ]] \
  || die "${SMARTFS_BACKING_DEV} is not a block device. Refusing to guess an alternative."

DEV_FSTYPE="$(blkid -o value -s TYPE "$SMARTFS_BACKING_DEV" 2>/dev/null || true)"
DEV_LABEL="$(blkid -o value -s LABEL "$SMARTFS_BACKING_DEV" 2>/dev/null || true)"

[[ "$DEV_FSTYPE" == "ext4" ]] \
  || fail "${SMARTFS_BACKING_DEV} fstype is '${DEV_FSTYPE:-<none>}', expected ext4"
[[ "$DEV_FSTYPE" == "ext4" ]] && pass "${SMARTFS_BACKING_DEV} is ext4" || true

# The label is the primary safety interlock. Do not relax this check.
if [[ "$DEV_LABEL" != "$SMARTFS_BACKING_LABEL" ]]; then
  die "SAFETY STOP: ${SMARTFS_BACKING_DEV} has label '${DEV_LABEL:-<none>}', expected
       '${SMARTFS_BACKING_LABEL}'. Later stages DESTROY data on this device. An
       unexpected label means this may not be the disposable test partition.
       Refusing to continue. Verify with: lsblk -o NAME,LABEL,FSTYPE,SIZE,MOUNTPOINT"
fi
pass "${SMARTFS_BACKING_DEV} carries the expected label '${SMARTFS_BACKING_LABEL}'"

# Mounted where we expect?
if ! findmnt -n -S "$SMARTFS_BACKING_DEV" >/dev/null 2>&1; then
  die "${SMARTFS_BACKING_DEV} is not mounted anywhere.
       Mount it first:  mkdir -p ${SMARTFS_BACKING_MOUNT} && mount ${SMARTFS_BACKING_DEV} ${SMARTFS_BACKING_MOUNT}
       (The plan assumes ${SMARTFS_BACKING_MOUNT}; override with SMARTFS_BACKING_MOUNT=...)"
fi
ACTUAL_MP="$(findmnt -n -o TARGET -S "$SMARTFS_BACKING_DEV" | head -n1)"
[[ "$ACTUAL_MP" == "$SMARTFS_BACKING_MOUNT" ]] \
  || die "${SMARTFS_BACKING_DEV} is mounted at '${ACTUAL_MP}', expected '${SMARTFS_BACKING_MOUNT}'.
       Refusing to continue against an unexpected mountpoint."
pass "${SMARTFS_BACKING_DEV} mounted at ${SMARTFS_BACKING_MOUNT}"

# ── The real-data-pool interlock ────────────────────────────────────────────
# Confirm the scratch device is not backing / or /home. If it were, the later
# stages would destroy the machine.
ROOT_SRC="$(findmnt -n -o SOURCE / | head -n1)"
HOME_SRC="$(findmnt -n -o SOURCE /home 2>/dev/null | head -n1 || true)"
BACK_SRC="$(findmnt -n -o SOURCE "$SMARTFS_BACKING_MOUNT" | head -n1)"

for pair in "/:${ROOT_SRC}" "/home:${HOME_SRC}"; do
  crit="${pair%%:*}"; src="${pair#*:}"
  [[ -z "$src" ]] && continue
  if [[ "$src" == "$BACK_SRC" || "$src" == "$SMARTFS_BACKING_DEV" ]]; then
    die "SAFETY STOP: ${SMARTFS_BACKING_DEV} also backs ${crit}. This is the real
       data pool, not a disposable test target. Refusing to continue."
  fi
done
pass "scratch device is distinct from / and /home (root=${ROOT_SRC}, scratch=${BACK_SRC})"

# Enough room to be useful.
AVAIL_MB="$(df -Pm "$SMARTFS_BACKING_MOUNT" | awk 'NR==2{print $4}')"
if (( AVAIL_MB < 10240 )); then
  fail "only ${AVAIL_MB} MB free on ${SMARTFS_BACKING_MOUNT}; xfstests generic/ needs ~10 GB+"
else
  pass "${AVAIL_MB} MB free on ${SMARTFS_BACKING_MOUNT}"
fi

# Blob store dir + the SmartFS mountpoint itself.
mkdir -p "$SMARTFS_STORE_PATH"
[[ -w "$SMARTFS_STORE_PATH" ]] || fail "blob store path ${SMARTFS_STORE_PATH} is not writable"
[[ -w "$SMARTFS_STORE_PATH" ]] && pass "blob store path ${SMARTFS_STORE_PATH} writable" || true

mkdir -p "$SMARTFS_MOUNT"
if mountpoint -q "$SMARTFS_MOUNT"; then
  fail "${SMARTFS_MOUNT} is already a mountpoint; unmount it before running the stages"
elif [[ -n "$(ls -A "$SMARTFS_MOUNT" 2>/dev/null)" ]]; then
  fail "${SMARTFS_MOUNT} is not empty; FUSE will refuse to mount over it"
else
  pass "${SMARTFS_MOUNT} exists, is empty, and is not currently mounted"
fi

# ── 2. Postgres ─────────────────────────────────────────────────────────────
info "2/3  Postgres at ${SMARTFS_DB_URL%%\?*}"

require_db "$SMARTFS_DB_URL"
pass "Postgres reachable and answering SELECT 1"

PG_VER="$(psql_q "SHOW server_version_num")"
if (( PG_VER < 160000 )); then
  fail "Postgres server_version_num=${PG_VER}; SmartFS v6.0 requires 16+"
else
  pass "Postgres version ok (server_version_num=${PG_VER})"
fi

HAS_VECTOR="$(psql_q "SELECT count(*) FROM pg_extension WHERE extname = 'vector'")"
if [[ "$HAS_VECTOR" != "1" ]]; then
  fail "pgvector extension is not installed in this database"
else
  pass "pgvector extension present"
fi

# Core tables from migrations 001-006 must already exist. Root Invariant #4:
# nothing at runtime may create them, so their absence is a hard stop here.
MISSING_TABLES=""
for t in storage_backends inode_registry file_versions blobs; do
  present="$(psql_q "SELECT count(*) FROM information_schema.tables
                     WHERE table_schema = 'public' AND table_name = '${t}'")"
  [[ "$present" == "1" ]] || MISSING_TABLES="${MISSING_TABLES} ${t}"
done
if [[ -n "$MISSING_TABLES" ]]; then
  die "missing core tables:${MISSING_TABLES}
       Apply migrations/001_core_schema.sql .. 006_fulltext_search.sql in order.
       Do NOT let anything create them at runtime — that violates Root Invariant #4."
fi
pass "core tables present (storage_backends, inode_registry, file_versions, blobs)"

# FIX-01 regression guard: blobs.refcount must NOT exist.
HAS_REFCOUNT="$(psql_q "SELECT count(*) FROM information_schema.columns
                        WHERE table_schema='public' AND table_name='blobs'
                          AND column_name='refcount'")"
if [[ "$HAS_REFCOUNT" != "0" ]]; then
  fail "blobs.refcount exists — FIX-01 has been re-introduced (refcount-free dedup is required)"
else
  pass "blobs has no refcount column (FIX-01 respected)"
fi

# ── 3. The build ────────────────────────────────────────────────────────────
info "3/3  workspace build"

BUILD_LOG="$(results_dir stage0-preflight)/cargo-build.log"
if cargo build --workspace --manifest-path "${SMARTFS_REPO}/Cargo.toml" >"$BUILD_LOG" 2>&1; then
  pass "cargo build --workspace succeeded (log: $BUILD_LOG)"
else
  tail -n 40 "$BUILD_LOG" >&2 || true
  die "cargo build --workspace FAILED; full log at $BUILD_LOG"
fi

[[ -x "$SMARTFS_CLI_BIN" ]] \
  && pass "smartfs-cli binary present at $SMARTFS_CLI_BIN" \
  || fail "smartfs-cli binary missing at $SMARTFS_CLI_BIN"

# ── The Stage 0 gate ────────────────────────────────────────────────────────
if [[ ! -x "$SMARTFS_DAEMON_BIN" ]]; then
  printf '\n%s================ STAGE 0 NOT DONE ================%s\n' "$C_RED" "$C_OFF" >&2
  cat >&2 <<'EOF'
No smartfsd binary exists, so SmartFS cannot be mounted at a real path, so NONE
of stages 1-4 can run. This is not a configuration problem; it is a missing
component.

  * crates/smartfs-fuse is a LIBRARY crate: src/ is error.rs, fs.rs, lib.rs,
    mount.rs, state.rs, syntax.rs. There is no main.rs and no [[bin]] target.
  * mount_smartfs() / spawn_mount_smartfs() exist in mount.rs but are called
    only from crates/smartfs-fuse/tests/fuse_integration_tests.rs.
  * smartfs-cli is a one-shot CLI and has no mount capability.

Implement the daemon per docs/testing/the-great-smartfs-test.md section 1
(flags, startup order, error handling and exit codes are fully specified there),
then re-run this script.
EOF
  fail "smartfsd not found at ${SMARTFS_DAEMON_BIN} — Stage 0 blocker"
else
  pass "smartfsd binary present at $SMARTFS_DAEMON_BIN"
  if "$SMARTFS_DAEMON_BIN" --crash-points >/dev/null 2>&1; then
    pass "smartfsd built with crash-test instrumentation (Stage 4 deterministic mode available)"
  else
    note "smartfsd has no crash-test feature; Stage 4 will run stochastic mode only."
    note "Build with: cargo build --workspace --features smartfs-db/crash-test"
  fi
fi

stage_end
