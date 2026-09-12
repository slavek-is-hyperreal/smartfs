# SmartFS v6.0 — Gemini Reconciliation Report (wrzesień 2026)

← [Opus Independent Review](opus-independent-review-2026-09.md) | [Implementation Plan](../../home/slavekm/.gemini/antigravity/brain/986652b5-ab96-43e3-bd3e-af46f9607aaa/implementation_plan.md)

**Data weryfikacji:** 2026-09-12  
**Weryfikujący:** Gemini (Antigravity 2.0) po przeczytaniu kodu, nie tylko dokumentacji  
**Commit bazowy przed naprawami:** `7e91626`

---

Poniżej wszystkie 35 znalezisk z audytu Opus z jednowierszowym werdyktem i krótkim uzasadnieniem opartym na faktycznym stanie kodu (nie tylko dokumentacji).

## Klasyfikacje

- **(a) JUŻ ROZWIĄZANE** — kod był poprawny, dokumentacja nie odzwierciedlała rzeczywistości → zaktualizowano tylko docs  
- **(b) REALNY BUG / LUKA W KODZIE** → naprawiono kod + docs  
- **(c) TYLKO DOKUMENTACYJNE** → zaktualizowano docs  

---

## 14 BLOCKING Findings

| # | Tytuł (Opus) | Werdykt | Akcja |
|---|---|---|---|
| **B-01** | Subagent isolation & pre-FIX v4.5 base doc | **(a) JUŻ ROZWIĄZANE w kodzie** | Dodano baner ostrzegawczy `⚠️ DOKUMENT ARCHIWALNY` na górę `SmartFS_Architecture_v4_5.md`; dodano `SmartFS_v4.5_to_v5.0_fixes.md` do kolejności czytania w `docs/06`. |
| **B-02** | Session-level `pg_try_advisory_lock` over `PgPool` | **(b) REALNY BUG — NAPRAWIONY** | Zamieniono na `pg_try_advisory_xact_lock` jako pierwsze polecenie wewnątrz transakcji `consolidate_batch` i `merge_centroids`. Rywalizacja zwraca `Ok(0)` + `debug!`. Usunięto redundantne `try_advisory_lock`/`release_advisory_lock` z `supervisor.rs`. |
| **B-03** | Missing ADR-51 and ADR-52 files | **(c) TYLKO DOKUMENTACYJNE** | Stworzono `docs/adr/ADR-51-centroid-tables-per-dimension.md` i `docs/adr/ADR-52-persistent-symbol-uuids.md`. |
| **B-04** | Crate graph vs embedding query strings in MCP | **(b) REALNY BUG — NAPRAWIONY** | Dodano `smartfs-ai` do `smartfs-mcp/Cargo.toml`; `search_semantic`, `search_functions`, `search_by_concept` akceptują teraz `query: Option<String>` i embeddują go server-side przez `CpuEmbeddingEngine`; brak obu parametrów zwraca `SmartFsError::SyntaxError`. Testy 7/7 ✅. |
| **B-05** | Phase 1 safe parallelism vs dependency graph | **(c) TYLKO DOKUMENTACYJNE** | Poprawiono opis Fazy 1 w `docs/06`: `smartfs-store` → `smartfs-compress` musi być sekwencyjne. |
| **B-06** | `embeddings_1024_qwen_vl` required but no migration | **(c) SCOPE DECISION — ZAIMPLEMENTOWANE** | Qwen3-VL-Embedding-2B jawnie zdegradowane do post-MVP. Wykreślone z tabel MVP w `README.md`, `smartfs-ai.md`; ADR-49 opatrzone addendum. |
| **B-07** | `consolidation_thresholds` never populated | **(a) JUŻ ROZWIĄZANE w kodzie** | `smartfs-cli calibrate --plugin-type X --model Y` jest zaimplementowane i przetestowane. Udokumentowane w `docs/crates/_unchanged.md` i `docs/03`. |
| **B-08** | Incompatible `search_by_concept` signatures & no-query mode | **(a+b) JUŻ ROZWIĄZANE + ROZSZERZONE** | Sygnatura spójna. Dodano `list_active_centroids` w `smartfs-semantic` (nowy `CentroidSummary`); `search_by_concept` bez wektora/query przeglądania aktywne centroidy. |
| **B-09** | `count_all_unconsolidated` vs `count_unconsolidated` | **(a) JUŻ ROZWIĄZANE w kodzie** | Kod ma sparametryzowane `count_unconsolidated(plugin_type, model_id)` w `smartfs-semantic` i `count_all_unconsolidated()` w `smartfs-db`. Udokumentowano różnicę. |
| **B-10** | `docs/adr/` absent from reading order | **(c) TYLKO DOKUMENTACYJNE** | Dodano `docs/adr/*.md` (w kolejności numerycznej) i `SmartFS_v4.5_to_v5.0_fixes.md` do kolejności czytania w `docs/06`. |
| **B-11** | Duplicate `@id` on contradictory supervisor signatures | **(a) JUŻ ROZWIĄZANE w kodzie** | Prawdziwy kod ma unikalne UUID. Poprawiono duplikat UUID w przykładzie w `docs/04`. |
| **B-12** | `claim_unconsolidated_batch` invalid SQL clause order | **(b) REALNY BUG — NAPRAWIONY** | Zmieniono kolejność na `ORDER BY created_at ASC LIMIT $3 FOR UPDATE SKIP LOCKED` w `consolidation.rs`. Dodano end-to-end test żywej bazy `test_consolidation_supervisor_full_cycle_live_db`. |
| **B-13** | Undefined `activity_monitor` free variable in supervisor | **(a) JUŻ ROZWIĄZANE w kodzie** | Prawdziwy kod używa `last_activity: Instant` z `elapsed()`. Poprawiono pseudokod w `docs/03`. |
| **B-14** | `create_centroid_from_cluster` NOT NULL columns & missing symbols | **(a) JUŻ ROZWIĄZANE w kodzie** | Kod binduje `plugin_type` i `model_id`. Udokumentowano 10 brakujących symboli w `docs/crates/smartfs-semantic.md`. |

