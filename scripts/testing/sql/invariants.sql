-- invariants.sql — machine-checkable predicates over SmartFS database state.
--
-- Used by 04_crash_consistency_test.sh after every crash-and-restart round.
-- Every row emitted is a VIOLATION. An empty result set is the passing outcome.
--
-- Output contract (three columns, pipe-separated by psql -qtAX):
--     <invariant>|<count>|<detail>
--
-- Deliberately written as predicates rather than prose: section 6 of
-- docs/testing/the-great-smartfs-test.md explains that a future model checker
-- (Metis/Themis) will need exactly this shape.
--
-- Root Invariant #1 (content_hash = SHA-256 of the ORIGINAL bytes) can only be
-- partially checked here — the structural half. The cryptographic half requires
-- reading and decompressing the blob, which the shell script does.

\set ON_ERROR_STOP on

-- ── Invariant #1 (structural half) ──────────────────────────────────────────
-- content_hash must be a full 64-char lowercase SHA-256 hex digest.
SELECT 'I1.hash_format', count(*),
       coalesce(string_agg(DISTINCT left(id::text, 8), ',' ), '')
FROM file_versions
WHERE content_hash !~ '^[0-9a-f]{64}$'
HAVING count(*) > 0;

-- The same content_hash must never disagree about size. A hash taken after
-- compression, or a hash-collision-by-truncation bug, shows up here.
SELECT 'I1.hash_size_disagreement', count(*),
       coalesce(string_agg(DISTINCT content_hash, ',' ), '')
FROM (
    SELECT content_hash FROM file_versions
    WHERE external_path IS NULL
    GROUP BY content_hash HAVING count(DISTINCT size) > 1
) t
HAVING count(*) > 0;

-- blobs.size must agree with every file_versions row carrying that hash.
SELECT 'I1.blob_size_mismatch', count(*),
       coalesce(string_agg(DISTINCT b.content_hash, ','), '')
FROM blobs b
JOIN file_versions fv ON fv.content_hash = b.content_hash
WHERE fv.external_path IS NULL AND fv.size <> b.size
HAVING count(*) > 0;

-- ── Invariant #2 (copy-on-write versioning) ─────────────────────────────────
-- version_number must be unique per inode. The UNIQUE constraint should make
-- this impossible; check anyway, because a crash test that trusts constraints
-- is not testing them.
SELECT 'I2.duplicate_version_number', count(*),
       coalesce(string_agg(DISTINCT inode_id::text, ','), '')
FROM (
    SELECT inode_id, version_number FROM file_versions
    GROUP BY inode_id, version_number HAVING count(*) > 1
) t
HAVING count(*) > 0;

-- version_number must be contiguous 1..n per inode. A gap means a committed
-- version vanished; an off-by-one start means the counter was not derived from
-- MAX(version_number)+1.
SELECT 'I2.non_contiguous_versions', count(*),
       coalesce(string_agg(inode_id::text || ':' || lo || '-' || hi || '/' || n, ','), '')
FROM (
    SELECT inode_id, min(version_number) AS lo, max(version_number) AS hi,
           count(*) AS n
    FROM file_versions GROUP BY inode_id
) t
WHERE lo <> 1 OR hi <> n
HAVING count(*) > 0;

-- The history DAG must be unbroken: exactly one root (parent_version_id NULL)
-- per inode, and it must be version 1.
SELECT 'I2.broken_version_chain', count(*),
       coalesce(string_agg(inode_id::text, ','), '')
FROM (
    SELECT inode_id FROM file_versions
    WHERE parent_version_id IS NULL
    GROUP BY inode_id HAVING count(*) <> 1
    UNION
    SELECT inode_id FROM file_versions
    WHERE parent_version_id IS NULL AND version_number <> 1
) t
HAVING count(*) > 0;

-- A parent must belong to the same inode.
SELECT 'I2.cross_inode_parent', count(*),
       coalesce(string_agg(c.id::text, ','), '')
