# ADR-54 — pg_search (Tantivy-w-Postgresie) jako backend pełnotekstowy

← [Mapa ADR](../01-architecture.md)

**Status:** Przyjęty do implementacji w v6.0

## Kontekst

v4.5/v5.0 nie definiowały żadnego wyszukiwania pełnotekstowego — tylko `search_semantic` (płaski cosine) i, od v6.0, `search_by_concept` (graf centroidów, ADR-50/53). Obie metody są przybliżone i systematycznie przegrywają dokładnie tam, gdzie klasyczne BM25 wygrywa: dosłowna nazwa symbolu, treść komunikatu błędu, fraza w cudzysłowie — rzeczy, których nikt nie szuka "czegoś podobnego do", tylko chce znaleźć dosłownie.

Rozważane były dwie naturalne opcje: natywny `tsvector`/GIN Postgresa, albo Tantivy jako osobny silnik obok bazy. Rozstrzygający fakt, sprawdzony zanim podjęto decyzję: **żadna z tych dwóch opcji nie daje polskiego "za darmo"**. Postgres nie ma wbudowanej konfiguracji językowej `polish` (tylko `english`/`german`/`french`/`russian` i kilkanaście innych — polski wymaga ręcznego doinstalowania zewnętrznego słownika ispell). Samo Tantivy przez większość swojej historii też nie miało polskiego stemmingu — jego domyślny `rust_stemmers` jest nieutrzymywany od 2021 roku i nigdy nie objął polskiego.

## Decyzja

Backendem pełnotekstowym SmartFS jest **pg_search** (rozszerzenie ParadeDB) — Tantivy skompilowane przez `pgrx` jako natywny typ indeksu wewnątrz tego samego procesu Postgresa. Bez forka, bez osobnego serwera, bez drugiego systemu do utrzymania w spójności z pierwszym: `CREATE EXTENSION pg_search CASCADE` (CASCADE dociąga `pgvector`, od którego pg_search i tak zależy — koegzystuje z tym, co już mamy, nie konkuruje).

Polski akurat w pg_search (nie w gołym Tantivy) dostał natywny stemming Snowball 4 grudnia 2025 (issue `paradedb/paradedb#1625` → PR `#3645`) — to jedyna z trzech rozważanych opcji, która ma polski z pudełka, bez doklejania zewnętrznego słownika.

### Co dokładnie się indeksuje

- **`ast_nodes.source`** — tekst źródłowy per funkcja/węzeł AST już istnieje w schemacie (v4.5 §3.7) i jest dokładnie tym, co `smartfs-ai` czyta do embeddingu kodu. BM25 dostaje ten sam tekst za darmo, bez nowej kolumny.
- **Nowa kolumna `file_versions.search_text`** (nullable) — dla warstwy plikowej (nie-AST) taki tekst dotąd nie istniał trwale w Postgresie: embeddingi ogólne (`embeddings_384/768/1024_qwen`, "wszystkie pliki", v4.5 §10.3) są liczone z bajtów czytanych transientnie ze store'u i odrzucanych po inferencji. `smartfs-ai` zapisuje ten tekst raz, przy tej samej operacji, która i tak czyta content do embeddingu warstwy ogólnej — zero dodatkowego odczytu ze storage.

**Zamknięcie ukrytej luki w v4.5:** dla plików nietekstowych (obraz, binarka) v4.5 nigdy nie precyzowało, co dokładnie jest "embedowane" w warstwie ogólnej. v6.0 rozstrzyga to explicite dla `search_text`: jeśli content nie jest poprawnym UTF-8, `search_text` = konkatenacja wartości string ze schematu wtyczki tego pliku (np. `text_metadata`, `color_type` z `png.json`, v4.5 §10.1) — plugin system i tak produkuje ten tekst "dla agentów semantycznych analizujących typy danych między sobą" (v4.5 §10), więc BM25 dostaje sensowną treść nawet dla plików, których nikt nigdy nie zembeduje jako prozy. Jeśli wtyczka nie ma żadnych pól string w schemacie, `search_text` zostaje `NULL` (partial index z migracji 006 to filtruje).

