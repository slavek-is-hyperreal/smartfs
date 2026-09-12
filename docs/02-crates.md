# Poziom 3 — Mapa crate'ów

← [Architektura systemu](01-architecture.md)

```
smartfs/
├── crates/
│   ├── smartfs-schema/     ← bez zmian w v6.0
│   ├── smartfs-store/      ← bez zmian w v6.0
│   ├── smartfs-db/         ← delta: kolumna consolidated, patrz niżej
│   ├── smartfs-compress/   ← bez zmian w v6.0
│   ├── smartfs-fuse/       ← bez zmian w v6.0
│   ├── smartfs-ai/         ← delta: domyślny model Qwen, patrz ADR-49
│   ├── smartfs-semantic/   ← NOWY — serce v6.0
│   ├── smartfs-mcp/        ← delta: nowe narzędzie search_by_concept
│   ├── smartfs-ipfs/       ← bez zmian w v6.0
│   ├── smartfs-cli/        ← delta: smartfs-cli concepts (przegląd grafu centroidów)
│   └── smartfs-docgen/     ← NOWY — dev-tool, nie wchodzi do binarki demona
└── migrations/
    └── 005_semantic_consolidation.sql   ← NOWA
```

## Dokumentacja per crate

Crate'y bez zmian merytorycznych w v6.0 mają dokumentację niezmienioną względem v4.5/v5.0 CLAUDE.md — nie duplikujemy jej tutaj. Poniżej tylko te, które się zmieniają lub są nowe:

- [`smartfs-schema`](crates/smartfs-schema.md) — delta: dwa nowe warianty `SmartFsError` (`MissingCalibration`, `AdvisoryLockUnavailable`), potrzebne przez `smartfs-semantic`.
- [`smartfs-semantic`](crates/smartfs-semantic.md) — **nowy.** Właściciel `concept_centroids`, `centroid_members`, grafu leksykalnego, workera konsolidacji i merge'a, `consolidation_thresholds`. Najbardziej szczegółowo rozpisany crate w tym dokumencie — patrz tam funkcja po funkcji.
- [`smartfs-docgen`](crates/smartfs-docgen.md) — **nowy.** Dev-tool: skanuje `crates/*/src` przez tree-sitter-rust, zarządza `@id` UUID-ami, generuje `docs/symbol_registry.json`, rozwiązuje linki `symbol://<uuid>`.
- [`smartfs-db`](crates/smartfs-db.md) — delta: `ALTER TABLE ... ADD COLUMN consolidated BOOLEAN` na tabelach embeddingów, nowa funkcja `count_all_unconsolidated`; plus (ADR-54) `set_search_text` i `search_fulltext_bm25`.
- [`smartfs-ai`](crates/smartfs-ai.md) — delta: `default_model` w konfiguracji wskazuje teraz na `Qwen3-Embedding-0.6B`; nowy opcjonalny model obrazowy `Qwen3-VL-Embedding-2B` dla pluginów bez `ast=true` i z `match_extensions` obrazowym; (ADR-54) wypełnia `file_versions.search_text` przy generic-embedding pass; (ADR-55) zyskuje drugi silnik inferencji — bindingi FFI do `ggml`/`llama.cpp` zbudowanego wyłącznie z backendem Vulkan (bez CUDA/HIP) — jako opcjonalna ścieżka GPU obok ONNX Runtime na CPU; reuse-before-rewrite zamiast pisania własnych kerneli od zera; zero CUDA, zero ROCm/HIP w kodzie projektu, z zasady, nie tylko z braku wsparcia.
- [`smartfs-mcp`](crates/smartfs-mcp.md) — delta: nowe narzędzia `search_by_concept(query, plugin_type?, limit)` i `search_fulltext(query, plugin_type?, limit)` (ADR-54).

## Zależność między crate'ami (kto może importować kogo)

```
smartfs-schema   ← importowany przez wszystkich, nie importuje nikogo
smartfs-store    ← importuje: smartfs-schema
smartfs-compress ← importuje: smartfs-schema, smartfs-store
smartfs-db       ← importuje: smartfs-schema
smartfs-ai       ← importuje: smartfs-schema, smartfs-db, smartfs-store
smartfs-semantic ← importuje: smartfs-schema, smartfs-db          (NIGDY smartfs-ai — patrz niżej)
smartfs-fuse     ← importuje: smartfs-schema, smartfs-db, smartfs-store, smartfs-compress
smartfs-mcp      ← importuje: smartfs-schema, smartfs-db, smartfs-semantic
smartfs-ipfs     ← importuje: smartfs-schema, smartfs-store
smartfs-cli      ← importuje: smartfs-schema, smartfs-db, smartfs-semantic
smartfs-docgen   ← samodzielny dev-tool, importuje tylko tree-sitter-rust; NIE importuje żadnego innego crate'a smartfs, bo skanuje ich pliki źródłowe jako tekst, nie linkuje się z nimi
```

**Dlaczego `smartfs-semantic` nie zależy od `smartfs-ai`:** `smartfs-ai` generuje embeddingi (wymaga ONNX, spawn_blocking, modeli w pamięci). `smartfs-semantic` tylko *czyta* już zapisane wektory z tabel, którymi zarządza `smartfs-db`. Rozdzielenie to nie jest kosmetyczne — gwarantuje, że proces konsolidacji może działać (i być testowany) bez ładowania jakiegokolwiek modelu ONNX, i że crash/spowolnienie w `smartfs-ai` nigdy nie propaguje się do `smartfs-semantic` przez zależność kompilacji, tylko co najwyżej przez brakujące dane w tabeli (co konsolidacja już musi obsługiwać — pusty batch to normalny, nie wyjątkowy stan).

Dalej, w kolejności rosnącej szczegółowości: [docs/crates/smartfs-semantic.md](crates/smartfs-semantic.md)
