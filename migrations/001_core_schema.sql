-- ─────────────────────────────────────────────────────────────────
-- migrations/001_core_schema.sql
-- Baza: SmartFS_Architecture_v4_5.md §4.2, §6 — z poprawką FIX-01
-- (SmartFS_v4.5_to_v5.0_fixes.md) już wtopioną w `blobs` poniżej.
--
-- Zawiera: extensions, processing_status enum, storage_backends,
-- inode_registry, file_versions, blobs.
--
-- Kolejność jest wymuszona zależnościami FK:
--   storage_backends  (brak zależności)
--       ↓ (backend_id)
--   inode_registry    (self-referencing parent_id)
--       ↓ (inode_id)
--   file_versions
--   blobs             (backend_id → storage_backends; blob_id NIE jest FK —
--                      to UUID w blob store, patrz smartfs-store)
--
-- UWAGA (domknięcie luki): §4.2 oryginalnego dokumentu architektury nie
-- przypisuje `storage_backends` do żadnej konkretnej migracji — trafia tu,
-- bo `inode_registry.backend_id` i `blobs.backend_id` są do niej FK, więc
-- musi istnieć pierwsza.
-- ─────────────────────────────────────────────────────────────────

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TYPE processing_status AS ENUM (
    'clean',        -- embedding gotowy
    'pending',      -- czeka na worker
    'processing',   -- zarezerwowane przez workera (SKIP LOCKED)
    'failed',       -- błąd; patrz retry_count
    'syntax_error'  -- zapis z flagą --force, AST niedostępne
);