## Odrzucone alternatywy

**Natywny `tsvector`/GIN Postgresa jako MVP** (wcześniejsza rekomendacja w tej rozmowie, zanim sprawdzono szczegóły). Odrzucone po weryfikacji: dla polskiego wymagałoby dokładnie tego samego zewnętrznego obejścia (słownik ispell), które i tak trzeba by utrzymywać — "natywne" nie dawało tu żadnej przewagi kosztowej, tylko iluzję prostoty.

**Gołe Tantivy jako osobna usługa/indeks obok Postgresa.** Odrzucone: wymagałoby własnego mechanizmu utrzymywania spójności z `file_versions`/`ast_nodes` — drugi system commitów, drugi system odzyskiwania po awarii. Dokładnie ta klasa problemu, którą CoW + `blobs` już raz rozwiązały wewnątrz jednej bazy; pg_search daje ten sam silnik (to dosłownie Tantivy w środku) bez tego kosztu.

**Rezygnacja z BM25, poleganie wyłącznie na `search_semantic`/`search_by_concept`.** Odrzucone: metody przybliżone systematycznie przegrywają z dosłownym dopasowaniem tam, gdzie użytkownik i tak wie dokładnie czego szuka — bardzo częsty tryb wyszukiwania w repozytorium kodu.

## Otwarte pytanie

Łączenie trzech trybów wyszukiwania (`search_fulltext` BM25, `search_semantic` cosine, `search_by_concept` graf centroidów) w jeden ranking hybrydowy (np. Reciprocal Rank Fusion) nie jest tu rozstrzygnięte. v6.0 udostępnia wszystkie trzy jako osobne narzędzia MCP, świadomie zostawiając scalanie rankingów — i decyzję o wagach — na później, gdy będzie już realny korpus do kalibracji na nim, a nie w próżni.

## Konsekwencje

- Migracja [`006_fulltext_search.sql`](../../migrations/006_fulltext_search.sql): `CREATE EXTENSION pg_search CASCADE`, `ALTER TABLE file_versions ADD COLUMN search_text TEXT`, indeksy BM25 na `file_versions.search_text` (partial, `WHERE search_text IS NOT NULL`) i na `ast_nodes.source`.
- `smartfs-ai` (delta — [docs/crates/smartfs-ai.md](../crates/smartfs-ai.md)): musi wypełniać `search_text` przy przebiegu generic-embedding, przez `smartfs-db::set_search_text`, nigdy bezpośrednim `sqlx`.
- `smartfs-db` (delta — [docs/crates/smartfs-db.md](../crates/smartfs-db.md)): nowe `set_search_text` i `search_fulltext_bm25`.
- `smartfs-mcp` (delta — [docs/crates/smartfs-mcp.md](../crates/smartfs-mcp.md)): nowe narzędzie `search_fulltext(query, plugin_type?, limit)`.
- **Licencja:** pg_search Community to AGPL-3.0 (copyleft sieciowy). Nieistotne dla SmartFS jako lokalnego narzędzia FUSE na jednej maszynie; staje się istotne, gdyby SmartFS kiedyś był oferowany jako usługa sieciowa, do której łączą się inni — wtedy AGPL wymusza udostępnienie źródeł tej usługi. Świadoma decyzja zapisana tutaj, żeby nie została odkryta przypadkiem za rok.
- **Zauważona przy okazji prawidłowość, warta zapisania wprost:** większość twardych zwycięstw poprawnościowych tego projektu do tej pory (FIX-01..10, partial unique index z FIX-06, `is_active`/`merged_into` zamiast `DELETE` w ADR-53, `NOT NULL`, które złapało błąd `plugin_type` przy review) została wymuszona na poziomie SQL/schematu, nie w kodzie Rust. pg_search kontynuuje ten wzorzec: wyszukiwanie pełnotekstowe jest kolejną rzeczą, którą Postgres-jako-platforma robi za nas porządnie, zamiast stawać się osobnym serwisem do ręcznego utrzymania w spójności.
