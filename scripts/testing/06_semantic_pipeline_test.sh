#!/usr/bin/env bash
#
# 06_semantic_pipeline_test.sh — The Great SmartFS Test, Stage 6.
#
# The one thing stages 1-5 never check: that the semantic layer sees anything.
#
# Stages 1-4 prove SmartFS is a filesystem — POSIX semantics, crash consistency,
# Root Invariants. Stage 5 measures how fast. None of them asks whether the
# thing SmartFS exists *for* actually works: write source code into the
# filesystem and have an agent find a function in it through MCP.
#
# That gap let a real one hide. A survey of the live database found 11 AST nodes
# across 220 versions, 2 function embeddings, and no pg_search extension at all
# — the pipeline is connected end to end and almost nothing has ever flowed
# through it. Nothing failed, because nothing was looking.
#
# The chain this walks, one link at a time, failing at the first break:
#
#   write .rs through the mount
#     -> flush() runs tree-sitter                        -> ast_nodes rows
#     -> release() queues, drain commits (ADR-58)        -> file_versions row
#     -> smartfs-ai worker embeds                        -> ast_embeddings_1536
#     -> MCP search_functions finds it by name           -> the agent can see it
#
# Every link is a separate assertion naming what broke, because "semantic search
# returned nothing" is useless on its own — it could be any of five components.
#
# Usage: sudo scripts/testing/06_semantic_pipeline_test.sh
# See:   docs/testing/the-great-smartfs-test.md

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib_common.sh"

stage_begin "stage6-semantic"
require_cmd jq psql
require_root
require_db "$SMARTFS_DB_URL"

RES="$(results_dir stage6-semantic)"
trap stop_daemon EXIT

start_daemon "$SMARTFS_MOUNT" "$SMARTFS_STORE_PATH" "$SMARTFS_DB_URL"
require_live_mount "$SMARTFS_MOUNT"

WORK="${SMARTFS_MOUNT}/semantic-$$"
mkdir -p "$WORK" || die "mkdir on the mount failed"

# A name no other file in the database can contain, so a hit proves this write
# was indexed rather than some earlier one.
MARKER="quicksilver_ledger_$$"
SRC="${WORK}/probe.rs"

# ── 6.1 tree-sitter runs at flush ───────────────────────────────────────────
info "6.1  write Rust source through the mount; tree-sitter must parse it"

cat > "$SRC" <<RUST
/// Reconciles a ledger against its upstream source.
pub fn ${MARKER}(entries: &[i64], opening_balance: i64) -> i64 {
    entries.iter().fold(opening_balance, |acc, e| acc + e)
}

pub struct ${MARKER}_Config {
    pub tolerance: i64,
}
RUST
sync
quiesce_queue "$SMARTFS_STORE_PATH"

INODE="$(psql_q "SELECT id FROM inode_registry WHERE name = 'probe.rs'
                 ORDER BY created_at DESC LIMIT 1")"
[[ -n "$INODE" ]] || die "no inode_registry row for probe.rs — the write never reached the database"

VERSION="$(psql_q "SELECT id FROM file_versions WHERE inode_id = '${INODE}'
                   ORDER BY version_number DESC LIMIT 1")"
if [[ -z "$VERSION" ]]; then
  fail "no file_versions row after quiesce: the ADR-58 drain never committed this write.
        Everything below depends on it, so the rest of this stage cannot conclude anything."
  stage_end
fi
pass "version row committed for the source file"

AST_COUNT="$(psql_q "SELECT count(*) FROM ast_nodes WHERE version_id = '${VERSION}'")"
if [[ "$AST_COUNT" == "0" ]]; then
  fail "tree-sitter produced NO ast_nodes for a valid Rust file.
        flush() is where extraction happens (validate_and_extract_ast_blocking);
        either it did not run, or it ran and the nodes were dropped before the
        marker was written. Without AST nodes nothing downstream can index
        functions, so search_functions can never work."
else
  pass "tree-sitter extracted ${AST_COUNT} AST node(s)"
fi

