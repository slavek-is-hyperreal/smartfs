# ADR-59 — Typy specjalne POSIX: FIFO, gniazda i węzły urządzeń

← [Mapa ADR](../01-architecture.md) | Mierzone przez: [The Great SmartFS Test](../testing/the-great-smartfs-test.md) §3 | Plan: [PLAN-posix-parity](../plans/PLAN-posix-parity-and-storage-policy.md) | Dotyka: [smartfs-fuse](../crates/smartfs-fuse.md), migracje

**Status:** Przyjęty, 2026-09-13. Wynika wprost z pomiaru, nie z przypuszczenia — patrz §Kontekst.

---

## Kontekst — liczba, która wywróciła priorytety

Pierwszy pełny przebieg pjdfstest przeciw żywemu SmartFS (commit `a7c7e59`) dał **5070 ok / 3722 not ok**, przy baseline ext4 **8799 ok / 28 not ok** na tej samej maszynie. Pierwsza, naiwna interpretacja grupowała porażki po katalogu testowym i wskazywała `rename` (1967) jako największy problem.

Grupowanie po **przyczynie** pokazuje coś innego:

| klasa | ile | udział |
|---|---:|---:|
| `EOPNOTSUPP` — nieobsługiwany typ pliku | 706 | 19% |
| `ENOENT` — kaskada po powyższym | 2310 | 62% |
| `lstat` zwraca inode, choć plik ma nie istnieć | 360 | 10% |
| szum harnessu (`# TODO` z pjdfstest, puste linie) | 151 | 4% |
| pozostałe realne (`EISDIR`, `EEXIST`, `EFBIG`…) | 195 | 5% |

Rozbicie `EOPNOTSUPP`: `mknod` 341, `mkfifo` 171, `bind` (gniazdo uniksowe) 158, `link` 13.

Mechanizm kaskady widać wprost w TAP-ie:

```
not ok 22 - tried 'mkfifo …', expected 0,    got EOPNOTSUPP
not ok 23 - tried 'chmod  …', expected 0,    got ENOENT
not ok 24 - tried 'stat   …', expected 0111, got ENOENT
```

Zestawy `rename/09.t` i `10.t` (4452 testy, 1772 porażki) iterują po **każdym typie pliku** — zwykły, katalog, FIFO, urządzenie blokowe, znakowe, gniazdo, symlink. Dla każdego: utwórz → `chown` → dopiero potem sprawdź semantykę sticky bitu. Nieudane utworzenie generuje kilka „not ok" i **ani jednej informacji o rename**. Z 1967 porażek w `rename` tylko **30** to `rename` faktycznie zwracające zły wynik.

**Wniosek: 3016 z 3722 porażek (81%) wywodzi się z czterech brakujących typów plików.** To jest największy pojedynczy zysk dostępny w projekcie i jest tani, bo — patrz §Decyzja punkt 4 — te typy nie mają ścieżki danych.

## Decyzja

1. **SmartFS obsługuje pełny zestaw typów POSIX**: `S_IFREG`, `S_IFDIR`, `S_IFLNK` (już są) oraz `S_IFIFO`, `S_IFSOCK`, `S_IFCHR`, `S_IFBLK` (nowe). `fuser::FileType` ma wszystkie siedem wariantów, więc warstwa FUSE niczego nie ogranicza.

2. **Typ mieszka w `mode`, nie w nowej kolumnie enum.** Kolumna `inode_registry.mode INT` **już dziś** trzyma pełny mode z bitami typu — root inode jest zasiany jako `16877` = `0o40755`. Błędem było maskowanie w kodzie: `inode_create` i `setattr` zapisują `mode & 0o7777`, gubiąc `S_IFMT`. Naprawiamy maskowanie, nie schemat.

   Odrzucono osobną kolumnę `file_type`: dawałaby dwa źródła prawdy o tym samym fakcie, a POSIX i tak definiuje typ jako bity w `mode`. Rozjazd między nimi byłby nowym, cichym błędem.

3. **`is_dir` zostaje** jako utrzymywana denormalizacja. Używa jej `CONSTRAINT blob_or_empty_or_virtual`, `inode_list_children` i kilka zapytań. Wyprowadzana z `mode` przy tworzeniu i nigdy niezależnie modyfikowana.

4. **Typy specjalne nie mają ścieżki danych — i to jest cała oszczędność.** FIFO, gniazdo i węzeł urządzenia to *wyłącznie* wpis inode. Semantykę implementuje jądro: potok dla FIFO, gniazdo dla `S_IFSOCK`, sterownik spod `rdev` dla urządzeń. SmartFS musi jedynie **zapamiętać typ i `rdev`** i podać je w `getattr`. Zero blobów, zero wersji, zero konsolidacji semantycznej, zero wpływu na `cow_commit` i na ADR-58.

   Stąd `size = 0` i `current_blob_id IS NULL` dla tych inode'ów — co spełnia istniejący `CONSTRAINT blob_or_empty_or_virtual` bez żadnej zmiany.

