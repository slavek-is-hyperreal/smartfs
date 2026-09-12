-- ─────────────────────────────────────────────────────────────────
-- migrations/005_semantic_consolidation.sql
-- Wymaga: 001_core_schema.sql, 002_embedding_models.sql, 003_embeddings.sql, 004_ast_nodes.sql
--
-- v6.0, po zamknięciu luk zgłoszonych w review: patrz docs/03-consolidation-design.md
-- §3 (dlaczego plugin_type jest tu denormalizowane) i §4b (merge_centroids).
-- ─────────────────────────────────────────────────────────────────

-- ── 1. Domyślny model Qwen (ADR-49) ─────────────────────────────────
INSERT INTO embedding_models (name, dimensions, version, is_local, is_default) VALUES
    ('Qwen3-Embedding-0.6B',     1024, '1.0', TRUE, TRUE),   -- nowy domyślny
    ('Qwen3-Embedding-4B',       2560, '1.0', TRUE, FALSE),  -- opt-in "accurate", zajmuje slot BGE-M3
    ('Qwen3-VL-Embedding-2B',    1024, '1.0', TRUE, FALSE);  -- opt-in, obrazy

UPDATE embedding_models SET is_default = FALSE WHERE name = 'all-MiniLM-L6-v2';

-- ── 2. plugin_type DENORMALIZOWANY na każdej tabeli embeddingów ─────
-- POWÓD (zamknięcie luki z review): ani ast_embeddings_1536, ani
-- embeddings_384/768 nie mają dziś plugin_type — jest osiągalny tylko
-- pośrednio (ast_embeddings_1536 -> ast_nodes -> file_versions.special_type,
-- albo embeddings_384 -> file_versions.special_type). Konsolidacja roszczący
-- batch po (plugin_type, model_id) potrzebowałaby JOIN-a przy KAŻDYM batchu.
-- Denormalizujemy plugin_type wprost na embedding, wypełniany przez
-- smartfs-ai w momencie INSERT-u (smartfs-ai i tak zna special_type pliku,
-- który właśnie embeduje — zero dodatkowego zapytania po jego stronie).
-- Backfill dla istniejących wierszy jest jednorazowy, w tej migracji.

ALTER TABLE embeddings_384 ADD COLUMN plugin_type TEXT;
UPDATE embeddings_384 e SET plugin_type = fv.special_type
    FROM file_versions fv WHERE fv.id = e.version_id;
ALTER TABLE embeddings_384 ALTER COLUMN plugin_type SET NOT NULL;

ALTER TABLE embeddings_768 ADD COLUMN plugin_type TEXT;
UPDATE embeddings_768 e SET plugin_type = fv.special_type
    FROM file_versions fv WHERE fv.id = e.version_id;
ALTER TABLE embeddings_768 ALTER COLUMN plugin_type SET NOT NULL;

ALTER TABLE ast_embeddings_1536 ADD COLUMN plugin_type TEXT;
UPDATE ast_embeddings_1536 ae SET plugin_type = fv.special_type
    FROM ast_nodes an JOIN file_versions fv ON an.version_id = fv.id
    WHERE an.id = ae.ast_node_id;
ALTER TABLE ast_embeddings_1536 ALTER COLUMN plugin_type SET NOT NULL;

-- ── 3. Kolumna consolidated (working-memory flag) na wszystkich ────
ALTER TABLE embeddings_384        ADD COLUMN consolidated BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE embeddings_768        ADD COLUMN consolidated BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE ast_embeddings_1536   ADD COLUMN consolidated BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX idx_embeddings_384_unconsolidated
    ON embeddings_384 (plugin_type, created_at) WHERE consolidated = FALSE;
CREATE INDEX idx_embeddings_768_unconsolidated
    ON embeddings_768 (plugin_type, created_at) WHERE consolidated = FALSE;