FN_ROW="$(psql_q "SELECT count(*) FROM ast_nodes
                  WHERE version_id = '${VERSION}' AND name = '${MARKER}'")"
[[ "$FN_ROW" == "1" ]] \
  && pass "the function is in ast_nodes under its own name" \
  || fail "ast_nodes has no row named '${MARKER}' — the parse ran but did not
          record this function, so no amount of embedding will make it findable"

# ── 6.2 the embedding worker ────────────────────────────────────────────────
info "6.2  smartfs-ai must turn those nodes into embeddings"

STATUS="$(psql_q "SELECT status FROM file_versions WHERE id = '${VERSION}'")"
log "version status immediately after commit: ${STATUS}"

WAITED=0
while (( WAITED < 120 )); do
  STATUS="$(psql_q "SELECT status FROM file_versions WHERE id = '${VERSION}'")"
  [[ "$STATUS" == "clean" ]] && break
  sleep 5; WAITED=$((WAITED + 5))
done

EMB="$(psql_q "SELECT count(*) FROM ast_embeddings_1536 e
               JOIN ast_nodes n ON n.id = e.ast_node_id
               WHERE n.version_id = '${VERSION}'")"

if [[ "$STATUS" != "clean" ]]; then
  BACKLOG="$(psql_q "SELECT count(*) FROM file_versions WHERE status IN ('pending','processing')")"
  OLDEST="$(psql_q "SELECT round(EXTRACT(epoch FROM now() - min(created_at)))
                    FROM file_versions WHERE status = 'pending'")"
  fail "the version is still '${STATUS}' after ${WAITED}s and has ${EMB} embedding(s).
        ${BACKLOG} version(s) are queued and the oldest has waited ${OLDEST}s, which is
        not a slow worker — it is no worker.

        smartfs-ai::run_worker_supervisor (worker.rs:141) is never called by anything:
        the crate has no [[bin]] target, and neither smartfsd nor smartfs-cli
        references smartfs_ai at all. This is the same shape of gap the plan
        catalogued in section 1.1 for smartfs-fuse and that was later found again
        for smartfs-mcp — a complete component with no entry point wiring it in.

        Consequence: every MCP tool that reads embeddings is permanently empty,
        and nothing before this stage existed noticed."
else
  pass "the worker processed the version to 'clean' in ${WAITED}s"
  [[ "$EMB" -gt 0 ]] \
    && pass "${EMB} function-level embedding(s) stored" \
    || fail "version is 'clean' but produced no ast_embeddings_1536 rows —
            the worker ran and indexed nothing, which is worse than not running"
fi

# ── 6.3 what an agent actually sees through MCP ─────────────────────────────
info "6.3  MCP — can an agent find this function?"

mcp_call() {
  local tool="$1" args="$2" req
  [[ -x "$SMARTFS_MCP_BIN" ]] \
    || die "no smartfs-mcp binary at ${SMARTFS_MCP_BIN}; the agent-facing surface
       cannot be exercised. Missing tooling is a FAILURE of this stage."
  req="$(jq -cn --arg t "$tool" --argjson a "$args" '
    {jsonrpc:"2.0",id:1,method:"initialize",
     params:{protocolVersion:"2024-11-05",capabilities:{},
             clientInfo:{name:"stage6",version:"1"}}},
    {jsonrpc:"2.0",method:"notifications/initialized"},
    {jsonrpc:"2.0",id:2,method:"tools/call",params:{name:$t,arguments:$a}}')"
  printf '%s\n' "$req" | timeout 90 env \
      SMARTFS_DATABASE_URL="$SMARTFS_DB_URL" DATABASE_URL="$SMARTFS_DB_URL" \
      "$SMARTFS_MCP_BIN" 2>>"${RES}/mcp-stderr.log" \
    | jq -c 'select(.id == 2)' 2>/dev/null | tail -n1
}

