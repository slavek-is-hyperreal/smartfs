#!/usr/bin/env bash
#
# 03_setup_check_smartfs_xfstests.sh — The Great SmartFS Test, Stage 3.
#
# Scaffolds `check-smartfs`, an xfstests wrapper that mounts SmartFS where
# xfstests expects a native filesystem, modelled directly on rfjakob's
# `check-gocryptfs` (https://github.com/rfjakob/fuse-xfstests), which is the
# working precedent for running the standard generic/ group against a FUSE
# filesystem.
#
# This script SETS UP. It does not run the suite - the run takes hours and
# belongs in its own session:
#     cd third_party/xfstests && sudo ./check-smartfs -g generic
#
# TODO(smartfs) markers are left ONLY where something genuinely cannot be
# inferred before the Stage 0 daemon exists and the first full run produces real
# failures. Everything else is filled in.
#
# Usage: sudo scripts/testing/03_setup_check_smartfs_xfstests.sh
# See:   docs/testing/the-great-smartfs-test.md section 4

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib_common.sh"

stage_begin "stage3-xfstests-setup"
require_cmd git make cc psql awk
require_root

RES="$(results_dir stage3-xfstests-setup)"
XFS_DIR="${XFS_DIR:-${SMARTFS_REPO}/third_party/xfstests}"

# xfstests needs TWO independent filesystems. For SmartFS that means two daemon
# instances, two mountpoints, two blob stores and - critically - two SEPARATE
# databases. Sharing one database across TEST_DEV and SCRATCH_DEV produces
# inode-namespace collisions that look exactly like filesystem corruption.
TEST_MNT="${TEST_MNT:-${SMARTFS_BACKING_MOUNT}/mnt-xfs-test}"
SCRATCH_MNT_DIR="${SCRATCH_MNT_DIR:-${SMARTFS_BACKING_MOUNT}/mnt-xfs-scratch}"
TEST_STORE="${TEST_STORE:-${SMARTFS_BACKING_MOUNT}/xfs-test-blobs}"
SCRATCH_STORE="${SCRATCH_STORE:-${SMARTFS_BACKING_MOUNT}/xfs-scratch-blobs}"
PG_ADMIN_URL="${PG_ADMIN_URL:-postgres://postgres:postgres@172.17.0.2:5432/postgres}"
TEST_DB="${TEST_DB:-smartfs_xfs_test}"
SCRATCH_DB="${SCRATCH_DB:-smartfs_xfs_scratch}"
PG_BASE="${SMARTFS_DB_URL%/*}"

[[ -x "$SMARTFS_DAEMON_BIN" ]] \
  || die "no smartfsd at ${SMARTFS_DAEMON_BIN} - Stage 0 is not done and the wrapper
       would have nothing to mount. See docs/testing/the-great-smartfs-test.md section 1."

# ── obtain xfstests ─────────────────────────────────────────────────────────
info "obtaining xfstests"
if [[ ! -d "${XFS_DIR}/.git" ]]; then
  mkdir -p "$(dirname "$XFS_DIR")"
  git clone --depth 1 https://github.com/kdave/xfstests "$XFS_DIR" \
    || die "could not clone xfstests. No network is a FAILURE of this stage; vendor it
       into third_party/ and re-run."
  pass "cloned xfstests into $XFS_DIR"
else
  pass "xfstests already present at $XFS_DIR"
fi

if [[ ! -x "${XFS_DIR}/check" ]]; then
  die "${XFS_DIR}/check is missing - the clone is incomplete"
fi

info "building xfstests helpers (this takes a few minutes)"
( cd "$XFS_DIR" && make >"${RES}/xfstests-build.log" 2>&1 ) \
  || die "xfstests build failed; see ${RES}/xfstests-build.log
       (usual cause: missing build deps - libtool, uuid-dev, libattr1-dev,
        libacl1-dev, libaio-dev, libgdbm-dev, xfslibs-dev, e2fsprogs, attr, acl)"
pass "xfstests built"

# xfstests refuses to run as an arbitrary user without these accounts.
for u in fsgqa fsgqa2; do
  id "$u" >/dev/null 2>&1 || useradd -m "$u" >/dev/null 2>&1 || true
  id "$u" >/dev/null 2>&1 \
    && pass "user ${u} exists" \
    || fail "xfstests requires the ${u} account; create it manually"
