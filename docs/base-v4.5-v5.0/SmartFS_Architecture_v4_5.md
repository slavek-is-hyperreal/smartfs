# SmartFS: Unified Knowledge Storage Layer
## Dokument Architektoniczny v4.5 (RFC / System Design)

**Status:** Gotowy do implementacji  
**Autorzy:** Ekipa z Hackerspace & AI Coach  
**Peer review:** Opus 4.8 (v3.0→v3.1 fixes), Gemini (v3.1 implementation)  
**Wersja:** 3.5 — synteza v3.0 + poprawki Opusa + poprawki Gemini + własne korekty  

**Zmiany względem v3.0:**
- Fix kolejności DDL: `file_versions` przed tabelami embeddingów
- `ast_embeddings_1536` jako osobna tabela (jedyna działająca opcja — FK do `ast_nodes.id`)
- `setval` po seedzie root inode (fix BIGSERIAL collision przy pierwszym `touch`)
- `processing` w enumie `processing_status` + `claim_pending_to_processing`
- `pg_advisory_xact_lock` dla TOCTOU race w dedupie
- `external_path` przeniesiony do `file_versions` per wersja (nie per inode)
- Parser tree-sitter w `flush()`, commit CoW w `release()` — spójność POSIX
- Worker z debounce + supervisor loop (nie ginie po pierwszej anulacji)
- `embeddings_768` — pełne DDL (nie tylko komentarz)
- `ast_nodes` bez kolumny `status` — worker ustawia clean przez `ast_embeddings`
- ADR-14 poprawiony: jasne stwierdzenie że cross-version dedup node'ów NIE istnieje w MVP
- Sekcja 21: usunięty tryumfalizm, fakty bronią się same
- `virtual size` = 0 zamiast -1 (u64 overflow → eksabajty)

---

## 1. Filozofia i Wizja Projektu

Tradycyjne systemy plików (Ext4, NTFS, APFS) traktują dane jako ślepe ciągi bajtów zorganizowane w sztywne, hierarchiczne drzewo katalogów. Cała semantyka, kontekst oraz historia zmian są spychane do warstwy aplikacji.

**SmartFS to nie jest filesystem. To jest Unified Knowledge Storage Layer.**

System rozdziela trzy warstwy które tradycyjnie są pomieszane:

- **Tożsamość danych** — co to jest, co znaczy, jak się zmieniało (PostgreSQL)
- **Fizyczne bajty** — gdzie leżą i w jakiej formie (Storage Backend)
- **Interfejs dostępu** — jak aplikacje i AI mogą to konsumować (FUSE + MCP)

Kluczowa filozofia: **plik nie jest ścieżką. Plik jest swoją treścią.** Ścieżka to tylko alias. Hash to tożsamość.

---

## 2. Architektura Systemu (Split-Path Storage)

```
         [ Użytkownik / Narzędzia CLI / System operacyjny / Aplikacje ]
                                    │
                                    ▼ (POSIX API: ls, cat, cp, mkdir)
               ┌──────────────────────────────────────────────────┐
               │              SmartFS Daemon (Rust)               │
               └───────────┬──────────────────────┬──────────────┘
                           │                      │
            (Metadane SQL) │                      │ (Odczyt/Zapis Blobów)
                           ▼                      ▼
              ┌─────────────────────┐   ┌────────────────────────────┐
              │  PostgreSQL (v16+)  │   │   Storage Backend (Trait)  │
              │  - pgvector         │   │  Local │ IPFS │ S3 │ SFTP  │
              │  - JSONB Indexes    │   │  Wiki  │ NASA │ Notion │…  │
              │  - CID / Hashes     │   └────────────────────────────┘
              └─────────────────────┘
                           │
                           ▼
              ┌─────────────────────┐
              │   MCP Server        │
              │  (JSON-RPC / stdio) │
              └─────────────────────┘
```

---

## 3. Cargo Workspace — Struktura Projektu

```
smartfs/
├── Cargo.toml
├── CLAUDE.md                   ← ROOT: mapa crate'ów + 5 niezmienników
├── .claude/settings.json       ← exclude: target/, *.onnx, /var/smartfs/blobs/
├── .claudeignore
├── migrations/
│   ├── 001_core_schema.sql     ← inode_registry, file_versions
│   ├── 002_embedding_models.sql
│   ├── 003_embeddings.sql      ← embeddings_384/768/1536, ast_embeddings_1536
│   └── 004_ast_nodes.sql
├── crates/
│   ├── smartfs-schema/         ← typy współdzielone, zero logiki
│   ├── smartfs-store/          ← BlobStore trait + LocalDiskStore
│   ├── smartfs-db/             ← cała logika PostgreSQL, CoW, dedup
│   ├── smartfs-compress/       ← zstd pipeline, hash-before-compress
│   ├── smartfs-fuse/           ← FUSE daemon
│   ├── smartfs-ai/             ← embedding worker, tree-sitter, ONNX
│   ├── smartfs-mcp/            ← MCP server JSON-RPC/stdio
│   ├── smartfs-ipfs/           ← IpfsStore + CID helpers
│   └── smartfs-cli/            ← smartfs-cli binary
└── plugins/
    ├── generic.json
    ├── png.json
    ├── rust.json
    └── python.json
```

### 3.1. ROOT CLAUDE.md

```markdown
# SmartFS — Root Context

## Crate map
- smartfs-schema   — shared types, no logic
- smartfs-store    — BlobStore trait + LocalDiskStore
- smartfs-db       — all PostgreSQL logic, CoW, dedup
- smartfs-compress — zstd pipeline, hash-before-compress
- smartfs-fuse     — FUSE daemon ← read crate CLAUDE.md before touching
- smartfs-ai       — embedding worker, tree-sitter, ONNX
- smartfs-mcp      — MCP server JSON-RPC/stdio
- smartfs-ipfs     — IpfsStore + CID helpers
- smartfs-cli      — CLI binary

## Invariants — never break these
1. content_hash = SHA-256(original bytes BEFORE compression) — never after
2. Every content change creates a new file_versions row (CoW) — never mutates blob
3. The only legal path to data is through the daemon — ext4 is a dumb blob store
4. Migrations are explicit in migrations/ — no CREATE TABLE in runtime code
5. Never mix embedding dimensions across queries — 384 with 384, 1536 with 1536

## Per-crate commands
cargo test -p <crate-name>
cargo clippy -p <crate-name>
```

---

### 3.2. smartfs-schema/CLAUDE.md

```markdown
# smartfs-schema — Shared Types

## Owns exclusively
All types shared between crates. No logic, no I/O, no SQL.

## What lives here
- Uuid re-exports
- ProcessingStatus enum mirroring the DB enum exactly
- FileMode, Uid, Gid newtype wrappers
- BlobId = Uuid newtype
- ContentHash = String newtype (hex SHA-256)
- VersionNumber = i32 newtype
- SmartFsError enum (top-level error type, all crates use this)

## Never
- No async, no tokio
- No sqlx, no fuser, no ort
- No business logic of any kind
- No Default impl that could mask a missing value

## Commands
cargo test -p smartfs-schema
cargo clippy -p smartfs-schema
```

---

### 3.3. smartfs-store/CLAUDE.md

```markdown
# smartfs-store — BlobStore Trait + LocalDiskStore

## Owns exclusively
The BlobStore trait and all backend implementations.
No SQL. No hashing. No compression. Receives already-compressed bytes.

## Trait signature (do not change without updating all impls)
- put(uuid, data: &[u8]) -> Result<()>
- get(uuid, external_path: Option<&str>) -> Result<Vec<u8>>
- put_stream(uuid, stream: AsyncRead, size_hint: Option<u64>) -> Result<()>
- get_stream(uuid, external_path: Option<&str>) -> Result<impl AsyncRead>
- delete(uuid) -> Result<()>
- exists(uuid) -> Result<bool>

## LocalDiskStore
- Blobs stored at {root}/{uuid} — flat directory, no subdirectories
- get with external_path=Some(p) reads from p directly, ignores uuid
- put is write-then-rename (atomic on same filesystem)
- root must be owned by smartfs system user, mode 0700

## Never
- No SHA-256 or any hashing — caller hashes before calling put
- No zstd or any compression — caller compresses before calling put
- No sqlx — zero database awareness
- No knowledge of inode_registry or file_versions

## Commands
cargo test -p smartfs-store
cargo clippy -p smartfs-store
```

---

### 3.4. smartfs-compress/CLAUDE.md

```markdown
# smartfs-compress — Hash + Compress Pipeline

## Owns exclusively
The pipeline: raw bytes → (SHA-256, compressed bytes, sizes).
Called by smartfs-db before storing. Never called by FUSE directly.

## Key function
write_blob_streaming(source: AsyncRead, level: i32, store: &dyn BlobStore)
  -> Result<(Uuid, ContentHash, orig_size: u64, compressed_size: u64)>

## Ordering — non-negotiable
1. Feed each chunk to Sha256::update()   ← hash from ORIGINAL bytes
2. Feed each chunk to zstd::Encoder      ← compress after hashing
3. Call store.put(uuid, compressed)      ← store compressed bytes

Reversing steps 1 and 2 breaks global dedup and IPFS CID compatibility.

## Chunk size
Use config constant CHUNK_SIZE (default 4MB, feature-gated by KOVAL).
Never hardcode 4MB inline.

## MVP constraint
FUSE buffers the entire file in RAM before calling this pipeline.
Streaming is used for CLI write and virtual adapters only.
Do not attempt streaming hash during FUSE write() callbacks.

## Never
- No SQL
- No FUSE awareness
- No knowledge of inode_registry or file_versions
- Do not choose compression level — receive it as parameter from caller

## Commands
cargo test -p smartfs-compress
cargo clippy -p smartfs-compress
```

---

### 3.5. smartfs-db/CLAUDE.md

