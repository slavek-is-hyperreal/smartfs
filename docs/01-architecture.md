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
| [ADR-51](adr/ADR-51-centroid-tables-per-dimension.md) | Centroidy trzymane per tabela embeddingów (tak jak same embeddingi — `concept_centroids_1536`, `concept_centroids_1024_qwen`, …), nie w jednej uniwersalnej tabeli | Zgodność z Invariant #5 (nigdy nie mieszaj wymiarów) — `vector(N)` w Postgresie wymaga stałego N per kolumna |
| [ADR-52](adr/ADR-52-persistent-symbol-uuids.md) | Każdy `pub fn`/`struct`/`impl`/`enum` w kodzie SmartFS dostaje trwały UUID w komentarzu dokumentującym (`@id:`), niezależny od numeru linii | Dokumentacja linkuje do symboli, nie do linii — edycja kodu nie może cichcem zrywać linków w dokumentacji |
| ADR-53 | Jeden supervisor konsolidacji per `(plugin_type, model_id)`, serializowany przez `pg_try_advisory_lock`; `merge_centroids` jako rzadki backstop, nigdy `DELETE` na centroidach | Zamyka lukę: bez tego dwie współbieżne konsolidacje tej samej kombinacji mogłyby tworzyć nierozłączalne duplikaty centroidów |
| ADR-54 | `pg_search` (Tantivy-w-Postgresie, `pgrx`) jako backend BM25, zamiast gołego `tsvector` albo osobnego Tantivy | Jedyna z trzech opcji z natywnym polskim stemmingiem (Snowball, od 12.2025); koegzystuje z pgvector zamiast konkurować |
| ADR-55 | Akceleracja GPU wyłącznie przez Vulkan — reuse-before-rewrite: `ggml`/`llama.cpp` budowany bez CUDA/HIP jako fundament (nie własne kernele od zera); CUDA i ROCm/HIP całkowicie wykluczone jako zasada; `koval.toml`: `gpu_acceleration = "vulkan" \| "cpu"` | Żaden zamknięty, jednowendorowy stack obliczeniowy GPU; Vulkan jako jedyny otwarty standard Khronos identyczny na każdym wendorze (i sięgający poza desktop — RPi, smartfony). GPU to bonus szybkości, nigdy warunek startu |

Pełne ADR: [docs/adr/ADR-49-qwen-default-model.md](adr/ADR-49-qwen-default-model.md), [docs/adr/ADR-50-working-memory-consolidation.md](adr/ADR-50-working-memory-consolidation.md), [ADR-51](adr/ADR-51-centroid-tables-per-dimension.md), [ADR-52](adr/ADR-52-persistent-symbol-uuids.md), [docs/adr/ADR-53-consolidation-concurrency.md](adr/ADR-53-consolidation-concurrency.md), [docs/adr/ADR-54-fulltext-search-backend.md](adr/ADR-54-fulltext-search-backend.md), [docs/adr/ADR-55-gpu-acceleration.md](adr/ADR-55-gpu-acceleration.md).

**Konwencja plików konfiguracyjnych:** `koval.toml` zawiera możliwości sprzętowe/budowlane maszyny (`gpu_acceleration = "vulkan" | "cpu"`, ścieżki do modeli, limity pamięci GPU) — zmienia się per maszyna, nie per uruchomienie demona. `smartfs.toml` to konfiguracja runtime demona (pool-size, katalogi, progi). Zasada: jeśli opcja wpływa na to, jak binarka jest zbudowana lub jaki hardware jest dostępny — idzie do `koval.toml`; jeśli wpływa tylko na zachowanie demona w trakcie działania — idzie do `smartfs.toml`.

**Post-MVP, poza zakresem Faz 0-6 (nie wymaga nowej migracji, nie zmienia promptu dla Antigravity):**

- [ADR-56](adr/ADR-56-agent-continuity.md) — prowieniencja `actor`/`session`/`task_id` w `file_versions.special_data` i narzędzie MCP `get_actor_activity`, żeby agent, który stracił kontekst (wygasła sesja, ucięta rozmowa), mógł sam odpytać "co ja już zrobiłem", zamiast wymagać ręcznej relacji od człowieka.
- [ADR-57](adr/ADR-57-pick-style-plugin-dictionary.md) — `describe_plugin_type`/`list_plugin_types` przez MCP, żeby agent odkrywał kształt `special_data` danego typu pliku (`plugins/*.json`) tym samym kanałem co same dane, zamiast czytać config poza SmartFS (inspiracja słownikami Picka/MultiValue — SmartFS już nieświadomie ma tę strukturę, brakuje tylko żywego dostępu).

