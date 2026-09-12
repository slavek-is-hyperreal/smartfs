-- ─────────────────────────────────────────────────────────────────
-- migrations/003_embeddings.sql
-- Wymaga: 001_core_schema.sql, 002_embedding_models.sql
-- Baza: SmartFS_Architecture_v4_5.md §6
--
-- Warstwy embeddingów ogólnych (§10.3: "Wszystkie pliki"). Wszystkie FK
-- do file_versions — musi istnieć wcześniej (001).
--
-- UWAGA (rozbieżność między dokumentami bazowymi, odnotowana świadomie
-- zamiast po cichu rozstrzygnięta): SmartFS_v4.5_to_v5.0_fixes.md FIX-07
-- opisuje docelowe partycjonowanie LIST(plugin_type) dla tabel embeddingów
-- i przypisuje je do "003_embeddings.sql" w swojej prozie — ALE FIX-06/07
-- dotyczą wprost `SmartFS_Known_Limitations_Roadmap.md` §3b (`embedding_index_shards`),
-- czyli POST-MVP skalowania przy ścianie pamięci HNSW, nie bazowego
-- schematu §6. Ta migracja implementuje §6 dosłownie (bez partycjonowania)
-- — FIX-07 zostaje zadaniem dla przyszłej migracji, gdy roadmap §3b
-- faktycznie wchodzi w zakres prac. Migracja 005_semantic_consolidation.sql
-- (v6.0) już zakłada TĘ, niespartycjonowaną wersję (dodaje plugin_type
-- przez ALTER TABLE, co nie miałoby sensu, gdyby kolumna już istniała).
-- ─────────────────────────────────────────────────────────────────

CREATE TABLE embeddings_384 (
    version_id UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    model_id   UUID NOT NULL REFERENCES embedding_models(id),
    embedding  vector(384) NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (version_id, model_id)
);
CREATE INDEX idx_embeddings_384_hnsw
    ON embeddings_384 USING hnsw (embedding vector_cosine_ops);

CREATE TABLE embeddings_768 (
    version_id UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    model_id   UUID NOT NULL REFERENCES embedding_models(id),
    embedding  vector(768) NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (version_id, model_id)
);
CREATE INDEX idx_embeddings_768_hnsw
    ON embeddings_768 USING hnsw (embedding vector_cosine_ops);

-- ── Weryfikacja po migracji ──────────────────────────────────────────
-- [ ] embeddings_384/768 istnieją, BEZ kolumny plugin_type (dodawana dopiero w 005)
-- [ ] indeksy HNSW istnieją na obu tabelach
