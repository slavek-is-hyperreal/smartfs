# ADR-60 — Architektura wtyczek: Rust w kompilacji, SPIR-V w locie

← [Mapa ADR](../01-architecture.md) | Poprzednicy: [ADR-57](ADR-57-pick-style-plugin-dictionary.md) (słownik `plugin_type` przez MCP), [ADR-55](ADR-55-gpu-acceleration.md) (Vulkan jako jedyny stack GPU), [ADR-59](ADR-59-posix-special-file-types.md) (typy plików) | Plan: [PLAN-posix-parity](../plans/PLAN-posix-parity-and-storage-policy.md) §4

**Status:** Proponowany — kierunek zatwierdzony przez właściciela projektu 2026-09-13, mechanizm do rozstrzygnięcia. **Trzy Otwarte pytania blokują implementację** (§Otwarte pytania). Ten ADR nie dopuszcza pisania kodu wtyczek przed ich zamknięciem.

---

## Kontekst

SmartFS ma dziś **deklaratywną** warstwę wtyczek i nie ma **wykonywalnej**. `file_versions.special_type` i `special_data` niosą dane zależne od typu pliku, a ADR-57 udostępnia ich kształt przez `describe_plugin_type`/`list_plugin_types`. To wystarcza, żeby *opisać* plik, i nie wystarcza, żeby cokolwiek z nim *zrobić*.

Trzy niezależne potrzeby zbiegły się w tym samym punkcie:

1. **Polityka składowania per typ** ([PLAN §4](../plans/PLAN-posix-parity-and-storage-policy.md)): kompresja ogólnego przeznaczenia na strumieniu H.264 to spalony CPU za ułamek procenta. Decyzja „czym to skompresować, albo czy w ogóle" jest funkcją typu pliku. Haczyk już istnieje — `inode_registry.compression_level` jest per-inode — i nikt go nie używa jako punktu decyzyjnego.
2. **Ekstrakcja metadanych** wykracza poza kod źródłowy. Dziś `smartfs-ai` parsuje Rusta przez tree-sitter. Kontener wideo, PDF, arkusz — każdy ma własną strukturę, której nie da się sensownie wcisnąć w jeden parser.
3. **Transformacje na GPU** ([IDEA multimedia](../ideas/IDEA-multimedia-vram-vulkan-pipeline.md)): dekodowanie kodeków bez sprzętowego dekodera, debayering, konwersje przestrzeni barw. To jest praca obliczeniowa zależna od typu pliku.

Wszystkie trzy to „zachowanie zależne od typu pliku". Bez wspólnego mechanizmu każda z nich dorobi się własnego, niekompatybilnego rozgałęzienia w innym miejscu drzewa.

## Decyzja

### 1. Dozwolone są dokładnie dwa języki: Rust i SPIR-V

Decyzja właściciela projektu, zapisana tu jako ograniczenie, nie jako propozycja do przedyskutowania. Uzasadnienie techniczne, które ją podpiera:

- **Bez interpreterów i bez maszyn wirtualnych.** Wtyczka Lua albo JavaScript wprowadza runtime, GC i granicę marshalingu na ścieżce, która obsługuje każdą operację na pliku.
- **WASM świadomie odrzucony**, mimo że jest oczywistym kandydatem na piaskownicę. Pomiary są niejednoznaczne: kod I/O-bound bywa w WASM na poziomie natywnego, ale **kernele obliczeniowe płacą zauważalny narzut**, a to właśnie kernele są tu głównym przypadkiem (dekodowanie, embeddingi, transformacje). Do tego SmartFS potrzebowałby *dwóch* mechanizmów — WASM na CPU i SPIR-V na GPU — zamiast jednego wzorca.
- **SPIR-V nie jest wyjątkiem od reguły „bez maszyn wirtualnych", tylko jej potwierdzeniem.** To skompilowany bytecode oddawany sterownikowi, dokładnie tak jak Rust jest oddawany procesorowi.

### 2. Rust wchodzi w kompilacji, nie przez `dlopen`

**To jest sedno tego ADR-a i jego najbardziej sporna część.**

Rust nie ma stabilnego ABI. Dynamiczne ładowanie wtyczki Rustowej wymaga zejścia do `repr(C)` i biblioteki w rodzaju `abi_stable` albo `stabby`, a i wtedy układ typów potrafi się różnić **nie tylko między wersjami kompilatora, ale między przebiegami kompilacji**. Dla systemu plików oznacza to klasę awarii, której nie da się odróżnić od uszkodzenia danych: wtyczka odczytuje strukturę pod złym offsetem i zwraca prawdopodobnie wyglądające śmieci.