# Counts real hits, not the JSON-RPC envelope. The envelope carries an "id", so
# counting parsed objects reports 1 for an empty result — the defect that made
# stage 1's B-04 probe unable to fail.
hit_count() {
  jq -r '
    (.result.content[0].text // "") as $t
    | if ($t | length) == 0 then 0
      else (($t | fromjson?) // null) as $p
           | if $p == null then -1 elif ($p|type) == "array" then ($p|length) else 1 end
      end' <<<"$1" 2>/dev/null || echo -1
}

SF="$(mcp_call search_functions "$(jq -cn --arg q "reconcile a ledger against upstream" '{query:$q, limit:10}')")"
printf '%s\n' "$SF" > "${RES}/search_functions.json"
SF_ERR="$(jq -r 'if .error then .error.message else "none" end' <<<"$SF" 2>/dev/null || echo parse-error)"
SF_N="$(hit_count "$SF")"
log "search_functions: error=${SF_ERR} hits=${SF_N}"

if [[ "$SF_ERR" != "none" ]]; then
  fail "search_functions returned a JSON-RPC error: ${SF_ERR}
        See ${RES}/search_functions.json"
elif (( SF_N <= 0 )); then
  fail "search_functions returned ${SF_N} hits for a function this stage just wrote.
        This is the end-to-end symptom: whatever broke above, an agent asking
        SmartFS about its own contents gets nothing back."
else
  pass "search_functions returned ${SF_N} hit(s)"
fi

# diff_functions needs no embeddings — it reads ast_nodes directly, so it
# isolates "the parser worked" from "the embedding worker worked".
printf '\npub fn %s_v2(x: i64) -> i64 { x * 2 }\n' "$MARKER" >> "$SRC"
sync
quiesce_queue "$SMARTFS_STORE_PATH"

DF="$(mcp_call diff_functions "$(jq -cn --arg p "/semantic-$$/probe.rs" '{path:$p, v1:1, v2:2}')")"
printf '%s\n' "$DF" > "${RES}/diff_functions.json"
DF_ERR="$(jq -r 'if .error then .error.message else "none" end' <<<"$DF" 2>/dev/null || echo parse-error)"
if [[ "$DF_ERR" == "none" ]]; then
  pass "diff_functions answered without error (AST path is reachable without embeddings)"
else
  fail "diff_functions failed: ${DF_ERR}
        This reads ast_nodes directly, so a failure here is the parser or the
        version chain, not the embedding worker. See ${RES}/diff_functions.json"
fi

# ── 6.4 full-text search must not pretend ──────────────────────────────────
info "6.4  search_fulltext — present, or explicitly absent"

HAS_PGSEARCH="$(psql_q "SELECT count(*) FROM pg_extension WHERE extname = 'pg_search'")"
BM25="$(psql_q "SELECT count(*) FROM pg_indexes WHERE indexname LIKE '%bm25%'")"

FT="$(mcp_call search_fulltext "$(jq -cn --arg q "$MARKER" '{query:$q, limit:5}')")"
printf '%s\n' "$FT" > "${RES}/search_fulltext.json"
FT_ERR="$(jq -r 'if .error then .error.message else "none" end' <<<"$FT" 2>/dev/null || echo parse-error)"
FT_N="$(hit_count "$FT")"

if [[ "$HAS_PGSEARCH" == "0" ]]; then
  fail "pg_search is NOT installed (bm25 indexes: ${BM25}), so search_fulltext cannot work.
        Migration 006 guards its index creation behind pg_available_extensions, so the
        migration succeeds and the capability silently does not exist. ADR-54 chose
        pg_search deliberately for Polish stemming; a deployment without it is running
        without a documented feature, and that has to be visible rather than inferred
        from an empty result. Observed: error=${FT_ERR} hits=${FT_N}"
elif [[ "$FT_ERR" != "none" ]]; then
  fail "pg_search is installed but search_fulltext errored: ${FT_ERR}"
elif (( FT_N <= 0 )); then
  fail "search_fulltext found nothing for a token written into this very file"
else
  pass "search_fulltext returned ${FT_N} hit(s)"
fi

rm -rf "$WORK" 2>/dev/null || true
info "artifacts written to ${RES}"
stage_end