```markdown
# smartfs-db — PostgreSQL Logic, CoW, Dedup

## Owns exclusively
All SQL. Every database read and write goes through this crate.
No other crate touches sqlx directly.

## Core operations
- inode_lookup(parent_id, name) -> Option<Inode>
- inode_create(parent_id, name, is_dir, uid, gid, mode) -> Inode
- inode_delete(id) — deferred if open_fd_count > 0
- cow_commit(inode_id, blob_id, content_hash, size, compressed_size,
             external_path, ast_nodes) -> VersionId
- version_get(inode_id, version_number?) -> FileVersion
- version_history(inode_id) -> Vec<FileVersion>
- dedup_check(content_hash) -> Option<BlobId>
- claim_pending_to_processing(limit) -> Vec<VersionId>
- mark_clean(version_id)
- mark_failed(version_id)
- revert_to_pending(version_id)

## Dedup — blobs table (replaces advisory lock since v4.5)
No pg_advisory_xact_lock. No hashtext. Use blobs table with UNIQUE on content_hash.
INSERT INTO blobs ... ON CONFLICT DO UPDATE SET refcount+1 RETURNING blob_id, inserted.
inserted=TRUE  → you are first → compress + store.put.
inserted=FALSE → reuse blob_id, discard your uuid.
UNIQUE constraint on full SHA-256 hex = zero false contention, zero I/O window.

## AST parse BEFORE BEGIN (not inside transaction)
When plugin has ast=true: run tree-sitter BEFORE opening any transaction.
spawn_blocking + C parser must NOT hold a row lock while running.
flush() does throwaway parse for EACCES only.

## CoW transaction shape (v4.5)
// Step 1 — outside any transaction
hash = SHA-256(data)
if ast=true: ast_nodes = tree_sitter_parse(data)  ← spawn_blocking, NO lock held
result = INSERT INTO blobs ON CONFLICT ... RETURNING blob_id, inserted
if inserted: compress(data) → store.put(result.blob_id, compressed)

// Step 2 — short transaction, SQL only, no I/O
BEGIN;
  SELECT id FROM inode_registry WHERE id=$inode_id FOR UPDATE;
  prev_version = latest version_id for this inode
  INSERT INTO file_versions (blob_id=result.blob_id, ...);
  INSERT INTO ast_nodes ... ON CONFLICT DO NOTHING;
  UPDATE inode_registry SET current_blob_id=result.blob_id, ...;
  UPDATE ast_embeddings_1536 SET is_current=FALSE
      WHERE ast_node_id IN (SELECT id FROM ast_nodes WHERE version_id=prev_version);
COMMIT;

## ast_embeddings_1536 — current version only
Worker sets is_current=FALSE for previous version embeddings on new commit.
search_functions uses partial index WHERE is_current=TRUE.
History diff still works through ast_nodes (all versions kept).

## Reaper — call at daemon startup before spawning worker
UPDATE file_versions SET status='pending' WHERE status='processing'

## Root inode seed (migration only)
INSERT inode_registry (ino=1, ...) then SELECT setval('inode_registry_ino_seq', 1).
Without setval: first non-seeded INSERT gets nextval=1 → UNIQUE violation on ino.

## Never
- No fuser or FUSE types
- No zstd or hashing
- No CREATE TABLE or ALTER TABLE in runtime code — migrations only
- No raw SQL strings in application code — sqlx query macros only

## Commands
cargo test -p smartfs-db  (requires Docker PostgreSQL)
cargo clippy -p smartfs-db
```

---

### 3.6. smartfs-fuse/CLAUDE.md

```markdown
# smartfs-fuse — FUSE Daemon

## Owns exclusively
FUSE operation handlers. Translates VFS calls into smartfs-db + smartfs-store calls.
No SQL. No hashing. No compression inline — delegates everything.

## Required operations (MVP)
- lookup        — smartfs-db::inode_lookup(parent_ino, name)
- getattr       — smartfs-db::inode_get(ino) → FileAttr
- readdir       — synthesise . and .. explicitly; stable ORDER BY ino
- create        — not mkdir+open; this is how touch and echo > work
- open / read / write / flush / release
- setattr       — chmod, chown, truncate, utimens all route here
- rename        — atomic; collision with existing target removed in same transaction
- unlink / rmdir
- statfs        — return real values from blob dir statvfs; never ENOSYS

## inode number mapping
FUSE kernel uses u64 ino. Schema has BIGSERIAL ino column.
root = FUSE_ROOT_ID = 1.
Never pass UUID to kernel. Never pass ino as UUID to db.

## write() is random-access
Buffer entire file in a per-fd HashMap<u64, Vec<u8>> keyed by fh.
Hash + compress + store only in release(). Never in write().

## flush vs release
flush():
  - May be called multiple times (every close() on every dup'd fd)
  - Run tree-sitter parse (spawn_blocking — C parser blocks thread)
  - Return EACCES if syntax error and no --force flag
  - Store parsed AST nodes in per-fd state for release() to use
  - Do NOT commit to database here

release():
  - Called exactly once per open()
  - kernel ignores error return — cannot signal EACCES here
  - Run CoW commit: call smartfs-db::cow_commit() with buffered data + AST nodes
  - Clear per-fd buffer

## rename and editor atomic saves
Also run syntax check on rename() when destination matches plugin ast=true.
rename over existing target: remove target inode in same transaction.

## unlink on open file
Track open_fd_count per inode_id in daemon state.
Delay CASCADE delete until open_fd_count reaches 0 in release().

## Mount options — both required
MountOption::DefaultPermissions  ← kernel enforces uid/gid/mode
MountOption::AllowOther          ← mountpoint visible to all users

## Never
- No sqlx — only smartfs-db public API
- No zstd or SHA-256 inline — delegates to smartfs-compress via smartfs-db
- No CREATE TABLE
- No -1 as size in FileAttr — u64 cast of -1 = 18 EB

## Commands
cargo test -p smartfs-fuse
cargo clippy -p smartfs-fuse
```

---

### 3.7. smartfs-ai/CLAUDE.md

```markdown
# smartfs-ai — Embedding Worker + Tree-sitter + ONNX

## Owns exclusively
The async embedding pipeline: pending file versions → embeddings in DB.
Tree-sitter parsing for AST extraction.
ONNX Runtime inference for vector generation.

## Worker loop (supervisor — never exits)
loop:
  activity_monitor.wait_for_idle(500ms)   ← debounce on WRITE activity only
  batch = db.claim_pending_to_processing(BATCH_SIZE)
  if batch.is_empty(): sleep(2s); continue
  for version_id in batch:
    select!:
      embed_version(version_id) → db.mark_clean() or db.mark_failed()
      activity_monitor.write_activity() → db.revert_to_pending(); break
  // loop continues — worker never exits

## claim_pending_to_processing
UPDATE status='processing' atomically with SKIP LOCKED.
Prevents double-processing on restart or multiple worker instances.

## Tree-sitter
Always run in tokio::task::spawn_blocking — C parser, not Send, blocks thread.
Returns Vec<AstNode>: kind, name, start_line, end_line, source, content_hash.

## Embeddings generated per file version
- embeddings_384 with all-MiniLM-L6-v2 (is_default=TRUE) — always
- embeddings_768 with BGE-M3 — only if configured in smartfs.toml
- ast_embeddings_1536 per ast_node — only if code_model configured

## ONNX inference
Always run in spawn_blocking — CPU-bound, blocks async executor.
Model files loaded once at daemon start, path from config — never hardcode.

## Retry
On error: mark_failed() increments retry_count.
Exponential backoff: skip versions where retry_count > N until backoff expires.

## Anti-starvation
Before wait_for_idle: check should_embed_despite_activity().
Override debounce if: oldest pending > 60s OR backlog > 1000 rows.
Without this: cargo build or bulk import permanently starves the worker.

## ast_embeddings_1536 — current version only
After embedding ast_nodes for a new version:
UPDATE ast_embeddings_1536 SET is_current=FALSE for previous version nodes.
search_functions uses partial index WHERE is_current=TRUE — always bounded size.

## Never
- No FUSE awareness
- No direct SQL — only smartfs-db public API
- No synchronous inference in async fn — always spawn_blocking
- BGE-M3 is opt-in, not default — never generate embeddings_768 unconditionally
- Never use ? operator in worker loop — log errors and continue

## Commands
cargo test -p smartfs-ai
cargo clippy -p smartfs-ai
```

---

### 3.8. smartfs-mcp/CLAUDE.md

```markdown
# smartfs-mcp — MCP Server (JSON-RPC over stdio)

## Owns exclusively
MCP protocol handling. Tool dispatch. JSON-RPC 2.0 framing over stdio.
No SQL inline — all data access through smartfs-db public API.

## Tools exposed
search_semantic(query, model?, limit, type_filter?)
  → embeddings_384 or embeddings_768; always filter by model_id
search_functions(query, language?, kind?)
  → ast_embeddings_1536 cosine search; returns ast_node results
get_file_history(path)
  → Vec<FileVersion> with parent_version_id chain
query_by_metadata(filter: JsonValue)
  → structured query on special_data JSONB; never raw SQL
find_by_hash(content_hash) → FileVersion or None
get_file_content(path, version?) → bytes (lazy fetch if on_prem=FALSE)
find_broken_files() → Vec<FileVersion> where status='syntax_error'
diff_functions(path, v1, v2) → added/changed/removed AstNode lists

## Protocol
JSON-RPC 2.0 over stdin/stdout. One request per line, one response per line.
Never use HTTP or TCP — stdio only.

## Destructive operations
Return confirmation token before executing delete or overwrite.
Do not execute until token confirmed in next call.

## Never
- No raw SQL — only smartfs-db public API
- No embedding inference
- No FUSE awareness
- Do not mix embedding dimensions in search results

## Commands
cargo test -p smartfs-mcp
cargo clippy -p smartfs-mcp
```

---

### 3.9. smartfs-ipfs/CLAUDE.md

