# ADR-62 — Dedup przełączany per plik, leniwy, ze sprzątaczem w fazie snu

← [Mapa ADR](../01-architecture.md) | Wynika z pomiaru: [PLAN §5c](../plans/PLAN-posix-parity-and-storage-policy.md) | Poprzednicy: [ADR-58](ADR-58-two-stage-cow-commit.md) (dwuetapowy commit), [ADR-60](ADR-60-plugin-architecture-rust-spirv.md) (polityka składowania z wtyczek), FIX-01 (dedup bez refcountów), FIX-03/FIX-04

**Status:** Przyjęty 2026-09-13. Kierunek zaproponowany przez właściciela projektu, cztery Otwarte pytania rozstrzygnięte na jego prośbę przez implementującego — patrz §Rozstrzygnięcia, gdzie zapisano również uzasadnienia, żeby dało się je później zakwestionować. Wdrożenie fazowe, patrz §Fazy.

---

## Kontekst

Rozbicie `close()` na fazy (`590bbb2`) wskazało winnego jednoznacznie:

| faza | ms | udział |
|---|---:|---:|
| **dedup (`insert_blob`)** | **43,3** | **58%** |
| blob (kompresja + `store.put` + fsync) | 25,8 | 35% |
| znacznik ADR-58 | 2,0 | 3% |
| lookup inode'a, hash | 0,7 | 1% |

Zmierzone poza SmartFS: pojedynczy `INSERT` do `blobs` w autocommicie kosztuje 27 ms, ten sam w transakcji zbiorczej 1 ms. To jest **wyłącznie oczekiwanie na fsync WAL-a** — dane Postgresa leżą na ZFS z `sync=standard` bez SLOG-a.

ADR-58 zdjął ze ścieżki potwierdzenia KROK 2 i to się zwróciło (2 ms zamiast 27–43 ms). Ale KROK 1 został, a `insert_blob` z niego jest teraz największym pojedynczym kosztem zapisu.

Dodatkowa obserwacja z przeglądu kodu: **GC blobów nie istnieje.** FIX-01 zniósł refcounty na rzecz „GC-by-scan", ale sam skan nigdy nie powstał. Każda dyskusja o dedupie musi się z tym zmierzyć, bo dedup bez odzyskiwania miejsca to wyciek z dodatkowymi krokami.

## Decyzja

### 1. `dedup_enabled` per inode, trzecie rodzeństwo istniejących polityk

`inode_registry` ma już `compression_level SMALLINT` i `versioning_enabled BOOLEAN` — polityki podejmowane per plik, nie globalnie. `dedup_enabled BOOLEAN NOT NULL DEFAULT TRUE` dołącza do nich; wartość ustala wtyczka typu pliku przy tworzeniu inode'a (ADR-60, punkt „polityka składowania").

### 2. Dedup wyłączony: blob prywatny

Blob takiego pliku jest **prywatny** — należy do dokładnie jednej wersji jednego inode'a i nie wchodzi do indeksu dedupu (`shared = FALSE`).

**Sprostowanie wcześniejszej wersji tego punktu.** Pisałem tu, że przy wyłączonym dedupie `insert_blob` znika ze ścieżki zapisu i 43 ms schodzi natychmiast. **To przestało być prawdą po poprawce z punktu 4**: skoro każdy blob zachowuje wiersz w inwentarzu, to również prywatny wymaga `INSERT`-a — a mierzone 43 ms to koszt *autocommitu*, nie samej logiki dedupu.

Właściwy rozkład jest taki, i jest czystszy:

| co daje | skąd się bierze |
|---|---|
| zdjęcie 43 ms ze ścieżki zapisu | **leniwość** (punkt 5) — dla obu ustawień jednakowo |
| gwarancja szybkiego kasowania | **prywatność** (punkty 3–4) — tylko przy `dedup_enabled = FALSE` |

Te dwie rzeczy są ortogonalne. Wcześniej je zlepiłem.

### 3. Dedup wyłączony daje gwarancję szybkiego kasowania — i to jest jego główna wartość

Skoro blob prywatny ma dokładnie jednego właściciela, `unlink` może zwolnić miejsce **od razu**, bez dowodzenia, że nikt inny go nie referuje. Przy dedupie ten dowód wymaga skanu (bo FIX-01 zniósł refcounty), więc zwolnienie miejsca jest z natury odroczone.

