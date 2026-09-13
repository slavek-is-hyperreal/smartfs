# Plan — dojście do parytetu POSIX i polityka składowania z wtyczek

← [Mapa ADR](../01-architecture.md) | Wyrocznia: [The Great SmartFS Test](../testing/the-great-smartfs-test.md) | Poprzedza: ADR-59 (do napisania)

**Status:** przyjęty do wykonania, 2026-09-13. Ten plik jest planem prac, nie wyrocznią — o tym, czy coś przeszło, rozstrzyga wyłącznie kod wyjścia skryptu z `scripts/testing/` i jego artefakty w `test-results/`.

---

## 1. Gdzie jesteśmy naprawdę

Pomiar z `b632a9f`, przebieg 2026-09-13 15:48–16:01:

| etap | wynik |
|---|---|
| 0 preflight | PASS |
| 1 smoke + sonda B-04 | PASS (26 sprawdzeń) |
| 2 pjdfstest | **FAIL** — 5070 ok / 3722 not ok |
| 3 scaffold xfstests | PASS (sam `generic/` **nigdy nie biegł**) |
| 4 spójność po craśhu | **FAIL** — przerwany w połowie |

Baseline ext4 na tej samej maszynie: **8799 ok / 28 not ok**. Czyli 58% wobec 99,7%.

Rozkład porażek jest znacznie lepszą wiadomością niż sama liczba:

| wywołanie | not ok | ok | udział w porażkach |
|---|---:|---:|---:|
| `rename` | 1967 | 2906 | 53% |
| `chown` | 847 | 658 | 23% |
| `link` | 206 | 163 | 6% |
| `unlink` | 204 | 248 | 5% |
| `chmod` | 128 | 209 | 3% |
| `mknod` | 121 | 69 | 3% |
| `open` | 84 | 273 | 2% |
| `mkfifo` | 69 | 57 | 2% |
| pozostałe | ~96 | 416 | 3% |

**`rename` + `chown` to 76% wszystkich porażek.** To nie jest 838 osobnych problemów, tylko dwa podsystemy.

## 2. Meta, która da się przekroczyć

Nie „poziom ext4". 99,7% jest nieosiągalne i gonienie tej liczby popycha w złą stronę: hardlinki i FIFO to udokumentowane `ENOTSUP`, a `chown` pod `MountOption::DefaultPermissions` ma ograniczenia wpisane w FUSE, nie w SmartFS.

Metą jest to, czego §3 planu testowego i tak wymaga:

> **Każda pozostała porażka zaklasyfikowana jako realny-bug / LIMITATION / FUSE, z jednolinijkowym uzasadnieniem w `pjdfstest-expected-failures.txt`, i zero porażek nieuzasadnionych.**

Wpis bez uzasadnienia oblewa etap 2 celowo. Plik jest dziś pusty (sam nagłówek), więc wszystkie 838 są w tej chwili niezaklasyfikowane.

## 2b. Postęp (aktualizowane w miarę pomiarów)

| pozycja | stan |
|---|---|
| [ADR-59](../adr/ADR-59-posix-special-file-types.md) — typy specjalne POSIX | **zaimplementowany** 2026-09-13, migracja 007; predykcja: ~3000 porażek mniej |
| [ADR-60](../adr/ADR-60-plugin-architecture-rust-spirv.md) — architektura wtyczek | **proponowany**, trzy Otwarte pytania blokują kod |
| klasa „`lstat` zwraca inode zamiast ENOENT" (360) | zdiagnozowana częściowo — opóźniony unlink z ADR-16 **nie** tłumaczy tych przypadków, bo dla zamkniętego pliku wiersz znika natychmiast. Do zbadania na świeżym TAP-ie |
| `expected-failures.txt` | nadal pusty; klasyfikacja **po** pomiarze ADR-59, nie przed |
| pierwszy przebieg `generic/` | nie wykonany |

## 3. Kolejność prac

