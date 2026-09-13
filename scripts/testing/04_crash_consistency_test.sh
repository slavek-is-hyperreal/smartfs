#!/usr/bin/env bash
#
# 04_crash_consistency_test.sh — The Great SmartFS Test, Stage 4.
#
# A transaction-scoped CrashMonkey analog. CrashMonkey/ACE records block-device
# write ordering and replays every crash point; SmartFS's durability boundary is
# the Postgres transaction, not the block device, so the faithful adaptation
# keeps the method (enumerate crash points, recover, check invariants) and moves
# the boundary: kill the daemon (or Postgres, or just the connection) at every
# meaningful point around cow_commit, restart, and assert Root Invariants 1-4.
#
# Kill points are derived from the canonical cow_commit in
# docs/base-v4.5-v5.0/SmartFS_v4.5_to_v5.0_fixes.md (FIX-04's merged pseudocode,
# which supersedes the version in Architecture section 11.1 / 12.1), plus
# FIX-02's worker boundary.
#
# RUNS AGAINST A SCRATCH DATABASE ONLY. It refuses to touch the main one.
#
# Usage: sudo scripts/testing/04_crash_consistency_test.sh [--iterations N]
# See:   docs/testing/the-great-smartfs-test.md section 5

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib_common.sh"

stage_begin "stage4-crash-consistency"
require_cmd psql sha256sum find awk timeout
require_root

RES="$(results_dir stage4-crash-consistency)"
INVARIANT_SQL="${SMARTFS_REPO}/scripts/testing/sql/invariants.sql"
[[ -f "$INVARIANT_SQL" ]] || die "missing $INVARIANT_SQL"

ITERATIONS=1
STOCHASTIC_ROUNDS="${STOCHASTIC_ROUNDS:-25}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --iterations) ITERATIONS="$2"; shift 2 ;;
    --stochastic-rounds) STOCHASTIC_ROUNDS="$2"; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done

# ── scratch isolation ───────────────────────────────────────────────────────
PG_BASE="${SMARTFS_DB_URL%/*}"
MAIN_DB="${SMARTFS_DB_URL##*/}"
CRASH_DB="${CRASH_DB:-smartfs_crash_scratch}"
CRASH_URL="${PG_BASE}/${CRASH_DB}"
CRASH_MOUNT="${CRASH_MOUNT:-/mnt/smartfs-crash}"
CRASH_STORE="${CRASH_STORE:-${SMARTFS_BACKING_MOUNT}/crash-blobs}"
PG_ADMIN_URL="${PG_ADMIN_URL:-${PG_BASE}/postgres}"
PG_CONTAINER="${PG_CONTAINER:-}"   # set to the docker container name to enable K12

[[ "$CRASH_DB" != "$MAIN_DB" ]] \
  || die "SAFETY STOP: CRASH_DB equals the main database (${MAIN_DB}). This stage destroys
       the database it runs against. Refusing to continue."
pass "scratch database ${CRASH_DB} is distinct from the main database ${MAIN_DB}"

[[ -x "$SMARTFS_DAEMON_BIN" ]] \
  || die "no smartfsd at ${SMARTFS_DAEMON_BIN} - Stage 0 is not done; there is no process
       to crash. See docs/testing/the-great-smartfs-test.md section 1."

require_db "$PG_ADMIN_URL"

# ── crash-point instrumentation ─────────────────────────────────────────────
DETERMINISTIC=1
COMPILED_POINTS="$("$SMARTFS_DAEMON_BIN" --crash-points 2>/dev/null || true)"
if [[ -z "$COMPILED_POINTS" ]]; then
  DETERMINISTIC=0
  note "smartfsd reports no compiled crash points."
  note "Deterministic mode needs the crash-test feature (Stage 0b):"
  note "    cargo build --workspace --features smartfs-db/crash-test"
  note "Running stochastic mode only. This is WEAKER coverage and must be reported as such."
else
  pass "deterministic mode available; compiled crash points: $(tr '\n' ' ' <<<"$COMPILED_POINTS")"
fi

