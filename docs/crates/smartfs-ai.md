# smartfs-ai — delta w v6.0

← [Mapa crate'ów](../02-crates.md) | Baza: [SmartFS_Architecture_v4_5.md](../base-v4.5-v5.0/SmartFS_Architecture_v4_5.md) §3.7 „smartfs-ai/CLAUDE.md" (bez zmian poza tym, co niżej)

## Zmiany konfiguracyjne (ADR-49)

```toml
[embedding]
default_model = "qwen3-embedding-0.6b"      # było: "all-minilm-l6-v2"
accurate_model = "qwen3-embedding-4b"       # opcjonalnie; było BGE-M3
image_model = "qwen3-vl-embedding-2b"       # NOWE — obrazy, opt-in
```

## Zmiana w `## Embeddings generated per file version`

```markdown
- embeddings_1024_qwen z Qwen3-Embedding-0.6B (is_default=TRUE) — zawsze
- embeddings_384/768 z modeli starszych — tylko dla wierszy, które je już
  mają (kompatybilność wstecz); nowe pliki NIE dostają już embeddings_384
  domyślnie, chyba że jawnie skonfigurowano legacy_model w smartfs.toml
- embeddings_1024_qwen_vl z Qwen3-VL-Embedding-2B **(post-MVP — tabela nie istnieje w migracjach 001-006)** — tylko dla plików, których
  plugin ma "embedding".model wskazujący na model obrazowy (nowość — png.json
  i podobne przestają mieć "embedding": null)
```

## Nowo wstawiony wiersz przy zapisie embeddingu

Każdy INSERT do tabeli embeddingów ustawia teraz też `consolidated = FALSE` (wartość domyślna kolumny) — tego `smartfs-ai` nie musi robić jawnie, bo to `DEFAULT` w schemacie.

**To, co `smartfs-ai` MUSI robić jawnie (poprawka po review):** wypełnić kolumnę `plugin_type` przy każdym INSERT do `embeddings_384`/`embeddings_768`/`embeddings_1024_qwen`/`ast_embeddings_1536`. Kolumna nie ma wartości domyślnej (patrz [migracja 005](../../migrations/005_semantic_consolidation.sql) §2 — `NOT NULL` bez `DEFAULT`), bo `smartfs-ai` i tak zna `special_type` pliku, który właśnie embeduje (przetwarza wiersz `file_versions`, który to pole ma) — zero dodatkowego zapytania, tylko przekazanie już posiadanej wartości do `INSERT`. Bez tego insert do tabeli embeddingów zwróci błąd `NOT NULL violation`, nie zostanie po cichu pominięty.

```rust
// smartfs-ai, miejsce insertu embeddingu — jedna dodatkowa kolumna względem v5.0
sqlx::query!(
    "INSERT INTO embeddings_1024_qwen (version_id, model_id, plugin_type, embedding)
     VALUES ($1, $2, $3, $4)",
    version_id, model_id, file_version.special_type, embedding_vector
).execute(db).await?;
```

Poza tym `smartfs-ai` nie wie nic więcej o `smartfs-semantic` — nie odczytuje `consolidated`, nie importuje crate'a. To jest świadome: worker embeddingów pozostaje dokładnie tak prosty jak w v5.0, plus jedna kolumna, którą i tak trywialnie zna.

## Nowa odpowiedzialność: `search_text` (ADR-54)

Przy przebiegu generic-embedding (embeddings_384/768/1024_qwen — dotyczy WSZYSTKICH plików, v4.5 §10.3) `smartfs-ai` dodatkowo wypełnia `file_versions.search_text`, przez `smartfs-db::set_search_text` — nigdy bezpośrednim `sqlx`:

- Jeśli content da się zdekodować jako poprawny UTF-8 — dokładnie ten tekst (ten sam, który idzie do modelu embeddingowego, żeby nie było dwóch źródeł prawdy o tym, "co system w ogóle widzi" jako treść pliku).
- Jeśli nie (plik binarny) — konkatenacja wartości string ze schematu wtyczki tego pliku (np. `text_metadata`, `color_type` z `png.json`, v4.5 §10.1), rozdzielonych spacją. Jeśli wtyczka nie ma żadnych pól string w schemacie — `search_text = None` (partial index BM25 z migracji 006 to filtruje).

```rust
smartfs_db::set_search_text(db, version_id, search_text).await?;
```

Patrz [ADR-54](../adr/ADR-54-fulltext-search-backend.md) po pełne uzasadnienie.

## Reszta bez zmian

Supervisor loop, tree-sitter, `claim_pending_to_processing`, anti-starvation valve (ADR-42), retry z exponential backoff — wszystko z v5.0 bez zmian.

## Delta ADR-63 — punkt wejścia, silnik, model na dysku

Trzy rzeczy, których ten dokument dotąd nie mówił, bo do przyjęcia [ADR-63](../adr/ADR-63-embedding-model-placement.md) nie były rozstrzygnięte:

### 1. `[[bin]] smartfs-worker` — punkt wejścia, którego nie było

`run_worker_supervisor` (`worker.rs:141`) nie był wołany przez nic: crate nie miał celu binarnego, a ani `smartfsd`, ani `smartfs-cli` nie referowały `smartfs_ai`. Skutkiem była pusta tabela embeddingów przy działającym, kompletnym kodzie workera — wykryte dopiero przez etap 6 Wielkiego Testu.

Teraz `smartfs-ai` ma binarkę `smartfs-worker`, a `smartfsd` uruchamia ją jako **proces potomny** w kroku 9 startu, z `PR_SET_PDEATHSIG` (śmierć demona zabija workera — istotne dla etapu 4, który ubija demona 25 razy na przebieg) i restartem z backoffem. Wyłączenie: `--no-embeddings` na demonie, symetryczne do `--no-semantic`.

Dlaczego osobny proces, a nie wątek: worker ładuje ~1,2 GB wag przez natywny kod C++, docelowo rozmawiający ze sterownikiem GPU. Jego upadek nie może odmontować systemu plików. To ten sam argument, którym [docs/02-crates.md](../02-crates.md) uzasadnia, że `smartfs-semantic` nie zależy od `smartfs-ai` — tu zastosowany o poziom niżej, do przestrzeni adresowej zamiast do grafu zależności.

### 2. Silnik: `ggml`/`llama.cpp`, GGUF — nie ONNX Runtime

`CpuEmbeddingEngine` **nie uruchamiał żadnego modelu**: liczył SHA-256 z tekstu, rozwijał go w `dimensions` kolejnych hashy i normalizował L2. Doc-comment mówił „ONNX model compatibility", a `Cargo.toml` nie miał ani `ort`, ani `tokenizers`. Nazwa i komentarz opisywały rzecz, której nie było.

Struktura nazywa się teraz `HashEmbeddingEngine` i jest opisana jako to, czym jest — deterministyczna atrapa do testów przepływu wierszy, **bez znaczenia semantycznego**, niewybieralna żadną flagą ani konfiguracją.

Prawdziwa inferencja idzie przez `llama-cpp-2` (FFI do `ggml`) na pliku GGUF. Jeden silnik, ale **wiele backendów** — CPU z wyborem zestawu instrukcji w czasie startu i Vulkan — a o tym, który liczy, rozstrzyga pomiar zapisany w `<model-path>/backend-calibration.json`, nie reguła (ADR-63 §1c). Brak pomiaru znaczy CPU.

Worker zyskuje `--calibrate`, który ten pomiar wykonuje. **Nigdy nie odpala się sam przy montowaniu** — demon systemu plików nie staje na benchmark.

Konsekwencja dla buildu: workspace zaczyna wymagać CMake i kompilatora C++, a flagi `ggml` mają znaczenie wydajnościowe (`GGML_NATIVE=OFF`, `GGML_CPU_ALL_VARIANTS=ON`, `GGML_BACKEND_DL=ON`, `GGML_VULKAN=ON`, `GGML_CUDA/HIP=OFF`) — patrz tabela w ADR-63 §1b.

Wariant wag idzie za ścieżką wykonania: f16 na CPU, **Q8_0 na GPU**, bo to ten wariant mieści cały model w karcie z 1 GB VRAM (15,9 MiB na warstwę, 28 warstw, cache KV 112 KiB na token — ADR-63 §1e).

### 3. Model na dysku i zakaz cichego fallbacku

Ścieżka rozstrzygana jak `--store-path`: `--model-path` → `SMARTFS_MODEL_PATH` → `/var/lib/smartfs/models`. Wagi pobiera jawnie `scripts/fetch-models.sh` (weryfikuje sha256 względem sum przypiętych w ADR-63); demon nigdy nie pobiera niczego przy starcie.

Gdy modelu nie ma albo nie da się go załadować: `error` z pełną ścieżką i niezerowy kod wyjścia workera, `embeddings=degraded` w pliku gotowości demona, system plików serwuje dalej. **Żadnego fallbacku na inny model** — model wyznacza przestrzeń wektorową, więc podstawienie innego wstawiłoby do jednej tabeli wektory z dwóch przestrzeni (Invariant #5 czytany na poziomie wiersza).

### 4. Poprawka wymiaru AST

`embed_version` wołał `engine.embed(&node.source, 1536)` i wstawiał wynik do `ast_embeddings_1536` z `model_id` modelu **1024-wymiarowego**. Działało to wyłącznie dzięki atrapie generującej dowolną liczbę wymiarów na żądanie. Po ADR-63 §5: migracja 010 tworzy `ast_embeddings_1024_qwen` (plus własną rodzinę centroidów), a `ast_embeddings_1536` zostaje tabelą `text-embedding-3-large`, dla której powstała w migracji 002.
