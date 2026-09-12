# SmartFS — Roadmap Naprawy Znanych Ograniczeń
## Jak rozwiązać rzeczy które teraz są w ADR jako "świadome decyzje MVP"

Każda z poniższych sekcji opisuje: co dokładnie jest ograniczeniem, dlaczego tak zostało
i konkretne techniczne ścieżki naprawy z szacunkowym nakładem.

---

## 1. OOM przy plikach >RAM (ADR-16)

**Ograniczenie:** Cały plik buforowany w `HashMap<fd, Vec<u8>>` przed hash+compress+store.
Plik 10GB → 10GB+ RAM usage.

**Dlaczego teraz:** FUSE `write()` jest random-access. SHA-256 wymaga sekwencyjnych danych.
Nie możesz hashować chunk po chunku gdy kolejność write() jest dowolna.

**Ścieżka naprawy: FastCDC + read-modify-write**

```
Nowa ścieżka dla plików >MAX_INLINE (np. 256MB):

write(offset, data) →
    ├── Jeśli offset+size mieści się w buforze RAM → bufor jak dotąd (małe pliki)
    └── Jeśli przekracza MAX_INLINE:
          ├── Pobierz current_blob z backendu do pliku tymczasowego
          ├── Nałóż patch (pwrite do temp file)
          └── Przy release(): przetwarzaj temp file przez FastCDC pipeline
```

**FastCDC pipeline dla dużych plików:**
```rust
// Zamiast hash całego pliku — hash przez Content-Defined Chunking
// Chunki ~1MB, granice przez rolling hash (nie stały rozmiar)
// Każdy chunk to osobny blob w store
// file_versions przechowuje Vec<chunk_hash> zamiast jednego content_hash
// Dedup działa na poziomie chunków — zmiana 1 bajtu zmienia tylko 1-2 chunki
```

**Schema change wymagana:**
```sql
CREATE TABLE file_chunks (
    version_id   UUID REFERENCES file_versions(id),
    chunk_index  INT NOT NULL,
    chunk_hash   TEXT NOT NULL REFERENCES blobs(content_hash),
    offset       BIGINT NOT NULL,
    size         BIGINT NOT NULL,
    PRIMARY KEY (version_id, chunk_index)
);
-- file_versions.blob_id staje się NULL dla chunked files
-- file_versions.content_hash = Merkle root hash wszystkich chunków
```

**Nakład:** Weekend 4-5. Wymaga nowego crate `smartfs-cdc` + zmiana schematu.
Do czego nie potrzeba zmieniać API BlobStore — każdy chunk jest normalnym blobem.

---

## 2. FUSE Overhead (USENIX FAST'17: do -83% throughput)

**Ograniczenie:** Każda operacja VFS = przełączenie kontekstu kernel↔userspace × 2.
Metadata-heavy workloads (kompilacja, rsync) odczuwają to najmocniej.

**Dlaczego teraz:** FUSE to świadomy wybór architektury — jedyna opcja bez kernel module.

**Trzy ścieżki naprawy (od tańszej do droższej):**

### 2a. io_uring dla FUSE (post-MVP, weekend 5+)
```toml
# koval.toml — już mamy
[[rules]]
require_io_uring = true
features = ["io_uring"]
```
Linux 6.1+ obsługuje FUSE przez io_uring (`IORING_OP_URING_CMD`). Eliminuje połowę
przełączeń kontekstu dla I/O path. Cargo feature już zarezerwowany.
Crate: `tokio-uring` dla async io_uring w Rust.
**Szacowany zysk:** -25% do -50% overhead zamiast -83%.

### 2b. Splice/sendfile zamiast read+write (zero-copy)
Dla odczytu dużych plików: `splice(fd_blob, NULL, fuse_fd, NULL, size)` zamiast
`read → bufor → write`. Eliminuje kopię danych przez przestrzeń użytkownika.
Wymagane: fuser crate musi obsługiwać splice (sprawdź API).

### 2c. Kernel module (nuclear option, post-v1.0)
Przenieść hot path (lookup, getattr, readdir) do kernel module w Rust (eBPF/kmod).
SmartFS pozostaje jako userspace daemon dla AI pipeline, metadata, MCP.
Tylko POSIX hot path jest w kernelu.
**Nakład:** Kilka miesięcy, duże ryzyko. Nie warto przed v1.0.