---

## 21 SIGNIFICANT Findings

| # | Tytuł (Opus) | Werdykt | Akcja |
|---|---|---|---|
| **S-01** | Default code-embedding model points at phantom `ast_embeddings_384` | **(c) TYLKO DOKUMENTACYJNE** | Dopisano notatkę przy `ast_embeddings_384` w `SmartFS_Architecture_v4_5.md` §10.3 (nie istnieje — AST wyłącznie w `ast_embeddings_1536`). |
| **S-02** | `smartfs-mcp.md` declares `search_semantic` unchanged | **(a) JUŻ ROZWIĄZANE** | Udokumentowano obsługę `query` vs `query_vector` i `is_default=TRUE` fallback w `docs/crates/smartfs-mcp.md`. |
| **S-03** | ADR-39 offline fallback broken by ADR-49 cutover | **(c) TYLKO DOKUMENTACYJNE** | Dodano addendum do ADR-49: fallback target = aktywny `is_default=TRUE` model, nie hardcoded MiniLM. |
| **S-04** | ADR-55 attributes ONNX CPU baseline to ADR-49 | **(c) TYLKO DOKUMENTACYJNE** | Poprawiono atrybucję w ADR-55: baseline CPU z v4.5 §3.7/§18, ADR-49 zmieniło tylko model. |
| **S-05** | `Qwen3-Embedding-4B` configured as `accurate_model` (2560d) without table | **(c) TYLKO DOKUMENTACYJNE** | Udokumentowano w ADR-49 addendum i `smartfs-ai.md` że 4B (2560d) to tier post-MVP. |
| **S-06** | `merge_centroids` never invoked and parameters missing | **(a) JUŻ ROZWIĄZANE** | Kod uruchamia merge co 20 cykli z `join_threshold / 2.0`. Udokumentowano w `docs/03`. |
| **S-07** | 10 symbols on `smartfs-semantic` critical path missing from contract | **(c) TYLKO DOKUMENTACYJNE** | Dodano pełny katalog funkcji i typów do `docs/crates/smartfs-semantic.md`. |
| **S-08** | Prompt says Root Invariants in README; they are in `docs/01` | **(c) TYLKO DOKUMENTACYJNE** | Dodano sekcję `## Root Invariants` do `README.md` z odesłaniem do `docs/01`. |
| **S-09** | Base ROOT CLAUDE.md crate map missing semantic and docgen | **(a) JUŻ ROZWIĄZANE** | `docs/01-architecture.md` już ma mapę 11 crate'ów. |
| **S-10** | Hard Rule 5 requires docgen at end of each phase | **(c) TYLKO DOKUMENTACYJNE** | Wyjaśniono w `docs/06`: docgen zbudowany w Fazie 5, weryfikowany (`check`) w Fazie 6. |
| **S-11** | Fixes checklist mandates LIST partitioning omitted in migrations | **(c) TYLKO DOKUMENTACYJNE** | Dopisano adnotację `(roadmap §3b — post-MVP)` przy pozycji LIST partitioning w `SmartFS_v4.5_to_v5.0_fixes.md`. |
| **S-12** | `koval.toml` vs `smartfs.toml` division of responsibility | **(c) TYLKO DOKUMENTACYJNE** | Dodano zasadę podziału konfiguracji do `docs/01-architecture.md`. |
| **S-13** | Nothing enforces exactly one `is_default=TRUE` on models | **(b) REALNY BUG — NAPRAWIONY** | Dodano `CREATE UNIQUE INDEX uq_single_default_embedding_model ON embedding_models (is_default) WHERE is_default = TRUE` do migracji 002 i zaaplikowano na żywej bazie. Constraint zweryfikowany (próba INSERT drugiego default zwraca błąd). |
| **S-14** | `smartfs-docgen` error type undefined and cannot be `SmartFsError` | **(a) JUŻ ROZWIĄZANE** | Kod definiuje własny `Error`/`Result` w `smartfs-docgen`. Udokumentowane. |
| **S-15** | Phase 3 stated rationale factually wrong about `smartfs-fuse` | **(c) TYLKO DOKUMENTACYJNE** | Poprawiono uzasadnienie Fazy 3: `smartfs-fuse` zależy od `smartfs-db`, nie od `smartfs-ai`, więc MOŻE biec równolegle z `smartfs-ai`. |
| **S-16** | Mapping from `(plugin_type, model_id)` to table pair unstated | **(a) JUŻ ROZWIĄZANE** | Kod używa `SchemaFamily`. Dodano tabelę routingu do `docs/crates/smartfs-semantic.md`. |
| **S-17** | `SymbolRecord` and JSON schema disagree on tombstone fields | **(a) JUŻ ROZWIĄZANE** | Kod ma `tombstoned: bool` i `tombstoned_reason: Option<String>`. Zaktualizowano struct w `docs/04`. |
| **S-18** | Roadmap §7c prescribes `CUDAExecutionProvider` | **(c) TYLKO DOKUMENTACYJNE** | Dodano adnotację `(superseded by ADR-55 — GPU wyłącznie przez Vulkan)` w `SmartFS_Known_Limitations_Roadmap.md`. |
| **S-19** | Lexical graph link population unspecified | **(a) JUŻ ROZWIĄZANE** | Kod implementuje TF-IDF `label_centroid_from_members` i `import_wordnet`. Udokumentowano w `docs/crates/smartfs-semantic.md`. |
| **S-20** | ADR-49 strands pre-computed Wikipedia corpus (ADR-06) | **(c) TYLKO DOKUMENTACYJNE** | Dodano do ADR-49 addendum: `embeddings_768` i `concept_centroids_768` zachowane dla zewnętrznych korpusów. |
| **S-21** | `attach_to_centroid` never persists label / 20% rule uncomputable | **(c) TYLKO DOKUMENTACYJNE** | Udokumentowano w `docs/03`: etykieta liczona asynchronicznie przez `label_centroid_from_members`, przeliczana przy >20% zmianie `member_count`. |

