-- ─────────────────────────────────────────────────────────────────
-- migrations/004_ast_nodes.sql
-- Wymaga: 001_core_schema.sql, 002_embedding_models.sql
-- Baza: SmartFS_Architecture_v4_5.md §6, §11 (AST — kod jako pierwszoklasowy obywatel)
--
-- Nie partycjonowane wg FIX-07 — patrz uwaga w 003_embeddings.sql o tej
-- samej rozbieżności między dokumentami; FIX-06/07 to roadmap §3b (post-MVP),
-- nie ten bazowy schemat.
-- ─────────────────────────────────────────────────────────────────

CREATE TABLE ast_nodes (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    version_id   UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,   -- "function", "struct", "impl", "class"
    name         TEXT NOT NULL,   -- np. "write_blob_streaming"
    start_line   INT NOT NULL,
    end_line     INT NOT NULL,
    source       TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),

    -- UNIQUE zapewnia idempotencję workera AST.
    -- Uwaga: NIE zapewnia cross-version dedup (każda wersja ma własne wiersze).
    -- Plik 100× edytowany z 50 funkcjami = 5000 wierszy — OK na MVP.
    -- Cross-version dedup wymagałby osobnej tabeli ast_content(content_hash UNIQUE)
    -- + tabeli złączeniowej version_ast_nodes — świadoma rezygnacja w MVP
    -- (patrz SmartFS_Known_Limitations_Roadmap.md §4).
    CONSTRAINT unique_ast_node UNIQUE (version_id, content_hash)
);

CREATE INDEX idx_ast_nodes_version ON ast_nodes(version_id);
CREATE INDEX idx_ast_nodes_name    ON ast_nodes(kind, name);
CREATE INDEX idx_ast_nodes_hash    ON ast_nodes(content_hash);

-- Dedykowana tabela embeddingów dla AST nodes.
-- FK do ast_nodes.id (NIE do file_versions.id — to był bug w v3.0).
CREATE TABLE ast_embeddings_1536 (
    ast_node_id UUID NOT NULL REFERENCES ast_nodes(id) ON DELETE CASCADE,
    model_id    UUID NOT NULL REFERENCES embedding_models(id),
    embedding   vector(1536) NOT NULL,
    is_current  BOOLEAN NOT NULL DEFAULT TRUE,  -- FALSE gdy nowa wersja pliku wypiera tę
    created_at  TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (ast_node_id, model_id)
    -- ON CONFLICT DO NOTHING wymagane przy INSERT — retry workera po crashu
);
-- Indeks HNSW tylko na bieżących wektorach — eliminuje eksplozję przy
-- 100 wersjach × 50 funkcjach.
CREATE INDEX idx_ast_embeddings_1536_hnsw
    ON ast_embeddings_1536 USING hnsw (embedding vector_cosine_ops)
    WHERE is_current = TRUE;

-- is_current jest zarządzane przez workera (smartfs-ai), NIE przez cow_commit
-- (FIX-02, SmartFS_v4.5_to_v5.0_fixes.md). Po zembedowaniu dowolnej wersji
-- inode: is_current=TRUE tylko dla wektorów najnowszej wersji tego inode,
-- reszta FALSE. Order-independent — SKIP LOCKED może kończyć wersje w innej
-- kolejności niż zostały utworzone, więc worker zawsze przelicza względem
-- prawdziwie najnowszej wersji, nigdy względem "poprzedniej" w sensie czasu
-- zapisu. Patrz docs/crates/smartfs-db.md (Root Invariant nowy w v5.0) i
-- base-v4.5-v5.0/SmartFS_v4.5_to_v5.0_fixes.md FIX-02 dla pełnego kodu.

-- ── Weryfikacja po migracji ──────────────────────────────────────────
-- [ ] ast_embeddings_1536 FK wskazuje na ast_nodes.id, NIE na file_versions.id (bug z v3.0)
-- [ ] indeks HNSW na ast_embeddings_1536 jest partial (WHERE is_current = TRUE)
-- [ ] BRAK kolumny plugin_type tutaj (dodawana dopiero w migracji 005 — patrz jej komentarz)