Dlatego **wtyczki Rustowe są crate'ami w workspace, rejestrowanymi w tablicy kompilowanej statycznie.** Konsekwencje przyjęte świadomie:

- Dodanie wtyczki wymaga przebudowy i restartu demona. Dla demona systemu plików to nie jest realne ograniczenie — restart i tak jest potrzebny przy zmianie konfiguracji montowania.
- Wysyłka to `match` albo statyczna tablica wskaźników, nie przeszukiwanie symboli. Zero narzutu ABI, zero kosztu ładowania, pełna inline'owalność.
- Nie ma wtyczek od stron trzecich w formie binarnej. Kod wtyczki przechodzi ten sam przegląd co reszta drzewa — patrz punkt 5.

### 3. SPIR-V ładuje się w locie, bo jest **danymi**

Tu asymetria jest uzasadniona, a nie niekonsekwentna: **SPIR-V ma z definicji stabilny format binarny.** To wersjonowany, walidowalny bytecode Khronosa — dokładnie ten sam rodzaj artefaktu co blob w store. Ładowanie go w czasie działania nie ma żadnego z problemów `dlopen` na Rustcie, bo nic tu nie zależy od układu typów w pamięci procesu.

Czyli: **część „dynamiczna" pluginizacji to kernele GPU, część „statyczna" to Rust.** Ten podział nie jest kompromisem — wynika wprost z tego, który z dwóch dozwolonych języków ma stabilny format wymiany.

### 4. Trzy punkty rozszerzenia i ani jednego więcej

| punkt | kiedy się wykonuje | gdzie |
|---|---|---|
| **polityka składowania** | raz, przy tworzeniu inode'a | ustala `compression_level`, wybór kodeka |
| **ekstrakcja metadanych** | asynchronicznie, po commicie | wypełnia `special_data`, węzły AST, teksty do indeksu |
| **transformacja GPU** | na żądanie, nigdy na ścieżce zapisu | kernele SPIR-V: dekodowanie, miniatury, embeddingi wizualne |

**Żaden z nich nie może blokować `cow_commit`.** To jest ta sama dyscyplina, którą Root Invariant #6 nakłada na `smartfs-semantic`, i z tego samego powodu: warstwa wzbogacająca nigdy nie decyduje o tym, czy zapis się powiedzie. Polityka składowania jest wyjątkiem tylko pozornym — to odczyt tablicy w pamięci przy tworzeniu pliku, bez I/O.

Rozszerzenia poza tą trójką wymagają nowego ADR-a. Lista jest krótka celowo: system wtyczek, który może wpiąć się wszędzie, jest systemem, którego wydajności nie da się uzasadnić.

### 5. Wtyczka natywna to pełne zaufanie i trzeba to powiedzieć wprost

Statycznie zlinkowany Rust działa w procesie demona, z jego uprawnieniami, bez piaskownicy. Wtyczka może zrobić wszystko, co demon. **To jest cena, którą płacimy za brak narzutu, i nie należy jej owijać w bawełnę** — WASM odrzucono świadomie, a razem z nim izolację, którą dawał.

Wynikające z tego zasady: wtyczki żyją w drzewie i przechodzą przegląd jak reszta kodu; nie ma mechanizmu ładowania wtyczek osób trzecich; kernel SPIR-V przed użyciem przechodzi walidację, bo jako jedyny wchodzi w czasie działania.

### 6. Deklaracja zostaje w JSON, implementacja idzie do kodu

ADR-57 ustanowił słownik `plugin_type` odpytywalny przez MCP. Zostaje bez zmian jako **deklaracja**: jakie pola ma `special_data`, jakie rozszerzenia należą do typu, jaka jest domyślna polityka składowania. Rust i SPIR-V to **implementacja**. Agent nadal odkrywa kształt typu przez `describe_plugin_type`, nie czytając kodu.

## Odrzucone alternatywy