# The full list from the plan. A point the binary does not implement is a
# reported gap, never a silent skip.
ALL_POINTS=(
  "K1:after SHA-256, before INSERT INTO blobs"
  "K2:after INSERT INTO blobs RETURNING, before store.put (inserted=TRUE)"
  "K3:after store.put Ok, before UPDATE blobs SET compressed_size"
  "K4:in the Err branch, after compensating DELETE FROM blobs, before returning Err"
  "K5:end of KROK 1, before BEGIN"
  "K6:after SELECT FOR UPDATE and MAX(version_number)+1, before INSERT file_versions"
  "K7:after INSERT file_versions, before INSERT ast_nodes"
  "K8:after INSERT ast_nodes, before UPDATE inode_registry"
  "K9:after UPDATE inode_registry, immediately before COMMIT"
  "K10:immediately after COMMIT, before the FUSE reply returns"
  "K11:during a concurrent smartfs-ai worker cycle (finish_embed / refresh_is_current)"
)

# ── helpers ─────────────────────────────────────────────────────────────────
reset_scratch() {
  psql "$PG_ADMIN_URL" -qtAX -v ON_ERROR_STOP=1 \
    -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname='${CRASH_DB}'" >/dev/null
  psql "$PG_ADMIN_URL" -qtAX -v ON_ERROR_STOP=1 -c "DROP DATABASE IF EXISTS ${CRASH_DB}" >/dev/null
  psql "$PG_ADMIN_URL" -qtAX -v ON_ERROR_STOP=1 -c "CREATE DATABASE ${CRASH_DB}" >/dev/null
  psql "$CRASH_URL" -qtAX -c "CREATE EXTENSION IF NOT EXISTS vector" >/dev/null || true
  local m
  for m in "${SMARTFS_REPO}"/migrations/0*.sql; do
    psql "$CRASH_URL" -qX -v ON_ERROR_STOP=1 -f "$m" >>"${RES}/migrate.log" 2>&1 \
      || die "migration $(basename "$m") failed on the scratch database"
  done
  fusermount -u -z "$CRASH_MOUNT" 2>/dev/null || umount -l "$CRASH_MOUNT" 2>/dev/null || true
  rm -rf "${CRASH_STORE:?}"; mkdir -p "$CRASH_STORE" "$CRASH_MOUNT"
  clear_crash_mountpoint
}

# A hammer writer that outlives the unmount reopens its path and lands a REAL
# file in the underlying directory. The next start_daemon then refuses to mount
# over a non-empty directory (exit 2) and the whole stage aborts mid-round. The
# daemon is right to refuse; the debris is the harness's to clear, and only ever
# while the path is not a mount, so this can never delete anything the
# filesystem under test is serving.
clear_crash_mountpoint() {
  mountpoint -q "$CRASH_MOUNT" 2>/dev/null && return 0
  if [[ -n "$(ls -A "$CRASH_MOUNT" 2>/dev/null)" ]]; then
    log "clearing $(ls -A "$CRASH_MOUNT" | wc -l) stray file(s) left in ${CRASH_MOUNT} by killed writers"
    find "$CRASH_MOUNT" -mindepth 1 -delete 2>/dev/null || true
  fi
  return 0
}

# Snapshot every blob file's digest. Invariant #2 says content-addressed blobs
# are append-only, so any pre-existing file whose digest CHANGES is a violation.
snapshot_blobs() {
  local out="$1"
  : > "$out"
  find "$CRASH_STORE" -type f -print0 2>/dev/null \
    | xargs -0 -r sha256sum 2>/dev/null | sort -k2 > "$out" || true
}

# Snapshot the schema. Invariant #4: any difference means runtime DDL happened.
snapshot_schema() {
  psql "$CRASH_URL" -qtAX -c "
    SELECT table_name || '.' || column_name || ':' || data_type
    FROM information_schema.columns
    WHERE table_schema = 'public' ORDER BY 1" > "$1"
}

