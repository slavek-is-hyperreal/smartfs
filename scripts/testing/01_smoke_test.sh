#!/usr/bin/env bash
#
# 01_smoke_test.sh — The Great SmartFS Test, Stage 1.
#
# Proves (a) the daemon actually serves a filesystem, (b) the FUSE and CLI write
# paths converge on the same rows, (c) Root Invariants 1-3 hold for simple
# operations, and (d) whether the B-04 plain-text-query gap in smartfs-mcp's
# search tools is real.
#
# EXPECTED RESULT TODAY: FAIL, on the B-04 probe. handler.rs's dispatch arm for
# search_semantic / search_functions / search_by_concept reads only
# "query_vector" and never calls smartfs_ai, so a plain-text "query" silently
# returns an empty list. Proving that concretely is this script's job. A green
# Stage 1 is the surprising outcome and needs explaining.
#
# Usage: sudo scripts/testing/01_smoke_test.sh
# See:   docs/testing/the-great-smartfs-test.md section 2

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib_common.sh"

stage_begin "stage1-smoke"
require_cmd jq psql sha256sum
require_root
require_db "$SMARTFS_DB_URL"

RES="$(results_dir stage1-smoke)"
trap stop_daemon EXIT

start_daemon "$SMARTFS_MOUNT" "$SMARTFS_STORE_PATH" "$SMARTFS_DB_URL"
require_live_mount "$SMARTFS_MOUNT"

WORK="${SMARTFS_MOUNT}/smoke-$$"
mkdir -p "$WORK" || die "mkdir on the SmartFS mount failed"

# inode_id for a path under the mount, resolved through SQL by name.
inode_of() {
  psql_q "SELECT id FROM inode_registry WHERE name = '$(basename "$1")'
          ORDER BY updated_at DESC LIMIT 1"
}

# ── 1.1 write / read round-trip ─────────────────────────────────────────────
info "1.1  basic write / read / stat round-trip"

F1="${WORK}/hello.txt"
CONTENT_A="the quick brown fox jumps over the lazy dog"
printf '%s' "$CONTENT_A" > "$F1" || die "write to the mount failed"
sync

READ_BACK="$(cat "$F1")"
[[ "$READ_BACK" == "$CONTENT_A" ]] \
  && pass "bytes round-trip exactly through the FUSE mount" \
  || fail "round-trip mismatch: wrote '${CONTENT_A}', read '${READ_BACK}'"

STAT_SIZE="$(stat -c %s "$F1")"
[[ "$STAT_SIZE" == "${#CONTENT_A}" ]] \
  && pass "stat() size correct (${STAT_SIZE})" \
  || fail "stat() size ${STAT_SIZE}, expected ${#CONTENT_A}"

# ── 1.2 Root Invariant #1: content_hash = SHA-256 of ORIGINAL bytes ─────────
info "1.2  Root Invariant #1 — content_hash is SHA-256 before compression"

EXPECTED_HASH="$(printf '%s' "$CONTENT_A" | sha256sum | awk '{print $1}')"
INODE1="$(inode_of "$F1")"
[[ -n "$INODE1" ]] || die "no inode_registry row for $F1 — the write never reached the database"

DB_HASH="$(psql_q "SELECT content_hash FROM file_versions
                   WHERE inode_id = '${INODE1}' ORDER BY version_number DESC LIMIT 1")"
[[ "$DB_HASH" == "$EXPECTED_HASH" ]] \
  && pass "content_hash matches independently computed SHA-256 of the plaintext" \
  || fail "Invariant #1 VIOLATED: db=${DB_HASH} expected=${EXPECTED_HASH}
          (a hash taken AFTER compression looks exactly like this)"

# ── 1.3 Root Invariant #2: copy-on-write versioning ─────────────────────────
info "1.3  Root Invariant #2 — every content change creates a new file_versions row"

printf '%s' "second revision of the file" > "$F1"; sync
printf '%s' "third revision of the file"  > "$F1"; sync

VER_COUNT="$(psql_q "SELECT count(*) FROM file_versions WHERE inode_id = '${INODE1}'")"
[[ "$VER_COUNT" == "3" ]] \
  && pass "three writes produced three file_versions rows" \
  || fail "Invariant #2: expected 3 versions, found ${VER_COUNT}"

VER_NUMS="$(psql_q "SELECT string_agg(version_number::text, ',' ORDER BY version_number)
                    FROM file_versions WHERE inode_id = '${INODE1}'")"