5. **Nowa kolumna `rdev BIGINT NOT NULL DEFAULT 0`** — jedyna zmiana schematu. Wymagana przez `S_IFCHR`/`S_IFBLK`; `BIGINT`, bo `dev_t` w Linuksie to 64 bity, a `makedev(major, minor)` przy dużym minorze nie mieści się w `INT`. Migracja jawnym plikiem, `007_special_file_types.sql`, nigdy DDL w runtime (Root Invariant #4).

6. **`chmod` nigdy nie zmienia typu.** `setattr` liczy `new_mode = (old_mode & S_IFMT) | (żądane & 0o7777)`. Dziś maskuje żądanie i zapisuje sam permset, co po tej zmianie zamieniłoby plik w „typ 0".

7. **Wiersze sprzed tej zmiany mają zamaskowany `mode`** (bez bitów typu). `inode_to_file_attr` przy `mode & S_IFMT == 0` wraca do `is_dir` jako źródła prawdy. Bez tego każdy istniejący plik stałby się nagle nieznanego typu. Wsteczna zgodność jest tu obowiązkowa, nie uprzejmościowa — migracja nie może przepisać historii, bo `file_versions` odwołuje się do tych inode'ów.

8. **Hardlinki (`link`) pozostają nieobsługiwane** i są udokumentowanym `LIMITATION`, nie długiem. To 13 `EOPNOTSUPP` + ~40 kaskady = ~53 porażki, a koszt jest nieproporcjonalny: licznik dowiązań przy Copy-on-Write oznacza, że jedna nazwa nie jest już właścicielem historii wersji, co dotyka Root Invariant #2 i #3. Osobna decyzja, jeśli kiedyś będzie potrzebna.

## Odrzucone alternatywy

**Osobna kolumna `file_type` (enum albo `SMALLINT`).** Czytelniejsza w `SELECT`, ale tworzy drugie źródło prawdy obok `mode`, którego POSIX i tak używa. Każde miejsce zapisujące jedno bez drugiego to cichy rozjazd.

**Zostawić jako `LIMITATION` i wpisać do `expected-failures.txt`.** Najtańsze dziś. Odrzucone, bo zostawia 81% porażek pjdfstest na stałe i czyni metę „wszystko zaklasyfikowane" spełnioną z gigantyczną gwiazdką — a przy okazji ukrywa te 360 realnych porażek klasy „plik nie zniknął" pod szumem, w którym nie sposób ich zobaczyć.

**Emulować FIFO/gniazda w przestrzeni użytkownika w demonie.** Nikt tego nie chce: jądro robi to poprawnie i szybciej, a emulacja oznaczałaby implementowanie semantyki potoków w warstwie, która nie ma po temu żadnej przewagi.

## Konsekwencje

- Migracja `007_special_file_types.sql` — jedna kolumna, `ADD COLUMN IF NOT EXISTS`, bezpieczna do ponownego uruchomienia (etapy 3 i 4 stosują migracje na świeżych bazach przy każdym `_scratch_mkfs`).
- `InodeRecord` zyskuje `rdev: i64`; `inode_create` zyskuje parametry `mode` (pełny) i `rdev`.
- `inode_to_file_attr` wyprowadza `FileType` z `mode & S_IFMT`, z fallbackiem z punktu 7, i podaje `rdev`.
- `mknod` przestaje odrzucać typy inne niż `S_IFREG`.
- `smartfs-fuse` przestaje maskować bity typu w `inode_create` i `setattr`.
- **Oczekiwany ruch w pomiarze:** ~3000 porażek pjdfstest. Liczba jest predykcją zapisaną **przed** uruchomieniem — jeśli przebieg pokaże istotnie mniej, ta analiza jest błędna i trzeba ją poprawić, a nie naciągnąć wynik.
- Po tej zmianie klasa „`lstat` zwraca inode, choć plik ma nie istnieć" (360) przestaje być przykryta szumem i staje się następnym priorytetem.

## Standard odniesienia

`mknod(2)` definiuje `mode` jako sumę typu (`S_IFREG`, `S_IFCHR`, `S_IFBLK`, `S_IFIFO`, `S_IFSOCK`) i uprawnień, przy czym `dev` ma znaczenie wyłącznie dla `S_IFCHR` i `S_IFBLK`, a dla pozostałych jest ignorowany — dokładnie tak, jak zapisano w punktach 4 i 5. Interfejs FUSE odwzorowuje to jeden do jednego: `create` jest wołane tylko dla `S_IFREG`, a wszystkie pozostałe typy idą przez `mknod` z `rdev`.

- [mknod(2), Linux manual page](https://man7.org/linux/man-pages/man2/mknod.2.html)
- [pjdfstest](https://github.com/pjd/pjdfstest) — zestaw, z którego pochodzą liczby w §Kontekst
