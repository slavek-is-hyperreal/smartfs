# smartfs-db — delta w v6.0

← [Mapa crate'ów](../02-crates.md) | Baza: CLAUDE.md z v4.5 §3.5 + poprawki FIX-01..04 z v5.0 (bez zmian, patrz tam)

Jedyna zmiana w v6.0: nowa kolumna `consolidated` na tabelach embeddingów (patrz [migracja 005](../../migrations/005_semantic_consolidation.sql)) i jedna nowa funkcja pomocnicza w publicznym API tego crate'a, wołana przez `smartfs-semantic`:

```rust
/// @id: 3d8e1f6a-7c42-4b90-a5e8-6c1f9d3b7a02
/// Zwraca sumę wierszy `consolidated = FALSE` po WSZYSTKICH tabelach
/// embeddingów naraz (embeddings_384, embeddings_768, embeddings_1024_qwen,
/// ast_embeddings_1536 WHERE is_current). Używane przez
/// smartfs-semantic::count_unconsolidated jako pojedyncze wywołanie zamiast
/// czterech osobnych zapytań z każdego miejsca, które tego potrzebuje.
pub async fn count_all_unconsolidated(db: &Pool) -> Result<i64> { ... }
```

`smartfs-db` **nie** zyskuje żadnej wiedzy o centroidach czy grafie leksykalnym — te tabele i logika należą wyłącznie do `smartfs-semantic` (patrz [docs/02-crates.md](../02-crates.md), sekcja o zależnościach między crate'ami). `count_all_unconsolidated` istnieje w `smartfs-db`, bo dotyczy tabel, którymi `smartfs-db` już zarządza (`embeddings_*`), nie tabel nowych.

## Nowe funkcje (ADR-54 — pg_search)

```rust
/// Ustawia (lub czyści, przy None) file_versions.search_text — jedyny
/// legalny sposób zapisu tej kolumny, wołany przez smartfs-ai przy
/// generic-embedding pass. Patrz docs/crates/smartfs-ai.md.
pub async fn set_search_text(db: &Pool, version_id: Uuid, text: Option<String>) -> Result<()> { ... }

/// Wyszukiwanie BM25 przez pg_search. Przeszukuje jednocześnie warstwę
/// plikową (file_versions.search_text) i kod per-funkcja (ast_nodes.source),
/// zwracając wyniki oznaczone FulltextHitKind. Filtr plugin_type: dla
/// trafień plikowych wprost z file_versions.special_type, dla trafień
/// AST przez JOIN ast_nodes -> file_versions.
pub async fn search_fulltext_bm25(
    db: &Pool, query: &str, plugin_type: Option<&str>, limit: i64,
) -> Result<Vec<FulltextHit>> { ... }
```

`smartfs-db` nadal nie zyskuje wiedzy o pg_search jako takim poza tymi dwiema funkcjami — logika rankingu/scoringu żyje w zapytaniu SQL wołanym przez tę funkcję, nie rozlewa się po reszcie crate'a.

Reszta CLAUDE.md tego crate'a — dedup przez `blobs`, `cow_commit`, reaper, `ast_embeddings_1536.is_current` — bez zmian względem v5.0.