done

# ── directories and databases ───────────────────────────────────────────────
info "preparing mountpoints, blob stores and databases"
for d in "$TEST_MNT" "$SCRATCH_MNT_DIR" "$TEST_STORE" "$SCRATCH_STORE"; do
  mkdir -p "$d"
  [[ -w "$d" ]] || fail "$d is not writable"
done
pass "mountpoints and blob stores prepared"

require_db "$PG_ADMIN_URL"
for db in "$TEST_DB" "$SCRATCH_DB"; do
  exists="$(psql_q "SELECT count(*) FROM pg_database WHERE datname = '${db}'" "$PG_ADMIN_URL")"
  if [[ "$exists" == "0" ]]; then
    psql_q "CREATE DATABASE ${db}" "$PG_ADMIN_URL" >/dev/null
    pass "created database ${db}"
  else
    pass "database ${db} already exists"
  fi
done

# Apply migrations as FILES, in order. Root Invariant #4: no improvised DDL.
apply_migrations() {
  local url="$1" m
  for m in "${SMARTFS_REPO}"/migrations/0*.sql; do
    psql "$url" -qX -v ON_ERROR_STOP=1 -f "$m" >>"${RES}/migrate.log" 2>&1 \
      || die "migration $(basename "$m") failed against ${url##*/}; see ${RES}/migrate.log"
  done
}
for db in "$TEST_DB" "$SCRATCH_DB"; do
  tables="$(psql_q "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'" "${PG_BASE}/${db}")"
  if (( tables == 0 )); then
    psql_q "CREATE EXTENSION IF NOT EXISTS vector" "${PG_BASE}/${db}" >/dev/null || true
    apply_migrations "${PG_BASE}/${db}"
    pass "migrations 001-006 applied to ${db}"
  else
    pass "database ${db} already initialized with schema (${tables} tables present)"
  fi
done

# ── local.config ────────────────────────────────────────────────────────────
info "writing ${XFS_DIR}/local.config"
cat > "${XFS_DIR}/local.config" <<EOF
# Generated by scripts/testing/03_setup_check_smartfs_xfstests.sh
# xfstests contract variables, SmartFS bindings.
export FSTYP=fuse.smartfs
export TEST_DEV=smartfs:${TEST_DB}
export TEST_DIR=${TEST_MNT}
export SCRATCH_DEV=smartfs:${SCRATCH_DB}
export SCRATCH_MNT=${SCRATCH_MNT_DIR}

# SmartFS-specific, consumed by check-smartfs
export SMARTFS_DAEMON_BIN=${SMARTFS_DAEMON_BIN}
export SMARTFS_PG_BASE=${PG_BASE}
export SMARTFS_TEST_DB=${TEST_DB}
export SMARTFS_SCRATCH_DB=${SCRATCH_DB}
export SMARTFS_TEST_STORE=${TEST_STORE}
export SMARTFS_SCRATCH_STORE=${SCRATCH_STORE}
export SMARTFS_MIGRATIONS=${SMARTFS_REPO}/migrations
EOF
pass "local.config written"

# ── the exclude list ────────────────────────────────────────────────────────
EXCLUDE="${XFS_DIR}/smartfs.exclude"
if [[ ! -f "$EXCLUDE" ]]; then
  cat > "$EXCLUDE" <<'EOF'
# Tests excluded from ./check-smartfs -g generic.
# Format: one test id per line (e.g. generic/003), justification on the line above.
#
# The GROUPS below are excluded via -x in the run command, not here; this file is
# for individual tests. Populate it AFTER the first full run, from real failures,
# and give every entry a reason. An entry without a reason is a finding, not an
# exclusion.
#
# TODO(smartfs): fill in after the first full generic/ run. Leaving it empty is
# correct right now - guessing exclusions before seeing failures hides bugs.
EOF
  pass "created empty ${EXCLUDE} (populate after the first run)"
else
  pass "${EXCLUDE} already exists"
fi

