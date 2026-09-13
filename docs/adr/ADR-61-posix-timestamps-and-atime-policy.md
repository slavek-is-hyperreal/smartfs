# ADR-61 — Znaczniki czasu POSIX i polityka `atime`

← [Mapa ADR](../01-architecture.md) | Mierzone przez: [The Great SmartFS Test](../testing/the-great-smartfs-test.md) §3 | Plan: [PLAN-posix-parity](../plans/PLAN-posix-parity-and-storage-policy.md) | Poprzednik: [ADR-59](ADR-59-posix-special-file-types.md)

**Status:** Przyjęty, 2026-09-13. Jedna decyzja jest nieoczywista i to ona jest powodem, dla którego to jest ADR, a nie zwykła poprawka — patrz §Decyzja punkt 3.

---

## Kontekst

`utimensat` daje 29 porażek pjdfstest po ADR-59. Przyczyna nie jest „nieobsługiwane", tylko coś gorszego: **cichy no-op**.

`SmartFsFuse::setattr` przyjmuje `_atime`, `_mtime`, `_ctime` i `_crtime` — wszystkie z podkreśleniem, wszystkie ignorowane. Wywołanie kończy się sukcesem, bo pozostałe pola (`mode`, `uid`, `gid`, `size`) przechodzą normalnie. Wołający dostaje `0`, czasy się nie zmieniają, a POSIX mówi, że miały.

Pod spodem nie ma gdzie ich zapisać. `inode_registry` ma `created_at` i `updated_at`, a `inode_to_file_attr` mapuje `updated_at` **jednocześnie** na `atime`, `mtime` i `ctime`. Trzy różne fakty POSIX-owe zwinięte w jedną kolumnę, która i tak znaczy „kiedy ostatnio cokolwiek ruszyło ten wiersz".

To nie jest luka kosmetyczna: `make`, `rsync`, `tar` i każdy system budowania podejmują decyzje na podstawie `mtime`. Filesystem, który go nie utrzymuje, cicho psuje narzędzia, które na nim stoją.

## Decyzja

