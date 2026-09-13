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

## 3. Kolejność prac

1. **`rename`** — 1967 porażek. Największy pojedynczy zysk w całym projekcie.
2. **`chown`** — 847. Rozstrzygnąć, ile z tego jest FUSE-inherent pod `DefaultPermissions`, zanim zacznie się kodować.
3. **Klasyfikacja reszty** — `link` i `mkfifo` (275) to prawdopodobnie legalne `LIMITATION` do wpisania z uzasadnieniem, nie do naprawy.
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

Zasada nadrzędna, z §7.3 planu testowego: **jeśli skrypt został zmieniony, żeby etap przeszedł, ta zmiana sama jest znaleziskiem i musi zostać zgłoszona przed wynikiem.** Dotyczy to również zmian wprowadzonych w trakcie pętli.

## 7. Znaleziska otwarte

- **Etap 4, Invariant #3 — nierozstrzygający.** 82 z 83 porażek to jedna klasa: po `kill -9` plik jest widoczny z treścią, która nie ma jeszcze wiersza `file_versions`, bo jego znacznik czeka w `pending/queue/`, a nakładka odczytu z ADR-58 go serwuje. §5.3 planu został pod to zrewidowany (najpierw quiesce), ale `04_crash_consistency_test.sh` nigdy nie dostał tej samej poprawki. Sprawdzian nie odróżnia dziś „zakolejkowane, zaraz się zacommituje" (legalne) od „widoczne, a nigdzie nie zapisane" (realne naruszenie). Do naprawy w skrypcie, ze zgłoszeniem.
- **Stage 0b nie istnieje.** `--crash-points` zwraca pustkę, więc K1–K11 są luką pokrycia, a etap 4 biegnie wyłącznie stochastycznie. Świadoma decyzja, nie awaria — ale historia crashowa pozostaje nieudowodniona.
- **`generic/` nigdy nie uruchomiony.** Największa niewiadoma w projekcie.