# Decompress a blob regardless of which codec smartfs-compress used.
decompress_blob() {
  local f="$1" magic
  magic="$(head -c4 "$f" | od -An -tx1 | tr -d ' \n')"
  case "$magic" in
    28b52ffd*) zstd -dc  "$f" 2>/dev/null ;;   # zstd
    1f8b*)     gzip -dc  "$f" 2>/dev/null ;;   # gzip
    04224d18*) lz4 -dc   "$f" 2>/dev/null ;;   # lz4
    *)         cat       "$f" ;;               # stored uncompressed
  esac
}

# Invariant #1, cryptographic half: independently recompute SHA-256 of the
# decompressed bytes and compare. Never trust a hash the daemon reports.
check_invariant_1_crypto() {
  local label="$1" bad=0 checked=0 row hash blob_id path actual
  while IFS='|' read -r hash blob_id; do
    [[ -z "$hash" ]] && continue
    path="$(find "$CRASH_STORE" -type f -name "*${blob_id}*" -print -quit 2>/dev/null || true)"
    if [[ -z "$path" ]]; then
      # Legitimate only at K2 (poisoned row) and K5-adjacent states.
      if [[ "$label" == "K2" ]]; then
        continue
      fi
      fail "[$label] I1: no physical blob for blob_id ${blob_id} (hash ${hash:0:16}...)"
      bad=$((bad + 1)); continue
    fi
    actual="$(decompress_blob "$path" | sha256sum | awk '{print $1}')"
    checked=$((checked + 1))
    if [[ "$actual" != "$hash" ]]; then
      fail "[$label] Invariant #1 VIOLATED: stored content_hash=${hash}
            recomputed SHA-256 of decompressed bytes=${actual} (blob ${path})
            A hash taken AFTER compression looks exactly like this."
      bad=$((bad + 1))
    fi
  done < <(psql "$CRASH_URL" -qtAX -c "
      SELECT DISTINCT fv.content_hash, b.blob_id
      FROM file_versions fv JOIN blobs b ON b.content_hash = fv.content_hash
      WHERE fv.external_path IS NULL")
  (( bad == 0 )) && pass "[$label] Invariant #1: ${checked} blobs hash-verified against plaintext SHA-256" || true
}

run_invariant_sql() {
  local label="$1" outf="${2}"
  psql "$CRASH_URL" -qtAX -v ON_ERROR_STOP=1 -f "$INVARIANT_SQL" > "$outf" 2>&1 \
    || die "[$label] invariants.sql failed to execute; see $outf"
  local violations=0 line name cnt detail
  while IFS='|' read -r name cnt detail; do
    [[ -z "$name" ]] && continue
    if [[ "$name" == *_INFO ]]; then
      log "[$label] ${name}: ${cnt} ${detail}"
      continue
    fi
    fail "[$label] ${name}: ${cnt} violation(s) ${detail}"
    violations=$((violations + 1))
  done < "$outf"
  (( violations == 0 )) && pass "[$label] all SQL invariant predicates clean" || true
}

# Assert no pre-existing blob file was mutated in place (Invariant #2).
check_blob_immutability() {
  local label="$1" before="$2" after="$3" mutated
  mutated="$(join -j2 -o 1.1,2.1,0 <(sort -k2 "$before") <(sort -k2 "$after") 2>/dev/null \
             | awk '$1 != $2 {print $3}' || true)"
  if [[ -n "$mutated" ]]; then
    fail "[$label] Invariant #2 VIOLATED: existing blob file(s) changed content in place:
$(printf '%s\n' "$mutated" | sed 's/^/          /')
          Content-addressed storage is append-only; this is copy-on-write being violated."
  else
    pass "[$label] Invariant #2: no pre-existing blob file was mutated in place"
  fi
}

check_schema_unchanged() {
  local label="$1" before="$2" after="$3"
  if diff -q "$before" "$after" >/dev/null; then
    pass "[$label] Invariant #4: schema unchanged (no runtime DDL)"
  else
    fail "[$label] Invariant #4 VIOLATED: the schema changed at runtime:
$(diff "$before" "$after" | head -20 | sed 's/^/          /')
          Migrations must be explicit SQL files; nothing may issue DDL at runtime."
  fi
}

WRITER_PID=""
# Drive a write through the mount so the daemon reaches the crash point.
start_writer() {
  local content="$1" target="${CRASH_MOUNT}/crash-target.txt"
  ( printf '%s' "$content" > "$target" ) >/dev/null 2>&1 &
  WRITER_PID=$!
}

# ── the deterministic loop ──────────────────────────────────────────────────
trap 'stop_daemon; true' EXIT

DETERMINISTIC_RUN=0
for iter in $(seq 1 "$ITERATIONS"); do
  for entry in "${ALL_POINTS[@]}"; do
    label="${entry%%:*}"
    desc="${entry#*:}"

    if (( DETERMINISTIC == 0 )); then break; fi
    if ! grep -qx "$label" <<<"$COMPILED_POINTS"; then
      fail "crash point ${label} (${desc}) is NOT compiled into the binary.
          The plan requires it. Add crash_point!(\"${label}\") at that location in
          smartfs-db's cow_commit (Stage 0b) and re-run. This is a coverage GAP,
          recorded as a failure so it cannot be quietly forgotten."
      continue
    fi

    RD="${RES}/${label}/iter-${iter}"; mkdir -p "$RD"
    info "crash point ${label} — ${desc} (iteration ${iter})"

    reset_scratch
    SMARTFS_CRASH_POINT="" start_daemon "$CRASH_MOUNT" "$CRASH_STORE" "$CRASH_URL" --no-semantic
    require_live_mount "$CRASH_MOUNT"

    # Seed one committed version so "roll back to the previous version" is meaningful.
    SEED="seed content for ${label}"
    SEED_HASH="$(printf '%s' "$SEED" | sha256sum | awk '{print $1}')"
    printf '%s' "$SEED" > "${CRASH_MOUNT}/crash-target.txt"; sync
    BASE_VERSIONS="$(psql "$CRASH_URL" -qtAX -c "SELECT count(*) FROM file_versions")"

    snapshot_blobs "${RD}/blobs-before.sha256"
    snapshot_schema "${RD}/schema-before.txt"
    stop_daemon

    # Restart with the crash point armed, then drive a write into it.
    export SMARTFS_CRASH_POINT="$label"
    start_daemon "$CRASH_MOUNT" "$CRASH_STORE" "$CRASH_URL" --no-semantic || true
    unset SMARTFS_CRASH_POINT

    NEW_CONTENT="post-crash content for ${label} iteration ${iter}"
    NEW_HASH="$(printf '%s' "$NEW_CONTENT" | sha256sum | awk '{print $1}')"
    start_writer "$NEW_CONTENT"

    # Wait for the abort. If it never aborts, the crash point did not fire, and
    # that is a failure of the instrumentation, not a pass.
    waited=0
    while kill -0 "$DAEMON_PID" 2>/dev/null && (( waited < 30 )); do sleep 1; waited=$((waited+1)); done
    if kill -0 "$DAEMON_PID" 2>/dev/null; then
      fail "[$label] the daemon never aborted; crash point did not fire. No conclusion
            can be drawn from this round - treat it as a failed round, not a clean one."
      kill -9 "$DAEMON_PID" 2>/dev/null || true
    else
      pass "[$label] daemon aborted at the crash point"
    fi
    wait "$WRITER_PID" 2>/dev/null || true
    DAEMON_PID=""
    mountpoint -q "$CRASH_MOUNT" && fusermount -u "$CRASH_MOUNT" 2>/dev/null || true

    # ── recover and assert ──────────────────────────────────────────────────
    start_daemon "$CRASH_MOUNT" "$CRASH_STORE" "$CRASH_URL" --no-semantic
    require_live_mount "$CRASH_MOUNT"

    snapshot_blobs "${RD}/blobs-after.sha256"
    snapshot_schema "${RD}/schema-after.txt"

    check_blob_immutability "$label" "${RD}/blobs-before.sha256" "${RD}/blobs-after.sha256"
    check_schema_unchanged  "$label" "${RD}/schema-before.txt"   "${RD}/schema-after.txt"
    check_invariant_1_crypto "$label"
    run_invariant_sql "$label" "${RD}/invariants.out"

    POST_VERSIONS="$(psql "$CRASH_URL" -qtAX -c "SELECT count(*) FROM file_versions")"
    READBACK="$(cat "${CRASH_MOUNT}/crash-target.txt" 2>/dev/null || echo '<unreadable>')"
    READBACK_HASH="$(printf '%s' "$READBACK" | sha256sum | awk '{print $1}')"
    printf 'versions before=%s after=%s\nreadback_hash=%s\nseed_hash=%s\nnew_hash=%s\n' \
      "$BASE_VERSIONS" "$POST_VERSIONS" "$READBACK_HASH" "$SEED_HASH" "$NEW_HASH" \
      > "${RD}/state.txt"

    # Point-specific expectations.
    case "$label" in
      K1|K6|K7|K8|K9)
        # Everything must have rolled back: previous version, unchanged count.
        [[ "$POST_VERSIONS" == "$BASE_VERSIONS" ]] \
          && pass "[$label] no new file_versions row survived the crash (full rollback)" \
          || fail "[$label] file_versions went ${BASE_VERSIONS} -> ${POST_VERSIONS}; a
                partially-committed version survived a crash inside the transaction."
        [[ "$READBACK_HASH" == "$SEED_HASH" ]] \
          && pass "[$label] file reads back as the previous version" \
          || fail "[$label] file does not read back as the previous version
                (got ${READBACK_HASH:0:16}..., expected ${SEED_HASH:0:16}...)"
        ;;
      K2)
        # FIX-04's poisoned row: a blobs row with no physical blob and no reference.
        ORPHAN_REFS="$(psql "$CRASH_URL" -qtAX -c "
            SELECT count(*) FROM file_versions WHERE content_hash = '${NEW_HASH}'")"
        [[ "$ORPHAN_REFS" == "0" ]] \
          && pass "[$label] the poisoned blobs row is referenced by no file_versions row" \
          || fail "[$label] a file_versions row references a blob that was never written
                (${ORPHAN_REFS} refs) - Invariant #3 is violated and the read will fail forever."
        # FIX-03 heal: re-writing the same content must make it readable.
        printf '%s' "$NEW_CONTENT" > "${CRASH_MOUNT}/crash-target.txt"; sync
        HEALED="$(cat "${CRASH_MOUNT}/crash-target.txt" 2>/dev/null || echo '')"
        [[ "$HEALED" == "$NEW_CONTENT" ]] \
          && pass "[$label] FIX-03/FIX-04 heal works: re-writing the same content recovers it" \
          || fail "[$label] the poisoned blobs row is PERMANENT: re-writing the same content
                did not restore readability. This is FIX-04's exact failure mode."
        ;;
      K3)
        # Blob present, compressed_size NULL. Must be tolerated, not corruption.
        NULLSZ="$(psql "$CRASH_URL" -qtAX -c "
            SELECT count(*) FROM blobs WHERE content_hash='${NEW_HASH}' AND compressed_size IS NULL")"
        log "[$label] blobs rows with NULL compressed_size for this hash: ${NULLSZ}"
        pass "[$label] NULL compressed_size after crash is tolerated (checked by the I1 pass above)"
        ;;
      K4|K5)
        # Orphan blob must be invisible through the mount.
        [[ "$READBACK_HASH" == "$SEED_HASH" ]] \
          && pass "[$label] the orphaned blob is not visible through the filesystem (Invariant #3)" \
          || fail "[$label] a file whose content never got a file_versions row is visible
                through the mount - Invariant #3 VIOLATED."
        ;;
      K10)
        # Committed before the reply. A lost update is acceptable POSIX; a torn
        # state is not. Whichever version won must be internally consistent.
        if [[ "$READBACK_HASH" == "$NEW_HASH" ]]; then
          pass "[$label] the post-COMMIT version survived and reads back correctly"
        elif [[ "$READBACK_HASH" == "$SEED_HASH" ]]; then
          fail "[$label] the transaction COMMITted but the file reads as the previous
                version - a committed version is not visible after restart."
        else
          fail "[$label] the file reads back as NEITHER version (${READBACK_HASH:0:16}...) - torn state."
        fi
        ;;
      K11)
        # FIX-02: at most one is_current=TRUE per ast_node; nothing stuck.
        STUCK="$(psql "$CRASH_URL" -qtAX -c "
            SELECT count(*) FROM file_versions WHERE status = 'processing'")"
        [[ "$STUCK" == "0" ]] \
          && pass "[$label] no version left stuck in 'processing' after restart" \
          || fail "[$label] ${STUCK} version(s) stuck in 'processing' after restart; the
                daemon must reclaim these to 'pending' on startup or they never embed."
        ;;
    esac

    stop_daemon
    DETERMINISTIC_RUN=$((DETERMINISTIC_RUN + 1))
  done