To jest własność widoczna dla użytkownika, nie mikrooptymalizacja: „skasowałem, miejsce wróciło" ma znaczenie dla kwot, dla przewidywalności i dla wymogów usuwania danych.

### 4. Reguła własności, bez której gwarancja z punktu 3 jest fikcją

**Blob prywatny nigdy nie uczestniczy w dedupie — ani jako cel, ani jako źródło.**

Bez tej reguły sprzątacz mógłby wskazać plik z dedupem włączonym na bloba pliku z dedupem wyłączonym, i skasowanie tego drugiego przestałoby zwalniać miejsce. Gwarancja wyparowałaby po cichu, w momencie niezwiązanym z żadną decyzją użytkownika.

**Konsekwencja schematowa — poprawiona 2026-09-13 po zarzucie właściciela projektu.**

Pierwsza wersja tego punktu mówiła: wersja prywatna nie ma wiersza w `blobs`, bo `content_hash` jest tam kluczem głównym i prywatny nie zmieści się obok współdzielonego o tej samej treści. **To było złe rozwiązanie**, i zarzut brzmiał: skoro baza obsługuje wyszukiwanie pełnotekstowe i resztę, to plik nie może istnieć bez wpisu w bazie.

Sam zarzut celuje obok — full-text search indeksuje `file_versions.search_text` i `ast_nodes.source`, nie `blobs`, a ścieżka odczytu FUSE nie pyta `blobs` **ani razu**; plik zawsze ma wiersze w `inode_registry` i `file_versions`. Ale prowadzi do właściwego wniosku: **niekompletny inwentarz blobów jest sam w sobie wadą**, bo stoją na nim dwie rzeczy — sprawdzian Invariantu #1 w etapie 4 i scrub sum kontrolnych, oba przez `JOIN blobs`. Obie po cichu przestałyby weryfikować bloby prywatne.

**Rozwiązanie: każdy fizyczny blob zachowuje wiersz; warunkowe jest wyłącznie uczestnictwo w dedupie.**

```sql
-- klucz główny na blob_id, nie na content_hash
blob_id      UUID PRIMARY KEY,
content_hash TEXT NOT NULL,
shared       BOOLEAN NOT NULL DEFAULT TRUE,

-- indeks dedupu: unikalność treści tylko wśród blobów współdzielonych
CREATE UNIQUE INDEX blobs_dedup ON blobs (content_hash) WHERE shared;
```

`blobs` pozostaje kompletnym inwentarzem **i jednocześnie** indeksem dedupu — tym drugim przez indeks częściowy, a nie przez klucz główny. Sprawdzian Invariantu #1 i scrub działają bez żadnej zmiany.

Sprawdzone empirycznie na PostgreSQL 16: dwa bloby prywatne o identycznej treści współistnieją; prywatny i współdzielony o identycznej treści współistnieją; drugi **współdzielony** o tej samej treści jest odrzucany przez `blobs_dedup`; a `INSERT ... ON CONFLICT (content_hash) WHERE shared DO UPDATE ... RETURNING blob_id, (xmax = 0)` — czyli serce dedupu z FIX-01 — działa na indeksie częściowym bez zmian w logice.

### 5. Dedup włączony: leniwy, rozstrzygany w drenażu

Zapis idzie pod **tymczasowym** UUID, bez pytania bazy. Drenaż ADR-58 wykonuje `insert_blob` i `cow_commit` w **jednej** transakcji — jeden commit zamiast dwóch, i poza ścieżką potwierdzenia. Gdy `ON CONFLICT ... RETURNING` zwróci istniejący `blob_id`, wersja dostaje jego, a świeżo zapisany plik tymczasowy jest kasowany.

Nic nie wymaga przepinania istniejących wierszy — duplikat jest rozstrzygany, zanim wersja w ogóle powstanie.

**Koszt odroczenia jest jawny: przy duplikacie zapisujemy bajty, których już mamy, i zaraz je kasujemy.** Dla obciążeń z rzadkimi duplikatami to dobry interes, dla backupów i artefaktów CI — zły. Dlatego jest to przełącznik, a nie decyzja globalna.