# ── check-smartfs ───────────────────────────────────────────────────────────
info "writing ${XFS_DIR}/check-smartfs"
cat > "${XFS_DIR}/check-smartfs" <<'WRAPPER'
#!/usr/bin/env bash
#
# check-smartfs - run xfstests against a live SmartFS FUSE mount.
#
# Modelled on rfjakob/fuse-xfstests' check-gocryptfs: the standard ./check driver
# is reused unchanged, and only the mount / unmount / mkfs hooks are replaced so
# that "formatting a device" means "reset a Postgres database plus a blob dir"
# and "mounting a device" means "start smartfsd and wait for its ready file".
#
# Usage:  sudo ./check-smartfs -g generic
#         sudo ./check-smartfs generic/001 generic/002

set -euo pipefail
cd "$(dirname "$0")"

[[ -f ./local.config ]] || { echo "check-smartfs: local.config missing; run 03_setup_check_smartfs_xfstests.sh" >&2; exit 1; }
# shellcheck disable=SC1091
source ./local.config

: "${SMARTFS_DAEMON_BIN:?not set in local.config}"
[[ -x "$SMARTFS_DAEMON_BIN" ]] || { echo "check-smartfs: no daemon at $SMARTFS_DAEMON_BIN (Stage 0 not done)" >&2; exit 1; }

_pidfile() { echo "/run/smartfs-xfs-$1.pid"; }
_readyfile() { echo "/run/smartfs-xfs-$1.ready"; }

# _smartfs_mount <role: test|scratch> <mountpoint>
_smartfs_mount() {
  local role="$1" mp="$2" db store
  case "$role" in
    test)    db="$SMARTFS_TEST_DB";    store="$SMARTFS_TEST_STORE" ;;
    scratch) db="$SMARTFS_SCRATCH_DB"; store="$SMARTFS_SCRATCH_STORE" ;;
    *) echo "check-smartfs: unknown role $role" >&2; return 1 ;;
  esac
  mountpoint -q "$mp" && return 0
  mkdir -p "$mp" "$store"
  rm -f "$(_readyfile "$role")"
  "$SMARTFS_DAEMON_BIN" \
      --mountpoint "$mp" \
      --database-url "${SMARTFS_PG_BASE}/${db}" \
      --store-path "$store" \
      --pid-file "$(_pidfile "$role")" \
      --ready-file "$(_readyfile "$role")" \
      --allow-other \
      --no-semantic \
      --foreground >>"/var/log/smartfs-xfs-${role}.log" 2>&1 &
  local waited=0
  while (( waited < 30 )); do
    [[ -f "$(_readyfile "$role")" ]] && return 0
    sleep 1; waited=$((waited+1))
  done
  echo "check-smartfs: ${role} daemon did not become ready in 30s" >&2
  return 1
}

# _smartfs_unmount <role> <mountpoint>
_smartfs_unmount() {
  local role="$1" mp="$2" pidf
  pidf="$(_pidfile "$role")"
  if [[ -f "$pidf" ]]; then
    kill -TERM "$(cat "$pidf")" 2>/dev/null || true
    local waited=0
    while mountpoint -q "$mp" && (( waited < 20 )); do sleep 1; waited=$((waited+1)); done
  fi
  mountpoint -q "$mp" && fusermount -u "$mp" 2>/dev/null || true
  rm -f "$pidf" "$(_readyfile "$role")"
  return 0
}

# _smartfs_mkfs <role> - the SmartFS analogue of mkfs: drop + recreate the
# database from the migration FILES (Root Invariant #4 forbids improvised DDL)
# and empty the blob directory.
_smartfs_mkfs() {
  local role="$1" db store admin
  case "$role" in
    test)    db="$SMARTFS_TEST_DB";    store="$SMARTFS_TEST_STORE" ;;
    scratch) db="$SMARTFS_SCRATCH_DB"; store="$SMARTFS_SCRATCH_STORE" ;;
  esac
  admin="${SMARTFS_PG_BASE}/postgres"
  psql "$admin" -qtAX -v ON_ERROR_STOP=1 \
    -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname='${db}'" >/dev/null
  psql "$admin" -qtAX -v ON_ERROR_STOP=1 -c "DROP DATABASE IF EXISTS ${db}" >/dev/null
  psql "$admin" -qtAX -v ON_ERROR_STOP=1 -c "CREATE DATABASE ${db}" >/dev/null
  psql "${SMARTFS_PG_BASE}/${db}" -qtAX -c "CREATE EXTENSION IF NOT EXISTS vector" >/dev/null
  local m
  for m in "${SMARTFS_MIGRATIONS}"/0*.sql; do
    psql "${SMARTFS_PG_BASE}/${db}" -qX -v ON_ERROR_STOP=1 -f "$m" >/dev/null \
      || { echo "check-smartfs: migration $(basename "$m") failed" >&2; return 1; }
  done
  rm -rf "${store:?}/"* 2>/dev/null || true
  mkdir -p "$store"
  return 0
}