### 2d. Profiling-first (rób to najpierw)
Zanim cokolwiek optymalizujesz: zmierz gdzie faktycznie jest bottleneck dla Twojego workloadu.
Dla knowledge base (notatki, kod) metadata overhead jest dużo mniejszy niż dla rsync czy kompilacji.
Użyj: `perf stat`, `strace -c`, `fuse_log_debug` zanim przyjmiesz że overhead jest problemem.

---

## 3. HNSW Memory Wall (ADR-20: ~6-7 GB / mln wektorów 1536d)

**Ograniczenie:** HNSW musi rezydować w RAM. Przy dużym corpus → page thrashing →
latency spike z 2ms do 365ms.

**ADR-41 już rozwiązuje największy driver:** `is_current=TRUE` partial index eliminuje
historyczne wektory. Dla pliku edytowanego 100× z 50 funkcjami: 5000 wierszy → 50 wektorów
w aktywnym indeksie.

**Docelowe rozwiązanie: Type-Partitioned HNSW z Automatic Index Splitting**

### 3a. Indeks per typ pliku (naturalny podział)

Każdy plugin JSON który definiuje `"embedding"` dostaje **własną rodzinę indeksów**.
Pliki bez adaptera JSON nie mają wektorów w ogóle. To wynika wprost z istniejącej
architektury pluginów — rozszerzamy ją o cykl życia indeksu.

```
rust.json   → embedding: 1536d → ast_embeddings_rust_1536_s0, _s1, ...
python.json → embedding: 1536d → ast_embeddings_python_1536_s0, ...
md.json     → embedding: 384d  → embeddings_md_384_s0, ...
png.json    → embedding: null  → brak indeksu (zdjęcia bez semantyki kodu)
```

Zalety:
- Wyszukiwanie kodu Rust nigdy nie trafi na embeddingi Markdown — naturalna izolacja
- Każdy typ ma swój osobny, mały HNSW który łatwo mieści się w RAM
- Nowy typ pliku = nowy plugin JSON = nowa rodzina indeksów, zero zmian w kodzie

### 3b. Automatic Index Splitting — "przerwanie indeksu"

Gdy indeks dla danego typu osiąga próg `max_vectors` (konfigurowalne przy `smartfs init`),
demon zamyka aktywny shard i otwiera nowy. Zamknięty shard jest read-only ale nadal
przeszukiwany przy każdym query (fan-out).

**Kluczowa zasada:** shard jest zawsze zamykany na **granicy pliku**, nigdy w środku.
Gdy `vector_count + vectors_in_next_file > max_vectors` → zamknij shard przed
commitem tego pliku, otwórz nowy shard, wstaw tam cały plik.

```sql
-- Rejestr shardów per typ pliku
CREATE TABLE embedding_index_shards (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    plugin_type  TEXT NOT NULL,        -- "rust", "markdown", "python"
    model_id     UUID NOT NULL REFERENCES embedding_models(id),
    shard_index  INT NOT NULL,         -- 0, 1, 2, ...
    vector_count BIGINT NOT NULL DEFAULT 0,
    max_vectors  BIGINT NOT NULL,      -- próg z smartfs.toml (np. 500_000)
    is_active    BOOLEAN NOT NULL DEFAULT TRUE,
    -- Aktywny = nowe embeddingi tu trafiają
    -- Nieaktywny = read-only, nadal przeszukiwany
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    CONSTRAINT one_active_per_type UNIQUE (plugin_type, model_id, is_active)
        DEFERRABLE INITIALLY DEFERRED  -- swap active→new jest atomowy
);
```

Tabele embeddingów tworzone dynamicznie gdy shard startuje:

```sql
-- Demon tworzy przy starcie nowego sharda:
CREATE TABLE ast_embeddings_rust_1536_s0 (
    ast_node_id UUID NOT NULL REFERENCES ast_nodes(id) ON DELETE CASCADE,
    embedding   vector(1536) NOT NULL,
    is_current  BOOLEAN NOT NULL DEFAULT TRUE,
    PRIMARY KEY (ast_node_id)
);
CREATE INDEX ON ast_embeddings_rust_1536_s0
    USING hnsw (embedding vector_cosine_ops)
    WHERE is_current = TRUE;
-- Każdy shard ma swój HNSW — mały, zawsze w RAM
-- Gdy vector_count ≥ max_vectors: is_active=FALSE, utwórz _s1
```