[[ "$VER_NUMS" == "1,2,3" ]] \
  && pass "version_number sequence is contiguous 1,2,3" \
  || fail "Invariant #2: version_number sequence is '${VER_NUMS}', expected '1,2,3'"

DISTINCT_HASHES="$(psql_q "SELECT count(DISTINCT content_hash) FROM file_versions
                           WHERE inode_id = '${INODE1}'")"
[[ "$DISTINCT_HASHES" == "3" ]] \
  && pass "three distinct content_hash values (no in-place blob mutation)" \
  || fail "Invariant #2: only ${DISTINCT_HASHES} distinct hashes across 3 versions"

# ── 1.4 dedup via blobs ─────────────────────────────────────────────────────
info "1.4  dedup — identical content must not create a second blobs row"

DUP_CONTENT="identical content for dedup check $$"
DUP_HASH="$(printf '%s' "$DUP_CONTENT" | sha256sum | awk '{print $1}')"
printf '%s' "$DUP_CONTENT" > "${WORK}/dup-a.txt"; sync
BLOBS_BEFORE="$(psql_q "SELECT count(*) FROM blobs")"
printf '%s' "$DUP_CONTENT" > "${WORK}/dup-b.txt"; sync
BLOBS_AFTER="$(psql_q "SELECT count(*) FROM blobs")"

[[ "$BLOBS_BEFORE" == "$BLOBS_AFTER" ]] \
  && pass "dedup works: blobs count unchanged (${BLOBS_AFTER}) for duplicate content" \
  || fail "dedup broken: blobs went ${BLOBS_BEFORE} -> ${BLOBS_AFTER} for identical content"

REFS="$(psql_q "SELECT count(*) FROM file_versions WHERE content_hash = '${DUP_HASH}'")"
[[ "$REFS" == "2" ]] \
  && pass "both files reference the same content_hash (2 file_versions rows)" \
  || fail "expected 2 file_versions rows for the deduped hash, found ${REFS}"

# ── 1.5 rename / unlink / mkdir / rmdir ─────────────────────────────────────
info "1.5  rename, unlink, mkdir, rmdir"

mv "${WORK}/dup-a.txt" "${WORK}/renamed.txt" \
  && pass "rename within a directory succeeded" \
  || fail "rename within a directory failed"
[[ -f "${WORK}/renamed.txt" && ! -e "${WORK}/dup-a.txt" ]] \
  && pass "rename is atomic from the caller's view (old gone, new present)" \
  || fail "rename left an inconsistent namespace"

mkdir -p "${WORK}/subdir"
mv "${WORK}/renamed.txt" "${WORK}/subdir/moved.txt" \
  && pass "rename across directories succeeded" \
  || fail "rename across directories failed"

rm -f "${WORK}/dup-b.txt" \
  && pass "unlink succeeded" \
  || fail "unlink failed"

rmdir "${WORK}/empty-dir" 2>/dev/null || true
mkdir -p "${WORK}/empty-dir" && rmdir "${WORK}/empty-dir" \
  && pass "mkdir + rmdir succeeded" \
  || fail "mkdir/rmdir failed"

# Hardlink: supported, or an explicit and documented ENOTSUP. Silence is not ok.
if ln "${WORK}/subdir/moved.txt" "${WORK}/hardlink.txt" 2>"${RES}/hardlink.err"; then
  pass "hardlink supported"
else
  ERRTXT="$(cat "${RES}/hardlink.err")"
  if grep -qiE 'not supported|not implemented|ENOTSUP|EPERM' <<<"$ERRTXT"; then
    pass "hardlink returns an explicit unsupported error (documented limitation): ${ERRTXT}"
  else
    fail "hardlink failed with an unclassified error: ${ERRTXT}"
  fi
fi

# ── 1.6 truncate must not corrupt the version chain ─────────────────────────
info "1.6  truncate-to-zero"

: > "$F1"; sync
TRUNC_SIZE="$(stat -c %s "$F1")"
[[ "$TRUNC_SIZE" == "0" ]] \
  && pass "truncate-to-zero reports size 0" \
  || fail "truncate-to-zero left size ${TRUNC_SIZE}"

ORPHAN_VERSIONS="$(psql_q "SELECT count(*) FROM file_versions fv
                           WHERE fv.inode_id = '${INODE1}'
                             AND fv.parent_version_id IS NULL
                             AND fv.version_number > 1")"
[[ "$ORPHAN_VERSIONS" == "0" ]] \
  && pass "version DAG intact after truncate (no orphaned non-first version)" \
  || fail "truncate broke the version chain: ${ORPHAN_VERSIONS} orphaned versions"

