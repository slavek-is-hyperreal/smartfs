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


## 5c. Gdzie naprawdę idzie czas zapisu — zmierzone

Rozbicie `close()` na fazy (`590bbb2`), mediana z 200 zapisów:

| faza | ms | udział |
|---|---:|---:|
| **dedup** (`insert_blob`) | **43,3** | **58%** |
| blob (kompresja + `store.put` + fsync) | 25,8 | 35% |
| marker (znacznik ADR-58 + fsync) | 2,0 | 3% |
| lookup inode'a | 0,7 | 1% |
| hash | 0,04 | 0% |
| razem | 74,5 | |

Faza `dedup` to **jeden `INSERT` do `blobs` w autocommicie**. Zmierzony poza SmartFS, gołym `psql` na kopii bazy:

```
50 INSERT-ów, każdy w autocommicie   →  27 ms/szt.
50 INSERT-ów w jednej transakcji     →   1 ms/szt.
50 przy synchronous_commit=off       →   1 ms/szt.
```

Dwadzieścia siedem milisekund to **wyłącznie oczekiwanie na fsync WAL-a**. Dane Postgresa leżą na `vectorlegis_ssd_pool/docker` — ZFS z `sync=standard` i `logbias=latency`, bez wydzielonego SLOG-a — więc każdy commit to synchroniczny zapis do ZIL-a na dyskach poola.

**Wniosek: gorąca ścieżka zapisu SmartFS jest zdominowana przez opóźnienie commitu Postgresa na ZFS, nie przez kod SmartFS.**

### Co to znaczy dla ADR-58

ADR-58 zdjął ze ścieżki potwierdzenia transakcję Postgresową i wstawił w to miejsce fsync znacznika na ext4. Pomiar wycenia tę zamianę: **znacznik kosztuje 2,0 ms, commit Postgresa kosztuje 27–43 ms.** ADR-58 zwrócił się z nawiązką i to jest pierwsza twarda odpowiedź na pytanie, które sam zostawił otwarte.

Ale zwrócił się **tylko w połowie**: przeniósł KROK 2, a `insert_blob` z KROKU 1 nadal jest synchroniczny — i to on jest teraz 58% kosztu. Ta sama logika, która uzasadniła ADR-58, uzasadnia zdjęcie z tej ścieżki również dedupu.

