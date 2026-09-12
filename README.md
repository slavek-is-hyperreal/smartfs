# SmartFS — Unified Knowledge Storage Layer

**Wersja:** 6.0 — v5.0 (poprawki poprawności) + domyślny model embeddingowy Qwen + warstwa konsolidacji semantycznej (bufor roboczy → krystalizacja → graf pojęć)

**Status:** Specyfikacja do implementacji. Nie zmienia niczego z v4.5/v5.0 poza tym, co wymienione w §"Delta względem v5.0" niżej — to jest przyrost, nie przepisanie.

---

## Jak czytać ten dokument (poziomy szczegółowości)

Ta dokumentacja jest zaprojektowana do czytania stopniowego — każdy poziom linkuje w dół do bardziej szczegółowego:

```
README.md  (jesteś tu)                                    ─── poziom 0: co to jest
  └─ docs/00-overview.md                                   ─── poziom 1: filozofia i warstwy
       └─ docs/01-architecture.md                          ─── poziom 2: schemat systemu, ADR, 8 Root Invariants
            └─ docs/02-crates.md                           ─── poziom 3: mapa crate'ów
                 └─ docs/crates/<crate>.md                 ─── poziom 4: struktury, funkcje, invarianty per crate
                      └─ symbol://<uuid>  (patrz docs/04)   ─── poziom 5: konkretna funkcja/struct w kodzie
```

Baza, na której v6.0 jest przyrostem (dokumenty źródłowe v4.5/v5.0, kopiowane
dosłownie, bo `docs/crates/*.md` cytuje ich numery sekcji):

- [`docs/base-v4.5-v5.0/SmartFS_Architecture_v4_5.md`](docs/base-v4.5-v5.0/SmartFS_Architecture_v4_5.md) — pełna architektura v4.5 (Root Invariants §3.1, CLAUDE.md per crate §3.2-3.10, schemat SQL §6, AST §11, MCP §13)
- [`docs/base-v4.5-v5.0/SmartFS_v4.5_to_v5.0_fixes.md`](docs/base-v4.5-v5.0/SmartFS_v4.5_to_v5.0_fixes.md) — FIX-01..10 (poprawki poprawności v4.5→v5.0)
- [`docs/base-v4.5-v5.0/SmartFS_Known_Limitations_Roadmap.md`](docs/base-v4.5-v5.0/SmartFS_Known_Limitations_Roadmap.md) — znane ograniczenia i roadmapa post-MVP

Dwa dokumenty projektowe głębokiego nurka, bo są sercem v6.0 i nie mieszczą się w żadnym pojedynczym crate:

- [`docs/03-consolidation-design.md`](docs/03-consolidation-design.md) — pełny mechanizm bufora roboczego i konsolidacji centroidów (pamięć robocza → akt scalenia → pamięć skrystalizowana)
- [`docs/04-uuid-doc-linking.md`](docs/04-uuid-doc-linking.md) — jak każda funkcja/struct/impl dostaje trwały UUID, żeby dokumentacja przetrwała edycje linii kodu

Dwa dokumenty operacyjne, dodane po pierwszym review specyfikacji:

- [`docs/05-code-comments.md`](docs/05-code-comments.md) — metodyka komentowania kodu (pre-LLM, dostosowana do Rusta) — jak pisać komentarze, które przeżyją lata, nie tylko sesję, w której powstały
- [`docs/06-agentic-execution-plan.md`](docs/06-agentic-execution-plan.md) — jak zlecić budowę tej specyfikacji narzędziu agentowemu (fazy budowy wg grafu zależności crate'ów, zasady dla subagentów, gotowy prompt)

## Delta względem v5.0

| Co | Zmiana | Gdzie |
|---|---|---|
| Domyślny model embeddingowy | `all-MiniLM-L6-v2` → `Qwen3-Embedding-0.6B` (1024d) | [ADR-49](docs/adr/ADR-49-qwen-default-model.md) |
| Nowa rodzina embeddingów obrazowych | `png.json` i inne pliki obrazowe dostają embedding (`Qwen3-VL-Embedding-2B`) zamiast `null` **(post-MVP)** | [ADR-49](docs/adr/ADR-49-qwen-default-model.md) |
| Nowa warstwa: konsolidacja semantyczna | Bufor roboczy (świeże embeddingi) + okresowy akt scalenia w graf centroidów + graf leksykalny (słowo → centroid) | [ADR-50](docs/adr/ADR-50-working-memory-consolidation.md), [docs/03](docs/03-consolidation-design.md) |
| Serializacja konsolidacji + merge duplikatów centroidów | Jeden supervisor per `(plugin_type, model_id)`, `pg_try_advisory_lock`, `merge_centroids` jako backstop przeciw dryfującym duplikatom | [ADR-53](docs/adr/ADR-53-consolidation-concurrency.md) |
| Backend pełnotekstowy (BM25) | `pg_search` (Tantivy w Postgresie) zamiast gołego `tsvector` — jedyna opcja z natywnym polskim stemmingiem; nowa kolumna `file_versions.search_text`, nowe narzędzie MCP `search_fulltext` | [ADR-54](docs/adr/ADR-54-fulltext-search-backend.md), [migracja 006](migrations/006_fulltext_search.sql) |
| Akceleracja GPU | Wyłącznie Vulkan, reuse-before-rewrite: `ggml`/`llama.cpp` budowany bez CUDA/HIP — zero CUDA, zero ROCm/HIP, z zasady; `koval.toml`: `gpu_acceleration = "vulkan" \| "cpu"` — CPU pozostaje jedynym wymogiem minimalnym | [ADR-55](docs/adr/ADR-55-gpu-acceleration.md) |
| Nowy crate | `smartfs-semantic` — właściciel centroidów, bufora, workera konsolidacji/merge'a, grafu leksykalnego, `consolidation_thresholds` | [docs/crates/smartfs-semantic.md](docs/crates/smartfs-semantic.md) |
| Nowy crate (dev-tool, nie wchodzi do binarki demona) | `smartfs-docgen` — ekstrakcja UUID symboli przez tree-sitter, rejestr, resolver linków | [docs/crates/smartfs-docgen.md](docs/crates/smartfs-docgen.md) |
| Nowe migracje | `005_semantic_consolidation.sql`, `006_fulltext_search.sql` | [migrations/](migrations/) |
| Nowe narzędzia MCP | `search_by_concept(query, plugin_type, limit)` — graf centroidów; `search_fulltext(query, plugin_type?, limit)` — BM25 | [docs/crates/smartfs-mcp.md](docs/crates/smartfs-mcp.md) |
| Plan wykonania agentowego | Fazy budowy wg grafu zależności crate'ów zamiast podziału na weekendy; zasady dla subagentów; gotowy prompt pod Antigravity 2.0 / Gemini 3.8 Flash | [docs/06-agentic-execution-plan.md](docs/06-agentic-execution-plan.md) |
| Ciągłość agenta (post-MVP, poza Fazami 0-6) | `special_data.agent` (`actor_id`/`task_id`) na `file_versions` + MCP `get_actor_activity` — agent, który stracił kontekst, sam odpytuje "co już zrobiłem", zamiast wymagać relacji od człowieka | [ADR-56](docs/adr/ADR-56-agent-continuity.md) |
| Słownik pluginów jako dane (post-MVP, poza Fazami 0-6) | `describe_plugin_type`/`list_plugin_types` przez MCP — agent odkrywa kształt `special_data` tym samym kanałem co dane, zamiast czytać `plugins/*.json` poza SmartFS (inspiracja Pick/MultiValue) | [ADR-57](docs/adr/ADR-57-pick-style-plugin-dictionary.md) |

Wszystko inne (FUSE, CoW, dedup przez `blobs`, AST, IPFS, uprawnienia) dziedziczone wprost z v4.5+v5.0 bez zmian — patrz [`docs/base-v4.5-v5.0/`](docs/base-v4.5-v5.0/) i [`docs/crates/_unchanged.md`](docs/crates/_unchanged.md).

## Filozofia (bez zmian od v3.0)

> Plik nie jest ścieżką. Plik jest swoją treścią. Hash to tożsamość.

v6.0 rozszerza to o jedno zdanie:

> Znaczenie nie jest pojedynczym wektorem. Znaczenie jest miejscem w grafie pojęć, do którego wektor został przypisany aktem konsolidacji.

## Root Invariants (8 zasad nienaruszalnych)

Pełna lista z uzasadnieniami: [docs/01-architecture.md §Invarianty](docs/01-architecture.md#invarianty--rozszerzenie-root-claudemd)

**Z v4.5 (Root CLAUDE.md §3.1):**
1. `content_hash = SHA-256(oryginalne bajty PRZED kompresją)` — nigdy po
2. Każda zmiana treści tworzy nowy wiersz `file_versions` (CoW) — nigdy nie mutuje bloba
3. Jedyna legalna ścieżka do danych wiedzie przez daemon — ext4 to głupi magazyn blobów
4. Migracje są jawne w `migrations/` — żadnego `CREATE TABLE` w kodzie runtime
5. Nigdy nie mieszaj wymiarów embeddingów między zapytaniami — 384 z 384, 1536 z 1536

**Nowe w v6.0:**
6. `smartfs-semantic` nigdy nie blokuje ani nie spowalnia `cow_commit` ani workera `smartfs-ai`
7. Centroid nie jest nigdy przeliczany globalnie w locie — tylko lokalnie, przy akcie konsolidacji
8. Każdy publiczny symbol Rust w `crates/` ma dokładnie jeden `@id` (UUID v4), nadany raz i nigdy nie zmieniany

## Wizja daleka (nie część v6.0 — nie wymagane czytanie przed budową)

[`docs/vision/north-star-kernel-native.md`](docs/vision/north-star-kernel-native.md) — notatka robocza, nie ADR: SmartFS jako natywny system plików jądra (nie FUSE), docelowo główny root własnej dystrybucji. Zawiera stan faktyczny Rust-w-jądrze (wrzesień 2026), rozwiązanie problemu bootstrapu roota (`switch_root`/early userspace) i sekwencjonowanie bootu jako grafu zależności (systemd). Świadomie odłożone do czasu, aż Fazy 0-6 będą działać — nic stąd nie wchodzi do promptu dla Antigravity.

Dalej: [docs/00-overview.md](docs/00-overview.md)