# ── 1.7 the two write paths must converge ───────────────────────────────────
info "1.7  FUSE path and smartfs-cli path must see each other"

CLI_CONTENT="written through smartfs-cli at $(date -Is)"
CLI_HASH="$(printf '%s' "$CLI_CONTENT" | sha256sum | awk '{print $1}')"
CLI_PATH="/smoke-$$/via-cli.txt"

printf '%s' "$CLI_CONTENT" | "$SMARTFS_CLI_BIN" \
    --database-url "$SMARTFS_DB_URL" --store-path "$SMARTFS_STORE_PATH" \
    write "$CLI_PATH" >"${RES}/cli-write.log" 2>&1 \
  || die "smartfs-cli write failed; see ${RES}/cli-write.log"

sync
if [[ -f "${SMARTFS_MOUNT}${CLI_PATH}" ]]; then
  FUSE_SEES="$(cat "${SMARTFS_MOUNT}${CLI_PATH}")"
  [[ "$FUSE_SEES" == "$CLI_CONTENT" ]] \
    && pass "a CLI write is visible and byte-identical through the FUSE mount" \
    || fail "CLI write visible through FUSE but content differs"
else
  fail "CLI write is NOT visible through the FUSE mount at ${SMARTFS_MOUNT}${CLI_PATH}"
fi

CLI_DB_HASH="$(psql_q "SELECT content_hash FROM file_versions
                       WHERE content_hash = '${CLI_HASH}' LIMIT 1")"
[[ "$CLI_DB_HASH" == "$CLI_HASH" ]] \
  && pass "CLI write stored with the correct pre-compression content_hash" \
  || fail "no file_versions row with the expected hash ${CLI_HASH} after CLI write"

# Reverse direction: FUSE write must be readable via the CLI.
FUSE_CONTENT="written through the FUSE mount at $(date -Is)"
printf '%s' "$FUSE_CONTENT" > "${WORK}/via-fuse.txt"; sync
if "$SMARTFS_CLI_BIN" --database-url "$SMARTFS_DB_URL" --store-path "$SMARTFS_STORE_PATH" \
     cat "/smoke-$$/via-fuse.txt" 2>/dev/null | grep -qF "$FUSE_CONTENT"; then
  pass "a FUSE write is readable through smartfs-cli cat"
else
  fail "smartfs-cli cat cannot read a file written through the FUSE mount"
fi

if "$SMARTFS_CLI_BIN" --database-url "$SMARTFS_DB_URL" --store-path "$SMARTFS_STORE_PATH" \
     history "/smoke-$$/hello.txt" >"${RES}/cli-history.log" 2>&1; then
  HIST_LINES="$(grep -cE '[0-9a-f]{8}' "${RES}/cli-history.log" || true)"
  (( HIST_LINES >= 3 )) \
    && pass "smartfs-cli history shows at least the 3 committed versions" \
    || fail "smartfs-cli history shows ${HIST_LINES} version lines, expected >= 3"
else
  fail "smartfs-cli history failed; see ${RES}/cli-history.log"
fi

# ── 1.8 Root Invariant #3: nothing visible without a file_versions row ──────
info "1.8  Root Invariant #3 — every visible file traces to a file_versions row"

I3_FAILURES=0
while IFS= read -r -d '' f; do
  rel="${f#"$SMARTFS_MOUNT"}"
  disk_hash="$(sha256_of "$f")"
  cnt="$(psql_q "SELECT count(*) FROM file_versions WHERE content_hash = '${disk_hash}'")"
  if [[ "$cnt" == "0" ]]; then
    fail "Invariant #3 VIOLATED: ${rel} is visible but its content (${disk_hash:0:16}...)
          traces to no file_versions row"
    I3_FAILURES=$((I3_FAILURES + 1))
  fi
done < <(find "$WORK" -type f -print0)
(( I3_FAILURES == 0 )) \
  && pass "every file visible under ${WORK} traces to a file_versions row with a matching hash" || true

# ── 1.9 THE B-04 PROBE ──────────────────────────────────────────────────────
info "1.9  B-04 probe — do the MCP search tools accept a plain-text query?"

# One-shot JSON-RPC over stdio. If smartfs-mcp speaks TCP in this deployment,
# set SMARTFS_MCP_TRANSPORT=tcp and SMARTFS_MCP_ADDR=host:port.
SMARTFS_MCP_TRANSPORT="${SMARTFS_MCP_TRANSPORT:-stdio}"