1. **`inode_registry` zyskuje `atime` i `mtime`** jako `TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()`. Migracja `008_posix_timestamps.sql`, jawnym plikiem (Root Invariant #4).

2. **`ctime` pozostaje `updated_at`, a `crtime` pozostaje `created_at`** — bez nowych kolumn. To nie jest oszczędność, tylko zgodność z semantyką: POSIX definiuje `ctime` jako „ostatnia zmiana metadanych inode'a", a `updated_at` jest dokładnie tym, bo każda zmiana wiersza go dotyka. Kolumna `ctime` byłaby duplikatem, który mógłby się rozjechać. `ctime` jest przy tym **niemodyfikowalny z zewnątrz** — POSIX zabrania ustawiania go przez `utimensat`, i to wychodzi tu za darmo.

3. **`atime` działa w trybie `relatime`, nie `strictatime`.** *To jest ta decyzja, dla której ten dokument jest ADR-em.*

   Ścisły `atime` znaczy „każdy odczyt aktualizuje metadane". W zwykłym systemie plików to jest znany problem wydajnościowy — dlatego Linux od 2.6.30 montuje z `relatime` domyślnie. **W SmartFS byłoby to znacznie gorsze niż zwykle**, bo aktualizacja metadanych to `UPDATE` w Postgresie: każdy `cat` zamieniałby się w zapis do bazy, a odczyt sekwencyjny dużego pliku — w strumień zapisów.

   `relatime` aktualizuje `atime` tylko wtedy, gdy poprzedni jest **starszy niż `mtime` lub `ctime`**, albo starszy niż dobę. To wystarcza narzędziom, które w ogóle patrzą na `atime` (`mutt` i wykrywanie „nowej poczty" to kanoniczny przypadek), i sprowadza koszt do zera dla powtarzanych odczytów.

   Odrzucone `noatime`: byłoby najszybsze, ale `atime`, który nigdy się nie zmienia, jest kłamstwem wobec `stat`, a nie optymalizacją. `relatime` jest kompromisem, który reszta świata już przyjęła.

4. **`utimensat` z `UTIME_NOW`/`UTIME_OMIT` obsługiwane wprost.** `fuser` podaje je jako `TimeOrNow::Now` i `None` — `None` znaczy „nie ruszaj", `Now` znaczy „ustaw na teraz".

5. **Zapis treści aktualizuje `mtime` i `ctime`, nigdy `atime`.** Odczyt aktualizuje `atime` wyłącznie zgodnie z regułą `relatime` z punktu 3. Zmiana metadanych (`chmod`, `chown`) aktualizuje `ctime`, nigdy `mtime` — to jest rozróżnienie, którego dziś w ogóle nie ma, bo obie rzeczy dotykają `updated_at`.

6. **Aktualizacja `atime` nigdy nie blokuje odczytu.** Ta sama dyscyplina co Root Invariant #6 i ADR-58: gdy `relatime` orzeknie, że zapis jest potrzebny, idzie on obok ścieżki odpowiedzi, a nie przed nią. Odczyt, który czeka na `UPDATE`, jest gorszy niż `atime` spóźniony o milisekundy.

## Odrzucone alternatywy

**Cztery osobne kolumny (`atime`, `mtime`, `ctime`, `crtime`).** Symetryczne i czytelne. Odrzucone, bo `ctime` i `crtime` mają już wierne odpowiedniki w `updated_at` i `created_at`; dublowanie ich tworzy pary, które mogą się rozjechać, i nie kupuje niczego.

**`strictatime`.** Wierne POSIX-owi w literze. Odrzucone z powodu z punktu 3: w systemie plików, którego metadane leżą w Postgresie, zamienia każdy odczyt w zapis do bazy.

**`noatime`.** Najszybsze i najprostsze. Odrzucone, bo `stat` zaczyna wtedy zwracać wartość, która nigdy nie była prawdziwa. Jeśli kiedykolwiek okaże się, że `relatime` realnie kosztuje, `noatime` powinien być opcją montowania — świadomym wyborem operatora, nie cichym domyślnym zachowaniem.

**Zostawić jako `LIMITATION`.** 29 porażek to niewiele. Odrzucone, bo cichy no-op jest gorszy od jawnego błędu, a `mtime` to nie jest ozdoba — stoją na nim `make`, `rsync` i `tar`.

## Konsekwencje

- Migracja `008_posix_timestamps.sql`: dwie kolumny, `ADD COLUMN IF NOT EXISTS`, idempotentna.
- `InodeRecord` zyskuje `atime` i `mtime`; `inode_to_file_attr` przestaje podawać `updated_at` w trzech polach naraz.
- `smartfs-db` zyskuje funkcję ustawiającą czasy; `setattr` przestaje ignorować swoje argumenty.
- Wiersze sprzed migracji dostają `NOW()` jako wartość domyślną — nie da się odtworzyć czasów, których nikt nie zapisał, a `NOW()` jest jedyną nieszkodliwą odpowiedzią.
- Marker migracji `008` dochodzi do bramki schematu w `smartfsd`, która odmówi startu na bazie bez niej.
- **Predykcja przed uruchomieniem: ~29 porażek `utimensat` znika.** Część porażek `chown` może też odejść, jeśli wynikają z `ctime`, którego dziś nie ma — ale tego nie zakładam z góry i policzę po pomiarze.

## Odniesienia

- `utimensat(2)`, `stat(2)` — semantyka `UTIME_NOW`/`UTIME_OMIT` i definicja `ctime` jako czasu zmiany metadanych inode'a
- `mount(8)`, opcje `strictatime`/`relatime`/`noatime` — `relatime` jest domyślnym zachowaniem Linuksa od 2.6.30 i punkt 3 idzie za tym precedensem