CREATE INDEX idx_ast_embeddings_1536_unconsolidated
    ON ast_embeddings_1536 (plugin_type, created_at) WHERE consolidated = FALSE AND is_current = TRUE;

-- Nowa tabela embeddingów Qwen — plugin_type i consolidated od razu w CREATE,
-- nie jako ALTER, bo tabela jest nowa w tej samej migracji.
CREATE TABLE embeddings_1024_qwen (
    version_id   UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    model_id     UUID NOT NULL REFERENCES embedding_models(id),
    plugin_type  TEXT NOT NULL,
    embedding    vector(1024) NOT NULL,
    consolidated BOOLEAN NOT NULL DEFAULT FALSE,
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (version_id, model_id)
);
CREATE INDEX idx_embeddings_1024_qwen_hnsw
    ON embeddings_1024_qwen USING hnsw (embedding vector_cosine_ops);
CREATE INDEX idx_embeddings_1024_qwen_unconsolidated
    ON embeddings_1024_qwen (plugin_type, created_at) WHERE consolidated = FALSE;

-- ── 4. Progi konsolidacji per (plugin_type, model_id) ───────────────
-- ZAMKNIĘCIE LUKI (config scoping): ConsolidationConfig w kodzie Rust NIE
-- jest już jedną globalną wartością — jest ładowana per kombinacja z tej
-- tabeli. calibrate_join_threshold (smartfs-cli init) zapisuje tu wynik
-- kalibracji zamiast do pliku smartfs.toml, żeby różne pluginy/modele
-- mogły mieć różne progi bez restartu demona.
CREATE TABLE consolidation_thresholds (
    plugin_type              TEXT NOT NULL,
    model_id                 UUID NOT NULL REFERENCES embedding_models(id),
    backlog_threshold        BIGINT NOT NULL DEFAULT 500,
    idle_before_sleep_secs   INT NOT NULL DEFAULT 1800,   -- 30 min
    max_wait_secs            INT NOT NULL DEFAULT 86400,  -- 24h — patrz docs/03 §2
    join_threshold           DOUBLE PRECISION NOT NULL,   -- BEZ sensownej wartości domyślnej — patrz docs/03 §5, wymaga kalibracji
    split_variance_threshold DOUBLE PRECISION NOT NULL DEFAULT 0.15,
    max_members_per_centroid BIGINT NOT NULL DEFAULT 5000,
    batch_size               INT NOT NULL DEFAULT 200,
    calibrated_at            TIMESTAMP WITH TIME ZONE,
    updated_at               TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    PRIMARY KEY (plugin_type, model_id)
);
-- Wiersz w tej tabeli jest WARUNKIEM koniecznym do uruchomienia supervisora
-- dla danej kombinacji — patrz docs/03 §2b. Brak wiersza = supervisor się
-- nie odpala dla tej kombinacji (fail-safe, nie fail-open: lepiej nic nie
-- konsolidować niż konsolidować z niewykalibrowanym join_threshold=0).

-- ── 5. Graf centroidów — pełne DDL dla WSZYSTKICH czterech wymiarów ─
-- ZAMKNIĘCIE LUKI: poprzednia wersja migracji miała pełne DDL tylko dla
-- 1536 i komentarz "replikować analogicznie". Poniżej wszystkie cztery,
-- jawnie, żeby różnice w kluczu obcym (ast_node_id vs version_id) nie
-- zostały zgadnięte niespójnie.