```markdown
# smartfs-ipfs — IpfsStore + CID Helpers

## Owns exclusively
IpfsStore implementation of BlobStore trait.
CID computation from content_hash.
Kubo daemon HTTP API client.

## IpfsStore
- get: HTTP GET {gateway}/ipfs/{cid}
- put: POST to Kubo /api/v0/add → returns CID → /api/v0/pin/add
- exists: HTTP HEAD {gateway}/ipfs/{cid}
- Kubo endpoint from config — never hardcode localhost:5001

## CID computation
Only for size < 256 * 1024 (256 KB). Return None for larger files.
CID v1: multibase(base32, multicodec(raw, multihash(sha2-256, hash)))
Use the `cid` crate — do not implement multihash manually.

## Never
- No SQL
- No compression or hashing — receives content_hash from caller
- No CID for files >= 256KB — return None, not an error
- No fallback to local store on IPFS failure — propagate error

## Commands
cargo test -p smartfs-ipfs  (requires local Kubo daemon or mock)
cargo clippy -p smartfs-ipfs
```

---

### 3.10. smartfs-cli/CLAUDE.md

```markdown
# smartfs-cli — CLI Binary

## Owns exclusively
CLI argument parsing. Thin delegation layer — no logic inline.

## Commands
smartfs-cli write [--force] <path>           read stdin, write to SmartFS
smartfs-cli cat <path> [--version N]         print file content
smartfs-cli history <path>                   list versions with hashes
smartfs-cli search <query>                   semantic search
smartfs-cli import --mode [index|cow] <dir>  register existing directory
smartfs-cli diff <path> <v1> <v2>            function-level diff

## --force flag
Passes override_syntax_check=true to smartfs-db.
Sets status='syntax_error' in file_versions. Never silently ignores errors.

## Never
- No SQL inline — smartfs-db only
- No embedding inference
- No FUSE calls — CLI operates through smartfs-db directly
- No interactive prompts except destructive confirmation

## Commands
cargo test -p smartfs-cli
cargo clippy -p smartfs-cli
```

---

## 4. Storage Backend — Modularny Wzorzec

### 4.1. Trait w Rust

Overlay mode wymaga `external_path` w sygnaturze `get` — bez tego OverlayStore nie może działać w ramach abstrakcji BlobStore.

```rust
#[async_trait]
trait BlobStore: Send + Sync {
    // Convenience API dla małych plików
    async fn put(&self, uuid: Uuid, data: &[u8]) -> Result<()>;
    async fn get(&self, uuid: Uuid, external_path: Option<&str>) -> Result<Vec<u8>>;

    // Streaming API dla dużych plików — stały footprint RAM
    async fn put_stream(
        &self,
        uuid: Uuid,
        stream: impl AsyncRead + Send + Unpin,
        size_hint: Option<u64>,
    ) -> Result<()>;
    async fn get_stream(
        &self,
        uuid: Uuid,
        external_path: Option<&str>,
    ) -> Result<impl AsyncRead + Send + Unpin>;

    async fn delete(&self, uuid: Uuid) -> Result<()>;
    async fn exists(&self, uuid: Uuid) -> Result<bool>;
}
```

Każdy backend implementuje zgodnie ze swoją naturą:
- `LocalDiskStore` → `tokio::io::copy` do pliku
- `OverlayStore` → otwiera `external_path` bezpośrednio
- `S3Store` → multipart upload dla plików >5GB
- `IpfsStore` → chunked API Kubo daemon

### 4.2. Konfiguracja backendów w bazie

```sql
CREATE TABLE storage_backends (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name         TEXT NOT NULL,
    backend_type TEXT NOT NULL,    -- "local", "ipfs", "s3", "virtual_wiki", "overlay"
    config       JSONB NOT NULL,
    is_virtual   BOOLEAN DEFAULT FALSE,
    priority     INT DEFAULT 0,
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
```

---

## 5. Kompresja — Aplikacyjna, Konfigurowalna Per-Plik

### 5.1. Kluczowa zasada: hash PRZED kompresją

```
Dane oryginalne → SHA-256 ← HASH TUTAJ
       │
       ▼
  Kompresja zstd (poziom z bazy)
       │
       ▼
  Zapis bloba
```

Dwa pliki o tej samej treści, skompresowane różnymi poziomami → identyczny hash → globalny dedup działa.

### 5.2. MVP: bufor całego pliku w RAM

FUSE `write()` jest random-access. SHA-256 wymaga sekwencyjnych danych. Na MVP buforujemy cały plik w RAM do `flush()`/`release()`. Pliki > RAM spowodują OOM (ADR-16, świadoma decyzja).

### 5.3. Streaming pipeline dla sekwencyjnego I/O

Dla CLI write i wirtualnych adapterów (nie FUSE):

```rust
const CHUNK_SIZE: usize = 4 * 1024 * 1024; // dobierane przez KOVAL

async fn write_blob_streaming(
    source: impl AsyncRead + Send + Unpin,
    compression_level: i32,
    store: &dyn BlobStore,
) -> Result<(Uuid, String, u64, u64)> {
    let mut hasher  = Sha256::new();
    let mut encoder = zstd::Encoder::new(Vec::new(), compression_level)?;
    let mut size    = 0u64;
    let mut buf     = vec![0u8; CHUNK_SIZE];

    loop {
        let n = source.read(&mut buf).await?;
        if n == 0 { break; }
        let chunk = &buf[..n];
        hasher.update(chunk);
        encoder.write_all(chunk)?;
        size += n as u64;
    }

    let hash       = hex::encode(hasher.finalize());
    let compressed = encoder.finish()?;
    let uuid       = Uuid::new_v4();
    store.put(uuid, &compressed).await?;

    Ok((uuid, hash, size, compressed.len() as u64))
}
```

---

## 6. Schemat Bazy Danych (PostgreSQL v16+)

**Uwaga o kolejności:** `file_versions` musi być zdefiniowane PRZED tabelami embeddingów. Migracje są numerowane tak żeby to wymusić.

