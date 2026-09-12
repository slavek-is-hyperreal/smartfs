-- ─────────────────────────────────────────────────────────────────
-- migrations/006_fulltext_search.sql
-- Wymaga: 001_core_schema.sql, 004_ast_nodes.sql, 005_semantic_consolidation.sql
--
-- Patrz: docs/adr/ADR-54-fulltext-search-backend.md
--
-- UWAGA IMPLEMENTACYJNA: składnia `CREATE INDEX ... USING bm25 (...) WITH
-- (key_field=...)` odpowiada pg_search w wersji ~0.22.x (2026). API tego
-- rozszerzenia ewoluuje szybko — przed uruchomieniem tej migracji zweryfikuj
-- dokładną składnię przeciwko aktualnemu README pg_search dla zainstalowanej
-- wersji, zamiast ufać tej migracji w ciemno.
-- ─────────────────────────────────────────────────────────────────

-- ── 1. Rozszerzenie ─────────────────────────────────────────────────
-- CASCADE dociąga pgvector, od którego pg_search zależy (i tak go używamy
-- od migracji 005 — brak konfliktu, tylko współdzielenie).
CREATE EXTENSION IF NOT EXISTS pg_search CASCADE;

-- ── 2. Kolumna tekstu przeszukiwalnego na poziomie pliku ────────────
-- POWÓD (ADR-54): embeddingi warstwy ogólnej (embeddings_384/768/1024_qwen)
-- są dziś liczone z bajtów czytanych transientnie ze store'u przez
-- smartfs-ai i odrzucanych po inferencji — nic z tego tekstu nie trafiało
-- dotąd do Postgresa. BM25 potrzebuje fizycznej kolumny do zaindeksowania,
-- więc smartfs-ai zapisuje ten sam tekst raz, przy tej samej operacji
-- odczytu, która i tak już zachodzi (patrz docs/crates/smartfs-ai.md).
-- NULL dla plików, których treść nie jest tekstem UTF-8 i których wtyczka
-- nie ma żadnych opisowych pól string w schemacie.
ALTER TABLE file_versions ADD COLUMN search_text TEXT;

-- ── 3. Indeksy BM25 ──────────────────────────────────────────────────

-- 3a. Warstwa plikowa — partial index, bo search_text bywa NULL.
CREATE INDEX idx_file_versions_bm25
    ON file_versions
    USING bm25 (id, search_text)
    WITH (key_field = 'id')
    WHERE search_text IS NOT NULL;

-- 3b. Warstwa kodu per-funkcja — source już istnieje od v4.5 §3.7,
--     zero nowej kolumny, tylko indeks nad tym, co smartfs-ai i tak
--     czyta do embeddingu AST.
CREATE INDEX idx_ast_nodes_bm25
    ON ast_nodes
    USING bm25 (id, source)
    WITH (key_field = 'id');

-- ── Weryfikacja po migracji ──────────────────────────────────────────
-- [ ] pg_search i pgvector obecne w \dx
-- [ ] file_versions.search_text istnieje, nullable
-- [ ] idx_file_versions_bm25 i idx_ast_nodes_bm25 istnieją (\d file_versions, \d ast_nodes)
-- [ ] smartfs-ai wypełnia search_text przy KAŻDYM przebiegu generic-embedding
--     (docs/crates/smartfs-ai.md) — bez tego indeks BM25 zostanie pusty
--     mimo poprawnie wykonanej migracji
