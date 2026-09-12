# ADR-51 — Osobne tabele centroidów i członków per wymiar wektora

← [Mapa ADR](../01-architecture.md) | Mechanizm: [docs/03-consolidation-design.md](../03-consolidation-design.md) §1, §5 | Migracja: `migrations/005_semantic_consolidation.sql`

**Status:** Przyjęty do implementacji w v6.0

## Kontekst

Rozszerzenie SmartFS v6.0 o pamięć roboczą i konsolidację pojęciową (ADR-50) wymaga składowania wektorów centroidów oraz przypisań elementów do tych centroidów. Architektura bazowa v4.5/v5.0 posiadała już dedykowane tabele embeddingów dla poszczególnych wymiarów: `ast_embeddings_1536` (OpenAI text-embedding-3-large), `embeddings_1024_qwen` (Qwen 2.5/3), `embeddings_768` (BGE-M3) oraz `embeddings_384` (all-MiniLM-L6-v2).

Należało podjąć decyzję architektoniczną dotyczącą struktury tabel centroidów i relacji członkostwa: czy wprowadzić jedną wspólną tabelę `concept_centroids` (np. z kolumną typu `vector` o nieokreślonym wymiarze lub wektorami spłaszczonymi do BLOB/JSON), czy też utrzymać ścisły podział per wymiar.

## Decyzja

Zgodnie z **Root Invariant #5** (Izolowane tabele per wymiar wektora), tworzymy ściśle wyodrębnione tabele dla każdego wspieranego wymiaru:

1. **Tabele centroidów:**
   - `concept_centroids_1536` — dla wektorów 1536d (AST, modele OpenAI).
   - `concept_centroids_1024_qwen` — dla wektorów 1024d (tekst Qwen/multimodalne).
   - `concept_centroids_768` — dla wektorów 768d (BGE-M3).
   - `concept_centroids_384` — dla wektorów 384d (all-MiniLM-L6-v2).

2. **Tabele członkostwa:**
   - `centroid_members_1536` — łączy `centroid_id` z tabelą `ast_nodes` (`ast_node_id UUID NOT NULL`).
   - `centroid_members_1024_qwen`, `centroid_members_768`, `centroid_members_384` — łączą `centroid_id` z tabelą `file_versions` (`version_id UUID NOT NULL`).

3. Każda tabela centroidów posiada dedykowany indeks wektorowy HNSW ze ściśle określonym wymiarem i metryką cosinusową (`vector_cosine_ops`), umożliwiający wydajne zapytania w pamięci skrystalizowanej.

## Odrzucone alternatywy

**Jedna wspólna tabela `concept_centroids` z wektorem bez określonego wymiaru (`vector`).** Odrzucone: rozszerzenie pgvector wymaga ściśle zdefiniowanego wymiaru `vector(N)` przy tworzeniu indeksów HNSW i IVFFlat. Kolumna bez wymiaru uniemożliwia indeksowanie wektorowe w PostgreSQL, wymuszając pełny skan sekwencyjny O(N).

**Polimorficzna tabela członkostwa z kolumnami opcjonalnymi (`ast_node_id` i `version_id` w jednym wierszu).** Odrzucone: narusza normalizację bazy danych, wymaga złożonych więzów CHECK i generuje niespójności referencyjne. `ast_nodes` reprezentuje poziom funkcji/klas, a `file_versions` poziom plików — ich powiązania z centroidami mają odmienne kardynalności i schematy zapytań.

## Konsekwencje

- Ścisła integralność referencyjna i pełne wsparcie dla indeksów wektorowych HNSW w pgvector na wszystkich tabelach centroidów.
- Crate `smartfs-semantic` implementuje generyczną obsługę schematów (`SchemaFamily` / routing tabelowy) w `consolidation.rs` i `search.rs`, kierując zapytania do właściwej pary tabel na podstawie wymiarowości modelu.
- Ewentualne dodanie nowego wymiaru wektora w przyszłości wymaga migracji DDL tworzącej nową parę tabel (zgodnie z Root Invariant #4: zakaz runtime DDL).
