# ADR-58 — cow_commit dwuetapowy: bufor `pending` na ext4 + kolejka FIFO ograniczona RAM-em

← [Mapa ADR](../01-architecture.md) | Dotyka: [docs/crates/_unchanged.md](../crates/_unchanged.md) (smartfs-fuse, smartfs-store przestają być "bez zmian") | Poprzednik w tym samym miejscu kodu: ADR-16 (bufor RAM w `write()`, [SmartFS_Architecture_v4_5.md](../base-v4.5-v5.0/SmartFS_Architecture_v4_5.md) §3.6)

**Status:** Przyjęty do implementacji (2026-09-13). Wszystkie trzy Otwarte pytania rozstrzygnięte przez właściciela projektu — patrz §Rozstrzygnięcia. Punkt 6 niżej pozostaje **kandydatem** do rozszerzenia listy Root Invariants, nie został dopisany automatycznie.

## Kontekst

ADR-16 ustalił, że `write()` w `smartfs-fuse` buforuje cały plik w RAM aż do `release()`, gdzie następuje `cow_commit`: hash → kompresja → zapis bloba do store'u → transakcja Postgresowa (`file_versions`, aktualizacja `inode_registry`). Dziś ten `cow_commit` jest **jednoetapowy i synchroniczny** — `release()` nie wraca do wołającego, dopóki transakcja Postgresowa nie zostanie zatwierdzona. To jest solidne (Root Invariant #3: jedyna legalna ścieżka do danych wiedzie przez daemona), ale wiąże czas odpowiedzi `close()` z czasem pełnej transakcji SQL (indeksy, MVCC, WAL Postgresa) przy każdym pojedynczym zapisie, nawet drobnym.