> **Skorygowane 2026-09-13 po analizie przyczyn.** Pierwsza wersja tej listy zaczynała się od „`rename` — 1967 porażek, największy pojedynczy zysk". To było grupowanie po **katalogu testowym**, nie po **przyczynie**, i było mylące: z tych 1967 tylko 30 to `rename` faktycznie zwracające zły wynik, reszta to kaskada `ENOENT` po nieudanym utworzeniu pliku nieobsługiwanego typu. To samo dotyczyło `chown`. Uzasadnienie i liczby: [ADR-59 §Kontekst](../adr/ADR-59-posix-special-file-types.md).

1. **Typy plików POSIX** — [ADR-59](../adr/ADR-59-posix-special-file-types.md), zrobione. ~81% porażek wywodzi się z ich braku.
2. **Klasa „plik nie zniknął"** (360× `lstat` zwraca inode tam, gdzie test oczekuje `ENOENT`) — pierwszy realny błąd, który po ADR-59 przestaje być przykryty szumem.
3. **Klasyfikacja reszty** — hardlinki (`link`) to `LIMITATION` z uzasadnieniem w [ADR-59 punkt 8](../adr/ADR-59-posix-special-file-types.md), nie do naprawy: licznik dowiązań przy CoW dotyka Root Invariant #2 i #3, a to ~53 porażki.
4. **Pierwszy przebieg `generic/`** — kilka godzin, nigdy nie wykonany. Do tego czasu nie wiemy, czego nie wiemy.
5. **Baseline wydajności** — etap 5, patrz §5.
6. **ADR-59** — polityka składowania z wtyczek, §4.

Pipeline multimedialny z [IDEA-multimedia-vram-vulkan-pipeline](../ideas/IDEA-multimedia-vram-vulkan-pipeline.md) jest świadomie **poza tym planem**. Jest to trzeci projekt obok systemu plików i warstwy semantycznej, a warstwa POSIX jest fundamentem, na którym stoją pozostałe dwa.

## 4. Dwa tryby zapisu sterowane wtyczką (materiał na ADR-59)

**Obserwacja:** kompresja ogólnego przeznaczenia na już skompresowanym strumieniu to spalony CPU za ułamek procenta. H.264, HEVC, JPEG, ZIP, większość formatów multimedialnych — zstd na nich nie zarabia.

**Haczyk już istnieje:** `inode_registry.compression_level` jest kolumną **per-inode** (jest w `InodeRecord`), nie per-wersja. Nikt jej dotąd nie wykorzystał jako punktu decyzyjnego.

**Propozycja:** wtyczka typu pliku (`plugins/*.json`, ten sam mechanizm, który ADR-57 udostępnia przez `describe_plugin_type`) zyskuje sekcję polityki składowania, nie tylko metadanych:

```jsonc
{
  "plugin_type": "video",
  "extensions": [".mov", ".mp4", ".mxf"],
  "storage": {
    "compression_level": 0,          // już skompresowane — nie kompresuj ponownie
    "rationale": "kontener niesie strumień skompresowany stratnie; zstd zwraca <1%"
  }
}
```

Rozstrzygnięcia potrzebne przed implementacją:

- **Kto i kiedy ustawia poziom.** `compression_level` jest per-inode, więc wtyczka ustawia go **przy tworzeniu pliku**, nie przy zapisie wersji. Co się dzieje, gdy plik zmieni typ przez zmianę nazwy — poziom zostaje historyczny czy się aktualizuje? (Zostawić historyczny: bloby są niezmienne, a zmiana poziomu nie może przepisać wstecz istniejących wersji.)
- **Czy `compression_level = 0` znaczy „bez kompresji", czy „zstd poziom 0".** Dziś to drugie. Potrzebny jawny wariant „stored", inaczej nie da się wyrazić intencji.
- **Jak to wchodzi w scrub.** `scrub_once` dostaje kodek jako domknięcie, więc bloby nieskompresowane muszą być rozpoznawalne — inaczej dekoder zgłosi `Undecodable` dla poprawnego bloba.

**Zysk uboczny:** nieskompresowane bloby to warunek konieczny ścieżki Direct-to-VRAM z IDEA. DMA skompresowanego zstd bloba do VRAM wymagałoby dekompresji zstd na GPU — dokładnie dlatego DirectStorage wozi ze sobą GDeflate. Ta zmiana odblokowuje tamtą, nie przesądzając o niej.