Obie traktowane analogicznie do GPU (ADR-55/Faza 7) — bonus ciągłości/ergonomii dla agentów, nigdy warunek pierwszego działającego demona.

**Przyjęty po Fazach 0-6, zmienia gorącą ścieżkę zapisu:**

- [ADR-58](adr/ADR-58-two-stage-cow-commit.md) — `cow_commit` rozdzielony na dwa etapy: `release()` zapisuje blob plus atomowy znacznik `pending` na ext4 (wzorzec Maildira: zapis do `pending/tmp/`, `fdatasync`, `rename()` do `pending/queue/`) i wraca do wołającego, a pojedynczy konsument drenuje kolejkę do Postgresa w tle. Trwałość daje `rename()`, nie kolejka w RAM — ta jest wyłącznie optymalizacją kolejności i jest w całości odtwarzalna ze skanu `pending/queue/` przy starcie demona. Kolejka ma twardy limit liczony od całkowitego RAM-u maszyny; po jego trafieniu `write()` blokuje do 30 s, a potem zwraca `EAGAIN` — nigdy nie gubi potwierdzonego zapisu. Klucz idempotencji replayu to `version_id` (UUID nadany przy znaczniku), egzekwowany przez `ON CONFLICT (id) DO NOTHING` — bez nowej migracji. `version_number` nadal liczy się jako `MAX+1` w transakcji drenażu, a że drenaż jest jednym konsumentem FIFO, kolejność numerów pozostaje kolejnością zapisów. Wypisuje `smartfs-fuse` i `smartfs-store` z [_unchanged.md](crates/_unchanged.md).
- [ADR-59](adr/ADR-59-posix-special-file-types.md) — pełny zestaw typów POSIX: FIFO, gniazda uniksowe i węzły urządzeń obok plików, katalogów i dowiązań. Typ mieszka w bitach `S_IFMT` kolumny `mode` (tam, gdzie stawia go POSIX), nie w osobnej kolumnie — dwa zapisy tego samego faktu to dwa miejsca, które mogą się rozjechać. Jedyna zmiana schematu to `rdev` (migracja 007). Typy specjalne nie mają ścieżki danych: jądro implementuje ich semantykę, gdy tylko `getattr` poda typ, więc kosztują jeden wiersz inode i nic więcej. Wynika z pomiaru: **81% z 3722 porażek pjdfstest** wywodzi się z ich braku, w większości jako kaskada `ENOENT` po nieudanym `mkfifo`/`mknod`/`bind`.
- [ADR-61](adr/ADR-61-posix-timestamps-and-atime-policy.md) — rozdzielenie znaczników czasu POSIX. `setattr` ignorował `atime`/`mtime`, a `inode_to_file_attr` podawał `updated_at` w trzech polach naraz, więc `utimensat` był **cichym no-opem**: wołający dostawał sukces, czasy się nie zmieniały. Migracja 008 dodaje `atime` i `mtime`; `ctime` zostaje jako `updated_at`, bo POSIX definiuje go jako czas zmiany metadanych — czyli dokładnie to, czym ta kolumna już jest. Decyzja, dla której to jest ADR: **`relatime`, nie `strictatime`** — w SmartFS metadane leżą w Postgresie, więc ścisły `atime` zamieniłby każdy odczyt w zapis do bazy. Aktualizacja `atime` idzie obok ścieżki odpowiedzi, nigdy przed nią.
- [ADR-62](adr/ADR-62-per-inode-dedup-and-lazy-reclaim.md) — **proponowany, cztery Otwarte pytania blokują kod.** `dedup_enabled` per inode, trzecie rodzeństwo obok `compression_level` i `versioning_enabled`, ustawiane przez wtyczkę typu pliku. Wyłączony: ścieżka zapisu **nie dotyka bazy w ogóle** (znika `insert_blob`, 58% kosztu `close()`), blob jest prywatny, a `unlink` zwalnia miejsce od razu — bo nie trzeba dowodzić, że nikt inny go nie referuje. Włączony: dedup rozstrzygany leniwie w drenażu ADR-58, a miejsce odzyskuje sprzątacz w **istniejącej** fazie snu (`idle_before_sleep_secs` z migracji 005). Reguła, bez której gwarancja szybkiego kasowania jest fikcją: blob prywatny nigdy nie uczestniczy w dedupie w żadną stronę — co czyni z `blobs` indeks dedupu, a nie inwentarz blobów.
- [ADR-60](adr/ADR-60-plugin-architecture-rust-spirv.md) — **proponowany, trzy Otwarte pytania blokują implementację.** Kierunek: pluginizacja zachowań zależnych od typu pliku, w dokładnie dwóch dozwolonych językach — Rust i SPIR-V. Rust wchodzi **w kompilacji** (statyczny rejestr, nie `dlopen`), bo brak stabilnego ABI zamieniłby niezgodność wersji w cichy odczyt spod złego offsetu, a w systemie plików to jest nieodróżnialne od uszkodzenia danych. SPIR-V ładuje się **w locie**, bo jest danymi o stabilnym formacie. Trzy punkty rozszerzenia i ani jednego więcej: polityka składowania, ekstrakcja metadanych, transformacje GPU — żaden nie może blokować `cow_commit` (ta sama dyscyplina co Root Invariant #6). WASM odrzucony świadomie, razem z izolacją, którą dawał.
- [ADR-63](adr/ADR-63-embedding-model-placement.md) — **przyjęty.** Gdzie mieszka model embeddingowy: w procesie, na dysku i w jakim formacie. ADR-49 wybrał *który* model, ale nie postawił pozostałych trzech pytań — i model nigdy nie ruszył. Etap 6 Wielkiego Testu pokazał, że `run_worker_supervisor` nie jest wołany przez nic (trzeci w tym projekcie kompletny komponent bez punktu wejścia), a przegląd kodu przy okazji — że `CpuEmbeddingEngine` **nie uruchamia żadnego modelu**: liczy SHA-256 z tekstu i rozwija go w wektor. Fałszywy silnik z podłączonym workerem zapełniłby tabele i przepuścił etap 6 na zielono, więc jest groźniejszy niż brakujący worker. Decyzje: jeden silnik `ggml`/GGUF, ale **wiele backendów wybieranych pomiarem, nie założeniem** (rewizja 2 — rozstrzyga Otwarte pytanie ADR-55; backend Vulkan w `ggml` *jest* zestawem kerneli compute w SPIR-V, więc nie ma czego pisać; `-ngl N` daje częściowy offload, a zmierzone z pliku 15,9 MiB na warstwę Q8_0 znaczy, że **karta z 1 GB VRAM mieści cały model z zapasem**; `llvmpipe` musi być odrzucony, bo to CPU udające kartę); worker jako osobny proces `smartfs-worker` nadzorowany przez `smartfsd` z `PDEATHSIG` — osobna przestrzeń adresowa, bo upadek biblioteki ML nie może odmontować systemu plików, ale dziecko demona zamiast unitu systemd, bo „pamiętaj włączyć” to dokładnie ten tryb awarii, który tu naprawiamy; wagi poza repozytorium, poza magazynem blobów (enumerują go scrub i sprzątacz) i poza partycją systemową; brak modelu jest błędem głośnym, nigdy fallbackiem na wektory zastępcze; migracja 010 domyka przeoczony przez ADR-49 cutover wymiaru w `ast_embeddings_1536`.

## Invarianty — rozszerzenie ROOT CLAUDE.md

Pięć invariantów z v4.5 (ROOT CLAUDE.md §3.1, pełny tekst także w
[docs/base-v4.5-v5.0/SmartFS_Architecture_v4_5.md](base-v4.5-v5.0/SmartFS_Architecture_v4_5.md) §3.1),
zacytowane tu dosłownie, żeby wszystkie 8 invariantów żyło w jednym miejscu:

```
1. content_hash = SHA-256(oryginalne bajty PRZED kompresją) — nigdy po
2. Każda zmiana treści tworzy nowy wiersz file_versions (CoW) — nigdy nie
   mutuje bloba
3. Jedyna legalna ścieżka do danych wiedzie przez daemona — ext4 to głupi
   magazyn blobów
4. Migracje są jawne w migrations/ — żadnego CREATE TABLE w kodzie
   runtime
5. Nigdy nie mieszaj wymiarów embeddingów między zapytaniami — 384 z 384,
   1536 z 1536
```

Do nich dochodzi trójka nowa w v6.0:

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