mcp_call() {
  local tool="$1" args="$2" out
  local req
  req="$(jq -cn --arg t "$tool" --argjson a "$args" '
    {jsonrpc:"2.0",id:1,method:"initialize",
     params:{protocolVersion:"2024-11-05",capabilities:{},
             clientInfo:{name:"great-smartfs-test",version:"1"}}},
    {jsonrpc:"2.0",method:"notifications/initialized"},
    {jsonrpc:"2.0",id:2,method:"tools/call",params:{name:$t,arguments:$a}}
  ')"
  case "$SMARTFS_MCP_TRANSPORT" in
    stdio)
      [[ -x "$SMARTFS_MCP_BIN" ]] \
        || die "smartfs-mcp binary not found at ${SMARTFS_MCP_BIN}; cannot run the B-04 probe.
       Missing tooling is a FAILURE of this stage, not a reason to skip it."
      out="$(printf '%s\n' "$req" | timeout 60 env \
              SMARTFS_DATABASE_URL="$SMARTFS_DB_URL" \
              DATABASE_URL="$SMARTFS_DB_URL" \
              "$SMARTFS_MCP_BIN" 2>>"${RES}/mcp-stderr.log")" \
        || die "smartfs-mcp exited non-zero during the B-04 probe; see ${RES}/mcp-stderr.log"
      ;;
    tcp)
      require_cmd nc
      out="$(printf '%s\n' "$req" | timeout 60 nc ${SMARTFS_MCP_ADDR/:/ })" \
        || die "cannot reach smartfs-mcp over TCP at ${SMARTFS_MCP_ADDR:-<unset>}"
      ;;
    *) die "unknown SMARTFS_MCP_TRANSPORT='${SMARTFS_MCP_TRANSPORT}'" ;;
  esac
  # Keep only the response to id=2.
  printf '%s\n' "$out" | jq -c 'select(.id == 2)' 2>/dev/null | tail -n1
}

# Give the embedding worker a chance to process the files just written.
info "     waiting for the smartfs-ai worker to drain the pending queue"
WAITED=0
while (( WAITED < 120 )); do
  PENDING="$(psql_q "SELECT count(*) FROM file_versions WHERE status IN ('pending','processing')")"
  [[ "$PENDING" == "0" ]] && break
  sleep 5; WAITED=$((WAITED + 5))
done
log "pending/processing versions after ${WAITED}s: ${PENDING:-unknown}"

# Locate the embeddings table (partitioned by plugin_type; name varies by model).
EMB_TABLE="$(psql_q "SELECT table_name FROM information_schema.tables
                     WHERE table_schema='public' AND table_name LIKE 'embeddings%'
                     ORDER BY table_name LIMIT 1")"
[[ -n "$EMB_TABLE" ]] \
  || die "no embeddings table found in the schema; the B-04 probe cannot be made
       conclusive. This is a FAILURE (missing precondition), not a skip."
log "using embeddings table: ${EMB_TABLE}"

EMB_ROWS="$(psql_q "SELECT count(*) FROM ${EMB_TABLE}")"
if [[ "$EMB_ROWS" == "0" ]]; then
  fail "B-04 probe INCONCLUSIVE-FAIL: ${EMB_TABLE} is empty, so a control query cannot
        distinguish 'B-04 is real' from 'the index is empty'. Get the embedding
        worker producing rows, then re-run. Do not report Stage 1 as skipped."