```sql
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TYPE processing_status AS ENUM (
    'clean',        -- embedding gotowy
    'pending',      -- czeka na worker
    'processing',   -- zarezerwowane przez workera (SKIP LOCKED)
    'failed',       -- błąd; patrz retry_count
    'syntax_error'  -- zapis z flagą --force, AST niedostępne
);

-- ─────────────────────────────────────────────────────────
-- migrations/001_core_schema.sql
-- ─────────────────────────────────────────────────────────

CREATE TABLE inode_registry (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    ino          BIGSERIAL UNIQUE NOT NULL, -- u64 dla FUSE kernel
    parent_id    UUID REFERENCES inode_registry(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    is_dir       BOOLEAN NOT NULL DEFAULT FALSE,

    -- Atrybuty POSIX
    uid          INT NOT NULL DEFAULT 1000,
    gid          INT NOT NULL DEFAULT 1000,
    mode         INT NOT NULL DEFAULT 33188, -- 0o100644
    size         BIGINT NOT NULL DEFAULT 0,
    nlink        INT NOT NULL DEFAULT 1,
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),

    -- Storage
    current_blob_id   UUID,
    backend_id        UUID REFERENCES storage_backends(id),
    on_prem           BOOLEAN NOT NULL DEFAULT TRUE,
    compression_level SMALLINT NOT NULL DEFAULT 3,
    versioning_enabled BOOLEAN NOT NULL DEFAULT TRUE,

    CONSTRAINT blob_or_empty_or_virtual CHECK (
        is_dir = TRUE
        OR on_prem = FALSE
        OR versioning_enabled = FALSE   -- tryb index: plik istnieje in-place, blob_id=NULL, size>0
        OR (current_blob_id IS NULL AND size = 0)  -- pusty plik (touch/create)
        OR current_blob_id IS NOT NULL
    ),
    CONSTRAINT unique_name_per_directory UNIQUE (parent_id, name)
);

-- Root inode: ino=1, is_dir=TRUE
-- Używamy jawnego INSERT z ino=1, potem przesuwamy sekwencję
-- żeby kolejny automatyczny INSERT dostał 2, nie 1.
INSERT INTO inode_registry (ino, name, is_dir, uid, gid, mode)
    VALUES (1, '', TRUE, 0, 0, 16877); -- 0o40755
SELECT setval('inode_registry_ino_seq', 1);
-- setval(seq, 1) ustawia "ostatnio zwrócona wartość = 1"
-- nextval zwróci 2 dla pierwszego INSERT bez jawnego ino

CREATE INDEX idx_inode_parent ON inode_registry(parent_id);
CREATE INDEX idx_inode_lookup ON inode_registry(parent_id, name);
CREATE INDEX idx_inode_ino    ON inode_registry(ino);

-- file_versions PRZED tabelami embeddingów — wymagane przez FK
CREATE TABLE file_versions (
    id             UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    inode_id       UUID NOT NULL REFERENCES inode_registry(id) ON DELETE CASCADE,
    version_number INT NOT NULL,
    created_at     TIMESTAMP WITH TIME ZONE DEFAULT NOW(),

    -- Dane fizyczne
    blob_id         UUID,
    size            BIGINT NOT NULL,
    compressed_size BIGINT,

    -- Overlay: external_path per WERSJA, nie per inode
    -- NULL dla natywnych wersji SmartFS
    -- ścieżka do oryginału dla wersji importowanych
    external_path  TEXT,

    -- Tożsamość treści (hash PRZED kompresją)
    content_hash   TEXT NOT NULL,
    -- CID tylko dla plików <256KB; większe wymagają UnixFS DAG-PB (post-MVP)
    cid            TEXT,
    ipfs_pinned    BOOLEAN DEFAULT FALSE,

    -- Plugin
    special_type   TEXT NOT NULL DEFAULT 'generic',
    special_data   JSONB NOT NULL DEFAULT '{}'::jsonb,

    -- Pipeline AI
    status         processing_status NOT NULL DEFAULT 'pending',
    retry_count    INT NOT NULL DEFAULT 0,

    -- DAG historii (NULL dla pierwszej wersji)
    -- Uwaga: w MVP historia jest zawsze liniowa — DAG to fundament pod post-MVP branching
    parent_version_id UUID REFERENCES file_versions(id),

    CONSTRAINT unique_version_per_inode UNIQUE (inode_id, version_number)
);

CREATE INDEX idx_versions_inode   ON file_versions(inode_id);
CREATE INDEX idx_versions_hash    ON file_versions(content_hash);
CREATE INDEX idx_versions_cid     ON file_versions(cid) WHERE cid IS NOT NULL;
CREATE INDEX idx_versions_special ON file_versions USING gin (special_data);
CREATE INDEX idx_versions_pending ON file_versions(status)
    WHERE status IN ('pending', 'processing');

-- ─────────────────────────────────────────────────────────
-- Tabela blobs — punkt serializacji dla dedupu
-- Zastępuje pg_advisory_xact_lock + hashtext (v4.0)
-- Powód: advisory lock zwalnia się przy tymczasowym COMMIT (I/O okno),
-- UNIQUE constraint działa na 256 bitach SHA-256, jest atomowy, daje refcount pod GC
-- ─────────────────────────────────────────────────────────
CREATE TABLE blobs (
    content_hash TEXT PRIMARY KEY,          -- pełne SHA-256 hex (256 bit, zero kolizji)
    blob_id      UUID NOT NULL,             -- UUID pliku w blob store
    backend_id   UUID REFERENCES storage_backends(id),
    refcount     INT NOT NULL DEFAULT 1,    -- liczba file_versions wskazujących na ten blob
    size         BIGINT NOT NULL,           -- rozmiar oryginalny (przed kompresją)
    compressed_size BIGINT,
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
-- refcount > 0 zawsze → GC usuwa tylko blobs WHERE refcount = 0
-- INSERT ... ON CONFLICT (content_hash) DO UPDATE SET refcount = blobs.refcount + 1

-- ─────────────────────────────────────────────────────────
-- migrations/002_embedding_models.sql
-- ─────────────────────────────────────────────────────────

CREATE TABLE embedding_models (
    id         UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name       TEXT NOT NULL UNIQUE,
    dimensions INT NOT NULL,
    version    TEXT,
    is_local   BOOLEAN NOT NULL DEFAULT TRUE,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

INSERT INTO embedding_models (name, dimensions, version, is_local, is_default) VALUES
    ('all-MiniLM-L6-v2',      384,  '1.0', TRUE,  TRUE),
    ('BGE-M3',                 768,  '1.0', TRUE,  FALSE),
    ('text-embedding-3-large', 1536, '1.0', FALSE, FALSE);

-- ─────────────────────────────────────────────────────────
-- migrations/003_embeddings.sql
-- Wszystkie FK do file_versions — musi istnieć wcześniej
-- ─────────────────────────────────────────────────────────

CREATE TABLE embeddings_384 (
    version_id UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    model_id   UUID NOT NULL REFERENCES embedding_models(id),
    embedding  vector(384) NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (version_id, model_id)
);
CREATE INDEX idx_embeddings_384_hnsw
    ON embeddings_384 USING hnsw (embedding vector_cosine_ops);

CREATE TABLE embeddings_768 (
    version_id UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    model_id   UUID NOT NULL REFERENCES embedding_models(id),
    embedding  vector(768) NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (version_id, model_id)
);
CREATE INDEX idx_embeddings_768_hnsw
    ON embeddings_768 USING hnsw (embedding vector_cosine_ops);

-- ─────────────────────────────────────────────────────────
-- migrations/004_ast_nodes.sql
-- ─────────────────────────────────────────────────────────

CREATE TABLE ast_nodes (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    version_id   UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,   -- "function", "struct", "impl", "class"
    name         TEXT NOT NULL,   -- "write_blob_streaming"
    start_line   INT NOT NULL,
    end_line     INT NOT NULL,
    source       TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT NOW(),

    -- UNIQUE zapewnia idempotencję workera AST
    -- Uwaga: NIE zapewnia cross-version dedup (każda wersja ma własne wiersze)
    -- Plik 100× edytowany z 50 funkcjami = 5000 wierszy — OK na MVP
    -- Cross-version dedup wymagałby osobnej tabeli ast_content(content_hash UNIQUE)
    -- + tabeli złączeniowej version_ast_nodes — świadoma rezygnacja w MVP
    CONSTRAINT unique_ast_node UNIQUE (version_id, content_hash)
);

CREATE INDEX idx_ast_nodes_version ON ast_nodes(version_id);
CREATE INDEX idx_ast_nodes_name    ON ast_nodes(kind, name);
CREATE INDEX idx_ast_nodes_hash    ON ast_nodes(content_hash);

-- Dedykowana tabela embeddingów dla AST nodes
-- FK do ast_nodes.id (NIE do file_versions.id — to był bug w v3.0)
CREATE TABLE ast_embeddings_1536 (
    ast_node_id UUID NOT NULL REFERENCES ast_nodes(id) ON DELETE CASCADE,
    model_id    UUID NOT NULL REFERENCES embedding_models(id),
    embedding   vector(1536) NOT NULL,
    is_current  BOOLEAN NOT NULL DEFAULT TRUE,  -- FALSE przy nowej wersji pliku
    created_at  TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (ast_node_id, model_id)
    -- ON CONFLICT DO NOTHING wymagane przy INSERT — retry workera po crashu
);
-- Indeks HNSW tylko na bieżących wektorach — eliminuje eksplozję przy 100 wersjach × 50 funkcji
CREATE INDEX idx_ast_embeddings_1536_hnsw
    ON ast_embeddings_1536 USING hnsw (embedding vector_cosine_ops)
    WHERE is_current = TRUE;

-- Przy commit nowej wersji pliku:
-- UPDATE ast_embeddings_1536 SET is_current = FALSE
--     WHERE ast_node_id IN (
--         SELECT id FROM ast_nodes WHERE version_id = $prev_version_id
--     )
-- search_functions używa tylko is_current=TRUE; historia diff nadal działa przez ast_nodes
```

---

## 7. Content-Addressable Storage i IPFS

### 7.1. Filozofia

> Plik nie jest ścieżką. Plik jest swoją treścią. Hash to tożsamość.

### 7.2. Trzy tryby egzystencji pliku

```
on_prem=TRUE,  external_path=NULL  → natywny blob w SmartFS
on_prem=TRUE,  external_path=SET   → overlay/import (ta wersja)
on_prem=FALSE, cid=EXISTS          → wirtualny, globalny (IPFS/adapter)
```

### 7.3. Dedup przez tabelę `blobs` (zastępuje advisory lock od v4.5)

**Dlaczego advisory lock nie wystarczał (v4.0 bug):**
`pg_advisory_xact_lock` jest transaction-scoped. Przy dedup miss potrzebowaliśmy tymczasowego COMMIT (żeby zwolnić lock przed I/O `store.put`). W oknie między tym COMMIT a kolejnym BEGIN drugi writer tej samej treści brał lock, widział miss i też zapisywał blob. Advisory lock zwężał okno, ale go nie zamykał.

**Fix: `blobs(content_hash PRIMARY KEY)` z `ON CONFLICT`:**

```sql
-- Ścieżka zapisu — atomowy punkt serializacji
INSERT INTO blobs (content_hash, blob_id, backend_id, size, compressed_size)
VALUES ($hash, $uuid, $backend_id, $orig_size, $compressed_size)
ON CONFLICT (content_hash) DO UPDATE
    SET refcount = blobs.refcount + 1
RETURNING blob_id, (xmax = 0) AS inserted;
-- inserted=TRUE  → jesteś pierwszy → zapisz blob do store
-- inserted=FALSE → ktoś inny już zapisał → użyj zwróconego blob_id, swój uuid odrzuć
```

Decyzja "kto pisze blob" zapada atomowo w INSERT — bez okna I/O, bez advisory lock, bez hashtext. UNIQUE constraint działa na 256 bitach SHA-256. Refcount jest przy okazji — GC usuwa tylko `WHERE refcount = 0`.

**Kompletny flow dedupu:**
```
hash = SHA-256(data)                         ← poza transakcją

result = INSERT INTO blobs ... ON CONFLICT RETURNING blob_id, inserted

if inserted:
    compressed = zstd::encode(data, level)
    store.put(result.blob_id, compressed)    ← I/O poza transakcją
    blob_id = result.blob_id
else:
    blob_id = result.blob_id                 ← reużyj, zero I/O

BEGIN;
  SELECT id FROM inode_registry WHERE id=$inode_id FOR UPDATE;
  INSERT INTO file_versions (blob_id=$blob_id, content_hash=$hash, ...);
  UPDATE inode_registry SET current_blob_id=$blob_id, ...;
COMMIT;
```

Przy crashu po `store.put` a przed `INSERT file_versions`: blob w store nie ma referencji w file_versions, ale ma wiersz w `blobs` (refcount=1). GC sprząta przez `WHERE refcount=0` po usunięciu wpisu z blobs. Spójność zachowana.

### 7.4. CID — ograniczenie do small files

CID obliczany tylko dla `size < 256 * 1024`. Większe pliki wymagają drzewa UnixFS/DAG-PB — post-MVP (ADR-17).

---

## 8. Wirtualne Adaptery — "Pliki Których Nie Masz"

```
/mnt/smartfs/
├── projekty/          ← on_prem=TRUE,  LocalDiskStore
├── wikipedia/
│   └── rust.md        ← on_prem=FALSE, WikipediaAdapter, size z indeksu
└── arxiv/
    └── attention.pdf  ← on_prem=FALSE, ArxivAdapter
```

### 8.1. Pre-computed embeddings dla Wikipedii

- **Cohere/wikipedia-22-12** — angielska Wikipedia, gotowe wektory 768d
- **Upstash/wikipedia-2024-06-bge-m3** — 11 języków (w tym PL), BGE-M3, ~144M wektorów

### 8.2. Virtual file size

`getattr` zwraca `size` zapisany przy indeksowaniu (Wikipedia/arXiv podają rozmiar w headerach HTTP). Jeśli nieznany — zwróć `0`, nie `-1`. Nigdy nie rób HEAD request per `getattr` — za wolne i pada offline.

---

## 9. Semantyczne Wyszukiwanie — Unified Query

```sql
-- Wyszukiwanie po modelu 384d — nigdy nie mieszaj modeli
SELECT
    i.name,
    i.on_prem,
    s.name AS source,
    1 - (e.embedding <=> $query_vector) AS similarity
FROM embeddings_384 e
JOIN file_versions  v ON e.version_id  = v.id
JOIN inode_registry i ON v.inode_id    = i.id
JOIN storage_backends s ON i.backend_id = s.id
WHERE v.status = 'clean'
  AND e.model_id = $model_384_id
ORDER BY e.embedding <=> $query_vector
LIMIT 20;
```