done

if (( DETERMINISTIC == 1 )); then
  pass "deterministic phase complete: ${DETERMINISTIC_RUN} crash rounds executed"
fi

# ── K12 / K13: crash Postgres, not the daemon ───────────────────────────────
info "K12/K13 — killing the database rather than the daemon"

if [[ -n "$PG_CONTAINER" ]] && command -v docker >/dev/null 2>&1; then
  RD="${RES}/K12"; mkdir -p "$RD"
  reset_scratch
  start_daemon "$CRASH_MOUNT" "$CRASH_STORE" "$CRASH_URL" --no-semantic
  require_live_mount "$CRASH_MOUNT"
  printf 'seed' > "${CRASH_MOUNT}/k12.txt"; sync
  snapshot_blobs "${RD}/blobs-before.sha256"

  ( for i in $(seq 1 500); do printf 'k12 write %s' "$i" > "${CRASH_MOUNT}/k12.txt"; done ) &
  HAMMER=$!
  sleep 2
  docker kill "$PG_CONTAINER" >/dev/null 2>&1 || fail "docker kill ${PG_CONTAINER} failed"
  wait "$HAMMER" 2>/dev/null || true
  stop_daemon
  docker start "$PG_CONTAINER" >/dev/null 2>&1 || die "could not restart Postgres container ${PG_CONTAINER}"
  sleep 5
  require_db "$CRASH_URL"

  start_daemon "$CRASH_MOUNT" "$CRASH_STORE" "$CRASH_URL" --no-semantic
  snapshot_blobs "${RD}/blobs-after.sha256"
  check_blob_immutability "K12" "${RD}/blobs-before.sha256" "${RD}/blobs-after.sha256"
  check_invariant_1_crypto "K12"
  run_invariant_sql "K12" "${RD}/invariants.out"
  stop_daemon