### 3c. Wyszukiwanie — fan-out po shardach

Demon zna listę shardów z `embedding_index_shards`. Query = `tokio::join_all` po
wszystkich shardach danego typu, merge wyników po similarity score:

```rust
// Pseudokod wyszukiwania dla "rust" code search
let shards = db.get_shards("rust", model_id).await;

let results = futures::future::join_all(
    shards.iter().map(|shard| {
        db.search_shard(shard, &query_vector, limit)
    })
).await;

// Merge i re-rank top-K globalnie
let merged = merge_by_similarity(results, limit);
```

Liczba shardów jest mała (kilka-kilkanaście przez lata użytkowania) — fan-out jest tani.

### 3d. Konfiguracja przy init

```toml
# smartfs.toml — ustawiane przy smartfs init
[vector_index]
max_vectors_per_shard = 500_000   # próg przerwania
# Przy 500k wektorów 1536d: ~3 GB HNSW — bezpieczne na maszynie z 16 GB RAM
# User godzi się że max plik (liczba funkcji) < max_vectors_per_shard
```

### 3e. Dodatkowe optymalizacje (niezależne od shardingu)

```sql
-- Quantization: 2 bajty zamiast 4 na wymiar, ~1-2% gorszy recall
ALTER TABLE ast_embeddings_rust_1536_s0
    ALTER COLUMN embedding TYPE halfvec(1536);
```

```toml
# Qdrant jako pluggable backend gdy pgvector przestaje wystarczać
[vector_store]
backend = "pgvector"   # default, zero deps
# backend = "qdrant"   # gdy corpus >10M wektorów globalnie
```

### 3f. Dlaczego to jest lepsze niż sharding po wielkości dysku

Sharding po wielkości dysku (np. "partycja per 5GB") ma problem przynależności:
plik musi wiedzieć do jakiej partycji należy, a przy wyszukiwaniu nie wiesz w której
partycji są pliki Rust. Type-partitioned sharding nie ma tego problemu — typ pliku
jest znany zawsze, fan-out jest zawsze po wszystkich shardach danego typu.

---

## 4. Cross-version Dedup AST Nodes (ADR-14)

**Ograniczenie:** Każda wersja pliku duplikuje wszystkie ast_nodes.
Plik 100× edytowany z 50 funkcjami = 5000 wierszy ast_nodes z pełnym `source TEXT`.

**ADR-41 rozwiązuje wektory.** Wiersze ast_nodes nadal rosną — ale tylko dla diff,
nie dla search.

**Ścieżka naprawy:**
```sql
-- Nowa tabela: treść funkcji oddzielona od przynależności do wersji
CREATE TABLE ast_content (
    content_hash TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    name         TEXT NOT NULL,
    source       TEXT NOT NULL   -- tu jest duży tekst, jeden raz
);

-- ast_nodes staje się tabelą złączeniową
CREATE TABLE ast_nodes (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    version_id   UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    content_hash TEXT NOT NULL REFERENCES ast_content(content_hash),
    start_line   INT NOT NULL,
    end_line     INT NOT NULL,
    CONSTRAINT unique_ast_node UNIQUE (version_id, content_hash)
);
-- Plik edytowany 100× z 50 funkcjami:
-- ast_content: 50 wierszy (jedna kopia każdej unikalnej funkcji)
-- ast_nodes: 100×50 = 5000 wierszy, ale bez source TEXT (tylko hash + linie)
-- Oszczędność: ~50-90% miejsca na dysku dla ast_nodes
```

**Nakład:** Migracja schematu + update cow_commit. Weekend 5-6.

---

## 5. CID tylko dla plików <256KB (ADR-17)

**Ograniczenie:** Większe pliki nie mają CID → brak IPFS cross-dedup dla dużych plików.

**Ścieżka naprawy: UnixFS DAG-PB chunking**
```rust
// Po implementacji FastCDC (patrz §1):
// Każdy chunk → osobny blob → blob.content_hash → CID
// Merkle root CID dla całego pliku = CID listy chunków
// Kompatybilne z `ipfs add --chunker=fastcdc` gdy to wejdzie do Kubo

// Tymczasowe rozwiązanie: raw-leaves CID
// ipfs add --raw-leaves file.bin  ← hash całego pliku bez UnixFS wrapper
// Kompatybilne tylko gdy plik nie jest chunked w IPFS — ok do 256KB
```