export -f _smartfs_mount _smartfs_unmount _smartfs_mkfs _pidfile _readyfile

# ── override the xfstests hooks ─────────────────────────────────────────────
# TODO(smartfs): xfstests resolves _scratch_mount / _scratch_unmount /
# _scratch_mkfs from common/rc. check-gocryptfs handles this by patching
# common/rc with a small overlay applied at setup time. Confirm which mechanism
# the pinned xfstests revision honours (an overlay file in common/, or a direct
# patch), then wire these three functions in. Until this is done, ./check will
# try a real mount(8) and fail immediately - which is the correct loud failure,
# not a silent skip.
if [[ -f ./common/smartfs ]]; then
  echo "check-smartfs: using ./common/smartfs overlay"
else
  cat > ./common/smartfs <<'OVERLAY'
# Sourced by common/rc when FSTYP=fuse.smartfs. Redefines the device hooks.
_scratch_mount()   { _smartfs_mount   scratch "$SCRATCH_MNT"; }
_scratch_unmount() { _smartfs_unmount scratch "$SCRATCH_MNT"; }
_scratch_mkfs()    { _smartfs_mkfs    scratch; }
_test_mount()      { _smartfs_mount   test    "$TEST_DIR"; }
_test_unmount()    { _smartfs_unmount test    "$TEST_DIR"; }
_test_mkfs()       { _smartfs_mkfs    test; }
_require_scratch_shutdown() { _notrun "SmartFS has no shutdown ioctl"; }
_require_dm_target()        { _notrun "SmartFS is not block-device backed"; }
OVERLAY
  echo "check-smartfs: wrote ./common/smartfs overlay"
fi

_smartfs_mkfs test    || exit 1
_smartfs_mkfs scratch || exit 1
_smartfs_mount test    "$TEST_DIR" || exit 1
_smartfs_mount scratch "$SCRATCH_MNT" || exit 1

trap '_smartfs_unmount scratch "$SCRATCH_MNT"; _smartfs_unmount test "$TEST_DIR"' EXIT

# Groups SmartFS structurally cannot support: raw block-device features FUSE
# does not expose. These are FUSE-inherent, not SmartFS defects.
EXCLUDE_GROUPS="dangerous_fsck,dangerous_scrub,dump,quota,defrag,shutdown,dax,realtime,dm_flakey,scrub,fsck"

exec ./check -E ./smartfs.exclude $(printf -- '-x %s ' "$EXCLUDE_GROUPS") "$@"
WRAPPER

chmod +x "${XFS_DIR}/check-smartfs"
pass "check-smartfs written and made executable"

# ── verify the scaffold is at least self-consistent ─────────────────────────
bash -n "${XFS_DIR}/check-smartfs" \
  && pass "check-smartfs parses as valid bash" \
  || fail "check-smartfs has a syntax error"

REMAINING_TODOS="$(grep -c 'TODO(smartfs)' "${XFS_DIR}/check-smartfs" "$EXCLUDE" 2>/dev/null | awk -F: '{s+=$2} END{print s+0}')"
note "${REMAINING_TODOS} TODO(smartfs) markers remain - both are genuinely undecidable"
note "  until the Stage 0 daemon exists and the first generic/ run produces failures."

cat <<EOF

Setup complete. Stage 3 is NOT yet run - the suite takes hours. Run it with:

    cd ${XFS_DIR}
    sudo ./check-smartfs -g generic 2>&1 | tee ${RES}/generic-run.log

Then archive ${XFS_DIR}/results/ and classify every failure as
real-bug / LIMITATION / FUSE before reporting anything.
EOF

stage_end