else
  fail "K12 not executed: set PG_CONTAINER=<docker container name> to enable killing
        Postgres mid-transaction. Recorded as a coverage GAP, not a skip - the plan
        requires this point and it has not been covered."
fi

# K13: terminate the daemon's backend mid-transaction. No docker needed.
RD="${RES}/K13"; mkdir -p "$RD"
reset_scratch
start_daemon "$CRASH_MOUNT" "$CRASH_STORE" "$CRASH_URL" --no-semantic
require_live_mount "$CRASH_MOUNT"
printf 'seed' > "${CRASH_MOUNT}/k13.txt"; sync
snapshot_blobs "${RD}/blobs-before.sha256"
( for i in $(seq 1 300); do printf 'k13 write %s' "$i" > "${CRASH_MOUNT}/k13.txt"; done ) &
HAMMER=$!
sleep 1
psql "$CRASH_URL" -qtAX -c "
  SELECT pg_terminate_backend(pid) FROM pg_stat_activity
  WHERE datname='${CRASH_DB}' AND pid <> pg_backend_pid()" >/dev/null 2>&1 || true
wait "$HAMMER" 2>/dev/null || true
snapshot_blobs "${RD}/blobs-after.sha256"
check_blob_immutability "K13" "${RD}/blobs-before.sha256" "${RD}/blobs-after.sha256"
check_invariant_1_crypto "K13"
run_invariant_sql "K13" "${RD}/invariants.out"
stop_daemon