---

## 6. Tree-sitter w Procesie Demona (Punkt Bezpieczeństwa)

**Ograniczenie:** C-parser w `spawn_blocking` = w procesie demona z uprawnieniami `smartfs`.
Exploitowalny bug w tree-sitter = RCE z dostępem do wszystkich blobów.

**Ścieżka naprawy: Osobny worker process z seccomp**
```rust
// Zamiast spawn_blocking w demonbez:
// Demon wysyła bufor przez Unix socket do parser-worker
// Parser-worker: osobny, nisko-uprzywilejowany process
// Po parsowaniu zwraca Vec<AstNode> przez socket

// Parser-worker z seccomp:
// - Może: read(), write(), parse()
// - Nie może: execve(), fork(), socket(), open() do innych plików

// W Rust: seccomp przez `seccomp` crate lub `landlock` crate
```

**Architektura:**
```
smartfs-daemon (uid=smartfs, full access)
    ↓ unix socket (bufor do parsowania)
smartfs-parser (uid=parser, droppped privs)
    seccomp: tylko read/write na socket
    ↓ Vec<AstNode> jako JSON przez socket
smartfs-daemon (kontynuuje cow_commit)
```

**Nakład:** Weekend 6. Głównie plumbing (IPC), nie logika parsowania.
**Priority:** Niski dla single-user personal storage. Wysoki dla multi-user server deployment.

---

## 7. BGE-M3 CPU Cost (560M parametrów)

**Ograniczenie:** BGE-M3 jest opt-in ale nawet jako opt-in jest ciężki na CPU.
Każdy plik = inference 560M parametrów = kilka sekund na CPU bez GPU.

**Ścieżki naprawy:**

### 7a. Quantization (GGUF/INT8)
BGE-M3 Q4_K_M przez llama.cpp lub ONNX INT8 quantization.
Inference ~4× szybszy, ~4× mniej RAM, minimalny spadek jakości.
```toml
[embedding]
bge_m3_quantization = "int8"   # domyślnie, dużo szybsze na CPU
```

### 7b. Batching
Zamiast inference per-plik: zbieraj batch N plików, jeden forward pass.
Worker już ma batch logic — dodaj batched inference przez ONNX.
```rust
// Zamiast:
for version_id in &batch { embed_version(version_id).await; }
// Zrób:
let texts: Vec<String> = batch.iter().map(|id| get_text(id)).collect();
let embeddings = model.encode_batch(&texts).await; // jeden forward pass
```

### 7c. GPU (jeśli dostępne)
KOVAL rule dla `min_gpu_vram_gb = 4.0` już w `koval.toml`.
ONNX Runtime z `CUDAExecutionProvider` — bez zmian w kodzie embeddingu.
Inference BGE-M3 na GPU: ~100ms zamiast ~5s na CPU.

---

## 8. Podsumowanie — Kolejność Implementacji Post-MVP

| Priorytet | Ograniczenie | Fix | Weekend |
|-----------|-------------|-----|---------|
| 🔴 Wysoki | OOM >RAM (ADR-16) | FastCDC + read-modify-write | W4-W5 |
| 🔴 Wysoki | AST cross-version dedup (ADR-14) | `ast_content` tabela | W5 |
| 🟡 Średni | FUSE overhead | io_uring (koval feature) | W5-W6 |
| 🟡 Średni | CID dla dużych plików (ADR-17) | UnixFS DAG-PB po FastCDC | W6 |
| 🟡 Średni | Tree-sitter w demonie | Parser subprocess + seccomp | W6 |
| 🟢 Niski | HNSW memory wall (ADR-20) | IVFFlat opt-in + halfvec | W7+ |
| 🟢 Niski | BGE-M3 CPU cost | Quantization + batching | W7+ |
| 🟢 Niski | Qdrant dla >10M wektorów | Pluggable vector backend | v1.0+ |

ADR-41 (is_current) i ADR-42 (anti-starvation) są już w v4.5 — to były najtańsze
i najbardziej impactful poprawki z tej listy.