Rozmowa poprzedzająca ten ADR (streszczenie w historii sesji) rozważyła i odrzuciła po kolei: (a) osobny "poziom L0" jako kolejkę czysto w RAM bez żadnego trwałego śladu — odrzucone, bo potwierdzony wołającemu zapis mógłby zniknąć bezśladowo przy padzie demona, tworząc sierotę identyczną z klasą problemu, którą FIX-01 (`blobs.refcount`) już raz zamykał; (b) dopisywanie metadanych ścieżki/wersji bezpośrednio do bajtów bloba — odrzucone, bo psuje `content_hash` jako czysty hash treści (Root Invariant #1) i unieważnia dedup; (c) pełny WAL w stylu SQLite jako pośredni magazyn — odrzucone jako nadmiarowe, gdy ten sam efekt trwałości daje tańszy, już znany w Uniksie wzorzec.

## Decyzja

1. **Znacznik `pending` na tym samym `store_path` co blob, zapisywany atomowo wzorcem Maildira**: `write()` do tymczasowej nazwy w `<store_path>/pending/tmp/`, `fdatasync`, potem `rename()` do `<store_path>/pending/queue/<seq>_<content_hash>.json`. Nazwa pliku i jego treść (mały JSON, nie blob) niosą wszystko, czego potrzebuje transakcja Postgresowa: `path`, `parent_inode`, `version_seq`, `content_hash`, `size`, `mode`/`uid`/`gid`, timestamp. Sam blob (zawartość pliku) trafia do store'u pod `content_hash` bez zmian — `content_hash` pozostaje czystym SHA-256 treści przed kompresją, zgodnie z Root Invariant #1. `rename()` w obrębie jednego systemu plików jest atomowy na poziomie jądra — to jest cała gwarancja trwałości tego etapu, bez potrzeby własnego formatu WAL.
2. **Kolejka FIFO w RAM to wyłącznie optymalizacja kolejności drenażu, nie mechanizm trwałości.** Po udanym `rename()` z punktu 1, `release()` wraca do wołającego (koniec `cow_commit`, etap "pending") i dopiero wtedy referencja do znacznika trafia do ograniczonej kolejki w pamięci. Utrata tej kolejki (crash procesu) nie traci żadnych danych — punkt 4 ją w całości odtwarza ze skanu `pending/queue/`.
3. **Jeden konsument, drenaż sekwencyjny.** Pojedynczy wątek/task ściąga znaczniki z kolejki w kolejności FIFO i wykonuje dla każdego dokładnie tę transakcję Postgresową, którą dziś wykonuje `cow_commit` synchronicznie. Po `COMMIT` w Postgresie znacznik jest usuwany z `pending/queue/` (`unlink`) — to jest checkpoint, analogiczny do checkpointu WAL w samym Postgresie.
4. **Skan `pending/queue/` obowiązkowy przy starcie demona i okresowy w tle.** Skan nie dotyka całego `store_path`, tylko tego jednego podkatalogu — koszt jest proporcjonalny do zaległości, nie do rozmiaru repozytorium. Dla każdego znalezionego znacznika: transakcja Postgresowa jak w punkcie 3, z kluczem idempotencji z §Rozstrzygnięcia #3 — bo znacznik mógł przeżyć commit do Postgresa, ale nie zdążyć się skasować przed padem, i replay musi być bezpieczny do uruchomienia dwa razy na tym samym pliku.
5. **Kolejka RAM ma twardy limit, liczony raz przy starcie jako ułamek CAŁKOWITEGO RAM-u maszyny (nie bieżącego wolnego), z twardym sufitem.** Dynamiczne przeliczanie limitu względem chwilowo wolnej pamięci odrzucone w rozmowie poprzedzającej — pod presją pamięciową z innych procesów limit policzony na bieżąco byłby nieaktualny szybciej, niż zdążyłby zadziałać. Statyczny ułamek całości to ten sam wybór co `shared_buffers` w Postgresie.
6. **Przy trafieniu limitu: `write()` blokuje, nigdy nie odrzuca cicho i nigdy nie gubi potwierdzonego zapisu.** *(Kandydat na Root Invariant #9 — patrz Status wyżej.)* Kolejność ta sama co w pkt 5 rozmowy poprzedzającej: lepiej spowolnić wołającego niż zgubić dane, analogicznie do `max.block.ms` przy wyczerpanym `buffer.memory` w producencie Kafki i do kontroli przepływu TCP. Dokładna semantyka blokady — §Rozstrzygnięcia #1.
7. **Osobna, rzadsza pętla: scrub sumami kontrolnymi już zatwierdzonych blobów** (bit rot, nie osierocenie) — porównanie zawartości na dysku z zapisanym `content_hash` dla losowej próbki albo pełnego przebiegu przy niskim obciążeniu. To jest mechanizm inny niż punkt 4 (tam wykrywamy brak wiersza w bazie dla istniejącego pliku; tu wykrywamy zepsuty plik dla istniejącego wiersza) i nie może być z nim pomylony w implementacji ani w logach.

## Rozstrzygnięcia (były: Otwarte pytania)

Rozstrzygnięte przez właściciela projektu 2026-09-13, przed startem kroku A. Treść pytań zachowana, żeby uzasadnienie dało się później odtworzyć.

### #1 — Semantyka blokady przy pełnej kolejce

*Pytanie:* indefinite block na `write()` (prosto, ale wygląda z zewnątrz jak zawieszony proces wołający) kontra zwrot `EAGAIN`/`EWOULDBLOCK` po progu czasu (jawny sygnał "spróbuj później", ale wymaga, żeby wołający program obsługiwał taki kod błędu na zapisie).

**Rozstrzygnięcie: blokada z limitem czasu, po jego przekroczeniu `EAGAIN`.**

Fakt, który przeważył: jądro Linuksa nie ma domyślnego timeoutu na żądania FUSE. Blokada bez limitu oznacza proces wołający w nieprzerywalnym śnie (stan `D`), odporny na `kill -9`, i mount, którego nie da się odmontować inaczej niż siłowo. Blokada z limitem zachowuje intencję punktu 6 Decyzji — potwierdzony zapis nigdy nie ginie, bo `EAGAIN` znaczy "nie przyjąłem", a nie "przyjąłem i zgubiłem" — i jednocześnie nie zostawia systemu w stanie, z którego nie ma wyjścia.

Wartość progu: **30 s**, nadpisywalna przez `SMARTFS_PENDING_BLOCK_TIMEOUT_MS`. Dobrana tak, żeby przetrwać chwilowo wolny drenaż (zdrowy drenaż zwalnia miejsce w milisekundach), a jednocześnie nie udawać zawieszenia, gdy drenaż stoi na dobre — na to i tak żaden próg nie pomoże, od tego jest `EAGAIN` i log.

### #2 — Format znacznika `pending`

*Pytanie:* JSON obok nazwy kodującej `seq`+hash, czy cała treść zakodowana w samej nazwie pliku bez osobnego odczytu zawartości.

**Rozstrzygnięcie: JSON, nazwa `<seq>_<content_hash>.json`, jak w punkcie 1 Decyzji.**

Nazwa niesie `seq` i `content_hash` — to wystarcza do posortowania FIFO i odsiania duplikatów bez czytania zawartości. Reszta pól idzie do JSON-a. Wariant "wszystko w nazwie" odrzucony: limit nazwy na ext4 to 255 bajtów, a sam hash zjada 64 znaki, `parent_inode` jako UUID kolejne 36; `special_data` nie zmieściłoby się w ogóle, a każde przyszłe pole wymuszałoby zmianę formatu nazwy. Skan z punktu 4 robi więc `readdir` + `read` na wpis — koszt proporcjonalny do zaległości, nie do rozmiaru repozytorium, zgodnie z tym, co punkt 4 i tak zakłada.

### #3 — Klucz idempotencji przy replayu

*Pytanie:* unikalność po `(path, version_seq)` czy po `content_hash`.

**Rozstrzygnięcie: `(inode_id, version_number)`, z nową jawną migracją zakładającą na tę parę unikalny indeks.**

Uściślenie względem treści pytania: `file_versions` nie ma kolumny `path` — ma `inode_id` i `version_number`. To jest naturalny kształt schematu i dokładnie ten, którego pilnuje Invariant #2 planu testowego ("żadne dwie wersje inode'a nie dzielą `version_number`").

`content_hash` odrzucony: w CAS-ie z deduplikacją ten sam hash legalnie wskazuje wiele wierszy `file_versions` — dwa różne pliki o identycznej treści to poprawny, testowany stan (sekcja 1.4 planu testowego) — więc hash nie jest kluczem wersji.

**Konsekwencja przyjęta świadomie:** `version_number` musi być znany już przy zapisie znacznika, więc numerowanie wersji wychodzi z transakcji drenażu do etapu pending. Serializatorem pozostaje Postgres — krótka transakcja `SELECT id FROM inode_registry WHERE id = $1 FOR UPDATE` + `MAX(version_number)+1`, wykonana w etapie pending. Nie da się tego zastąpić licznikiem w pamięci demona, bo `smartfs-cli` pisze do tej samej bazy niezależnie od demona (zbieżność obu ścieżek zapisu to sekcja 1.7 planu testowego), a dwa demony w xfstests to dwie osobne bazy, ale jeden demon i jedno CLI to ta sama. Ścieżka zapisu nadal traci ciężką część transakcji — `ast_nodes`, `UPDATE inode_registry`, indeksy `file_versions` — i to jest zysk, o który chodzi w ADR-ze; zostaje jedno tanie zapytanie pod blokadą wiersza.

## Odrzucone alternatywy

**Czysta kolejka RAM bez znacznika na dysku (pierwsza wersja pomysłu z rozmowy).** Odrzucone: `write()` mógłby zwrócić sukces do wołającego, a wpis zniknąłby bezpowrotnie przy padzie demona przed drenażem — słabsza gwarancja niż dzisiejszy status quo (ADR-16), nie nowy, szybszy poziom nad nim.

**Metadane wsadzone do bajtów bloba zamiast obok niego.** Odrzucone: `content_hash` przestałby być hashem samej treści, dwa identyczne pliki pod różnymi ścieżkami przestałyby się deduplikować przez `blobs` — łamie Root Invariant #1 i cel istnienia CAS-a.

**Pełny WAL własnego formatu albo SQLite w trybie WAL jako pośredni magazyn.** Rozważone jako bezpieczna alternatywa dla czystego RAM-u. Odrzucone na rzecz punktu 1: atomowy `rename()` na tym samym ext4 daje identyczną gwarancję przeżycia crasha co lokalny WAL, bez dodawania nowej zależności i nowego formatu do utrzymania.

**Limit kolejki przeliczany na bieżąco względem aktualnie wolnego RAM-u.** Odrzucone w punkcie 5 — ryzyko niestabilności sprzężenia zwrotnego pod presją pamięciową z zewnątrz procesu demona.

**UUID wersji generowany przy znaczniku jako klucz idempotencji** (rozważony przy rozstrzyganiu #3). Nie wymagałby migracji, bo `ON CONFLICT (id) DO NOTHING` działałby na istniejącym kluczu głównym. Odrzucony, bo `version_number` byłby wtedy nadal liczony w transakcji drenażu — czyli kolejność numerów wersji wynikałaby z kolejności drenażu, nie z kolejności zapisów, co jest obserwowalną zmianą semantyki wersjonowania, a nie tylko szczegółem implementacyjnym.

## Konsekwencje

- `smartfs-fuse` i `smartfs-store` przestają być "bez zmian merytorycznych" — wychodzą z [docs/crates/_unchanged.md](../crates/_unchanged.md) i dostają własne pliki `docs/crates/smartfs-fuse.md` / `docs/crates/smartfs-store.md`, tak jak zrobiono to wcześniej dla `smartfs-db.md`/`smartfs-ai.md`/`smartfs-mcp.md`.
- `SmartFsError` zyskuje wariant `PendingQueueFull`, mapowany w `error_to_errno` na `EAGAIN` — zgodnie z §Rozstrzygnięcia #1 jest zwracany, nie tylko logowany.
- `smartfs-cli status` (dziś `StatusArgs` bez pól) zyskuje w wyniku: liczbę znaczników w `pending/queue/`, wiek najstarszego nieskonsumowanego znacznika, bieżący rozmiar kolejki RAM względem limitu.
- Nowa, jawnie nazwana podkomenda `smartfs-cli` do ręcznego wymuszenia skanu naprawczego poza harmonogramem — nazwa i sygnatura ustalane przy implementacji kroku E, nie zgadywane z góry (zasada 8 z [docs/06](../06-agentic-execution-plan.md)).
- Scrub sum kontrolnych (punkt 7 Decyzji) to osobny, nowy komponent w `smartfs-store` — nie rozszerzenie skanu `pending/`.
- Migracje SQL: **jedna nowa migracja**, wymuszona przez §Rozstrzygnięcia #3 — unikalny indeks na `(inode_id, version_number)` w `file_versions`. Jawny plik w `migrations/`, nigdy DDL w runtime (Root Invariant #4). Poza tym cały mechanizm punktów 1–6 żyje na ext4 i w RAM, nie w schemacie Postgresa.
- **Plan testowy przestaje opisywać aktualny system.** [docs/testing/the-great-smartfs-test.md](../testing/the-great-smartfs-test.md) §2.2 sprawdza SQL-em zaraz po `sync`, że trzy nadpisania dały trzy wiersze `file_versions` — przy asynchronicznym drenażu to jest wyścig. §5.2 K6–K9 oczekuje po craśhu "pełny rollback, plik czyta się jako poprzednia wersja", podczas gdy pod tym ADR-em crash po `rename()` znacznika musi zapis **odtworzyć**. Aktualizacja obu sekcji należy do kroku F i idzie osobnym commitem, żeby zmiana asercji była widoczna jako zmiana asercji, a nie schowana w implementacji.

## Plan wykonania dla Claude Code

Ten ADR wchodzi na już zbudowany workspace (Fazy 0-6 z [docs/06](../06-agentic-execution-plan.md) zakończone) jako pojedynczy przyrost, nie kolejna faza budowy od zera. Dekompozycja:

| Krok | Crate(y) | Tryb | Zależy od |
|---|---|---|---|
| A | `smartfs-store`: konwencja katalogu `pending/{tmp,queue}/`, funkcje zapisu/odczytu/usunięcia znacznika | jeden agent | — |
| B | `smartfs-fuse`: `release()` rozdzielony na etap "pending" (zwraca wołającemu) i etap "commit" (drenaż), kolejka RAM z limitem z punktu 5 Decyzji, blokada z punktu 6 | jeden agent, **po** kroku A | krok A |
| C | `smartfs-fuse` lub nowy moduł: skan startowy + okresowy z punktu 4 Decyzji, idempotentny replay | jeden agent, może równolegle z krokiem B po ukończeniu kroku A | krok A |
| D | `smartfs-store`: pętla scrub z punktu 7 Decyzji | niezależnie, może iść równolegle z B/C | krok A |
| E | `smartfs-cli`: rozszerzenie `status`, nowa podkomenda naprawcza | sekwencyjnie, na końcu | B, C |
| F | Aktualizacja `docs/crates/smartfs-fuse.md`, `docs/crates/smartfs-store.md`, wypisanie ich z `_unchanged.md`, aktualizacja planu testowego, backfill `@id` przez `smartfs-docgen` | jeden agent, na końcu | A-E |

**Twarde zasady (rozszerzają, nie zastępują, zasady z [docs/06](../06-agentic-execution-plan.md)):**

1. ~~Nie zaczynaj kroku B ani C, dopóki Otwarte pytania #1 i #2 nie mają odpowiedzi zapisanej w tym pliku~~ — spełnione: §Rozstrzygnięcia, 2026-09-13.
2. Root Invariant #1 (`content_hash` = hash czystej treści przed kompresją) obowiązuje również dla treści zapisanej w blobie spod znacznika `pending` — znacznik i jego metadane NIGDY nie wchodzą do liczenia hasha.
3. `cargo check --workspace` i `cargo clippy --workspace --all-targets` czyste po każdym kroku A-F osobno, tak jak przy oryginalnej budowie.
4. Commit po każdym kroku osobno, z odniesieniem do `ADR-58` w treści commit message, nigdy do treści tej rozmowy.
5. Jeśli w trakcie implementacji okaże się, że punkt 6 Decyzji (blokada zamiast odrzucenia) koliduje z czymkolwiek w `fuser`/FUSE (np. kernel ma własny timeout na operacje FUSE i długa blokada `write()` powoduje coś gorszego niż powolny zapis) — ZATRZYMAJ SIĘ i zgłoś, zamiast po cichu zamieniać na odrzucenie.