---

## 10. System Wtyczek Typów Specjalnych

Pluginy w British English z opisami pól — standard dla agentów semantycznych analizujących typy danych między sobą.

### 10.1. Plugin bez embeddingu (png.json)

```json
{
  "type": "png",
  "description": "PNG image file containing raster graphics data with optional metadata.",
  "match_extensions": [".png"],
  "schema": {
    "width":  { "type": "integer", "description": "Image width in pixels." },
    "height": { "type": "integer", "description": "Image height in pixels." },
    "color_type": { "type": "string", "description": "Colour mode, e.g. RGBA, RGB, Greyscale." },
    "has_alpha": { "type": "boolean", "description": "Whether image contains a transparency channel." },
    "text_metadata": { "type": "object", "description": "Key-value pairs from PNG tEXt chunks, e.g. Software, Author." }
  },
  "ast": false,
  "embedding": null
}
```

### 10.2. Plugin z AST i embeddingiem (rust.json)

```json
{
  "type": "rust",
  "description": "Rust source code file containing compiled, memory-safe system-level code.",
  "match_extensions": [".rs"],
  "schema": {
    "language": { "type": "string", "description": "Programming language identifier as detected by tree-sitter." },
    "node_count": { "type": "integer", "description": "Total number of top-level AST nodes extracted from this file." },
    "has_syntax_errors": { "type": "boolean", "description": "Whether tree-sitter detected any syntax errors during parsing." },
    "has_unsafe": { "type": "boolean", "description": "Whether this file contains any unsafe blocks." }
  },
  "ast": true,
  "embedding": {
    "model": "text-embedding-3-large",
    "dimensions": 1536,
    "description": "Semantic embedding of full source code, optimised for code similarity search."
  }
}
```

### 10.3. Trzy warstwy embeddingów

| Warstwa | Tabela | Model | Dla kogo |
|---|---|---|---|
| Ogólna szybka | `embeddings_384` | all-MiniLM-L6-v2 (domyślny, offline) | Wszystkie pliki |
| Ogólna dokładna | `embeddings_768` | BGE-M3 multilingual (opt-in, offline) | Wszystkie pliki |
| Kod per-funkcja | `ast_embeddings_1536` | patrz niżej | AST nodes |

**Model dla `ast_embeddings_1536`:**
- Domyślnie: `all-MiniLM-L6-v2` (384d w tabeli `ast_embeddings_384`) — offline, zawsze działa
- Opt-in: `text-embedding-3-large` (1536d) — wymaga `OPENAI_API_KEY`
- Opt-in lokalny: `CodeBERT` lub `StarEncoder` przez ONNX — offline, lepszy dla kodu niż MiniLM

Bez skonfigurowanego modelu kodowego `search_functions` zwraca wyniki z `embeddings_384` (plik-level). Funkcja działa od razu offline — tylko jakość code-specific search jest niższa. `code_model` w `smartfs.toml` odblokowuje pełną granularność per-funkcja.

---

## 11. AST — Kod jako Pierwszoklasowy Obywatel

### 11.1. Pipeline zapisu — podział flush/release i ścieżki zapisu

Parsowanie AST odbywa się w `smartfs-db::cow_commit` — wspólne dla **obu ścieżek zapisu** (FUSE i CLI). `flush()` wykonuje tylko throwaway parse dla walidacji EACCES. Tree-sitter parse odbywa się **przed** BEGIN transakcją — nie trzyma row locka przez czas parsowania C-parsera.

```
── Ścieżka FUSE ─────────────────────────────────────────────────────────
FUSE write(offset, data) → bufor RAM (cały plik)

flush():                               ← EACCES dociera tutaj
    ├── tree-sitter parse(bufor)       ← spawn_blocking, throwaway (tylko walidacja)
    │     ├── Błąd + brak --force → EACCES zwrócone do procesu → koniec
    │     └── OK → AST nodes odrzucone (cow_commit parsuje ponownie)
    └── Nie zapisuj nic do bazy tutaj

release():                             ← błąd ignorowany przez kernel
    └── db::cow_commit(inode_id, bufor, override=false)

── Ścieżka CLI ──────────────────────────────────────────────────────────
smartfs-cli write → db::cow_commit(inode_id, bufor, override_syntax_check)

── smartfs-db::cow_commit (wspólne) ─────────────────────────────────────
cow_commit(inode_id, data, override_syntax_check):

    // KROK 1: Wszystko POZA transakcją (nie trzymamy row locka przez I/O ani parse)
    ├── hash = SHA-256(data)
    │
    ├── Jeśli plugin ma ast=true i nie override:
    │     └── tree-sitter parse(data)   ← spawn_blocking, PRZED BEGIN
    │           ├── Błąd → zwróć błąd wcześnie (CLI: exit 1, FUSE: już po EACCES)
    │           └── OK  → ast_nodes = Vec<AstNode> w pamięci
    │
    ├── // Atomowy punkt serializacji dedupu (blobs table)
    ├── INSERT INTO blobs (content_hash, blob_id, ...) ON CONFLICT DO UPDATE SET refcount+1
    │     RETURNING blob_id, inserted
    │
    └── Jeśli inserted=TRUE:
          compress(data) → store.put(blob_id, compressed)   ← I/O poza transakcją

    // KROK 2: Krótka transakcja — tylko metadane
    BEGIN;
      SELECT id FROM inode_registry WHERE id=$inode_id FOR UPDATE;
      // Poprzednia wersja — do is_current=FALSE w ast_embeddings
      prev_version = SELECT id FROM file_versions
                         WHERE inode_id=$inode_id ORDER BY version_number DESC LIMIT 1;
      INSERT INTO file_versions (blob_id, content_hash, status='pending', parent_version_id=prev);
      INSERT INTO ast_nodes ON CONFLICT DO NOTHING;
      UPDATE inode_registry SET current_blob_id=blob_id, size=orig_size, updated_at=NOW();
      // Zdezaktualizuj wektory poprzedniej wersji (is_current → FALSE)
      UPDATE ast_embeddings_1536 SET is_current = FALSE
          WHERE ast_node_id IN (SELECT id FROM ast_nodes WHERE version_id = prev_version.id);
    COMMIT;
```

**Kluczowe właściwości (v4.5):**
- Tree-sitter parse PRZED BEGIN — row lock nie trzymany przez czas parsowania C-parsera
- Dedup przez `blobs` — bez advisory lock, bez okna I/O
- `ast_embeddings_1536` — tylko bieżąca wersja ma `is_current=TRUE`; HNSW index jest partial
- Transakcja jest krótka (tylko SQL, zero I/O) — minimalizuje czas trzymania row locka

### 11.2. Wymuszony zapis flagą

```bash
# Normalny zapis — walidacja wymagana
echo "fn broken( {" > /mnt/smartfs/src/main.rs
# → EACCES: syntax error — use 'smartfs-cli write --force' to override

# Wymuszony
smartfs-cli write --force /mnt/smartfs/src/main.rs < broken.rs
# → status='syntax_error', ast_nodes=∅
```

### 11.2a. setattr O_TRUNC — nie tworzy wersji

`echo > file` robi `open(O_TRUNC) → setattr(size=0) → write → release`.

`setattr(size=0)` **nie commituje nowej wersji** — tylko zeruje per-fd bufor w pamięci daemona. Wersja powstaje dopiero w `release()` gdy jest co zapisać. Bez tego `echo > file` tworzyłoby dwie wersje: pustą (setattr) i właściwą (release).

W implementacji `setattr` z flagą `FATTR_SIZE` i `size=0`: wyczyść `fd_buffers[fh]`, nie wywołuj `cow_commit`.

### 11.3. Walidacja przy rename (atomic save edytorów)

Vim/VSCode/Emacs zapisują przez `write-temp-then-rename`. Walidacja musi się odpalić też w `rename()` gdy nazwa docelowa pasuje do pluginu z `"ast": true`.

### 11.4. Diff na poziomie funkcji

```sql
-- Które funkcje zmieniły się między wersjami?
SELECT a.kind, a.name, a.content_hash AS v1, b.content_hash AS v2
FROM ast_nodes a
JOIN ast_nodes b ON a.name = b.name AND a.kind = b.kind
WHERE a.version_id = $v1
  AND b.version_id = $v2
  AND a.content_hash != b.content_hash;

-- Które zniknęły?
SELECT name, kind FROM ast_nodes WHERE version_id = $v1
EXCEPT
SELECT name, kind FROM ast_nodes WHERE version_id = $v2;
```

### 11.5. Obsługiwane języki

```toml
tree-sitter            = "0.22"
tree-sitter-rust       = "0.21"
tree-sitter-python     = "0.21"
tree-sitter-javascript = "0.21"
tree-sitter-typescript = "0.21"
tree-sitter-go         = "0.21"
```

Nowy język = nowa zależność + nowy plik JSON pluginu. Zero zmian w kodzie demona.

---

## 12. Natywne Wersjonowanie (Copy-on-Write)

### 12.1. Mechanizm zapisu

Szczegółowy flow `cow_commit` — patrz §11.1. Poniżej skrót dla kontekstu FUSE.

```
echo "Nowa idea" > /mnt/smartfs/notatki.md

write(offset, data) → bufor RAM (cały plik, MVP)

flush() →
    ├── [ast=true]: throwaway parse → EACCES jeśli błąd + brak --force
    └── Nie zapisuj nic do bazy

release() →
    └── db::cow_commit(inode_id, bufor, override=false):
          ├── hash = SHA-256(bufor)                          ← poza TX
          ├── [ast=true]: tree-sitter parse(bufor)           ← spawn_blocking, poza TX
          ├── INSERT INTO blobs ON CONFLICT DO UPDATE        ← atomowy dedup
          │     inserted=TRUE  → compress + store.put        ← I/O poza TX
          │     inserted=FALSE → reużyj blob_id z blobs
          ├── BEGIN;
          │     ├── SELECT inode FOR UPDATE
          │     ├── INSERT file_versions (blob_id, hash, 'pending', parent_id)
          │     ├── INSERT ast_nodes ON CONFLICT DO NOTHING
          │     ├── UPDATE inode_registry
          │     └── UPDATE ast_embeddings_1536 SET is_current=FALSE (poprzednia wersja)
          └── COMMIT;
```

