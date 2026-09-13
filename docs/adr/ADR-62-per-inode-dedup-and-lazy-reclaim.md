# ADR-62 — Dedup przełączany per plik, leniwy, ze sprzątaczem w fazie snu

← [Mapa ADR](../01-architecture.md) | Wynika z pomiaru: [PLAN §5c](../plans/PLAN-posix-parity-and-storage-policy.md) | Poprzednicy: [ADR-58](ADR-58-two-stage-cow-commit.md) (dwuetapowy commit), [ADR-60](ADR-60-plugin-architecture-rust-spirv.md) (polityka składowania z wtyczek), FIX-01 (dedup bez refcountów), FIX-03/FIX-04

**Status:** Proponowany — **cztery Otwarte pytania blokują implementację** (§Otwarte pytania). Kierunek zaproponowany przez właściciela projektu 2026-09-13.

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

### 2. Dedup wyłączony: zero zapytań do bazy na ścieżce zapisu

Bez dedupu nie ma czego serializować, więc `insert_blob` znika ze ścieżki: hash → kompresja → `store.put` → znacznik → potwierdzenie. **43 ms schodzi natychmiast, bez leniwego dedupu i bez GC.**

Blob takiego pliku jest **prywatny**: należy do dokładnie jednej wersji jednego inode'a.

### 3. Dedup wyłączony daje gwarancję szybkiego kasowania — i to jest jego główna wartość

Skoro blob prywatny ma dokładnie jednego właściciela, `unlink` może zwolnić miejsce **od razu**, bez dowodzenia, że nikt inny go nie referuje. Przy dedupie ten dowód wymaga skanu (bo FIX-01 zniósł refcounty), więc zwolnienie miejsca jest z natury odroczone.

To jest własność widoczna dla użytkownika, nie mikrooptymalizacja: „skasowałem, miejsce wróciło" ma znaczenie dla kwot, dla przewidywalności i dla wymogów usuwania danych.

### 4. Reguła własności, bez której gwarancja z punktu 3 jest fikcją

**Blob prywatny nigdy nie uczestniczy w dedupie — ani jako cel, ani jako źródło.**

Bez tej reguły sprzątacz mógłby wskazać plik z dedupem włączonym na bloba pliku z dedupem wyłączonym, i skasowanie tego drugiego przestałoby zwalniać miejsce. Gwarancja wyparowałaby po cichu, w momencie niezwiązanym z żadną decyzją użytkownika.

**Konsekwencja schematowa:** `blobs` przestaje być inwentarzem wszystkich blobów i staje się tym, czym w istocie jest — **indeksem dedupu**. Wersja prywatna ma `blob_id` wskazujący plik i **nie ma wiersza w `blobs`**. Inaczej się nie da: `content_hash` jest tam kluczem głównym, więc prywatny i współdzielony blob o identycznej treści nie mogą tam współistnieć.

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
- `blobs` zmienia znaczenie z „inwentarz blobów" na „indeks dedupu" — wymaga to poprawki w dokumentacji schematu, bo dziś czyta się inaczej.
- **Etap 4 planu testowego przestaje weryfikować bloby prywatne.** `check_invariant_1_crypto` robi `JOIN blobs b ON b.content_hash = fv.content_hash`, więc wersja bez wiersza w `blobs` zostanie po cichu pominięta — sprawdzian, który cicho przestaje sprawdzać, jest gorszy niż jego brak. Do rozszerzenia razem z tą zmianą.
- FIX-03 i FIX-04 tracą swoje uzasadnienie na ścieżce leniwej: ich sens polega na tym, że wiersz w `blobs` powstaje **przed** zapisem bajtów, więc crash zostawia wykrywalny „zatruty wiersz". Po odwróceniu crash zostawia osierocony plik bez wiersza — stan niewidoczny przez mount (Invariant #3) i sprzątany przez GC. To wygląda na **uproszczenie** semantyki crashowej, ale K1–K5 mierzą dokładnie ten obszar i muszą zostać przepisane, a nie założone.
- GC-by-scan przestaje być teoretyczny i staje się warunkiem koniecznym.

## Otwarte pytania — zatrzymać się i zapytać

1. **Co jest domyślne dla pliku bez wtyczki?** `dedup_enabled = TRUE` oszczędza miejsce i odracza kasowanie; `FALSE` jest szybsze i przewidywalne. Wybór ustawia charakter systemu i nie powinien wyjść z przypadku.
2. **Czy `dedup_enabled` można przełączyć po utworzeniu pliku, i co wtedy z istniejącymi wersjami?** Włączenie jest łatwe (przyszłe wersje wejdą do indeksu). Wyłączenie jest trudne: istniejący blob może być już współdzielony, więc gwarancja z punktu 3 nie obowiązuje wstecz — chyba że wymusimy kopię, czego cała ta konstrukcja unika.
3. **Czy „szybkie kasowanie" znaczy `unlink()` kasujący plik synchronicznie?** Wtedy I/O wraca na ścieżkę kasowania, tylko z drugiej strony. Alternatywa: kasowanie natychmiastowe, ale wykonane przez drenaż — miejsce wraca w milisekundach, nie w mikrosekundach, za to `unlink()` zostaje szybki.
4. **Czy GC powstaje przed tym, czy razem z tym?** Rekomendacja: **przed**. GC jest potrzebny tak czy owak (dziś nie istnieje, a bloby po każdej edycji już się gromadzą), a leniwy dedup bez niego to wyciek z dodatkowymi krokami.

## Odniesienia

- Pomiar faz i izolacja przyczyny: [PLAN §5c](../plans/PLAN-posix-parity-and-storage-policy.md)
- FIX-01 (dedup bez refcountów), FIX-03, FIX-04 — [SmartFS_v4.5_to_v5.0_fixes.md](../base-v4.5-v5.0/SmartFS_v4.5_to_v5.0_fixes.md)