-- ── storage_backends (§4.2) ─────────────────────────────────────────
CREATE TABLE storage_backends (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name         TEXT NOT NULL,
    backend_type TEXT NOT NULL,    -- "local", "ipfs", "s3", "virtual_wiki", "overlay"
    config       JSONB NOT NULL,
    is_virtual   BOOLEAN DEFAULT FALSE,
    priority     INT DEFAULT 0,
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- ── inode_registry (§6) ──────────────────────────────────────────────
CREATE TABLE inode_registry (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    ino          BIGSERIAL UNIQUE NOT NULL, -- u64 dla FUSE kernel
    parent_id    UUID REFERENCES inode_registry(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    is_dir       BOOLEAN NOT NULL DEFAULT FALSE,

    -- Atrybuty POSIX
    uid          INT NOT NULL DEFAULT 1000,
    gid          INT NOT NULL DEFAULT 1000,
    mode         INT NOT NULL DEFAULT 33188, -- 0o100644
    size         BIGINT NOT NULL DEFAULT 0,
    nlink        INT NOT NULL DEFAULT 1,
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),

    -- Storage
    current_blob_id    UUID,
    backend_id         UUID REFERENCES storage_backends(id),
    on_prem            BOOLEAN NOT NULL DEFAULT TRUE,
    compression_level  SMALLINT NOT NULL DEFAULT 3,
    versioning_enabled BOOLEAN NOT NULL DEFAULT TRUE,

    CONSTRAINT blob_or_empty_or_virtual CHECK (
        is_dir = TRUE
        OR on_prem = FALSE
        OR versioning_enabled = FALSE   -- tryb index: plik istnieje in-place, blob_id=NULL, size>0
        OR (current_blob_id IS NULL AND size = 0)  -- pusty plik (touch/create)
        OR current_blob_id IS NOT NULL
    ),
    CONSTRAINT unique_name_per_directory UNIQUE (parent_id, name)
);

-- Root inode: ino=1, is_dir=TRUE
-- Używamy jawnego INSERT z ino=1, potem przesuwamy sekwencję
-- żeby kolejny automatyczny INSERT dostał 2, nie 1.
INSERT INTO inode_registry (ino, name, is_dir, uid, gid, mode)
    VALUES (1, '', TRUE, 0, 0, 16877); -- 0o40755
SELECT setval('inode_registry_ino_seq', 1);
-- setval(seq, 1) ustawia "ostatnio zwrócona wartość = 1"
-- nextval zwróci 2 dla pierwszego INSERT bez jawnego ino

CREATE INDEX idx_inode_parent ON inode_registry(parent_id);
CREATE INDEX idx_inode_lookup ON inode_registry(parent_id, name);
CREATE INDEX idx_inode_ino    ON inode_registry(ino);

-- ── file_versions (§6) — PRZED tabelami embeddingów, wymagane przez FK ──
CREATE TABLE file_versions (
    id             UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    inode_id       UUID NOT NULL REFERENCES inode_registry(id) ON DELETE CASCADE,
    version_number INT NOT NULL,
    created_at     TIMESTAMP WITH TIME ZONE DEFAULT NOW(),

    -- Dane fizyczne
    blob_id         UUID,
    size            BIGINT NOT NULL,
    compressed_size BIGINT,

    -- Overlay: external_path per WERSJA, nie per inode
    -- NULL dla natywnych wersji SmartFS; ścieżka do oryginału dla wersji importowanych
    external_path  TEXT,

    -- Tożsamość treści (hash PRZED kompresją — Root Invariant #1)
    content_hash   TEXT NOT NULL,
    -- CID tylko dla plików <256KB; większe wymagają UnixFS DAG-PB (post-MVP)
    cid            TEXT,
    ipfs_pinned    BOOLEAN DEFAULT FALSE,

    -- Plugin
    special_type   TEXT NOT NULL DEFAULT 'generic',
    special_data   JSONB NOT NULL DEFAULT '{}'::jsonb,

    -- Pipeline AI
    status         processing_status NOT NULL DEFAULT 'pending',
    retry_count    INT NOT NULL DEFAULT 0,

    -- DAG historii (NULL dla pierwszej wersji)
    -- Uwaga: w MVP historia jest zawsze liniowa — DAG to fundament pod post-MVP branching
    parent_version_id UUID REFERENCES file_versions(id),

    CONSTRAINT unique_version_per_inode UNIQUE (inode_id, version_number)
);

CREATE INDEX idx_versions_inode   ON file_versions(inode_id);
CREATE INDEX idx_versions_hash    ON file_versions(content_hash);
CREATE INDEX idx_versions_cid     ON file_versions(cid) WHERE cid IS NOT NULL;
CREATE INDEX idx_versions_special ON file_versions USING gin (special_data);
CREATE INDEX idx_versions_pending ON file_versions(status)
    WHERE status IN ('pending', 'processing');

-- ── blobs (§6, WERSJA PO FIX-01 — refcount-free) ────────────────────
-- Punkt serializacji dedupu (content-addressed), zastępuje
-- pg_advisory_xact_lock + hashtext (v4.0, ADR-04, superseded przez ADR-40).
-- GC: scan po file_versions (SmartFS_v4.5_to_v5.0_fixes.md §7.5), NIE
-- refcount. Refcount celowo pominięty: nie da się go utrzymać atomowo
-- względem referencji tworzonej w osobnej transakcji (krok 2 cow_commit) —
-- to był FIX-01, znaleziony w review v4.5.
CREATE TABLE blobs (
    content_hash    TEXT PRIMARY KEY,          -- pełne SHA-256 hex (256 bit, zero kolizji)
    blob_id         UUID NOT NULL,             -- UUID pliku w blob store (nie FK — patrz smartfs-store)
    backend_id      UUID REFERENCES storage_backends(id),
    size            BIGINT NOT NULL,           -- rozmiar oryginalny (przed kompresją)
    compressed_size BIGINT,                    -- ustawiane po udanym store.put
    created_at      TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
-- Dedup: INSERT ... ON CONFLICT (content_hash) DO UPDATE SET content_hash = blobs.content_hash
--        RETURNING blob_id, (xmax = 0) AS inserted
-- (no-op UPDATE — jedyny cel to zwrócenie istniejącego wiersza przy konflikcie)
-- GC-by-scan (post-MVP):
--   DELETE FROM blobs b WHERE NOT EXISTS (
--       SELECT 1 FROM file_versions fv WHERE fv.blob_id = b.blob_id
--   ) AND b.created_at < now() - interval '1 hour'   -- grace window, patrz FIX-01 §7.5
--   RETURNING blob_id, backend_id;
-- Bloby z external_path NIE mają wiersza w blobs — GC ich nie dotyczy (ADR-38).

-- ── Weryfikacja po migracji ──────────────────────────────────────────
-- [ ] storage_backends istnieje przed inode_registry/blobs (kolejność FK)
-- [ ] inode_registry ma dokładnie jeden wiersz z ino=1 (root), reszta sekwencji zaczyna się od 2
-- [ ] blobs NIE MA kolumny refcount (FIX-01 — jeśli jest, migracja jest z przedwersji v4.5, nie v5.0)
-- [ ] file_versions.content_hash jest NOT NULL (Root Invariant #1: hash liczony przed kompresją)
