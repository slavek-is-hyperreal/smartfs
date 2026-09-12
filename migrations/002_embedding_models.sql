-- ─────────────────────────────────────────────────────────────────
-- migrations/002_embedding_models.sql
-- Wymaga: 001_core_schema.sql
-- Baza: SmartFS_Architecture_v4_5.md §6
--
-- UWAGA: wiersze poniżej to stan v4.5 (przed cutoverem na Qwen, ADR-49
-- w v6.0). Migracja 005_semantic_consolidation.sql dopiero PÓŹNIEJ dodaje
-- modele Qwen i przełącza is_default — nie zmieniaj kolejności migracji,
-- żeby historia cutoverów (patrz ADR-49) zostawała czytelna w samych
-- migracjach, nie tylko w dokumentacji.
-- ─────────────────────────────────────────────────────────────────

CREATE TABLE embedding_models (
    id         UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name       TEXT NOT NULL UNIQUE,
    dimensions INT NOT NULL,
    version    TEXT,
    is_local   BOOLEAN NOT NULL DEFAULT TRUE,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

INSERT INTO embedding_models (name, dimensions, version, is_local, is_default) VALUES
    ('all-MiniLM-L6-v2',      384,  '1.0', TRUE,  TRUE),
    ('BGE-M3',                 768,  '1.0', TRUE,  FALSE),
    ('text-embedding-3-large', 1536, '1.0', FALSE, FALSE);

-- ── Weryfikacja po migracji ──────────────────────────────────────────
-- [ ] dokładnie jeden wiersz is_default=TRUE (all-MiniLM-L6-v2, do cutoveru w migracji 005)

-- Partial unique index: co najwyżej jeden is_default=TRUE w całej tabeli (S-13).
-- Migracja 005 robi UPDATE SET is_default=FALSE na starym domyślnym i INSERT
-- nowego z is_default=TRUE — kolejność ma znaczenie, bo index działa od razu.
CREATE UNIQUE INDEX uq_single_default_embedding_model
    ON embedding_models (is_default)
    WHERE is_default = TRUE;
