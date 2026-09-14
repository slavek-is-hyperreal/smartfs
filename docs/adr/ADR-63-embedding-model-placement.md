# ADR-63 — Gdzie mieszka model embeddingowy: proces, dysk, format artefaktu

← [Mapa ADR](../01-architecture.md) | Poprzednicy: [ADR-49](ADR-49-qwen-default-model.md) (wybór modelu), [ADR-55](ADR-55-gpu-acceleration.md) (Vulkan, reuse-before-rewrite), [ADR-42/47](../base-v4.5-v5.0/SmartFS_v4.5_to_v5.0_fixes.md) (zawór antygłodowy) | Wynika z pomiaru: [etap 6 Wielkiego Testu](../testing/the-great-smartfs-test.md)

**Status:** Przyjęty 2026-09-14. ADR-49 wybrał *który* model. Ten ADR odpowiada na trzy pytania, których ADR-49 nie postawił, a bez których model nie może zacząć działać: **w jakim procesie** żyje, **w jakim katalogu** leżą jego wagi i **w jakim formacie** je pobieramy. Rozstrzyga przy okazji Otwarte pytanie z ADR-55 (jeden silnik inferencji czy dwa).

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

### 1. Jeden silnik inferencji: `ggml`/`llama.cpp`, format GGUF — dla CPU i dla GPU

