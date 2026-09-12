# Poziom 2 — Architektura systemu

← [Filozofia i warstwy](00-overview.md) | → [Mapa crate'ów](02-crates.md)

## Schemat (v4.5 + dopisana warstwa semantyczna)

```
         [ Użytkownik / Narzędzia CLI / System operacyjny / Aplikacje ]
                                    │
                                    ▼ (POSIX API: ls, cat, cp, mkdir)
               ┌──────────────────────────────────────────────────┐
               │              SmartFS Daemon (Rust)               │
               └───────────┬──────────────────────┬──────────────┘
                           │                      │
            (Metadane SQL) │                      │ (Odczyt/Zapis Blobów)
                           ▼                      ▼
              ┌─────────────────────┐   ┌────────────────────────────┐
              │  PostgreSQL (v16+)  │   │   Storage Backend (Trait)  │
              │  - pgvector         │   │  Local │ IPFS │ S3 │ SFTP  │
              │  - JSONB Indexes    │   │  Wiki  │ NASA │ Notion │…  │
              │  - CID / Hashes     │   └────────────────────────────┘
              └──────────┬──────────┘
                         │
                         │  (nowe w v6.0 — działa asynchronicznie,
                         │   nie na ścieżce zapisu/odczytu)
                         ▼
              ┌─────────────────────────────┐
              │  smartfs-semantic            │
              │  - concept_centroids         │
              │  - lexical graph             │
              │  - consolidation worker      │
              └──────────────┬──────────────┘
                             │
                             ▼
              ┌─────────────────────┐
              │   MCP Server        │
              │  (JSON-RPC / stdio) │
              │  + search_by_concept│
              └─────────────────────┘
```

Kluczowa własność architektoniczna: **`smartfs-semantic` nigdy nie jest na krytycznej ścieżce `cow_commit`.** Zapis pliku, dedup, AST, nawet generowanie surowego embeddingu (`smartfs-ai`) działają dokładnie tak jak w v5.0, bez wiedzy o istnieniu warstwy konsolidacji. `smartfs-semantic` czyta to, co `smartfs-ai` już zapisało (embeddingi z `consolidated=FALSE`), pracuje w tle, i tylko `smartfs-mcp` wie, że taka warstwa istnieje. To jest świadome powtórzenie tej samej zasady, która już raz ochroniła projekt: `is_current` też nie jest ustawiane w `cow_commit`, tylko przez workera, po fakcie (FIX-02).

## ADR nowe w v6.0

| # | Decyzja | Powód |
|---|---------|-------|
| ADR-49 | `Qwen3-Embedding-0.6B` (1024d) jako nowy `is_default=TRUE`; `Qwen3-VL-Embedding-2B` jako embedding dla plików obrazowych | Lepsza jakość/wielojęzyczność przy tym samym profilu kosztowym co MiniLM; obrazy przestają być semantycznie nieme |
| ADR-50 | Konsolidacja semantyczna jako proces wsadowy z dwoma niezależnymi zegarami (limit bufora, cisza zapisu), nie jako streaming online-clustering | Streaming clustering powtarzałby klasę błędu z `is_current` (FIX-02) na trudniejszym obiekcie (centroid zamiast flagi boolowskiej) |
| ADR-51 | Centroidy trzymane per tabela embeddingów (tak jak same embeddingi — `concept_centroids_1536`, `concept_centroids_1024_qwen`, …), nie w jednej uniwersalnej tabeli | Zgodność z Invariant #5 (nigdy nie mieszaj wymiarów) — `vector(N)` w Postgresie wymaga stałego N per kolumna |
| ADR-52 | Każdy `pub fn`/`struct`/`impl`/`enum` w kodzie SmartFS dostaje trwały UUID w komentarzu dokumentującym (`@id:`), niezależny od numeru linii | Dokumentacja linkuje do symboli, nie do linii — edycja kodu nie może cichcem zrywać linków w dokumentacji |
| ADR-53 | Jeden supervisor konsolidacji per `(plugin_type, model_id)`, serializowany przez `pg_try_advisory_lock`; `merge_centroids` jako rzadki backstop, nigdy `DELETE` na centroidach | Zamyka lukę: bez tego dwie współbieżne konsolidacje tej samej kombinacji mogłyby tworzyć nierozłączalne duplikaty centroidów |
| ADR-54 | `pg_search` (Tantivy-w-Postgresie, `pgrx`) jako backend BM25, zamiast gołego `tsvector` albo osobnego Tantivy | Jedyna z trzech opcji z natywnym polskim stemmingiem (Snowball, od 12.2025); koegzystuje z pgvector zamiast konkurować |
| ADR-55 | Akceleracja GPU wyłącznie przez Vulkan — reuse-before-rewrite: `ggml`/`llama.cpp` budowany bez CUDA/HIP jako fundament (nie własne kernele od zera); CUDA i ROCm/HIP całkowicie wykluczone jako zasada; `koval.toml`: `gpu_acceleration = "vulkan" \| "cpu"` | Żaden zamknięty, jednowendorowy stack obliczeniowy GPU; Vulkan jako jedyny otwarty standard Khronos identyczny na każdym wendorze (i sięgający poza desktop — RPi, smartfony). GPU to bonus szybkości, nigdy warunek startu |

Pełne ADR: [docs/adr/ADR-49-qwen-default-model.md](adr/ADR-49-qwen-default-model.md), [docs/adr/ADR-50-working-memory-consolidation.md](adr/ADR-50-working-memory-consolidation.md), [docs/adr/ADR-53-consolidation-concurrency.md](adr/ADR-53-consolidation-concurrency.md), [docs/adr/ADR-54-fulltext-search-backend.md](adr/ADR-54-fulltext-search-backend.md), [docs/adr/ADR-55-gpu-acceleration.md](adr/ADR-55-gpu-acceleration.md).

## Invarianty — rozszerzenie ROOT CLAUDE.md

Do pięciu invariantów z v4.5 dochodzi:

```
6. smartfs-semantic nigdy nie blokuje ani nie spowalnia cow_commit ani workera
   smartfs-ai — czyta tylko już zapisane, is_current/consolidated=FALSE dane
7. Centroid nie jest nigdy przeliczany globalnie w locie — tylko lokalnie,
   przy akcie konsolidacji, nad ograniczonym batchem
8. Każdy pub-liczny symbol Rust w crates/ ma dokładnie jeden @id (UUID v4),
   nadany raz i nigdy nie zmieniany, nawet przy przenoszeniu pliku
```

## Wyszukiwanie: trzy tryby, świadomie nieujednolicone

Od v6.0 SmartFS udostępnia trzy tryby wyszukiwania, wybierane przez wołającego, nie przez system automatycznie: `search_semantic` (płaski cosine, v4.5), `search_by_concept` (graf centroidów, ADR-50/53), `search_fulltext` (BM25 przez pg_search, [ADR-54](adr/ADR-54-fulltext-search-backend.md)). Diagram w tym dokumencie pokazuje `smartfs-semantic` jako warstwę asynchroniczną nad danymi zapisanymi przez `smartfs-ai` — pg_search działa inaczej: to indeks Postgresa, aktualizowany synchronicznie przy INSERT/UPDATE jak każdy inny indeks, bez osobnego workera. Scalenie tych trzech trybów w jeden ranking hybrydowy jest jawnie odłożone — patrz "Otwarte pytanie" w ADR-54.

Dalej: [docs/02-crates.md](02-crates.md)