---

## Podsumowanie statystyk

| Kategoria | Liczba | Naprawiono kod | Tylko docs |
|---|---|---|---|
| **BLOCKING** | 14 | 4 (B-02, B-04, B-08\*, B-12) | 10 |
| **SIGNIFICANT** | 21 | 2 (S-13, S-08†) | 19 |
| **RAZEM** | 35 | 6 | 29 |

\* B-08: kod częściowo istniał, rozszerzono o `list_active_centroids`  
† S-08: zmiana README (dokumentacja projektu, nie kod Rust)

---

## Wykaz zmienionych plików (kod)

- `crates/smartfs-semantic/src/consolidation.rs` — B-02 (advisory xact lock), B-12 (SQL order)
- `crates/smartfs-semantic/src/supervisor.rs` — B-02 (removed redundant session lock)
- `crates/smartfs-semantic/src/search.rs` — B-08 (`list_active_centroids`)
- `crates/smartfs-semantic/src/types.rs` — B-08 (`CentroidSummary`)
- `crates/smartfs-semantic/src/lib.rs` — B-08 (pub export)
- `crates/smartfs-semantic/tests/integration_tests.rs` — B-12 (end-to-end live DB test)
- `crates/smartfs-mcp/Cargo.toml` — B-04 (`smartfs-ai` dependency)
- `crates/smartfs-mcp/src/handler.rs` — B-04 (`query` parameter, server-side embedding)
- `crates/smartfs-mcp/src/tools.rs` — B-04 (tool schema `query` field)
- `crates/smartfs-mcp/tests/dispatch_tests.rs` — B-04 (new test)
- `crates/smartfs-db/src/embeddings.rs` — refactor (moved sqlx query to `get_model_dimensions`)
- `crates/smartfs-db/src/lib.rs` — re-export `get_model_dimensions`
- `migrations/002_embedding_models.sql` — S-13 (partial unique index)