**Właściwości v4.5:**
- Zero advisory locków — `blobs PRIMARY KEY` jest punktem serializacji
- Parse przed BEGIN — row lock nie trzymany przez czas parsowania
- Transakcja krótka (tylko SQL) — minimalizuje czas locka
- `is_current=FALSE` na poprzednich wektorach — HNSW index zawsze mały

### 12.2. Asynchroniczny Preemptible AI Pipeline

Worker rezerwuje zadania przez zmianę statusu na `processing` — eliminuje podwójne przetwarzanie przy wielu workerach lub restarcie.

```rust
// Reaper: przy starcie demona zresetuj wiersze które utknęły w 'processing'
// (crash workera zostawia je bez właściciela na zawsze)
async fn reset_stale_processing(db: &Pool) {
    if let Err(e) = sqlx::query!(
        "UPDATE file_versions SET status = 'pending' WHERE status = 'processing'"
    ).execute(db).await {
        tracing::warn!("reaper reset failed: {e}");
    }
}

// Rezerwacja — atomowa, odporna na race
async fn claim_pending(db: &Pool, limit: i64) -> Vec<Uuid> {
    // Zwraca Vec<Uuid>, nie Result — błąd logujemy i zwracamy pusty Vec
    match sqlx::query_scalar!(
        r#"
        UPDATE file_versions
        SET status = 'processing'
        WHERE id IN (
            SELECT id FROM file_versions
            WHERE status = 'pending'
            ORDER BY created_at ASC
            FOR UPDATE SKIP LOCKED
            LIMIT $1
        )
        RETURNING id
        "#,
        limit
    ).fetch_all(db).await {
        Ok(ids) => ids,
        Err(e)  => { tracing::error!("claim_pending failed: {e}"); vec![] }
    }
}

// Supervisor loop — worker nie ginie po żadnym błędzie
// Uruchom reset_stale_processing() PRZED spawn
let worker = tokio::spawn(async move {
    loop {
        // Debounce: czekaj na ciszę I/O zapisu (nie getattr/lookup)
        activity_monitor.wait_for_idle(Duration::from_millis(500)).await;

        let batch = claim_pending(&db, BATCH_SIZE).await;  // bez ?
        if batch.is_empty() {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        let mut preempted = false;
        for version_id in &batch {
            if preempted { break; }
            tokio::select! {
                result = embed_version(&db, &store, *version_id) => {
                    match result {
                        Ok(_)  => {
                            // błąd mark_clean logujemy, nie przerywamy pętli
                            if let Err(e) = mark_clean(&db, *version_id).await {
                                tracing::error!("mark_clean {version_id}: {e}");
                            }
                        }
                        Err(e) => {
                            tracing::warn!("embed {version_id} failed: {e}");
                            let _ = increment_retry(&db, *version_id).await;
                        }
                    }
                }
                _ = activity_monitor.write_activity_detected() => {
                    preempted = true;
                }
            }
        }

        if preempted {
            // Cofnij CAŁY pozostały batch — nie tylko bieżący wiersz
            // Bez tego reszta batcha zostaje w 'processing' do restartu
            for version_id in &batch {
                let _ = revert_to_pending(&db, *version_id).await;
            }
        }
        // loop kontynuuje — worker nigdy nie wychodzi
    }
});
```

**Pięć decyzji projektowych w workerze:**
- `reset_stale_processing` przy starcie — reaper dla wierszy po crashu
- `claim_pending` zwraca `Vec` nie `Result` — błąd DB nie zabija workera
- Wszystkie operacje DB w pętli: `match` + log, bez `?`
- Preempcja rewertuje **cały batch**, nie tylko bieżący wiersz
- **Anti-starvation valve**: override debounce gdy backlog stary lub duży

```rust
// Anti-starvation — dodaj do wait_for_idle
async fn should_embed_despite_activity(db: &Pool) -> bool {
    // Nie czekaj na ciszę jeśli:
    // 1. Najstarszy pending jest starszy niż MAX_AGE (np. 60s)
    // 2. Backlog pending > MAX_BACKLOG (np. 1000 wierszy)
    let oldest_pending_age: Option<f64> = sqlx::query_scalar!(
        "SELECT EXTRACT(EPOCH FROM (NOW() - MIN(created_at)))
         FROM file_versions WHERE status = 'pending'"
    ).fetch_one(db).await.ok().flatten();

    let backlog: i64 = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM file_versions WHERE status = 'pending'"
    ).fetch_one(db).await.unwrap_or(0);

    oldest_pending_age.unwrap_or(0.0) > 60.0 || backlog > 1000
}

// W supervisor loop:
loop {
    if !should_embed_despite_activity(&db).await {
        activity_monitor.wait_for_idle(Duration::from_millis(500)).await;
    }
    // reszta pętli bez zmian...
}
```

Zapobiega permanentnemu głodzeniu przy `cargo build` lub bulk import — po 60s lub 1000 wierszach worker embeduje mimo aktywności zapisu.

---

## 13. MCP Server (AI Interface)

```
┌────────────────────────┐              ┌────────────────────────┐
│     Klient LLM         │ ←── MCP ──> │   SmartFS MCP Server   │
│  (Cursor / Claude API) │  JSON-RPC   │   (stdio)              │
└────────────────────────┘  over stdio └────────────────────────┘
```

### 13.1. Eksponowane narzędzia

**`search_semantic(query, model?, limit, type_filter?)`**
Cosine search w `embeddings_384` lub `embeddings_768`. Zawsze filtruje po `model_id`.

**`search_functions(query, language?, kind?)`**
Cosine search w `ast_embeddings_1536` — per funkcja/obiekt, nie per plik.

**`get_file_history(path)`**
Historia wersji z `content_hash` i `parent_version_id` per wersja.

**`query_by_metadata(jsonb_filter)`**
Strukturalne zapytanie po `special_data`. Przyjmuje strukturę — nie surowy SQL.

**`find_by_hash(content_hash)`**
Znajdź plik lokalnie i przez CID/IPFS.

**`get_file_content(path, version?)`**
Lazy fetch jeśli `on_prem=FALSE` lub `external_path` w danej wersji.

**`find_broken_files()`**
`WHERE status='syntax_error'` — agent wie gdzie są problemy.

**`diff_functions(path, v1, v2)`**
SQL diff na poziomie funkcji między wersjami.

---

## 14. Model Uprawnień

### 14.1. Zasada fundamentalna

> **Jedyna legalna droga do danych to przez demona. Ext4 jest głupim blob store, nie warstwą uprawnień.**

### 14.2. Dedykowany system user

```bash
useradd --system --no-create-home --shell /usr/sbin/nologin smartfs
mkdir -p /var/smartfs/blobs
chown smartfs:smartfs /var/smartfs/blobs
chmod 0700 /var/smartfs/blobs
```

Demon jako `smartfs` — ma dostęp. Root ma dostęp (kontrakt Linuksa). Nikt inny nie czyta blobów bezpośrednio na ext4.

### 14.3. Wirtualne pliki

```sql
uid=0, gid=0, mode=0o100444  -- r--r--r-- domyślnie
```

`chmod` zapisuje nowy mode do bazy — `0o444` to zachowawczy default, nie restrykcja na stałe.

### 14.4. Montowanie — systemd service

```ini
[Unit]
Description=SmartFS Daemon
After=network.target postgresql.service
Requires=postgresql.service

[Service]
Type=simple
User=smartfs
Group=smartfs
ExecStart=/usr/local/bin/smartfs-daemon /mnt/smartfs
ExecStop=/bin/fusermount -u /mnt/smartfs
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

`Type=simple` na MVP — `Type=notify` wymaga `sd_notify(READY=1)`, bez tego timeout zabija demona.

```rust
fuser::mount2(
    SmartFS::new(pool),
    "/mnt/smartfs",
    &[
        MountOption::DefaultPermissions, // WYMAGANE — bez tego kernel nie egzekwuje uprawnień
        MountOption::AllowOther,
        MountOption::AutoUnmount,
    ],
)?;
```

```
# /etc/fuse.conf
user_allow_other
```

### 14.5. xattr

MVP zwraca `ENOTSUP`. Świadoma decyzja (ADR-13).

---

## 15. Import i Overlay — Istniejące Dane

### 15.1. Cztery tryby egzystencji pliku

```
on_prem=TRUE,  external_path=NULL (w wersji), versioning=TRUE
→ Native: SmartFS kontroluje blob od zera

on_prem=TRUE,  external_path=SET  (w wersji), versioning=TRUE
→ Import+CoW: v1 wskazuje na oryginalny plik; v2+ są natywne

on_prem=TRUE,  external_path=SET  (w wersji), versioning=FALSE
→ Read-only index: plik nigdy nie jest kopiowany; tylko indeks

on_prem=FALSE, cid=EXISTS
→ Virtual: plik istnieje globalnie (IPFS/adapter)
```

### 15.2. external_path per wersja — nie per inode

Historia jest zachowana: `get_file_content(path, version=1)` odczyta oryginał z `external_path` z v1. `get_file_content(path, version=2)` odczyta natywny blob z v2.

```
smartfs-cli import --mode index /home/user/dokumenty
→ INSERT inode_registry (bez external_path na inodzie)
→ INSERT file_versions v1 (external_path='/home/user/dokumenty/raport.md')
→ status='pending' → worker embeddinguje