**To nie jest jednak zwykła optymalizacja i nie wolno jej zrobić poprawką.** `insert_blob` jest punktem serializacji dedupu *przed* `store.put` — na tym stoją FIX-03 (sprawdzenie fizycznego istnienia bloba) i FIX-04 (kompensacja przy nieudanym `store.put`, „zatruty wiersz"). Przesunięcie go zmienia semantykę crashową, czyli dokładnie to, co mierzy etap 4 punktami K1–K5. Wymaga ADR-a i decyzji właściciela, nie commita.

### Opcje infrastrukturalne, poza kodem

Niezależnie od powyższego, 27 ms na commit to cecha ustawienia, nie prawo natury. Do rozważenia przez właściciela: wydzielony SLOG dla poola, albo przeniesienie wolumenu Postgresa z ZFS na `sda3`/ext4. `synchronous_commit=off` **odpada** — to handel trwałością, czyli tym, o co chodzi w całym zestawie invariantów.


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

## 6b. Zagrożenie: edytowanie wrappera w trakcie jego działania

`scripts/run-great-smartfs-test.sh --watch` jest **działającym skryptem basha**, a bash doczytuje własny plik po offsecie bajtowym, nie wczytuje go w całości na starcie. Zmiana tego pliku w trakcie działania — `git commit`, `git checkout`, edytor — może sprawić, że interpreter wznowi wykonanie w środku innej instrukcji.

W nocy z 13 września zmieniałem ten plik kilkakrotnie przy działającym watcherze i nic się nie stało. To było szczęście, nie poprawność: dowodem, że bash faktycznie doczytuje, jest to, że wybór podzbioru etapów (dodany w `46aa006`) **zadziałał w iteracji 4**, mimo że watcher wystartował z wersji sprzed tej zmiany.

Z tego wynikają dwie zasady:

1. **Nie zmieniaj `scripts/run-great-smartfs-test.sh`, dopóki watcher działa.** Dotyczy to również `git checkout` innego commita, co wyklucza klasyczny bisect przez przewijanie repozytorium w trakcie sesji pomiarowej.
2. Trwała naprawa to samo-przekopiowanie: skrypt na starcie kopiuje się do katalogu tymczasowego i `exec`-uje kopię, po czym plik w repo przestaje być tym, co jest wykonywane. **Do zrobienia przy następnym ręcznym starcie**, nie w trakcie — bo sama ta zmiana jest tym zagrożeniem.

Praktyczna konsekwencja dla bisectu wydajnościowego: zamiast przewijać commity, instrumentuj. Rozbicie `close()` na fazy (`590bbb2`) odpowiada „gdzie idą milisekundy" bez dotykania repozytorium i zostaje jako narzędzie.


## 6c. Etap 6 — warstwa semantyczna, czyli to, po co ten system istnieje

Etapy 1–4 dowodzą, że SmartFS jest systemem plików. Etap 5 mierzy, jak szybkim. **Żaden z nich nie pyta, czy warstwa semantyczna cokolwiek widzi** — a to jest rzecz, dla której ten projekt powstał.

Ta luka przykryła realną: przegląd żywej bazy dał **11 węzłów AST na 220 wersji, 2 embeddingi funkcji i brak rozszerzenia `pg_search`**. Rura jest podłączona na całej długości i prawie nic przez nią nie przepłynęło. Nic nie oblało, bo nikt nie patrzył.

Etap 6 przechodzi łańcuch ogniwo po ogniwie, oblewając na pierwszym pęknięciu:

```
zapis .rs przez mount
  → flush() uruchamia tree-sitter        → wiersze ast_nodes
  → release() kolejkuje, drenaż commituje → wiersz file_versions
  → worker smartfs-ai liczy embeddingi    → ast_embeddings_1536
  → MCP search_functions znajduje po nazwie → agent to widzi
```

Każde ogniwo ma osobną asercję nazywającą, co pękło — „wyszukiwanie semantyczne nic nie zwróciło" samo w sobie jest bezużyteczne, bo może oznaczać którykolwiek z pięciu komponentów.

### Znalezisko, które ten etap unieruchamia

**`smartfs-ai::run_worker_supervisor` (worker.rs:141) nie jest przez nic wołany.** Crate nie ma `[[bin]]`, a ani `smartfsd`, ani `smartfs-cli` nie odwołują się do `smartfs_ai` w ogóle. To **trzeci przypadek tego samego wzorca**, po `smartfs-fuse` (skatalogowanym w §1.1 planu) i `smartfs-mcp` (znalezionym później): kompletny komponent bez punktu wejścia, który by go włączył.

Skutek: każde narzędzie MCP czytające embeddingi jest trwale puste, a kolejka `pending` w `file_versions` rośnie bez końca — przy pisaniu tego było w niej 14 wersji, najstarsza od piętnastu godzin.

**Gdzie ma mieszkać ten worker, to decyzja architektoniczna, nie poprawka.** Demon hostuje już supervisory konsolidacji (§1.3 krok 9) i to samo rozumowanie tu pasuje — ale `smartfs-ai` ładuje modele ONNX do pamięci, więc uruchomienie go wewnątrz demona FUSE znaczy, że system plików nosi w sobie wagi modelu. To jest kompromis, którego nie rozstrzygam sam; wymaga ADR-a.

### `search_fulltext` nie może dziś działać

`pg_search` nie jest zainstalowany, indeksów BM25 jest zero. Migracja 006 tworzy je warunkowo, więc **migracja przechodzi, a zdolność po cichu nie istnieje**. ADR-54 wybrał `pg_search` świadomie, dla polskiego stemmingu; wdrożenie bez niego działa bez udokumentowanej funkcji. Etap 6 oblewa na tym jawnie, zamiast pozwolić wnioskować to z pustego wyniku.


## 7. Znaleziska otwarte

- **Etap 4, Invariant #3 — nierozstrzygający.** 82 z 83 porażek to jedna klasa: po `kill -9` plik jest widoczny z treścią, która nie ma jeszcze wiersza `file_versions`, bo jego znacznik czeka w `pending/queue/`, a nakładka odczytu z ADR-58 go serwuje. §5.3 planu został pod to zrewidowany (najpierw quiesce), ale `04_crash_consistency_test.sh` nigdy nie dostał tej samej poprawki. Sprawdzian nie odróżnia dziś „zakolejkowane, zaraz się zacommituje" (legalne) od „widoczne, a nigdzie nie zapisane" (realne naruszenie). Do naprawy w skrypcie, ze zgłoszeniem.
- **Stage 0b nie istnieje.** `--crash-points` zwraca pustkę, więc K1–K11 są luką pokrycia, a etap 4 biegnie wyłącznie stochastycznie. Świadoma decyzja, nie awaria — ale historia crashowa pozostaje nieudowodniona.
- **`generic/` nigdy nie uruchomiony.** Największa niewiadoma w projekcie.