FROM file_versions c
JOIN file_versions p ON p.id = c.parent_version_id
WHERE p.inode_id <> c.inode_id
HAVING count(*) > 0;

-- ── Invariant #3 (the daemon is the only legal path to data) ────────────────
-- Every non-external version must reference a blob that exists in `blobs`.
-- A dangling reference means a file exists that cannot be read back — the
-- database-side symptom of a crash between KROK 1 and KROK 2.
SELECT 'I3.dangling_blob_reference', count(*),
       coalesce(string_agg(DISTINCT fv.content_hash, ','), '')
FROM file_versions fv
LEFT JOIN blobs b ON b.content_hash = fv.content_hash
WHERE fv.external_path IS NULL AND fv.blob_id IS NOT NULL AND b.content_hash IS NULL
HAVING count(*) > 0;

-- inode_registry.current_blob_id must match the newest version's blob_id.
SELECT 'I3.current_blob_desync', count(*),
       coalesce(string_agg(ir.id::text, ','), '')
FROM inode_registry ir
JOIN LATERAL (
    SELECT blob_id FROM file_versions
    WHERE inode_id = ir.id ORDER BY version_number DESC LIMIT 1
) latest ON TRUE
WHERE ir.is_dir = FALSE
  AND ir.current_blob_id IS DISTINCT FROM latest.blob_id
HAVING count(*) > 0;

-- Reported as information, not an automatic failure: blobs with no referencing
-- version are legitimate GC candidates (a crash at K5 produces exactly one).
-- The shell script fails the stage only if this count GROWS across rounds.
SELECT 'I3.orphan_blobs_INFO', count(*), ''
FROM blobs b
WHERE NOT EXISTS (SELECT 1 FROM file_versions fv WHERE fv.content_hash = b.content_hash)
HAVING count(*) > 0;

-- ── Invariant #4 (migrations are explicit SQL files; no runtime DDL) ────────
-- FIX-01 regression: refcount-free dedup is required.
SELECT 'I4.blobs_refcount_reintroduced', count(*), 'blobs.refcount exists'
FROM information_schema.columns
WHERE table_schema = 'public' AND table_name = 'blobs' AND column_name = 'refcount'
HAVING count(*) > 0;

-- Core tables must all still be present.
SELECT 'I4.missing_core_table', count(*),
       coalesce(string_agg(t, ','), '')
FROM unnest(ARRAY['storage_backends','inode_registry','file_versions','blobs']) AS t
WHERE NOT EXISTS (
    SELECT 1 FROM information_schema.tables
    WHERE table_schema = 'public' AND table_name = t
)
HAVING count(*) > 0;

-- ── FIX-02 (is_current belongs to the worker, and is single-valued) ─────────
-- Guarded so the file still runs on a schema where the AST embedding table is
-- absent. At most one is_current=TRUE per ast_node is the actual invariant;
-- more than one is the exact bug FIX-02 was written to eliminate.
DO $$
DECLARE bad bigint;
BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.tables
             WHERE table_schema='public' AND table_name='ast_embeddings_1536') THEN
    EXECUTE $q$
        SELECT count(*) FROM (
            SELECT ast_node_id FROM ast_embeddings_1536
            WHERE is_current GROUP BY ast_node_id HAVING count(*) > 1
        ) t $q$ INTO bad;
    IF bad > 0 THEN
      RAISE WARNING 'FIX02.multiple_is_current|%|ast nodes with >1 is_current=TRUE', bad;
    END IF;
  END IF;
END $$;

-- ── pipeline liveness: nothing may be stuck in 'processing' after a restart ─
SELECT 'PIPE.stuck_processing', count(*),
       coalesce(string_agg(id::text, ','), '')
FROM file_versions
WHERE status = 'processing' AND created_at < now() - interval '10 minutes'
HAVING count(*) > 0;