# ── stochastic phase ────────────────────────────────────────────────────────
info "stochastic phase — ${STOCHASTIC_ROUNDS} rounds of kill -9 during concurrent writes"
note "This complements the deterministic list; it does NOT replace it. A random-only"
note "crash test is how a project convinces itself it is crash-safe without ever"
note "hitting the one window that matters."

for r in $(seq 1 "$STOCHASTIC_ROUNDS"); do
  RD="${RES}/stochastic/round-${r}"; mkdir -p "$RD"
  reset_scratch
  start_daemon "$CRASH_MOUNT" "$CRASH_STORE" "$CRASH_URL"
  require_live_mount "$CRASH_MOUNT"

  snapshot_blobs "${RD}/blobs-before.sha256"
  snapshot_schema "${RD}/schema-before.txt"

  for w in 1 2 3 4; do
    ( for i in $(seq 1 400); do
        printf 'writer %s round %s iter %s payload %s' "$w" "$r" "$i" "$RANDOM" \
          > "${CRASH_MOUNT}/hammer-${w}.txt" 2>/dev/null || exit 0
      done ) &
  done

  sleep "0.$((RANDOM % 9 + 1))"
  kill -9 "$DAEMON_PID" 2>/dev/null || true
  wait 2>/dev/null || true
  DAEMON_PID=""
  # Lazy, like everywhere else: a killed FUSE daemon leaves an endpoint that a
  # plain fusermount -u refuses to detach.
  mountpoint -q "$CRASH_MOUNT" 2>/dev/null \
    && { fusermount -u -z "$CRASH_MOUNT" 2>/dev/null || umount -l "$CRASH_MOUNT" 2>/dev/null || true; }
  clear_crash_mountpoint

  start_daemon "$CRASH_MOUNT" "$CRASH_STORE" "$CRASH_URL"
  snapshot_blobs "${RD}/blobs-after.sha256"
  snapshot_schema "${RD}/schema-after.txt"

  check_blob_immutability "stochastic-${r}" "${RD}/blobs-before.sha256" "${RD}/blobs-after.sha256"
  check_schema_unchanged  "stochastic-${r}" "${RD}/schema-before.txt"   "${RD}/schema-after.txt"
  check_invariant_1_crypto "stochastic-${r}"
  run_invariant_sql "stochastic-${r}" "${RD}/invariants.out"

  # Invariant #3: everything visible must trace to a file_versions row.
  BAD=0
  while IFS= read -r -d '' f; do
    h="$(sha256_of "$f")"
    c="$(psql "$CRASH_URL" -qtAX -c "SELECT count(*) FROM file_versions WHERE content_hash='${h}'")"
    [[ "$c" == "0" ]] && { fail "[stochastic-${r}] Invariant #3: ${f} visible with no file_versions row"; BAD=1; }
  done < <(find "$CRASH_MOUNT" -maxdepth 1 -type f -print0 2>/dev/null)
  (( BAD == 0 )) && pass "[stochastic-${r}] Invariant #3 holds for all visible files" || true

  stop_daemon
done

# Orphan-blob leak check across the whole stage.
ORPHANS="$(psql "$CRASH_URL" -qtAX -c "
  SELECT count(*) FROM blobs b
  WHERE NOT EXISTS (SELECT 1 FROM file_versions fv WHERE fv.content_hash = b.content_hash)" 2>/dev/null || echo 0)"
log "orphan blobs remaining after the final round: ${ORPHANS} (GC-by-scan candidates)"

info "artifacts written to ${RES}"
if (( DETERMINISTIC == 0 )); then
  fail "Stage 4 ran WITHOUT deterministic crash points. Coverage is incomplete and this
        stage must not be reported as passing. Implement Stage 0b (crash_point! macro
        behind the crash-test feature) and re-run."
fi
stage_end