ADR-55 zostawił to jako Otwarte pytanie („czy ujednolicić CPU i GPU pod jeden silnik, zamiast ONNX Runtime na CPU + `ggml` na GPU"). Rozstrzygamy je na **jeden silnik**, i to jest moment, w którym decyzja jest najtańsza: ścieżka CPU nie istnieje jeszcze w kodzie, więc nie ma czego migrować. Odkładając to, dopisalibyśmy `ort` + `tokenizers` + własną implementację poolingu tylko po to, żeby za jakiś czas napisać to drugi raz pod `ggml`.

Za ujednoliceniem:

- **Jeden format artefaktu.** `Qwen/Qwen3-Embedding-0.6B-GGUF` to release oficjalny (Qwen), nie społecznościowa konwersja. Ścieżka ONNX wymagałaby `onnx-community/Qwen3-Embedding-0.6B-ONNX` — konwersji strony trzeciej, z osobnym łańcuchem zaufania dla wag.
- **Tokenizer jest w pliku.** GGUF niesie tokenizer razem z wagami. ONNX wymaga osobnego `tokenizer.json` plus crate'a `tokenizers` — dwa artefakty, które mogą się rozjechać wersjami, i nic tego nie sprawdza.
- **Pooling pisalibyśmy sami.** Qwen3-Embedding używa poolingu last-token z EOS plus normalizacji L2, a dla zapytań prefiksu instrukcyjnego. Przy ONNX to nasz kod (i nasz błąd, gdy się pomylimy — cicho, bo wektory zawsze *jakieś* wyjdą). `llama.cpp` ma to w API embeddingowym, przetestowane na tym konkretnym modelu.
- **`gpu_acceleration = "vulkan" | "cpu"` przestaje być przełącznikiem między silnikami**, a staje się flagą jednego silnika. ADR-55 §7 zakładał to pole; przy dwóch silnikach oznaczałoby ono „inny kod, inny format modelu, inne błędy", co jest znacznie mocniejszą obietnicą niż nazwa sugeruje.
- **Zasada reuse-before-rewrite z ADR-55** stosuje się tu tak samo jak do GPU.

Cena, zapisana uczciwie: `llama-cpp-2` (bindingi FFI) wymaga CMake i kompilacji C++ w buildzie workspace'u — wolniej niż `ort` z pobieraną binarką. Akceptujemy to, bo ADR-55 i tak wprowadza tę zależność dla ścieżki GPU; alternatywą jest mieć **obie**.

Konsekwencją jest zmiana w [ADR-49](ADR-49-qwen-default-model.md): zapis „ONNX Runtime jako silnik CPU-baseline, odziedziczony bez zmian z v4.5 §3.7/§18" przestaje obowiązywać. ONNX Runtime nie pojawi się w projekcie w ogóle — nigdy nie był zaimplementowany, więc to nie jest wycofanie działającego kodu, tylko wycofanie planu.

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

Domyślny wariant to **f16 (1,2 GB)**, nie Q8_0 (640 MB). Kwantyzacja psuje geometrię przestrzeni wektorowej, a to jest jedyne, co embedding sprzedaje; 600 MB oszczędności na maszynie z 298 GB wolnego to zły handel. Q8_0 zostaje pobrany i udokumentowany jako wariant dla sprzętu, gdzie f16 się nie mieści — wybór przez `--model-file`, nigdy automatyczny, bo automatyczna podmiana modelu za plecami zmienia przestrzeń wektorową bazy bez śladu w `embedding_models`.

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

**ONNX Runtime na CPU, `ggml` na GPU (stan planu przed tym ADR).** Odrzucony w punkcie 1: dwa silniki, dwa formaty wag, dwie implementacje poolingu, dwa zestawy trybów awarii — za coś, czego nikt nie zmierzył jako szybsze. Gdyby pomiar z §Konsekwencje pokazał, że `llama.cpp` na CPU jest istotnie wolniejszy niż ONNX Runtime dla tego modelu, ta decyzja wraca na stół z liczbami.

**Kwantyzacja Q8_0 jako domyślna.** Odrzucona: oszczędza 600 MB na maszynie, która ma 298 GB wolnego, kosztem jedynej rzeczy, którą embedding dostarcza.

**Trzymanie wag w `--store-path`, żeby „wszystko dane SmartFS-a było w jednym miejscu".** Odrzucone: model nie jest danymi SmartFS-a, a katalog blobów ma dwóch mieszkańców (scrub, sprzątacz), którzy enumerują jego zawartość i mają prawo zakładać, że wszystko w środku jest blobem.

**Pobieranie modelu przy pierwszym uruchomieniu (auto-download).** Odrzucone: demon systemu plików nie odpytuje internetu przy starcie. Pobranie jest osobnym, jawnym krokiem (`scripts/fetch-models.sh`), a jego brak jest błędem głośnym zgodnie z punktem 4.

---

## Konsekwencje

- `smartfs-ai` zyskuje zależności `llama-cpp-2` (FFI do `ggml`) i cel `[[bin]] smartfs-worker`. Build workspace'u zaczyna wymagać CMake i kompilatora C++ — do odnotowania w dokumentacji instalacyjnej i w preflight (etap 0).
- `smartfsd` zyskuje `--model-path`, `--model-file`, `--no-embeddings` oraz nadzór nad procesem potomnym z `PDEATHSIG`. Krok 9 §1.2–1.4 Wielkiego Testu wymaga aktualizacji, a plik gotowości nowego wiersza `embeddings=`.
- Migracja 010: `ast_embeddings_1024_qwen` plus rodzina centroidów dla niej.
- Etap 6 przestaje być testem „czy ktoś włączył workera" i staje się testem tego, czym miał być: czy agent znajduje funkcję, którą ten etap właśnie zapisał. Do rozważenia przy implementacji: asercja, że wektor **nie** jest szumem — np. że funkcja o opisanym zachowaniu wypada wyżej niż losowa inna funkcja z tego samego przebiegu. Bez takiej asercji etap 6 nadal przeszedłby na fałszywym silniku.
- Pozostaje niezmierzone i trzeba to zmierzyć przed uznaniem punktu 1 za zamknięty: **czas jednej inferencji na CPU tej maszyny**. ADR-49 wybrał 0,6B dokładnie dlatego, że zawór antygłodowy (ADR-42/FIX-05) zakłada inferencję rzędu dziesiątek–setek milisekund; jeśli `llama.cpp` na tym CPU da sekundy na plik, wraca livelock, który FIX-05 miał zamknąć. Etap 5 jest właściwym miejscem na tę liczbę.
- `search_fulltext` pozostaje niezależnie zepsuty — brak `pg_search` (etap 6, §6.4). Ten ADR go nie dotyczy.

## Stan pobrania (wykonane przy przyjęciu tego ADR)

Repozytorium `Qwen/Qwen3-Embedding-0.6B-GGUF` jest **publiczne i niebramkowane** (`gated: false`) — pobranie nie wymagało tokenu Hugging Face i nie ma powodu, żeby projekt taki token w ogóle przechowywał. Gdyby kiedyś pojawił się model bramkowany, token jest sekretem środowiska, nigdy plikiem w repo.

Artefakty i sumy kontrolne: `SHA256SUMS` w katalogu modelu.

## Otwarte pytanie

Czy `smartfs-worker` powinien z czasem przejąć również tree-sitter (dziś w `flush()`, na ścieżce zapisu, w `smartfs-fuse`). Argument za: parsowanie to ta sama klasa pracy co embedding — kosztowna, semantyczna, niepotrzebna do potwierdzenia zapisu. Argument przeciw: `ast_nodes` muszą być spójne z wersją, a przeniesienie ich za kolejkę ADR-58 wprowadza drugi asynchroniczny punkt rozjazdu. Nierozstrzygnięte tutaj — wymaga pomiaru, ile z 4,47 ms nieobjętych instrumentacją `close()` zjada tree-sitter.