else
  pass "${EMB_ROWS} embedding rows available for the control call"

  # --- CONTROL: pass a real vector as query_vector. Must return results. ---
  QVEC="$(psql_q "SELECT embedding::text FROM ${EMB_TABLE}
                  WHERE embedding IS NOT NULL LIMIT 1")"
  [[ -n "$QVEC" ]] || die "could not read an embedding vector for the control call"
  # pgvector's text form is already a JSON array ("[0.1,0.2,...]"); validate rather
  # than reshape, so a surprise format fails loudly instead of producing junk JSON.
  QVEC_JSON="$QVEC"
  jq -e 'type == "array" and length > 0' >/dev/null 2>&1 <<<"$QVEC_JSON" \
    || die "the embedding read from ${EMB_TABLE} is not a JSON array: ${QVEC:0:60}...
       Cannot construct a control call, so the B-04 probe cannot be made conclusive."

  CONTROL_RESP="$(mcp_call search_semantic \
      "$(jq -cn --argjson v "$QVEC_JSON" '{query_vector:$v, limit:5}')")"
  printf '%s\n' "$CONTROL_RESP" > "${RES}/b04-control-response.json"

  CONTROL_ERR="$(jq -r 'if .error then "yes" else "no" end' <<<"$CONTROL_RESP" 2>/dev/null || echo parse-error)"
  CONTROL_N="$(jq -r '[.. | objects | select(has("content_hash") or has("path") or has("id"))] | length' \
                 <<<"$CONTROL_RESP" 2>/dev/null || echo 0)"
  CONTROL_TEXTLEN="$(jq -r '(.result.content[0].text // "") | length' <<<"$CONTROL_RESP" 2>/dev/null || echo 0)"
  log "control: error=${CONTROL_ERR} result_objects=${CONTROL_N} text_len=${CONTROL_TEXTLEN}"

  # --- SUBJECT: pass plain text as query, no query_vector. ---
  SUBJECT_RESP="$(mcp_call search_semantic \
      '{"query":"the quick brown fox jumps over the lazy dog","limit":5}')"
  printf '%s\n' "$SUBJECT_RESP" > "${RES}/b04-subject-response.json"

  SUBJECT_ERR="$(jq -r 'if .error then "yes" else "no" end' <<<"$SUBJECT_RESP" 2>/dev/null || echo parse-error)"
  SUBJECT_N="$(jq -r '[.. | objects | select(has("content_hash") or has("path") or has("id"))] | length' \
                 <<<"$SUBJECT_RESP" 2>/dev/null || echo 0)"
  log "subject: error=${SUBJECT_ERR} result_objects=${SUBJECT_N}"

  if [[ "$CONTROL_ERR" == "yes" ]] || (( CONTROL_N == 0 && CONTROL_TEXTLEN < 3 )); then
    fail "B-04 probe INCONCLUSIVE-FAIL: the CONTROL call (explicit query_vector) returned
          no results or an error. A vector is always its own nearest neighbour, so this
          means the search path itself is broken or the index is unpopulated. Nothing can
          be concluded about the plain-text path until this is fixed.
          See ${RES}/b04-control-response.json"
  elif [[ "$SUBJECT_ERR" == "yes" ]]; then
    pass "B-04 PARTIAL: plain-text query returns an explicit JSON-RPC error rather than a
          silent empty list. The gap exists but is honest. Record as B-04-partial.
          See ${RES}/b04-subject-response.json"
  elif (( SUBJECT_N == 0 )); then
    fail "B-04 CONFIRMED: the control call (query_vector) returned ${CONTROL_N} results, but
          the same intent sent as plain text in \"query\" returned an EMPTY list with no error.
          This matches the read of handler.rs exactly: the dispatch arm for search_semantic /
          search_functions / search_by_concept reads only \"query_vector\", never \"query\", and
          smartfs_ai is never called from handler.rs — so no embedding is ever computed
          server-side. tools.rs advertises the \"query\" field and
          dispatch_tests.rs::test_search_semantic_and_functions_query_handling claims to cover
          it, but that test opens with
              let pool = match connect_pool(&get_db_url()).await { Ok(p) => p, Err(_) => return };
          and therefore passes with ZERO assertions when Postgres is unreachable.
          Artifacts: ${RES}/b04-control-response.json ${RES}/b04-subject-response.json"
  else
    pass "B-04 appears FIXED: plain-text query returned ${SUBJECT_N} results (control ${CONTROL_N}).
          Verify by hand that handler.rs now reads \"query\" and calls smartfs_ai — a green
          result here is the surprising one."
  fi

  # Same differential for search_by_concept.
  BYCON="$(mcp_call search_by_concept '{"query":"brown fox","limit":5}')"
  printf '%s\n' "$BYCON" > "${RES}/b04-search-by-concept.json"
  BYCON_ERR="$(jq -r 'if .error then "yes" else "no" end' <<<"$BYCON" 2>/dev/null || echo parse-error)"
  BYCON_N="$(jq -r '[.. | objects | select(has("content_hash") or has("path") or has("id"))] | length' \
               <<<"$BYCON" 2>/dev/null || echo 0)"
  if [[ "$BYCON_ERR" == "no" ]] && (( BYCON_N == 0 )); then
    fail "B-04 CONFIRMED for search_by_concept: plain-text query returned an empty list with
          no error. See ${RES}/b04-search-by-concept.json"
  else
    pass "search_by_concept plain-text query: error=${BYCON_ERR} results=${BYCON_N} (not a silent empty)"
  fi
fi

# ── cleanup ─────────────────────────────────────────────────────────────────
rm -rf "$WORK" 2>/dev/null || true
info "artifacts written to ${RES}"
stage_end