Użytkownik edytuje plik:
→ release() tworzy v2 (blob_id=nowy_uuid, external_path=NULL)
→ UPDATE inode_registry (current_blob_id=nowy_uuid)
→ v1 nadal ma external_path — historia zachowana
```

### 15.3. Watch mode (post-MVP)

inotify/fanotify na katalogach overlay — wykrywa zewnętrzne zmiany i ustawia `status='pending'` dla reembeddingowania.

---

## 16. Domyślny Model i Preemptible Worker

### 16.1. Domyślny model wbudowany

`all-MiniLM-L6-v2` (384d) — lokalny, offline, ~80MB, `is_default=TRUE`. Demon zawsze go ma. Upgrade przez `smartfs.toml`:

```toml
[embedding]
default_model = "all-MiniLM-L6-v2"  # zawsze działa offline
# code_model = "text-embedding-3-large"  # wymaga OPENAI_API_KEY
```

### 16.2. Co worker generuje per plik

Worker generuje embedding dla **domyślnego modelu** (384d). BGE-M3 (768d) jest opt-in — konfigurowalny, nie domyślny, bo ~560M parametrów na CPU to koszt który użytkownik musi świadomie wybrać.

Dla plików z `ast=true` — dodatkowo embeddingi per node do `ast_embeddings_1536` (jeśli `code_model` skonfigurowany).

---

## 17. Plan Implementacji — Trzy Weekendy

### Weekend 1: Schema + Store + FUSE Navigation

**Etap 0 (~3h)**
- PostgreSQL 16 + pgvector w Docker
- Migracje SQL (001-004 w kolejności)
- Cargo workspace, crate'y
- Seed: root inode (ino=1, setval), storage_backends, embedding_models

**Etap 1: BlobStore + DB Layer (~6h)**
- `smartfs-store`: `LocalDiskStore` + `BlobStore` trait
- `smartfs-compress`: streaming hash+compress
- `smartfs-db`: CoW write, advisory lock dedup, historia wersji

**Deliverable:** `smartfs-cli write/cat/history` bez FUSE.

**Etap 2: FUSE Navigation (~6h)**
- `lookup`, `getattr`, `readdir` (z `.` i `..`)
- `mkdir`, `create`, `unlink`, `rmdir`
- BIGSERIAL → u64 ino mapping
- Mount z `DefaultPermissions` + `AllowOther`
- `statfs`

**Deliverable:** `ls`, `mkdir`, `stat`, `touch` na `/mnt/smartfs`.

---

### Weekend 2: Pełny Zapis + AI Pipeline

**Etap 3: FUSE Write (~6h)**
- `write` (bufor RAM), `flush`, `release` — commit w `release`
- `setattr` (chmod, chown, truncate, utimens)
- `rename` (atomic, walidacja nazwy docelowej dla AST pluginów)
- Unlink na otwartym pliku (odroczone CASCADE)

**Deliverable:** `cat`, `echo >`, `cp`, `mv`, `vim` działają.

**Etap 4: AI Pipeline (~5h)**
- `smartfs-ai`: supervisor loop z `claim_pending_to_processing`
- ONNX Runtime — `all-MiniLM-L6-v2` (384d)
- INSERT `embeddings_384`
- Debounce na write activity
- Retry + exponential backoff

**Deliverable:** Nowe pliki embeddingowane. `search_semantic` w CLI.

---

### Weekend 3: AST + IPFS + MCP

**Etap 5: AST Pipeline (~5h)**
- tree-sitter w `spawn_blocking`
- Parser `.rs`, `.py`, `.js`
- `ast_nodes` INSERT z `ON CONFLICT DO NOTHING`
- `ast_embeddings_1536` per node (opt-in `code_model`)
- Walidacja w `flush()` + flaga `--force`
- Walidacja w `rename()` dla atomic save

**Deliverable:** Code search per funkcja.

**Etap 6: IPFS (~4h)**
- `IpfsStore` (HTTP API Kubo)
- CID dla plików <256KB

**Etap 7: MCP Server (~4h)**
- JSON-RPC over stdio
- Wszystkie narzędzia z §13.1
- Test integracyjny z Claude Desktop/Cursor

---

### Post-Hackathon: Produkcja

**Etap 8:**
- GC blobów — refcount-aware (dedup współdzieli blob_id między inodami)
  - Wersje z `external_path != NULL` wskazują na pliki hosta — `store.delete` ich **nie tyka**
  - GC usuwa tylko wiersze gdzie `blob_id` nie ma żadnej referencji w `file_versions` i `external_path IS NULL`
- Adaptery: Wikipedia (pre-computed BGE-M3), Notion, arXiv
- S3Store, SftpStore
- FastCDC dla plików >RAM (post read-modify-write)
- UnixFS chunking dla CID >256KB
- `Type=notify` + `sd_notify`
- Watch mode (inotify) dla overlay katalogów

**Etap 9: KOVAL Compatibility**

```toml
[[rules]]
cpu_flags = ["avx2"]
features  = ["avx2"]
rustflags = ["-C", "target-feature=+avx2"]

[[rules]]
require_io_uring = true
features = ["io_uring"]

[[rules]]
min_l3_cache_kb = 8192
features = ["large_chunk_buffer"]
```

```rust
#[cfg(feature = "large_chunk_buffer")]
pub const CHUNK_SIZE: usize = 8 * 1024 * 1024;