-- 5a. AST (kod per-funkcja) — klucz obcy do ast_nodes
CREATE TABLE concept_centroids_1536 (
    id                    UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    plugin_type           TEXT NOT NULL,
    model_id              UUID NOT NULL REFERENCES embedding_models(id),
    centroid              vector(1536) NOT NULL,
    m2                    DOUBLE PRECISION NOT NULL DEFAULT 0,
    member_count          BIGINT NOT NULL DEFAULT 0,
    label                 TEXT,
    is_active             BOOLEAN NOT NULL DEFAULT TRUE,   -- FALSE = scalony w inny (merge), zachowany do audytu
    merged_into           UUID REFERENCES concept_centroids_1536(id),
    created_at            TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    last_consolidated_at  TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
CREATE INDEX idx_concept_centroids_1536_hnsw
    ON concept_centroids_1536 USING hnsw (centroid vector_cosine_ops) WHERE is_active = TRUE;

CREATE TABLE centroid_members_1536 (
    centroid_id     UUID NOT NULL REFERENCES concept_centroids_1536(id) ON DELETE CASCADE,
    ast_node_id     UUID NOT NULL REFERENCES ast_nodes(id) ON DELETE CASCADE,
    distance        DOUBLE PRECISION NOT NULL,
    consolidated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    PRIMARY KEY (centroid_id, ast_node_id)
);

-- 5b. Ogólny szybki (MiniLM legacy / cokolwiek jest is_default=FALSE po
--     cutoverze) — klucz obcy do file_versions
CREATE TABLE concept_centroids_384 (
    id                    UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    plugin_type           TEXT NOT NULL,
    model_id              UUID NOT NULL REFERENCES embedding_models(id),
    centroid              vector(384) NOT NULL,
    m2                    DOUBLE PRECISION NOT NULL DEFAULT 0,
    member_count          BIGINT NOT NULL DEFAULT 0,
    label                 TEXT,
    is_active             BOOLEAN NOT NULL DEFAULT TRUE,
    merged_into           UUID REFERENCES concept_centroids_384(id),
    created_at            TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    last_consolidated_at  TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
CREATE INDEX idx_concept_centroids_384_hnsw
    ON concept_centroids_384 USING hnsw (centroid vector_cosine_ops) WHERE is_active = TRUE;

CREATE TABLE centroid_members_384 (
    centroid_id     UUID NOT NULL REFERENCES concept_centroids_384(id) ON DELETE CASCADE,
    version_id      UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    distance        DOUBLE PRECISION NOT NULL,
    consolidated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    PRIMARY KEY (centroid_id, version_id)
);

-- 5c. Ogólny dokładny (BGE-M3 legacy) — klucz obcy do file_versions
CREATE TABLE concept_centroids_768 (
    id                    UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    plugin_type           TEXT NOT NULL,
    model_id              UUID NOT NULL REFERENCES embedding_models(id),
    centroid              vector(768) NOT NULL,
    m2                    DOUBLE PRECISION NOT NULL DEFAULT 0,
    member_count          BIGINT NOT NULL DEFAULT 0,
    label                 TEXT,
    is_active             BOOLEAN NOT NULL DEFAULT TRUE,
    merged_into           UUID REFERENCES concept_centroids_768(id),
    created_at            TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    last_consolidated_at  TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
CREATE INDEX idx_concept_centroids_768_hnsw
    ON concept_centroids_768 USING hnsw (centroid vector_cosine_ops) WHERE is_active = TRUE;

CREATE TABLE centroid_members_768 (
    centroid_id     UUID NOT NULL REFERENCES concept_centroids_768(id) ON DELETE CASCADE,
    version_id      UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    distance        DOUBLE PRECISION NOT NULL,
    consolidated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    PRIMARY KEY (centroid_id, version_id)
);

-- 5d. Nowy domyślny (Qwen3-Embedding-0.6B) — klucz obcy do file_versions
CREATE TABLE concept_centroids_1024_qwen (
    id                    UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    plugin_type           TEXT NOT NULL,
    model_id              UUID NOT NULL REFERENCES embedding_models(id),
    centroid              vector(1024) NOT NULL,
    m2                    DOUBLE PRECISION NOT NULL DEFAULT 0,
    member_count          BIGINT NOT NULL DEFAULT 0,
    label                 TEXT,
    is_active             BOOLEAN NOT NULL DEFAULT TRUE,
    merged_into           UUID REFERENCES concept_centroids_1024_qwen(id),
    created_at            TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    last_consolidated_at  TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
CREATE INDEX idx_concept_centroids_1024_qwen_hnsw
    ON concept_centroids_1024_qwen USING hnsw (centroid vector_cosine_ops) WHERE is_active = TRUE;

CREATE TABLE centroid_members_1024_qwen (
    centroid_id     UUID NOT NULL REFERENCES concept_centroids_1024_qwen(id) ON DELETE CASCADE,
    version_id      UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    distance        DOUBLE PRECISION NOT NULL,
    consolidated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    PRIMARY KEY (centroid_id, version_id)
);

-- ── 6. Graf leksykalny (słowo → centroid) ───────────────────────────
-- Niezależny od wymiaru (lemma to lemma niezależnie od modelu), ale
-- word_centroid_links jest per wymiar, bo centroidy są per wymiar.
CREATE TABLE lexical_nodes (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    lemma        TEXT NOT NULL,
    pos          TEXT,
    language     TEXT NOT NULL DEFAULT 'pl',
    external_ref TEXT,
    CONSTRAINT unique_lexical_node UNIQUE (lemma, pos, language)
);

CREATE TABLE lexical_edges (
    from_id  UUID NOT NULL REFERENCES lexical_nodes(id) ON DELETE CASCADE,
    to_id    UUID NOT NULL REFERENCES lexical_nodes(id) ON DELETE CASCADE,
    relation TEXT NOT NULL,
    PRIMARY KEY (from_id, to_id, relation)
);

CREATE TABLE word_centroid_links_1536 (
    lexical_node_id UUID NOT NULL REFERENCES lexical_nodes(id) ON DELETE CASCADE,
    centroid_id     UUID NOT NULL REFERENCES concept_centroids_1536(id) ON DELETE CASCADE,
    weight          DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (lexical_node_id, centroid_id)
);
CREATE TABLE word_centroid_links_384 (
    lexical_node_id UUID NOT NULL REFERENCES lexical_nodes(id) ON DELETE CASCADE,
    centroid_id     UUID NOT NULL REFERENCES concept_centroids_384(id) ON DELETE CASCADE,
    weight          DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (lexical_node_id, centroid_id)
);
CREATE TABLE word_centroid_links_768 (
    lexical_node_id UUID NOT NULL REFERENCES lexical_nodes(id) ON DELETE CASCADE,
    centroid_id     UUID NOT NULL REFERENCES concept_centroids_768(id) ON DELETE CASCADE,
    weight          DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (lexical_node_id, centroid_id)
);
CREATE TABLE word_centroid_links_1024_qwen (
    lexical_node_id UUID NOT NULL REFERENCES lexical_nodes(id) ON DELETE CASCADE,
    centroid_id     UUID NOT NULL REFERENCES concept_centroids_1024_qwen(id) ON DELETE CASCADE,
    weight          DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (lexical_node_id, centroid_id)
);

-- ── Weryfikacja po migracji ──────────────────────────────────────────
-- [ ] embedding_models ma dokładnie jeden wiersz is_default=TRUE (Qwen3-Embedding-0.6B)
-- [ ] embeddings_384/768/1024_qwen i ast_embeddings_1536 mają kolumny plugin_type (NOT NULL) i consolidated
-- [ ] każda z czterech tabel embeddingów ma odpowiadającą jej concept_centroids_* i centroid_members_* z POPRAWNYM kluczem obcym (ast_node_id dla 1536, version_id dla reszty)
-- [ ] concept_centroids_* mają is_active + merged_into (wsparcie dla merge_centroids, docs/03 §4b)
-- [ ] consolidation_thresholds istnieje i NIE ma wiersza z sensownym join_threshold domyślnie — wymaga jawnej kalibracji przed uruchomieniem supervisora dla nowej kombinacji (plugin_type, model_id)
-- [ ] lexical_nodes/lexical_edges są niezależne od wymiaru; word_centroid_links_* są per wymiar