### 6. Sprzątacz działa w istniejącej fazie snu

`consolidation_thresholds.idle_before_sleep_secs` istnieje od migracji 005 (domyślnie 1800 s) i steruje już supervisorami konsolidacji. Sprzątacz podpina się pod ten sam sygnał bezczynności, zamiast wprowadzać drugie, konkurencyjne pojęcie „system nic nie robi".

Jego zadanie: znaleźć bloby, których nie referuje żaden wiersz `file_versions`, i skasować je. Dwa warunki bezpieczeństwa, oba obowiązkowe:

- **Wyklucz bloby wskazywane przez znaczniki w `pending/queue/`.** Pod ADR-58 istnieje okno, w którym zapis jest już potwierdzony wołającemu, blob leży na dysku, a wiersza jeszcze nie ma. Skan bez tego wykluczenia kasuje dane, o których użytkownikowi powiedziano, że są zapisane.
- **Zachowaj okno karencji** (`created_at < now() - interval '1 hour'`, jak przewiduje komentarz w migracji 001) jako drugą warstwę na te same wyścigi.

## Odrzucone alternatywy

**Przywrócić refcounty.** Rozwiązałoby kasowanie natychmiastowo i dla wszystkich plików. Odrzucone: to jest dokładnie FIX-01, zniesiony świadomie, a preflight ma osobną asercję pilnującą, żeby `blobs.refcount` nie wrócił. Licznik pod współbieżnym CoW to klasa błędów, z której ten projekt już raz wyszedł.

**Dedup zawsze synchroniczny (stan dzisiejszy).** Poprawny i prosty. Odrzucony: kosztuje 43 ms na każdy zapis, czyli 58% ścieżki, w zamian za oszczędność miejsca, która dla większości plików nie występuje.

**Dedup zawsze leniwy, bez przełącznika.** Prostsze niż to, co tu proponujemy. Odrzucone, bo znosi gwarancję szybkiego kasowania dla wszystkich plików naraz — a to jest własność, którą część zastosowań potrzebuje twardo.

**`synchronous_commit=off` w Postgresie.** Usunęłoby te 27 ms jednym ustawieniem. Odrzucone bez dyskusji: to handel trwałością, czyli tym, czego pilnują Root Invariants.

## Konsekwencje

- Migracja: `dedup_enabled BOOLEAN NOT NULL DEFAULT TRUE` w `inode_registry`.
- `blobs` pozostaje inwentarzem i **dodatkowo** pełni rolę indeksu dedupu, przez indeks częściowy zamiast klucza głównego.
- **Etap 4 i scrub działają bez zmian** dzięki poprawce z punktu 4: inwentarz zostaje kompletny. W pierwszej wersji tego ADR-a obie te weryfikacje po cichu przestałyby obejmować bloby prywatne — sprawdzian, który cicho przestaje sprawdzać, jest gorszy niż jego brak. Warto zapisać, że to była realna pułapka, a nie hipotetyczna.
- Migracja zmienia klucz główny `blobs` z `content_hash` na `blob_id` i dokłada `shared` plus indeks częściowy. To jedyna zmiana w tej tabeli; `insert_blob` zyskuje `WHERE shared` w klauzuli `ON CONFLICT` i poza tym zostaje bez zmian.
- FIX-03 i FIX-04 tracą swoje uzasadnienie na ścieżce leniwej: ich sens polega na tym, że wiersz w `blobs` powstaje **przed** zapisem bajtów, więc crash zostawia wykrywalny „zatruty wiersz". Po odwróceniu crash zostawia osierocony plik bez wiersza — stan niewidoczny przez mount (Invariant #3) i sprzątany przez GC. To wygląda na **uproszczenie** semantyki crashowej, ale K1–K5 mierzą dokładnie ten obszar i muszą zostać przepisane, a nie założone.
- GC-by-scan przestaje być teoretyczny i staje się warunkiem koniecznym.

## Rozstrzygnięcia

