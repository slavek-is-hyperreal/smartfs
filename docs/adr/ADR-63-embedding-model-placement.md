# ADR-63 — Gdzie mieszka model embeddingowy: proces, dysk, format artefaktu

← [Mapa ADR](../01-architecture.md) | Poprzednicy: [ADR-49](ADR-49-qwen-default-model.md) (wybór modelu), [ADR-55](ADR-55-gpu-acceleration.md) (Vulkan, reuse-before-rewrite), [ADR-42/47](../base-v4.5-v5.0/SmartFS_v4.5_to_v5.0_fixes.md) (zawór antygłodowy) | Wynika z pomiaru: [etap 6 Wielkiego Testu](../testing/the-great-smartfs-test.md)

**Status:** Przyjęty 2026-09-14. **Rewizja 2** (ten sam dzień, po uwadze właściciela projektu: „dobierz silniki tak, by oferowały max wydajność na każdym sprzęcie — idealnie żeby nawet na 1 GB VRAM istniała możliwość akceleracji na Vulkanie"). Rewizja 1 uzasadniała §1 prostotą — jeden silnik zamiast dwóch — co stawiało wydajność jako cenę porządku. To było odwrócone pytanie: maksimum wydajności na danym sprzęcie rozstrzyga się na poziomie **backendu i dyspozycji w czasie działania**, nie liczby silników. §1 jest przepisany wokół tego, z budżetem VRAM policzonym z pobranych plików, a nie ze specyfikacji; §3 traci przy okazji jedno zdanie postawione za mocno. Wniosek (`ggml`/GGUF) się nie zmienia — zmienia się to, co z niego wyciągamy.

ADR-49 wybrał *który* model. Ten ADR odpowiada na trzy pytania, których ADR-49 nie postawił, a bez których model nie może zacząć działać: **w jakim procesie** żyje, **w jakim katalogu** leżą jego wagi i **w jakim formacie** je pobieramy. Rozstrzyga przy okazji Otwarte pytanie z ADR-55 (jeden silnik inferencji czy dwa).

---

## Kontekst

### Co pokazał etap 6

Etap 6 Wielkiego Testu przeszedł łańcuch semantyczny ogniwo po ogniwie i zerwał go w jednym miejscu:

```
[PASS] version row committed for the source file
[PASS] tree-sitter extracted 2 AST node(s)
[PASS] the function is in ast_nodes under its own name
[FAIL] the version is still 'pending' after 120s and has 0 embedding(s)
[FAIL] search_functions returned 0 hits for a function this stage just wrote
[PASS] diff_functions answered without error
[FAIL] pg_search is NOT installed
```

Przyczyna jest jednozdaniowa: `smartfs_ai::run_worker_supervisor` (`worker.rs:141`) **nie jest wołany przez nic**. Crate nie ma `[[bin]]`, a ani `smartfsd`, ani `smartfs-cli` nie referują `smartfs_ai`. To trzeci przypadek tego samego wzorca w tym projekcie (wcześniej `smartfs-fuse`, potem `smartfs-mcp`): kompletny komponent bez punktu wejścia.

### Co pokazał przegląd kodu przy okazji — gorsze

`CpuEmbeddingEngine::compute_vector` (`crates/smartfs-ai/src/engine.rs`) **nie uruchamia żadnego modelu.** Liczy SHA-256 z tekstu, rozwija go w `dimensions` kolejnych hashy i normalizuje L2:

```rust
let mut h = Sha256::new();
h.update(base_hash);
h.update((i as u32).to_le_bytes());
```

To jest szum deterministyczny. Dwa zdania o tym samym znaczeniu dostają wektory ortogonalne; jedna zmieniona litera daje wektor całkowicie inny. Kosinus między takimi wektorami nie niesie żadnej informacji semantycznej.

Doc-comment nad tą strukturą mówi „ONNX model compatibility (ADR-49, ADR-55)", a `Cargo.toml` crate'a `smartfs-ai` nie ma **ani `ort`, ani `tokenizers`, ani żadnej zależności inferencyjnej**. Nazwa i komentarz opisują rzecz, której nie ma — dokładnie ten rodzaj metryki-która-kłamie, który już raz złapaliśmy przy fazie `dedup`.

**To jest ważniejsze niż brakujący punkt wejścia.** Brak workera daje pustą tabelę — widać od razu. Fałszywy silnik z podłączonym workerem dałby tabelę **pełną** i `search_functions` zwracające wyniki: etap 6 przeszedłby na zielono, a wyszukiwanie semantyczne zwracałoby losowe pliki. Wyciągając wnioski z §0 Fail-Loud: nieprawdziwe wyniki są gorsze niż brak wyników, bo brak wyników widać.

### Trzecia rzecz, którą trzeba rozstrzygnąć przy okazji

`embed_version` woła `engine.embed(&node.source, 1536)` i wstawia wynik do `ast_embeddings_1536` — z `default_model_id` wskazującym na wiersz `Qwen3-Embedding-0.6B`, którego `dimensions` wynosi **1024**.

Prawdziwy model nie umie wyprodukować 1536 wymiarów z przestrzeni 1024-wymiarowej (MRL Qwen skraca w dół, nie w górę). Ten kod może działać **wyłącznie** dlatego, że silnik jest fałszywką generującą dowolną liczbę wymiarów na żądanie. Jest to zarazem naruszenie Invariantu #5 zapisane w kodzie: wiersz w `ast_embeddings_1536` z `model_id` modelu 1024-wymiarowego to wektor podpisany cudzą przestrzenią.

---

## Decyzja

### 1. Jeden silnik, wiele backendów, wybór przez pomiar — nie jeden backend z założenia

**Rewizja 2 zmienia tu rozumowanie, nie wniosek.** Rewizja 1 wybrała `ggml`/`llama.cpp` (format GGUF) i uzasadniła to prostotą: jeden format wag, tokenizer w pliku, pooling po stronie silnika. To zostaje. Ale postawiła to jako „jeden silnik zamiast dwóch", jakby ceną była wydajność, a zyskiem porządek. To było odwrócenie problemu. **Wymaganie brzmi: maksymalna wydajność na każdym sprzęcie** — a to jest pytanie o *backend i dyspozycję w czasie działania*, nie o liczbę silników.

`ggml` nie jest jednym backendem. Jest warstwą dyspozycyjną nad kilkoma: CPU z wyborem zestawu instrukcji w czasie działania (AVX2/AVX-512 na x86, NEON/i8mm/SVE na ARM), Vulkan, plus backendy wendorowe, które ADR-55 wyklucza zasadowo. „Max wydajność na każdym sprzęcie" realizuje się **wewnątrz** tego silnika i to on jest do tego narzędziem, nie przeszkodą.

#### 1a. Tak, Vulkan compute kernels — i one już istnieją

Pytanie „może Vulkan compute kernels?" ma odpowiedź, która jest lepsza, niż się wydaje: **backend Vulkan w `ggml` *jest* zestawem kerneli compute**. To shadery GLSL kompilowane do SPIR-V i uruchamiane przez `vkCmdDispatch` — żadnego potoku graficznego, czyste obliczenia. Nie trzeba ich pisać; trzeba ich dobrze użyć.

Warto odnotować zbieżność: [ADR-60](ADR-60-plugin-architecture-rust-spirv.md) dopuszcza w tym projekcie dokładnie dwa języki — Rust i SPIR-V. Silnik inferencji dostarczający swoje jądra jako SPIR-V nie jest wyjątkiem od tej zasady, tylko jej najlepszym przykładem.

Pisanie własnych kerneli (CubeCL/`krnl`, odrzucone w ADR-55 rewizja 2) nadal nic tu nie daje, i to z konkretnego powodu: **problem małego VRAM-u nie jest problemem kerneli.** Rozwiązują go kwantyzacja i częściowy offload warstw — jedno i drugie `ggml` ma. Własny matmul nie zmniejszy modelu.

#### 1b. Build: jedna binarka, która nie zakłada maszyny, na której powstała

To jest miejsce, w którym najłatwiej stracić wydajność na cudzym sprzęcie, i robi się to jednym flagiem:

| flaga CMake | wartość | po co |
|---|---|---|
| `GGML_VULKAN` | `ON` | jedyna dopuszczona akceleracja GPU (ADR-55) |
| `GGML_CUDA`, `GGML_HIP` | `OFF` | binarka fizycznie nie zawiera kodu wendorowego |
| `GGML_NATIVE` | `OFF` | **nie** `-march=native`: szybkie tu, `SIGILL` gdzie indziej |
| `GGML_CPU_ALL_VARIANTS` + `GGML_BACKEND_DL` | `ON` | warianty CPU (AVX2, AVX-512, …) wybierane **w czasie startu**, po sprawdzeniu CPUID |

Ostatni wiersz jest tym, co realizuje „max wydajność na każdym sprzęcie" na ścieżce CPU — bez niego mamy albo binarkę dostrojoną do maszyny budującej i wywracającą się na starszej, albo binarkę zbudowaną na najniższy wspólny mianownik i wolną na wszystkich.

#### 1c. Backend wybierany pomiarem, nigdy założeniem

ADR-55 zebrał liczby, które zakazują reguły „jest GPU, więc używaj GPU": na telefonach Vulkan przez `llama.cpp` bywa **~15× wolniejszy niż CPU tego samego urządzenia**. Reguła „użyj GPU, jeśli jest" byłaby wtedy regułą „bądź piętnaście razy wolniejszy".

Dlatego: `smartfs-worker --calibrate` uruchamia stałą sondę (ustalony zestaw tekstów, ustalona długość kontekstu) na każdej kandydującej konfiguracji — CPU, Vulkan z pełnym offloadem, Vulkan z offloadem częściowym — i zapisuje zwycięzcę do `<model-path>/backend-calibration.json`. Klucz wpisu: UUID urządzenia + wersja sterownika + sha256 pliku modelu + wersja workera; zmiana któregokolwiek unieważnia pomiar, bo każde z nich może odwrócić wynik.

Dwie reguły wokół tego, obie w duchu §0 Fail-Loud:

- **Kalibracja nigdy nie odpala się sama przy montowaniu.** Demon systemu plików nie zatrzymuje się na trzydziestosekundowy benchmark. Jest osobnym, jawnym poleceniem, tak samo jak `fetch-models.sh`.
- **Brak pomiaru znaczy CPU**, z wpisem w logu, że kalibracja nie była uruchomiona. Niezmierzone GPU jest traktowane jak nieobecne — nie jak szybsze.

#### 1d. `llvmpipe` nie jest kartą graficzną

Wykrywanie GPU musi odrzucać `VK_PHYSICAL_DEVICE_TYPE_CPU`. To nie jest hipotetyczne — `vulkaninfo` na tej maszynie wylicza dwa urządzenia:

```
GPU0: AMD Radeon R7 200 Series (RADV BONAIRE)   DISCRETE_GPU
GPU1: llvmpipe (LLVM 20.1.2, 256 bits)          CPU          ← rasteryzator programowy
```

Naiwne „czy jest urządzenie Vulkan?" na maszynie bez karty znajdzie `llvmpipe`, zgłosi „akceleracja GPU włączona" i będzie liczyć **wolniej niż backend CPU**, bo to ten sam procesor, tylko przez sterownik graficzny. To dokładnie ta klasa kłamiącej metryki, którą ten projekt już dwa razy łapał.

#### 1e. Mały VRAM: częściowy offload, nie wykluczenie

Kluczowa własność: **`-ngl N` offloaduje N warstw, reszta liczy się na CPU.** Karta nie musi zmieścić całego modelu, żeby cokolwiek przyspieszyć. Do tego w embeddingach — inaczej niż w czacie — **to my wybieramy długość kontekstu**, bo i tak dzielimy plik na fragmenty. Zużycie VRAM jest więc pokrętłem polityki, nie wyrokiem sprzętu.

Liczby zmierzone bezpośrednio z pobranych plików, nie ze specyfikacji modelu. Narzędzie, które je wypisuje, leży w repozytorium, żeby tabela poniżej była odtwarzalna, a nie do przepisania na wiarę:

```
scripts/gguf-info.py <model>.gguf --vram 1024 --ctx 1024
```

| | f16 | Q8_0 |
|---|---:|---:|
| `token_embd` (tablica wejściowa) | 296,2 MiB | 157,4 MiB |
| 28 warstw (`blk`) | 840,2 MiB | 446,5 MiB |
| **jedna warstwa** | **30,0 MiB** | **15,9 MiB** |
| razem | 1136,5 MiB | 603,9 MiB |

Architektura z tych samych nagłówków: 28 bloków, `embedding_length` 1024, 16 głów uwagi przy 8 głowach KV, `key/value_length` 128, `pooling_type = 3` (last-token — czyli pooling naprawdę przychodzi z pliku, jak zakładała rewizja 1). Stąd cache KV: `2 × 8 × 128 × 2 B × 28` = **112 KiB na token**.

Budżet:

```
warstwy_na_gpu = min(28, ⌊(vram_wolny − 112 KiB × ctx − 128 MiB zapasu) / bajty_warstwy⌋)
```

| wolny VRAM | wariant | ctx | warstw na GPU |
|---:|---|---:|---|
| 1024 MiB | Q8_0 | 1024 | **28 z 28** — cały model, ~340 MiB zapasu |
| 1024 MiB | f16 | 1024 | 26 z 28 — częściowy |
| 512 MiB | Q8_0 | 512 | 20 z 28 |
| 256 MiB | Q8_0 | 512 | 4 z 28 |

**Odpowiedź na „idealnie żeby nawet 1 GB VRAM": przy Q8_0 karta z 1 GB mieści cały model z zapasem.** Nie jest to przypadek graniczny — jest z marginesem. Poniżej tego progu offload schodzi warstwami, aż do zera, i nic się po drodze nie psuje.

Zastrzeżenie, którego nie wolno pominąć: liczy się VRAM **wolny**, nie całkowity, a na karcie obsługującej ekran jest on zmienny. Na tej maszynie `vulkaninfo` pokazuje 768 MiB sterty urządzenia, z czego wolne w chwili pomiaru było **35,8 MiB** — resztę trzyma pulpit. Ta karta (Bonaire, GCN 2 z 2013 r.) nie ma też `cooperative matrix` ani szybkiej arytmetyki fp16, więc `ggml` zejdzie na ścieżki zapasowe. Czy wyjdzie szybciej niż jej CPU — nie wiadomo, i właśnie dlatego rozstrzyga o tym §1c, a nie reguła.

Drugi zastrzeżenie tej samej klasy: RADV wystawia stertę host-visible (tu 11,71 GiB), z której `ggml` potrafi alokować, gdy VRAM się skończy. Alokacja wtedy **się udaje**, a liczenie idzie przez PCIe i zwykle jest wolniejsze niż CPU. „Zmieściło się" nie znaczy „jest szybciej" — kolejny powód, żeby wynik ustalał pomiar.

#### 1f. Dlaczego mimo wszystko nie drugi silnik

Skoro celem jest maksimum na każdym sprzęcie, trzeba uczciwie sprawdzić, czy ONNX Runtime coś dokłada. Jego przewaga sprowadza się dziś do dostawców wykonawczych, których ten projekt i tak nie może użyć: QNN (Qualcomm, zamknięty), OpenVINO (tylko Intel), DirectML (tylko Windows), CUDA/TensorRT (wykluczone w ADR-55). Na sprzęcie w zakresie projektu — Linux, dowolny wendor — nie zostaje ani jedna konfiguracja, na której ORT wygrywa z `ggml`.

Drugi silnik kosztowałby drugi format wag, drugą tokenizację, drugą implementację poolingu i drugi zestaw trybów awarii, **nie kupując ani jednej maszyny więcej**. Decyzja zostaje: jeden silnik, wiele backendów, wybór pomiarem.


### 2. Model żyje w osobnym procesie `smartfs-worker`, nadzorowanym jako dziecko `smartfsd`

Dwa wymagania są w napięciu i oba są realne:

- **Izolacja.** Model f16 to ~1,2 GB w przestrzeni adresowej, ładowany przez kod natywny C++, docelowo rozmawiający ze sterownikiem GPU. Segfault w `ggml`, OOM przy ładowaniu wag, `ErrorDeviceLost` ze sterownika Vulkan (ADR-55 wymienia je z numerami zgłoszeń) — każde z tego zabiłoby proces. Gdyby tym procesem był `smartfsd`, **odmontowałby się system plików**, bo biblioteka ML się wywróciła. System plików nie może umierać od tego, co robi warstwa semantyczna.
- **Brak zapomnianego komponentu.** Wzorzec „kompletny crate bez punktu wejścia" wystąpił w tym projekcie trzy razy. Rozwiązanie „napisz unit systemd i pamiętaj go włączyć" to czwarty raz czekający na swoją kolej.

Architektura projektu już raz rozstrzygnęła połowę tego sporu i warto zacytować jej własne uzasadnienie ([docs/02-crates.md](../02-crates.md)):

> `smartfs-semantic` tylko *czyta* już zapisane wektory. Rozdzielenie gwarantuje, że proces konsolidacji może działać (i być testowany) bez ładowania jakiegokolwiek modelu ONNX, i że crash/spowolnienie w `smartfs-ai` nigdy nie propaguje się do `smartfs-semantic`.

Ta troska dotyczy tak samo — a bardziej — `smartfs-fuse`. Konsolidacja przeżywa upadek `smartfs-ai`, bo ich nie łączy zależność kompilacji; `smartfsd` przeżyje go, bo nie łączy ich przestrzeń adresowa.

Decyzja bierze oba wymagania:

- `smartfs-ai` dostaje `[[bin]] name = "smartfs-worker"`. To jest brakujący punkt wejścia do `run_worker_supervisor`.
- `smartfsd` **uruchamia go jako proces potomny** w kroku 9 sekwencji startowej, obok supervisorów konsolidacji, i restartuje z backoffem, gdy padnie.
- Dziecko dostaje `PR_SET_PDEATHSIG = SIGTERM` przez `pre_exec`. Zabicie `smartfsd` — co etap 4 robi 25 razy na przebieg — zabija workera razem z nim. Bez tego każda runda testu crashowego zostawiałaby osieroconego workera trzymającego 1,2 GB i piszącego do bazy pod nieistniejącym już montowaniem.
- Flaga `--no-embeddings`, symetryczna do istniejącej `--no-semantic`. Etapy 2, 3, 4 i 5 uruchamiają demona z tą flagą — nie potrzebują embeddingów, a etap 5 mierzyłby ich koszt jako koszt zapisu.
- Awaria workera jest **niefatalna dla montowania**, tak samo jak krok 9 dziś: `smartfsd` loguje, zapisuje do pliku gotowości i serwuje dalej. System plików bez embeddingów to zdegradowany system plików, nie martwy.
- Plik gotowości (§1.4) zyskuje wiersz `embeddings=ok|degraded|off` — analogicznie do istniejącego `semantic=`. Etap 6 czyta go zamiast zgadywać.

### 3. Wagi leżą poza repozytorium, poza magazynem blobów i poza partycją systemową

Rozstrzygnięcie ścieżki, w kolejności, tak samo jak dla `--store-path`:

```
--model-path  →  SMARTFS_MODEL_PATH  →  /var/lib/smartfs/models
```

Układ katalogu:

```
<model-path>/qwen3-embedding-0.6b/
    Qwen3-Embedding-0.6B-f16.gguf      ← domyślny
    Qwen3-Embedding-0.6B-Q8_0.gguf     ← wariant niskopamięciowy
    SHA256SUMS
    README.md                          ← karta modelu + licencja Apache-2.0
```

Trzy miejsca, w których wagi **nie** mieszkają, każde z powodem:

- **Nie w repozytorium.** 1,8 GB binariów; git nie jest magazynem artefaktów. `.gitignore` i `docs/` mówiące, skąd je wziąć.
- **Nie w `--store-path`.** Katalog blobów jest enumerowany przez scrub (ADR-58 punkt 7) i sprzątacza (ADR-62). Plik, który nie jest blobem, a leży wśród blobów, to albo fałszywy alarm scrubu, albo — gorzej — kandydat do usunięcia dla sprzątacza.
- **Nie na partycji systemowej.** Na maszynie testowej `/` ma 5,3 GB wolnego, a domyślne `/var/lib/smartfs/models` leży właśnie tam. Domyślna wartość zostaje zgodna z FHS, bo dla normalnego wdrożenia jest poprawna, ale **preflight (etap 0) sprawdza rozwiązaną ścieżkę tym samym `assert_not_on_root`, który już stosuje do pozostałych ścieżek zapisu**, a demon loguje ją przy starcie. Na tej maszynie: `SMARTFS_MODEL_PATH=/vectorlegis_ssd_pool/smartfs-models`.

**Wariant artefaktu zależy od tego, gdzie model liczy** — rewizja 2 zmienia tu poprzednie ustalenie:

- **Q8_0 (604 MiB) na ścieżce GPU.** To ten wariant mieści cały model w karcie z 1 GB VRAM (§1e), a to jest różnica między „akceleracja działa" a „karta za mała".
- **f16 (1137 MiB) na ścieżce CPU** i tam, gdzie VRAM-u jest w nadmiarze.

Rewizja 1 pisała tu, że „kwantyzacja psuje geometrię przestrzeni wektorowej". **To było postawione za mocno.** Q8_0 jest w pomiarach perplexity `llama.cpp` uznawany za praktycznie bezstratny; dopiero Q4 i niżej degradują realnie. Czy „praktycznie bezstratny" w perplexity znaczy to samo dla **kosinusów między embeddingami** — tego nie zmierzyłem i nie wiem, a jest to inna wielkość niż ta, którą tamte pomiary badały.

Dlatego jest to teraz zadanie pomiarowe, nie założenie, i ma próg: na wspólnym zbiorze tekstów liczymy embeddingi obydwoma wariantami i sprawdzamy średni kosinus między parami f16↔Q8_0 tego samego tekstu oraz zgodność rankingu top-k. **Jeśli ranking się rozjeżdża, Q8_0 wypada ze ścieżki GPU i niska półka VRAM zostaje przy częściowym offloadzie f16** — bo tańsza akceleracja nie jest warta cichego pogorszenia wyszukiwania.

Czego nie wolno w żadnym wariancie: **automatycznej podmiany pliku modelu za plecami.** Wybór idzie przez `--model-file`, a wariant jest zapisany razem z wynikiem kalibracji — zmiana wag to zmiana przestrzeni wektorowej i musi zostawiać ślad, tak samo jak zmiana modelu w `embedding_models`.

### 4. Brak modelu jest błędem głośnym; fałszywy silnik nigdy nie jest ścieżką produkcyjną

`CpuEmbeddingEngine` zostaje przemianowany na `HashEmbeddingEngine` i opisany dokładnie tym, czym jest: deterministyczną atrapą do testów, która **nie niesie znaczenia**. Nie jest wybieralny żadną flagą ani konfiguracją; istnieje wyłącznie w testach, gdzie sprawdza się przepływ wierszy, nie jakość wektorów.

Gdy `smartfs-worker` nie znajdzie pliku modelu pod rozwiązaną ścieżką albo nie zdoła go załadować: **loguje na `error` z pełną ścieżką i kończy z niezerowym kodem**. `smartfsd` odnotowuje `embeddings=degraded` i serwuje dalej. Czego nie robi: nie generuje wektorów zastępczych.

Wymaga to również poprawienia Addendum ADR-49, które mówi „fallback idzie na aktywny model `is_default=TRUE` w chwili startu". Ten zapis jest pusty — brakujący model *jest* modelem `is_default=TRUE`. Właściwa reguła: **żadnego fallbacku między modelami**, bo model wyznacza przestrzeń wektorową, a cichy fallback wstawiłby do jednej tabeli wektory z dwóch przestrzeni. To jest Invariant #5 czytany na poziomie wiersza, nie zapytania.

### 5. AST dostaje własną tabelę 1024, `ast_embeddings_1536` zostaje jako spuścizna

Migracja 010 tworzy `ast_embeddings_1024_qwen` — dokładnie tak, jak ADR-49 §„Konsekwencja schematowa" postąpił na poziomie plików, gdy wymiar zmienił się z 384 na 1024. Nie jest to nowy wzorzec, tylko ten sam, zastosowany do przeoczonej wtedy tabeli.

`ast_embeddings_1536` **nie znika**. Migracja 002 stworzyła ją dla `text-embedding-3-large` (1536, `is_local=FALSE`) i ta tabela pozostaje poprawna dla tego modelu; `search_functions` z jawnym `model_id` nadal ją odpytuje. Znika tylko zapisywanie do niej wektorów z modelu 1024-wymiarowego.

Centroidy: AST dostaje własną rodzinę (`concept_centroids_1024_qwen_ast` i towarzyszące), nie współdzieli `concept_centroids_1024_qwen` z embeddingami plikowymi. Wymiar by się zgadzał, więc Invariant #5 sam tego nie zabrania — zabrania tego sens: centroid nad zbiorem, w którym „ciało funkcji" konkuruje z „całym plikiem", uśrednia dwie różne granulacje w jeden punkt, a tej decyzji nikt nie podjął. ADR-51 formułuje regułę jako „centroidy per tabela embeddingów" i ta lektura ją respektuje.

---

## Odrzucone alternatywy

**Worker jako wątek w `smartfsd`.** Najprostszy w implementacji i eliminuje problem punktu wejścia. Odrzucony przez izolację: `ggml` to kod natywny C++ ze sterownikiem GPU pod spodem, a jego upadek odmontowałby system plików. Dokumentacja projektu już raz podjęła tę samą decyzję dla `smartfs-semantic` i uzasadniła ją tym samym argumentem.

**Worker jako niezależny unit systemd, nie dziecko demona.** Czystszy operacyjnie i tak pewnie będzie wyglądać produkcyjne wdrożenie. Odrzucony **jako jedyny mechanizm**, bo odtwarza dokładnie ten tryb awarii, który ten ADR naprawia: komponent, który działa tylko wtedy, gdy ktoś pamiętał go włączyć. Nadzór przez `smartfsd` daje gwarancję, że działający montaż oznacza działającego workera (albo głośno zapisane `degraded`). Unit systemd może zostać dodany później jako alternatywa dla wdrożeń, które go chcą — wtedy `--no-embeddings` jest sposobem, żeby demon go nie dublował.

**ONNX Runtime na CPU, `ggml` na GPU (stan planu przed tym ADR).** Odrzucony w §1f: przewagi ORT sprowadzają się do dostawców wykonawczych, których ten projekt nie może użyć (QNN, OpenVINO, DirectML, CUDA/TensorRT), więc na sprzęcie w zakresie — Linux, dowolny wendor — nie zostaje ani jedna konfiguracja, na której wygrywa. Drugi silnik kosztowałby drugi format wag, drugą tokenizację, drugi pooling i drugi zestaw trybów awarii, nie kupując ani jednej maszyny więcej. Wraca na stół, jeśli pomiar z §Konsekwencje pokaże `llama.cpp` na CPU istotnie wolniejszym dla tego modelu.

**Własne kernele Vulkan (CubeCL/`krnl`) dla małego VRAM-u.** Odrzucone w §1a, i to nie dlatego, że pomysł jest zły, tylko dlatego, że celuje obok: backend Vulkan w `ggml` **już jest** zestawem kerneli compute w SPIR-V, a mały VRAM rozwiązuje kwantyzacja i częściowy offload warstw — nie szybszy matmul. Własne jądro nie zmniejszy modelu. Pozostaje zarezerwowane dokładnie tam, gdzie zostawił je ADR-55 §4.

**Reguła „jest urządzenie Vulkan, więc licz na GPU".** Odrzucona przez liczby zebrane w ADR-55: na części sprzętu (telefony) ta reguła znaczy „bądź ~15× wolniejszy", a na maszynie bez karty trafiłaby w `llvmpipe`, czyli w ten sam procesor przez sterownik graficzny, meldując przy tym akcelerację. Stąd §1c: rozstrzyga pomiar, a brak pomiaru znaczy CPU.

**Kwantyzacja Q8_0 jako jedyny wariant — albo f16 jako jedyny wariant.** Odrzucone oba. f16 wszędzie odcina od akceleracji karty z 1 GB VRAM (§1e); Q8_0 wszędzie przyjmuje bez pomiaru, że kwantyzacja nie rusza geometrii kosinusów. Wariant idzie za ścieżką wykonania, a próg dopuszczenia Q8_0 jest pomiarem opisanym w §3.

**Trzymanie wag w `--store-path`, żeby „wszystko dane SmartFS-a było w jednym miejscu".** Odrzucone: model nie jest danymi SmartFS-a, a katalog blobów ma dwóch mieszkańców (scrub, sprzątacz), którzy enumerują jego zawartość i mają prawo zakładać, że wszystko w środku jest blobem.

**Pobieranie modelu przy pierwszym uruchomieniu (auto-download).** Odrzucone: demon systemu plików nie odpytuje internetu przy starcie. Pobranie jest osobnym, jawnym krokiem (`scripts/fetch-models.sh`), a jego brak jest błędem głośnym zgodnie z punktem 4.

---

## Konsekwencje

- `smartfs-ai` zyskuje zależności `llama-cpp-2` (FFI do `ggml`) i cel `[[bin]] smartfs-worker`. Build workspace'u zaczyna wymagać CMake i kompilatora C++ — do odnotowania w dokumentacji instalacyjnej i w preflight (etap 0).
- `smartfsd` zyskuje `--model-path`, `--model-file`, `--no-embeddings` oraz nadzór nad procesem potomnym z `PDEATHSIG`. Krok 9 §1.2–1.4 Wielkiego Testu wymaga aktualizacji, a plik gotowości nowego wiersza `embeddings=`.
- Migracja 010: `ast_embeddings_1024_qwen` plus rodzina centroidów dla niej.
- Etap 6 przestaje być testem „czy ktoś włączył workera" i staje się testem tego, czym miał być: czy agent znajduje funkcję, którą ten etap właśnie zapisał. Do rozważenia przy implementacji: asercja, że wektor **nie** jest szumem — np. że funkcja o opisanym zachowaniu wypada wyżej niż losowa inna funkcja z tego samego przebiegu. Bez takiej asercji etap 6 nadal przeszedłby na fałszywym silniku.
- Pozostaje niezmierzone i trzeba to zmierzyć przed uznaniem punktu 1 za zamknięty: **czas jednej inferencji na CPU tej maszyny**. ADR-49 wybrał 0,6B dokładnie dlatego, że zawór antygłodowy (ADR-42/FIX-05) zakłada inferencję rzędu dziesiątek–setek milisekund; jeśli `llama.cpp` na tym CPU da sekundy na plik, wraca livelock, który FIX-05 miał zamknąć. Etap 5 jest właściwym miejscem na tę liczbę.
- Build `ggml` ma jawną macierz flag (§1b). Dwie z nich są łatwe do przeoczenia i kosztowne: `GGML_NATIVE=OFF` (inaczej binarka wywraca się `SIGILL` na starszym CPU niż budujący) oraz `GGML_CPU_ALL_VARIANTS=ON` z `GGML_BACKEND_DL=ON` (inaczej tracimy AVX-512/AMX tam, gdzie są). Do sprawdzenia w preflight razem z obecnością CMake.
- `smartfs-worker` zyskuje `--calibrate` i czyta `<model-path>/backend-calibration.json` (§1c). Etap 0 sprawdza, czy plik istnieje i czy jego klucz pasuje do bieżącego sprzętu — nie po to, żeby oblać, tylko żeby wynik etapu 5 dało się później czytać ze świadomością, na czym liczył.
- Dwa pomiary są teraz warunkiem zamknięcia §1, obok czasu inferencji: **(a)** CPU kontra Vulkan na tej karcie (Bonaire/GCN 2, 768 MiB dzielone z pulpitem — całkiem możliwe, że CPU wygrywa i to jest poprawny wynik, nie porażka); **(b)** zgodność kosinusów i rankingu top-k między f16 a Q8_0 (§3), bo od niej zależy, czy niska półka VRAM w ogóle dostaje akcelerację.
- `search_fulltext` pozostaje niezależnie zepsuty — brak `pg_search` (etap 6, §6.4). Ten ADR go nie dotyczy.

## Stan pobrania (wykonane przy przyjęciu tego ADR)

Repozytorium `Qwen/Qwen3-Embedding-0.6B-GGUF` jest **publiczne i niebramkowane** (`gated: false`) — pobranie nie wymagało tokenu Hugging Face i nie ma powodu, żeby projekt taki token w ogóle przechowywał. Gdyby kiedyś pojawił się model bramkowany, token jest sekretem środowiska, nigdy plikiem w repo.

Artefakty i sumy kontrolne: `SHA256SUMS` w katalogu modelu; te same sumy są przypięte w `scripts/fetch-models.sh`, który odmawia przyjęcia pliku wag niezgodnego z nimi — plik o innej sumie to nie „nowszy upload", tylko inny model, produkujący inną przestrzeń wektorową niż ta, którą opisuje `embedding_models`.

Wszystkie liczby o rozmiarach warstw i architekturze w §1e pochodzą z **nagłówków tych konkretnych plików**, odczytanych przez `scripts/gguf-info.py`, nie ze specyfikacji modelu. Ma to znaczenie przy budżecie VRAM: gdyby wagi kiedyś podmieniono, budżet trzeba przeliczyć z nowego pliku, a nie przepisać stąd.

## Otwarte pytanie

Czy `smartfs-worker` powinien z czasem przejąć również tree-sitter (dziś w `flush()`, na ścieżce zapisu, w `smartfs-fuse`). Argument za: parsowanie to ta sama klasa pracy co embedding — kosztowna, semantyczna, niepotrzebna do potwierdzenia zapisu. Argument przeciw: `ast_nodes` muszą być spójne z wersją, a przeniesienie ich za kolejkę ADR-58 wprowadza drugi asynchroniczny punkt rozjazdu. Nierozstrzygnięte tutaj — wymaga pomiaru, ile z 4,47 ms nieobjętych instrumentacją `close()` zjada tree-sitter.