**WASM/WASI jako format wtyczek.** Najmocniejszy odrzucony kandydat: prawdziwa piaskownica, stabilny format, ładowanie w locie, i można by w nim pisać w Rust. Odrzucony, bo (a) kernele obliczeniowe płacą mierzalny narzut, a to jest tu główny przypadek użycia, (b) wymagałby drugiego mechanizmu dla GPU, więc dwóch systemów wtyczek zamiast jednego, (c) właściciel projektu ograniczył dozwolone języki do Rusta i SPIR-V. Punkt (c) sam by wystarczył; (a) i (b) są powodem, dla którego to ograniczenie jest rozsądne, a nie arbitralne.

**Dynamiczne `.so` z `abi_stable`/`stabby`.** Daje ładowanie w locie i zachowuje Rusta. Odrzucony z powodu z punktu 2: brak gwarancji układu typów między przebiegami kompilatora zamienia niezgodność wersji w cichy odczyt spod złego offsetu. W systemie plików ta klasa awarii jest nieodróżnialna od uszkodzenia danych, a Root Invariant #1 istnieje właśnie po to, żeby takich rzeczy nie było.

**Wtyczki jako osobne procesy przez IPC.** Izolacja bez problemu ABI. Odrzucone dla ścieżki metadanych i składowania: przełączenie kontekstu i serializacja na operację pliku to dokładnie ten koszt, którego ADR-58 właśnie się pozbywał. Pozostaje rozsądne dla drogich, rzadkich zadań — ale to jest opis workera `smartfs-ai`, który już istnieje, a nie nowego systemu wtyczek.

**Skrypty (Lua, Rhai, JS).** Nierozważane poważnie: interpreter na ścieżce operacji plikowej.

## Konsekwencje

- Nowy crate `smartfs-plugin` z definicją traitów i statycznym rejestrem; wtyczki jako crate'y `smartfs-plugin-*`.
- `inode_create` konsultuje politykę składowania, żeby ustalić `compression_level` — pierwsze realne użycie tej kolumny.
- `smartfs-ai` przestaje zakładać, że każdy plik to kod źródłowy; ekstrakcja tekstu i AST staje się jednym z punktów rozszerzenia.
- Kernele SPIR-V wymagają warstwy Vulkan z ADR-55, której **jeszcze nie ma**. Punkt rozszerzenia GPU jest zdefiniowany, ale niebudowalny do tego czasu — i to jest w porządku, bo pierwsze dwa punkty niosą całą bieżącą wartość.
- Brak nowych migracji. Wszystkie trzy punkty operują na istniejących kolumnach.

## Otwarte pytania — zatrzymać się i zapytać, nie zgadywać

1. **Czy statyczna rejestracja jest akceptowalna?** Punkt 2 wymienia ładowanie w locie na brak problemu ABI i zerowy narzut wysyłki, ale kosztem tego, że „wtyczka" znaczy „przebuduj i zrestartuj demona". Jeśli intencją była instalacja wtyczki bez przebudowy, ten ADR trafia obok i punkt 2 trzeba napisać od nowa — z pełną świadomością ryzyka ABI. **To jest pytanie o sygnaturę całego mechanizmu, nie o szczegół.**
2. **Czy wtyczka może odmówić zapisu?** Ekstraktor składni już dziś zwraca `EACCES` przy błędzie parsowania (bufor `flush()` w `smartfs-fuse`), czyli precedens na „wtyczka blokuje zapis" *istnieje* i stoi w napięciu z punktem 4. Zachować go, ograniczyć do walidacji składni, czy usunąć?
3. **Co ustala typ wtyczki dla pliku?** Rozszerzenie nazwy jest najprostsze i myli się na plikach bez rozszerzenia. Sniffing treści jest dokładniejszy i wymaga przeczytania początku pliku na ścieżce tworzenia. Jawna deklaracja przez `smartfs-cli`/MCP jest precyzyjna i przerzuca pracę na wołającego. Wybór wpływa na to, czy punkt rozszerzenia „polityka składowania" da się w ogóle wykonać bez I/O.

## Odniesienia

- [stabby — a stable ABI for Rust](https://github.com/ZettaScaleLabs/stabby) i [abi_stable](https://docs.rs/abi_stable/) — stan sztuki w dynamicznym ładowaniu Rusta i źródło zastrzeżeń z punktu 2
- [Plugins in Rust: Getting Started](https://nullderef.com/blog/plugin-start/) — przegląd kompromisów, w tym problemu układu typów między przebiegami kompilatora
- [VM Matters: A Comparison of WASM VMs](https://arxiv.org/pdf/2012.01032) — pomiary narzutu WASM na kernelach obliczeniowych