## 5. Wydajność — diagnostyka, nigdy bramka

`scripts/testing/05_perf_diagnostics.sh` zapisuje liczby i **nie oblewa na wolnym wyniku**. Powód: próg wydajnościowy wewnątrz zestawu poprawnościowego tworzy presję, żeby handlować poprawność za czas, a na zajętej maszynie mierzy głównie to, co jeszcze na niej działa. Oblewa wyłącznie na niemożności zmierzenia — brak mountu to nadal FAIL, zgodnie z regułą Fail-Loud.

Mierzy: opóźnienie `close()` (p50/p95/p99), czas drenażu kolejki, przepustowość zapisu dla danych ściśliwych i nieściśliwych osobno, przepustowość odczytu, tempo operacji metadanych.

**Luka, którą to zamyka:** ADR-58 był zmianą wydajnościową — przeniósł transakcję Postgresową poza ścieżkę `release()` — i **nikt nigdy nie zmierzył, czy się zwrócił.** Opóźnienie `close()` jest dokładnie tą wielkością, którą ruszył. Jest realna możliwość, że pomiar pokaże, iż nie zarobił na swoją złożoność; to też jest wynik.

Porównanie idzie względem `scripts/testing/perf-baseline.json`, odświeżanego **ręcznie i świadomie**, nigdy automatycznie.

## 5b. Co pokazał pierwszy pomiar wydajności

Etap 5 przy pierwszym uruchomieniu znalazł błąd, którego nikt nie szukał: **odczyt szedł 1,16 MB/s**, bo `read()` nigdy nie wypełniał bufora per-fd i każde wywołanie dekompresowało cały plik, żeby zwrócić jeden wycinek. Naprawione (`971aab1`), ale przy okazji odsłoniło dwie rzeczy warte zapisania.

**Pierwsza: `ensure_buffer` trzymał globalny lock przez I/O.** Brał zapis na `handles` i *trzymając go* wołał loader robiący zapytanie do bazy i pobranie bloba — jeden lock na wszystkie otwarte uchwyty, przez czas I/O. Było do przeżycia, dopóki używał go tylko `write()`. Rozdzielone na `needs_buffer()` + `set_buffer_if_absent()`: ładowanie poza lockiem, publikacja pod nim.

**Druga, strukturalna: ADR-58 dołożył fsync do każdego zapisu.** Ścieżka `close()` robi dziś **dwa** `sync_all` — jeden na blobie w `store.put`, drugi na znaczniku w `PendingQueue::enqueue`. Przedtem był jeden. ADR-58 usunął ze ścieżki zapisu transakcję Postgresową i wstawił w to miejsce trwały zapis na dysk. Czy to się opłaciło, jest dokładnie tym pytaniem, dla którego etap 5 powstał.

**Ostrzeżenie metodologiczne, wpisane tu celowo:** pierwsze porównania między przebiegami były bezwartościowe, bo etap 5 biegł po pjdfstest i po 25 rundach crash-testu młócących ten sam dysk. Izolacja (`echo "0 5" > _control/run`) odzyskała większość metryk. **Zanim jakakolwiek różnica zostanie czemukolwiek przypisana, trzeba znać rozrzut przebieg-do-przebiegu przy tym samym commicie.** Porównywanie pojedynczych pomiarów bez tego to zgadywanie z liczbami dla ozdoby.


## 6. Pętla „napraw → ponów" bez nadzoru

Cel: agent iteruje samodzielnie, aż wszystkie porażki są zaklasyfikowane.