Rozstrzygnięte 2026-09-13 na wyraźną prośbę właściciela projektu („sam odpowiedz na 4 pytania"). Uzasadnienia zapisane, bo decyzja podjęta w zastępstwie musi dać się później podważyć na podstawie powodów, a nie autorytetu.

### #1 — Domyślne `dedup_enabled = TRUE`

Trzy powody, w kolejności wagi.

**To zachowuje dzisiejsze zachowanie.** Dedup jest dziś bezwarunkowy. Domyślne `FALSE` znaczyłoby, że po aktualizacji system po cichu przestaje deduplikować i zużycie miejsca rośnie bez niczyjej decyzji. Zmiana ma być opt-out, nie opt-in.

**Dedup jest konstytutywny dla SmartFS.** To magazyn adresowany treścią, w którym Root Invariant #2 tworzy nową wersję przy każdej zmianie. Zbiór wersji jednego pliku jest z natury pełen powtórzeń; wyłączenie dedupu domyślnie stawiałoby system przeciw własnemu projektowi.

**Wybór nie dotyczy już szybkości zapisu** — po sprostowaniu w punkcie 2 leniwość zdejmuje 43 ms dla obu ustawień. Zostaje wyłącznie „szybkie kasowanie plus zmarnowane I/O na duplikatach" kontra „odroczone kasowanie plus oszczędność miejsca". Dla pliku, o którym nic nie wiadomo, drugie jest bezpieczniejszym domyślnym.

Wtyczka typu pliku (ADR-60) ustawia `FALSE` tam, gdzie to ma sens — wideo, obrazy dysków, artefakty budowania — czyli tam, gdzie duplikaty są rzadkie, pliki duże, a natychmiastowe zwolnienie miejsca istotne.

### #2 — Przełączanie wolno, ale nie działa wstecz; gwarancja jest własnością WERSJI, nie pliku

`dedup_enabled` można zmienić w dowolnym momencie i dotyczy **wyłącznie wersji zapisanych po zmianie**. Nic nie jest przepisywane, nic nie jest kopiowane.

Kluczowe następstwo, i to ono jest właściwą odpowiedzią: **„czy skasowanie zwolni miejsce" jest własnością konkretnego bloba (`blobs.shared`), nie flagi na inode'ie.** Wyłączenie dedupu na pliku, którego bieżąca wersja siedzi na blobie współdzielonym, nie czyni tego bloba prywatnym — i nie powinno, bo jedyną drogą byłaby kopia, której cała ta konstrukcja unika.

Żeby to nie było cichą pułapką, stan musi być **obserwowalny**: `smartfs-cli status` na pliku pokazuje, czy jego bieżąca wersja leży na blobie prywatnym czy współdzielonym. Użytkownik przekonany, że ma gwarancję, której nie ma, jest gorszy niż użytkownik bez gwarancji.

### #3 — Kasowanie bloba prywatnego jest synchroniczne w `unlink()`

Rozważałem przerzucenie tego na drenaż, dla symetrii z zapisem. Odrzucone, bo argument nie wytrzymuje pomiaru: `unlink()` **już dziś** robi synchroniczne `DELETE FROM inode_registry`, czyli commit Postgresa — te same 27 ms. Dołożenie jednego `unlink(2)` na ext4 (dziesiątki mikrosekund, bez fsynca) jest przy tym szumem.

Za synchronicznością przemawia też to, że gwarancja staje się **dosłowna** zamiast „prawdziwa po chwili". Cała wartość punktu 3 polega na przewidywalności; odroczenie o milisekundy nic nie kosztuje, ale wymaga tłumaczenia, kiedy dokładnie miejsce wraca.

Blob współdzielony pozostaje nietknięty przy `unlink` — jego zwolnienie wymaga dowodu, że nikt go nie referuje, a to jest zadanie sprzątacza.

### #4 — GC powstaje PRZED leniwym dedupem, jako warunek konieczny

GC-by-scan nie istnieje w kodzie w ogóle, a **bloby przeciekają już dziś**, niezależnie od czegokolwiek w tym ADR-ze: skasowanie inode'a kasuje kaskadowo jego `file_versions`, a pliki blobów zostają na dysku na zawsze. Podobnie kompensacja z FIX-04 kasuje wiersz, nie plik.

Leniwy dedup bez GC byłby wyciekiem z dodatkowymi krokami. Odwrotnie — GC jest użyteczny natychmiast i sam z siebie, jeszcze zanim cokolwiek stanie się leniwe.

## Fazy

Rozstrzygnięcia wyżej nie znaczą, że wszystko wchodzi naraz. Kolejność wynika z #4 i z tego, które kroki dają się zweryfikować dostępnymi dziś narzędziami.

| faza | zawartość | ryzyko |
|---|---|---|
| **A** | migracja: klucz `blobs` na `blob_id`, `shared`, indeks częściowy, `inode_registry.dedup_enabled` | niskie, addytywne |
| **B** | GC-by-scan + sprzątacz w fazie snu; `smartfs-cli` pokazuje prywatny/współdzielony | niskie, sam zysk |
| **C** | `insert_blob` przenoszony do drenażu (leniwość) — **to zdejmuje 43 ms** | **wysokie: zmienia semantykę crashową, unieważnia FIX-03/FIX-04 i K1–K5** |

Faza C jest świadomie ostatnia. Odwraca kolejność `insert_blob` → `store.put`, czyli dokładnie to, na czym stoją FIX-03 i FIX-04, i mierzą to punkty K1–K5 etapu 4 — który **dziś nie kończy jeszcze czystego przebiegu**. Wdrażanie zmiany semantyki crashowej, gdy jedyny instrument zdolny ją sprawdzić sam nie działa, byłoby zgadywaniem. Faza C czeka na zielony etap 4 i na przepisany §5.2 planu.

## Wynik pomiaru fazy C

Zmierzone 2026-09-14 na commicie `9b45baf`, izolowanym etapem 5 (bez pjdfstest i crash-testu przed nim), obie strony z nieskażonego logu.

**Ścieżka zapisu, mediana z 200 zapisów:**

| faza | przed C | po C |
|---|---:|---:|
| dedup (`insert_blob`) | **43,34** | — |
| blob (kompresja + `store.put`) | 25,81 | — |
| `compress` | — | 0,07 |
| `store.put` | — | 1,48 |
| znacznik | 1,98 | 1,37 |
| lookup inode'a | 0,70 | 0,54 |
| hash | 0,04 | 0,03 |
| **razem** | **74,45 ms** | **3,77 ms** |

**Opóźnienie `close()`, ta sama metodologia:**

| | p50 | p99 |
|---|---:|---:|
| `b60a58a`, przed C (dwa przebiegi) | 57,40 i 77,56 ms | 195–213 ms |
| `9b45baf`, po C | **8,24 ms** | **19,05 ms** |

Ścieżka zapisu skróciła się **dwudziestokrotnie**, a `close()` siedmio- do dziewięciokrotnie przy rozrzucie przebieg-do-przebiegu rzędu 30%. Ani jednego commitu do Postgresa nie ma już na tej ścieżce — zostało jedno tanie zapytanie odczytowe (`lookup`, 0,54 ms).

**Koszt odroczenia, rozdzielony i potwierdzony:**

| | MB/s |
|---|---:|
| pierwszy zapis treści | 31,6 |
| zapis duplikatu | 13,8 |

Duplikat jest ~2,3× wolniejszy, bo pisarz nie wie jeszcze, że treść istnieje: kompresuje i zapisuje pełną kopię, którą drenaż zaraz kasuje. **To jest dokładnie kompromis z punktu 5 Decyzji, zmierzony zamiast obiecanego** — i powód, dla którego `dedup_enabled` jest przełącznikiem, a nie decyzją globalną. Dla obciążenia z rzadkimi duplikatami wygrywa się dwudziestokrotnie na opóźnieniu; dla backupów traci się dwukrotnie na przepustowości.

Pierwotny pomiar fazy C był **skażony** i został wycofany: `start_daemon` dopisuje do `smartfsd.log`, a etap 5 agregował cały plik, mieszając mediany z kilku commitów. Zdradziło to `blob = 7,93 ms` dla odcinka między dwoma kolejnymi `Instant::now()`. Naprawione w `9b45baf`; `close()` nigdy nie było skażone, bo mierzy je sam skrypt.

## Odniesienia

- Pomiar faz i izolacja przyczyny: [PLAN §5c](../plans/PLAN-posix-parity-and-storage-policy.md)
- FIX-01 (dedup bez refcountów), FIX-03, FIX-04 — [SmartFS_v4.5_to_v5.0_fixes.md](../base-v4.5-v5.0/SmartFS_v4.5_to_v5.0_fixes.md)