#[cfg(not(feature = "large_chunk_buffer"))]
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;
```

---

## 18. Zależności (Cargo.toml workspace)

```toml
[workspace.dependencies]
fuser              = "0.14"
sqlx               = { version = "0.7", features = ["postgres", "uuid", "runtime-tokio"] }
uuid               = { version = "1", features = ["v4"] }
zstd               = "0.13"
sha2               = "0.10"
hex                = "0.4"
ipfs-api-backend-hyper = "0.6"
ort                = "2.0"
tree-sitter        = "0.22"
tree-sitter-rust   = "0.21"
tree-sitter-python = "0.21"
tree-sitter-javascript = "0.21"
tree-sitter-typescript = "0.21"
tree-sitter-go     = "0.21"
tokio              = { version = "1", features = ["full"] }
serde              = { version = "1", features = ["derive"] }
serde_json         = "1"
async-trait        = "0.1"
```

---

## 19. Kluczowe Decyzje Architektoniczne (ADR)

| # | Decyzja | Powód |
|---|---------|-------|
| ADR-01 | ext4 jako blob store | Zero setup, debugowalny, kompresja w aplikacji daje kontrolę per-plik |
| ADR-02 | Kompresja w Rust (zstd), nie filesystem | Poziom z bazy per inode, przenośność backendu |
| ADR-03 | Hash PRZED kompresją | Globalny dedup niezależny od poziomu kompresji; zgodność z IPFS CID |
| ADR-04 | `pg_advisory_xact_lock(hashtext($hash))` dla dedupu | Eliminuje TOCTOU race dla nowych treści; `FOR SHARE` nie pomaga gdy wiersza jeszcze nie ma |
| ADR-05 | `current_blob_id` nullable + constraint dopuszcza pusty plik | `create`/`touch` tworzy inode bez bloba — legalny stan |
| ADR-06 | Pre-computed Wikipedia embeddings | 144M wektorów gotowych; nie embeddingujemy sami |
| ADR-07 | MCP przyjmuje strukturę, nie surowy SQL | Ochrona przed injection od LLM |
| ADR-08 | Streaming pipeline 4MB chunks (CLI/adaptery) | Stały RAM footprint dla sekwencyjnego I/O |
| ADR-09 | System user `smartfs` dla blob dir | Jedyna droga do danych przez demona |
| ADR-10 | Wirtualne pliki `0o444` domyślnie, `chmod` do bazy | Zachowawczy default |
| ADR-11 | `Type=simple` w systemd na MVP | `Type=notify` wymaga `sd_notify` — bez tego timeout zabija demona |
| ADR-12 | `DefaultPermissions` + `AllowOther` + `user_allow_other` | Bez `DefaultPermissions` kernel nie egzekwuje uid/gid/mode |
| ADR-13 | `ENOTSUP` dla xattr | Świadoma decyzja, nie przeoczenie |
| ADR-14 | AST w `ast_nodes` z `UNIQUE(version_id, content_hash)` — bez cross-version dedup | Idempotencja workera. Cross-version dedup wymagałby osobnej tabeli `ast_content` — celowa rezygnacja w MVP. Plik 100× edytowany z 50 funkcjami = 5000 wierszy, co jest akceptowalne. |
| ADR-15 | Walidacja AST w `flush()`, commit CoW w `release()` | `flush()` to jedyne miejsce gdzie EACCES dociera do procesu; `release()` błąd jest ignorowany przez kernel |
| ADR-16 | MVP bufuje cały plik w RAM | FUSE write() jest random-access; streaming hash wymaga read-modify-write (post-MVP z FastCDC) |
| ADR-17 | CID tylko dla plików <256KB | Większe wymagają UnixFS DAG-PB — post-MVP |
| ADR-18 | Tabele embeddingów per-wymiar (`embeddings_384/768`) | Jeden indeks HNSW per model; `model_id` w wierszu zapobiega mieszaniu |
| ADR-19 | `ast_embeddings_1536` osobna tabela (FK do `ast_nodes.id`) | `embeddings_1536.version_id REFERENCES file_versions(id)` — FK violation dla AST nodes |
| ADR-20 | HNSW sufit: ~miliony wektorów per-funkcja na dev-box | Per-funkcja mnoży wektory ×10-100 vs per-plik |
| ADR-21 | BIGSERIAL `ino` + `setval` po seedzie roota | FUSE wymaga u64; bez `setval` pierwszy `touch` daje UNIQUE violation |
| ADR-22 | Cargo workspace + CLAUDE.md per crate | Agenci ładują tylko kontekst crate'a nad którym pracują |
| ADR-23 | KOVAL compatibility przez `koval.toml` | Kompilacja optymalna per-maszyna; artefakty hostowane w SmartFS |
| ADR-24 | Cztery tryby egzystencji pliku | Istniejące dane nie wymagają migracji |
| ADR-25 | `external_path` w `file_versions` (per wersja, nie per inode) | Historia overlay zachowana: v1 wskazuje na oryginał, v2+ są natywne |
| ADR-26 | `all-MiniLM-L6-v2` jako `is_default=TRUE`, BGE-M3 opt-in | BGE-M3 ~560M parametrów na CPU — koszt który użytkownik musi wybrać świadomie |
| ADR-27 | Worker z debounce na write activity, nie na getattr | `getattr`/`lookup` lecą bez przerwy; debounce tylko na zapisy eliminuje głodzenie |
| ADR-28 | Supervisor loop w workerze (`continue` nie `return`) | Worker nie ginie po przerwaniu; pętla jest wbudowana |
| ADR-29 | `parent_version_id` w `file_versions` | Fundament pod post-MVP DAG historii; w MVP historia jest zawsze liniowa |
| ADR-30 | FastCDC post-MVP dla plików >RAM | Perkeep cierpiał na byte-shift problem; potrzebne przy read-modify-write |
| ADR-31 | `cow_commit` jako wspólna ścieżka dla FUSE i CLI | AST parse tylko w FUSE oznaczałby brak ast_nodes dla CLI write |
| ADR-32 | `flush()` — throwaway parse dla EACCES, bez zapisu | Zapis AST w cow_commit; parse w flush tylko żeby EACCES dotarł do procesu |
| ADR-33 | `reset_stale_processing` przy starcie demona | Wiersze processing po crashu nigdy nie byłyby podjęte; reset prostszy niż claimed_at+reaper |
| ADR-34 | Worker bez `?` — błędy DB logowane, pętla kontynuuje | Mrugnięcie Postgresa nie może cicho zatrzymać całego pipeline |
| ADR-35 | Preempcja rewertuje cały batch, nie tylko bieżący wiersz | Reszta batcha inaczej zostaje w processing do restartu |
| ADR-36 | `ast_embeddings_1536` INSERT z `ON CONFLICT DO NOTHING` | Retry po crashie uderza w PK; idempotencja jak w ast_nodes |
| ADR-37 | `setattr O_TRUNC` zeruje per-fd bufor, nie commituje wersji | echo > file tworzyłoby dwie wersje; wersja powstaje tylko w release() |
| ADR-38 | GC nie usuwa plików z `external_path` | external_path to pliki hosta — store.delete ich nie tyka |
| ADR-39 | `search_functions` fallback do `embeddings_384` offline | Bez code_model zwraca file-level zamiast nic; pełna granularność przez opt-in |
| ADR-40 | Tabela `blobs(content_hash PRIMARY KEY)` zamiast `pg_advisory_xact_lock` | Advisory lock zwalnia się przy tymczasowym COMMIT (okno I/O); UNIQUE na 256-bit SHA-256 jest atomowym punktem serializacji bez okna; daje refcount pod GC za darmo |
| ADR-41 | `ast_embeddings_1536.is_current` + partial HNSW index | Bez tego: 100 edycji × 50 funkcji = 5000 wektorów 1536d → ~30 GB HNSW; z tym: zawsze max 50 wektorów per plik aktywny |
| ADR-42 | Anti-starvation valve w workerze (max-age 60s, backlog 1000) | Debounce 500ms permanentnie głodzi workera przy cargo build lub bulk import; zawór nadpisuje debounce gdy backlog jest stary lub duży |
| ADR-43 | Tree-sitter parse PRZED BEGIN transakcją | C-parser w spawn_blocking trzymałby row lock FOR UPDATE przez cały czas parsowania; parse poza transakcją — lock trzymany tylko przez krótką SQL-only transakcję |

---

## 20. Analiza Przestrzeni Rozwiązań

Przed zamrożeniem schematu przeprowadziliśmy systematyczny research. Żaden istniejący system nie robi dokładnie tego co SmartFS — ale każdy rozwiązał fragment przestrzeni i zostawił lekcje.

### 20.1. Perkeep (dawniej Camlistore) — Brad Fitzpatrick, 2009, Go

**Co robią:** Content-addressable personal storage. Permanode (stabilna tożsamość) + claim (podpisana mutacja) + blob (niezmienne dane). FUSE mount, importery zewnętrznych serwisów, OpenPGP per zapis.

**Zbieżności z SmartFS:**
Separacja tożsamości od treści od mutacji jest identyczna: permanode = `inode_registry`, claim = `file_versions`, blob = blob store. Hash przed kompresją — ta sama decyzja, te same powody. Importery zewnętrznych serwisów — odpowiednik naszych wirtualnych adapterów.

**Błędy których unikamy:**
- Katalogi jako płaska tablica JSON → bottleneck. My: `parent_id` + B-tree w PostgreSQL
- Asynchroniczny indeks → niespójność odczytu. My: PostgreSQL jako autorytet
- GPG per write → narzut CPU. My: UUID z PostgreSQL
- `parent_version_id` brak → płaska historia bez DAG. My: jawny łańcuch przodków
- Synchroniczny HTTP per chunk w FUSE → crash przy >76MB. My: bufor RAM + commit w `release()`
- `Type=notify` bez `sd_notify`. My: `Type=simple` na MVP

**Status (2024-2025):** Low-activity maintenance. `golang.org/x/crypto/openpgp` deprecated — paraliżuje kryptografię projektu.

### 20.2. LSFS — AIOS, arXiv:2410.11843, wrzesień 2024

**Co robią:** Semantyczny interfejs FS w języku naturalnym. LLM tłumaczy komendy na syscalle. Model embeddingowy: `all-MiniLM-L6-v2` — ten sam co nasz, wybrany niezależnie.

**Zbieżności:** Ten sam model embeddingowy (trzecia niezależna walidacja). Podział syscalli na atomowe i kompozytowe — warto przenieść do MCP layer. Bezpieczniki przed halucynacjami LLM przy operacjach destrukcyjnych.

**Czego nie mają:** LSFS jest skryptem Python symulującym FS w lokalnym folderze. Brak FUSE, brak POSIX, brak CAS, brak CoW, brak AST. Kod: `github.com/agiresearch/AIOS-LSFS`.

### 20.3. Git

Rozwiązał CoW i CAS dekadę przed Perkeep. SHA-256 jako tożsamość każdego obiektu. Nasze `file_versions` to git commits generowane transparentnie przez FUSE — użytkownik nie musi robić nic. Git wymaga świadomego `git commit`.

### 20.4. IPFS

CID jest naszym `content_hash` — zbieżność filozofii. Dla plików <256KB SHA-256 → CID jest trywialny. Dla większych — UnixFS DAG-PB (post-MVP, ADR-17).

### 20.5. Czego brakuje każdemu z nich

| System | Real POSIX FS | CAS + dedup | CoW versioning | Semantic search | AST per-function | MCP |
|--------|:---:|:---:|:---:|:---:|:---:|:---:|
| Perkeep | ✓ (ograniczone) | ✓ | ✓ | ✗ | ✗ | ✗ |
| LSFS | ✗ (symulacja) | ✗ | ✗ | ✓ | ✗ | częściowo |
| Git | ✗ | ✓ | ✓ | ✗ | ✗ | ✗ |
| IPFS | ✗ | ✓ | ✗ | ✗ | ✗ | ✗ |
| **SmartFS** | **✓** | **✓** | **✓** | **✓** | **✓** | **✓** |

Kombinacja wszystkich sześciu kolumn nie istnieje w żadnym publicznym systemie który znaleźliśmy. To jest przestrzeń którą SmartFS eksploruje.

---

## 21. Nota o Raporcie Zewnętrznym (Gemini Deep Research)

Raport Gemini zawierał kilkadziesiąt punktów krytycznych. Po analizie przez Opus 4.8 podzielone na trzy kategorie:

**Propagowane do v4.5 (prawdziwe i naprawione):**
- Okno I/O w advisory lock → tabela `blobs` (ADR-40)
- Eksplozja wektorów bez cross-version dedup → `is_current` + partial index (ADR-41)
- Głodzenie workera przez debounce → anti-starvation valve (ADR-42)
- Tree-sitter wewnątrz transakcji → parse przed BEGIN (ADR-43)

**Udokumentowane świadome ograniczenia (nie są bugami — mamy ADR):**
- FUSE overhead do 83% (USENIX FAST'17) → ADR: SmartFS nie jest pod hot-path; io_uring post-MVP
- OOM przy plikach >RAM → ADR-16: znane, read-modify-write + FastCDC post-MVP
- HNSW memory wall → ADR-20: znany sufit; ADR-41 rozwiązuje największy driver (ast per-version)
- CID tylko dla <256KB → ADR-17: UnixFS DAG-PB post-MVP

**Odrzucone (błędne lub niezweryfikowane):**
- `CVE-2026-42945` ("NGINX Rift, 18-letni heap overflow") — data z przyszłości, prawdopodobna halucynacja modelu; nie cytujemy
- `arXiv 2606.22263` ("Revelio") — data z przyszłości, prawdopodobna halucynacja; nie cytujemy
- Deadlock przez `UNNEST` batch-locking — krytyka kodu którego nie ma w projekcie
- Perkeep "porzucony przez monolityczne bloki" — sprzeczne z własnym researchem (§20.1); Perkeep używa CDC, nie bloków monolitycznych
- Birthday paradox na `hashtext` "całkowicie destabilizuje" system — kolizja advisory locka to false contention (chwilowe czekanie), nie błąd poprawności; i tak nieistotne po ADR-40
- PgBouncer + advisory locks "gubią kontekst" — myli `pg_advisory_xact_lock` (tx-scoped, bezpieczny) z session-level `pg_advisory_lock`

---

## 22. Wizja Docelowa

SmartFS staje się węzłem w globalnej sieci wiedzy:

- Twoje lokalne pliki dostępne przez IPFS dla autoryzowanych klientów
- Zewnętrzne zasoby (Wikipedia, arXiv, NASA, Notion) jako część drzewa katalogów
- LLM widzi jeden spójny filesystem i przeszukuje go semantycznie
- Historia każdego pliku natywna — bez Gita, bez zewnętrznych narzędzi
- Tożsamość danych to hash treści — niezależna od lokalizacji i nazwy
- Kod przeszukiwalny na poziomie funkcji, diffowany przez SQL
- Agenci (Claude Code) budują ten system przez warstwowe CLAUDE.md
- SmartFS hostuje własne artefakty KOVAL — wersjonowane we własnym systemie

> *"Każdy dobry system danych zaczyna wyglądać podobnie, bo dane mają swoją fizykę."*