**Ryzyko, które to tworzy, jest realne i trzeba je nazwać:** agent iterujący przeciw wskaźnikowi zdawalności znajdzie tanią drogę. Nie ze złej woli — dlatego, że rozszerzenie `expected-failures.txt` jest zawsze tańsze niż naprawa `rename()`. To nie jest hipoteza: w tym zestawie znaleziono już przyrząd, który **nie potrafił się zapalić na czerwono** (sonda B-04 liczyła kopertę JSON-RPC zamiast trafień, więc gałąź „CONFIRMED" była nieosiągalna).

Dlatego pętla ma blokady mechaniczne, nie deklaratywne:

| blokada | mechanizm |
|---|---|
| harness nie zmienia się po cichu | `HARNESS.sha256` liczony i porównywany przy każdym przebiegu, wpisywany do manifestu; rozbieżność krzyczy i pokazuje `git diff` |
| każda iteracja rozliczalna | osobny commit z liczbą przed/po w treści |
| środowisko pod kontrolą | baseline ext4 przebiega co iterację; jego zmiana znaczy „maszyna", nie „SmartFS" |
| brak cichego regresu | twarde zatrzymanie, gdy zdawalność spada albo etap, który przechodził, przestaje |
| pętla się kończy | limit iteracji, raport niezależnie od wyniku |

### Kanał sterowania — root zostaje przy człowieku

Właściciel uruchamia zestaw **raz**, z sudo:

```
sudo scripts/run-great-smartfs-test.sh --watch
```

Po każdym przebiegu skrypt nie kończy się — zostaje rezydentny i czeka na plik:

| plik | znaczenie |
|---|---|
| `test-results/_control/run` | ponów cały przebieg |
| `test-results/_control/stop` | zwolnij roota i zakończ |
| `test-results/_control/STATUS` | co robi w tej chwili (czyta się bez uprawnień) |
| `test-results/_control/LAST-RUN.txt` | manifest ostatniej iteracji |

Katalog należy do wywołującego, więc agent bez uprawnień może iterować: edytuje kod, robi `touch .../run`, czyta artefakty. **Nigdy nie trzyma roota.** Idle-budżet (domyślnie 12 h) zwalnia roota sam, jeśli nikt nie wróci.

Interfejs to świadomie dwa puste pliki i nic więcej. Plik sterujący, którego *treść* byłaby wykonywana, to root shell przebrany za automatyzację; „ponów zacommitowany zestaw" i „zatrzymaj się" są audytowalne, a jedyne, co zmienia się między iteracjami, to kod w gicie.

Każda iteracja przebudowuje workspace jako wywołujący (nie root), więc poprawki wchodzą do przebiegu automatycznie i `target/` nie zmienia właściciela.

### Nic nie pisze na partycję systemową

Etap 4 zabija demona z założenia, a writer, który przeżyje unmount, otwiera ścieżkę na nowo i kładzie **prawdziwy** plik w katalogu pod spodem. Dlatego domyślne `CRASH_MOUNT`, `TEST_MNT` i `SCRATCH_MNT_DIR` przeniesiono z `/mnt/*` na partycję testową, a wrapper **asertuje** — nie zakłada — że blob store, mountpointy, tmp, logi i wyniki nie rozwiązują się na urządzenie roota. Ta awaria jest cicha, dopóki `/` się nie zapełni, więc musi być sprawdzana, a nie pilnowana dyscypliną.

Zasada nadrzędna, z §7.3 planu testowego: **jeśli skrypt został zmieniony, żeby etap przeszedł, ta zmiana sama jest znaleziskiem i musi zostać zgłoszona przed wynikiem.** Dotyczy to również zmian wprowadzonych w trakcie pętli.

## 7. Znaleziska otwarte

- **Etap 4, Invariant #3 — nierozstrzygający.** 82 z 83 porażek to jedna klasa: po `kill -9` plik jest widoczny z treścią, która nie ma jeszcze wiersza `file_versions`, bo jego znacznik czeka w `pending/queue/`, a nakładka odczytu z ADR-58 go serwuje. §5.3 planu został pod to zrewidowany (najpierw quiesce), ale `04_crash_consistency_test.sh` nigdy nie dostał tej samej poprawki. Sprawdzian nie odróżnia dziś „zakolejkowane, zaraz się zacommituje" (legalne) od „widoczne, a nigdzie nie zapisane" (realne naruszenie). Do naprawy w skrypcie, ze zgłoszeniem.
- **Stage 0b nie istnieje.** `--crash-points` zwraca pustkę, więc K1–K11 są luką pokrycia, a etap 4 biegnie wyłącznie stochastycznie. Świadoma decyzja, nie awaria — ale historia crashowa pozostaje nieudowodniona.
- **`generic/` nigdy nie uruchomiony.** Największa niewiadoma w projekcie.
