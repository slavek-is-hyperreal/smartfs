# Independent pre-flight review — SmartFS v6.0 specification

**Reviewer:** Claude Opus 5, independent architecture/documentation review
**Date:** 2026-09-12
**Scope:** every file listed in the repo manifest (README, docs/00-06, docs/adr/ADR-49..57, docs/crates/*, docs/base-v4.5-v5.0/*, docs/vision/*, migrations/001-006, symbol_registry.schema.json)
**Question asked:** is this safe to hand to an autonomous coding agent for Phases 0-6, with no human available to resolve ambiguity?

---

## Verdict

**Not yet — do not hand this to an autonomous agent as it stands.** The spec is unusually well-reasoned at the *decision* level (the ADRs are genuinely good, the migrations are careful, the fail-safe design of `consolidation_thresholds` is exactly right), but it has a small number of defects that are individually fatal for an unsupervised builder. The most serious is structural rather than technical: **`docs/06-agentic-execution-plan.md` explicitly limits each subagent to its own crate doc plus the corresponding `§3.x` section of the pre-FIX v4.5 base document, and deliberately withholds `SmartFS_v4.5_to_v5.0_fixes.md`** — so the `smartfs-db` and `smartfs-ai` subagents will read `refcount+1` dedup and `UPDATE ... is_current` inside the CoW transaction as their authoritative contract and faithfully re-implement FIX-01 and FIX-02, the two HIGH-severity correctness bugs v5.0 was created to eliminate. Beyond that: two ADRs are cited by number in the architecture doc but do not exist as files (ADR-51, ADR-52), the `docs/adr/` directory is never included in the prompt's reading order at all, the advisory-lock design in ADR-53/docs/03 uses session-level locks over a connection pool (the precise failure mode the base document's own §21 identifies and dismisses), no crate in the dependency graph is permitted to turn a query string into a vector, the image-embedding feature advertised on the README's front page has no table in any migration, and `smartfs-cli calibrate` — the hard precondition without which the entire consolidation layer never starts — appears in no crate document's command list. Fixing the 14 BLOCKING items below is mostly mechanical (a day of editing, not a redesign); the spec's foundations are sound and none of these require rethinking an architectural decision.

Counts: **14 BLOCKING · 21 SIGNIFICANT · 14 MINOR · 6 verified-solid**

---

# 1. BLOCKING

Findings that would cause the autonomous builder to produce wrong code, get stuck, or halt on Hard Rule 6/8.

---

### B-01 — The subagent isolation rule feeds pre-FIX v4.5 text as authoritative, re-introducing FIX-01 and FIX-02

**Files:** `docs/06-agentic-execution-plan.md` (bullet 3 of "Zasady dla każdego subagenta"), `docs/base-v4.5-v5.0/SmartFS_Architecture_v4_5.md` §3.5, §3.7, §6, §7.3, §11.1, §12.1

`docs/06` says each subagent receives **exclusively**:

> "plik dokumentacji swojego crate'a (`docs/crates/<crate>.md` albo odpowiednią sekcję §3.x [`SmartFS_Architecture_v4_5.md`] dla crate'ów "bez zmian"), Root Invariants (…), i już scommitowany `smartfs-schema` — **nic więcej**, żeby nie zgadywał cudzych API."

`SmartFS_v4.5_to_v5.0_fixes.md` is not on that list. But the base document in this repo has **never had FIX-01..10 applied to it.** Verified directly:

- §3.5 `smartfs-db/CLAUDE.md`, "Dedup" section still reads: `INSERT INTO blobs ... ON CONFLICT DO UPDATE SET refcount+1 RETURNING blob_id, inserted.` — FIX-01 removed `refcount` entirely as unmaintainable.
- §3.5 "CoW transaction shape (v4.5)", Step 2, still contains inside the transaction:
  `UPDATE ast_embeddings_1536 SET is_current=FALSE WHERE ast_node_id IN (SELECT id FROM ast_nodes WHERE version_id=prev_version);`
  — FIX-02 removed exactly this line ("is_current is NOT set here… owned by smartfs-ai worker, not cow_commit").
- §6 `CREATE TABLE blobs` still declares `refcount INT NOT NULL DEFAULT 1` and the comment `GC usuwa tylko blobs WHERE refcount = 0`.
- §19's ADR table stops at ADR-43; ADR-04 is **not** marked SUPERSEDED (FIX-09) and ADR-44..48 were never appended (they live only in the fixes doc). The document header still says "v4.5" (FIX-08).

`docs/crates/smartfs-db.md` line 3 does point at the fixes doc ("Baza: … §3.5 … + poprawki … FIX-01..04 (bez zmian, patrz tam)"), but under the isolation rule the subagent is told it gets *nothing more* than its crate doc and the §3.x section. The two documents it is handed both contain the buggy version; the corrective patch-spec is out of its context by explicit instruction.

**Consequence:** the delivered `smartfs-db` will have a `refcount` column that drifts upward and a GC predicate that never fires, plus a race between `cow_commit` and the async embedding worker producing multiple `is_current=TRUE` rows. Migration 001/004 will simultaneously *not* have those columns — so the code will not even compile against the schema, but the agent will most likely "fix" that by altering the migration, which violates the migration comments' own verification checkboxes.

**Fix:** either apply FIX-01..10 to the base document in place (it is a patch-spec written to be applied — it says so in its own header), or add `SmartFS_v4.5_to_v5.0_fixes.md` to the mandatory per-subagent context list and put a prominent "THIS DOCUMENT IS PRE-FIX" banner at the top of `SmartFS_Architecture_v4_5.md`.

---

### B-02 — Session-level `pg_try_advisory_lock` over a `sqlx::Pool`: the lock is not held by the connection that does the work

**Files:** `docs/03-consolidation-design.md` §2, §4b, §10; `docs/adr/ADR-53-consolidation-concurrency.md` Decyzja §2; `docs/crates/smartfs-semantic.md` (function table)

The supervisor code is:

```rust
if try_advisory_lock(&db, lock_key).await.unwrap_or(false) {
    match consolidate_batch(&db, &plugin_type, model_id, &cfg).await { ... }
    release_advisory_lock(&db, lock_key).await.ok();
}
```

with `try_advisory_lock: async fn(&Pool, i64) -> Result<bool>` and `consolidate_batch` internally doing `db.begin()`.

`pg_try_advisory_lock` is a **session-level** lock: it is bound to the backend connection that acquired it. With a pool, three separate checkouts happen — acquire, work, release — and there is no guarantee any two are the same connection. In practice: the lock is taken on connection A, `consolidate_batch` opens its transaction on connection B (so it runs *without* the lock, and a second daemon can be inside the same critical section), and `pg_advisory_unlock` runs on connection C where it returns `false` and leaks the lock on A until that connection is recycled. Under a connection pooler (PgBouncer in transaction mode) it is worse still.

This is not a subtle point the project has never encountered — **the base document already names it.** `SmartFS_Architecture_v4_5.md` §21, "Odrzucone (błędne lub niezweryfikowane)":

> "PgBouncer + advisory locks 'gubią kontekst' — myli `pg_advisory_xact_lock` (tx-scoped, bezpieczny) z session-level `pg_advisory_lock`"

v6.0 then goes on to specify the session-level variant. Relatedly, ADR-53's justification is factually wrong about its own code:

> "operacja mieści się w całości w jednej krótkiej transakcji SQL, więc żaden z problemów, które zdyskwalifikowały advisory lock w dedupie blobów (ADR-40 — zwolnienie locka przy tymczasowym COMMIT, okno I/O) tutaj nie występuje."

The lock is acquired *outside* any transaction in the code as written, so the operation demonstrably does not sit inside one transaction with it.

**Fix:** use `pg_try_advisory_xact_lock(key)` as the first statement inside the transaction opened by `consolidate_batch`/`merge_centroids` (auto-released at COMMIT/ROLLBACK, no unlock call, no pool hazard), and restate ADR-53 §2 accordingly. If a session-level lock is genuinely wanted, the spec must say "acquire a dedicated `PoolConnection` and hold it for the duration" and every signature must take `&mut PgConnection`, not `&Pool`.

---

### B-03 — ADR-51 and ADR-52 are cited as existing ADRs; no such files exist

**Files:** `docs/01-architecture.md` lines 50-51 and 56; `docs/adr/ADR-57-pick-style-plugin-dictionary.md` line 32; `docs/vision/north-star-kernel-native.md` line 21

`docs/01-architecture.md`'s "ADR nowe w v6.0" table contains full rows for:

| ADR-51 | Centroidy trzymane per tabela embeddingów … nie w jednej uniwersalnej tabeli |
| ADR-52 | Każdy `pub fn`/`struct`/`impl`/`enum` … dostaje trwały UUID … (`@id:`) |

Both are presented as normative v6.0 decisions. Neither has a file. The very next line — "Pełne ADR: …" — enumerates only 49, 50, 53, 54, 55, silently skipping 51 and 52 without comment, which reads as an oversight rather than a deliberate choice. ADR-57 then cites ADR-52 as an established decision ("dokładnie ta sama klasa problemu co brak `@id` przed ADR-52"), and the vision doc asserts "Wszystkie ADR-y 01-57 przeżywają to przejście", implying a complete 1..57 range.

This is the exact failure class the project has hit before. Note that ADR-52's *content* is Root Invariant #8 and is fully specified in `docs/04`; ADR-51's content matches migration 005 §5. So nothing is *missing* substantively — but an agent instructed (Hard Rule 6) to stop and report contradictions will either halt on the dangling reference or, worse, invent an ADR-51/52 file.

**Fix:** write the two ADR files (they can be short, pointing at migration 005 §5 and docs/04 respectively), or mark them in the table as "decyzja udokumentowana w migracji 005 §5 / docs/04, bez osobnego pliku ADR" and add them to the "Pełne ADR" line.

---

### B-04 — Nothing in the crate graph is allowed to embed a query string, yet three MCP tools take one

**Files:** `docs/02-crates.md` (dependency graph, and the "Dlaczego smartfs-semantic nie zależy od smartfs-ai" rationale); `docs/crates/smartfs-semantic.md` ("Nigdy" list); `docs/crates/smartfs-mcp.md`; `SmartFS_Architecture_v4_5.md` §3.8

Three statements that cannot all hold:

1. `docs/02-crates.md`: `smartfs-semantic ← importuje: smartfs-schema, smartfs-db (NIGDY smartfs-ai — patrz niżej)` and `smartfs-mcp ← importuje: smartfs-schema, smartfs-db, smartfs-semantic` (also no `smartfs-ai`).
2. `docs/crates/smartfs-semantic.md`, "Nigdy" list: *"Nie generuje embeddingów — do wektora zapytania **woła publiczne API smartfs-ai**"* — which requires importing `smartfs-ai`, directly contradicting (1) two documents up the chain.
3. Base §3.8 `smartfs-mcp/CLAUDE.md`, "Never" list: *"No embedding inference"*.

Meanwhile `smartfs_semantic::search_by_concept` is specified (docs/03 §7) as taking `query_vector: &[f32]`, while the MCP tool `search_by_concept(query, …)` takes text. Same for `search_semantic`. There is no component anywhere in the graph that is both allowed to load an ONNX model and reachable from the MCP request path.

This hole is partly inherited from v4.5 (which never said who embeds the query for `search_semantic` either), but v6.0 makes it explicit and self-contradictory by writing the forbidden call into the crate's own "Nigdy" list.

**Fix:** decide and state it once. Cleanest option consistent with the stated rationale ("konsolidacja może działać bez ładowania jakiegokolwiek modelu ONNX"): let **`smartfs-mcp` import `smartfs-ai`** for query-vector generation only, amend base §3.8's "No embedding inference" line, and change `smartfs-semantic`'s "Nigdy" entry to "nie generuje embeddingów — wektor zapytania dostaje od wołającego jako parametr".

---

### B-05 — Phase 1's "safe parallelism" contradicts the dependency graph it cites

**Files:** `docs/06-agentic-execution-plan.md` (phase table, rows 1 and 3); `docs/02-crates.md` (dependency graph)

Phase 1 row:

> | 1 | `smartfs-store`, `smartfs-compress`, `smartfs-ipfs` | równolegle (3 subagenty) | medium | **Brak wzajemnych zależności poza schema (patrz graf w 02-crates.md)** — bezpieczne do prawdziwej równoległości |

The graph it points at says the opposite:

> `smartfs-compress ← importuje: smartfs-schema, **smartfs-store**`

`smartfs-compress` is the hash+zstd pipeline whose entire ordering invariant (base §3.4, "Ordering — non-negotiable") is about feeding `store.put`. Running it as an isolated parallel subagent that has been given *only* `crates/_unchanged.md` §3.4 and a not-yet-existing `smartfs-store` means it must invent the `BlobStore` trait signature — precisely the "dwa subagenty niezależnie wymyślające niezgodne sygnatury" failure the document opens by warning against.

The same row-level error appears in Phase 3: *"`smartfs-semantic`, `smartfs-fuse` … Oba zależą tylko od db+schema"* — but the graph says `smartfs-fuse ← importuje: smartfs-schema, smartfs-db, smartfs-store, smartfs-compress`. (Phase 3 is harmless in practice since store/compress land in Phase 1, but the stated reason is wrong and an agent may act on the reason.)

**Fix:** Phase 1 becomes `smartfs-store` + `smartfs-ipfs` in parallel, then `smartfs-compress`; or move `smartfs-compress` to the front of Phase 2. Correct the Phase 3 rationale text.

---

### B-06 — `embeddings_1024_qwen_vl` is required by two documents and created by no migration

**Files:** `docs/crates/smartfs-ai.md` line 21; `docs/adr/ADR-49-qwen-default-model.md` "Qwen3-VL-Embedding dla obrazów — zakres decyzji"; `README.md` delta row 2; `migrations/005_semantic_consolidation.sql`

`docs/crates/smartfs-ai.md`, under "Embeddings generated per file version":

> `- embeddings_1024_qwen_vl z Qwen3-VL-Embedding-2B — tylko dla plików, których plugin ma "embedding".model wskazujący na model obrazowy`

ADR-49 confirms it must be a distinct table: *"Traktowane jako osobny, dodatkowy embedding (**nowa tabela**…) — mieszanie przestrzeni wektorowej obrazu i tekstu w jednej tabeli łamałoby Invariant #5."*

Migration 005 registers the model row (`('Qwen3-VL-Embedding-2B', 1024, '1.0', TRUE, FALSE)`) but creates only `embeddings_1024_qwen`. There is no `embeddings_1024_qwen_vl`, no `concept_centroids_1024_qwen_vl`, no `centroid_members_*` for it, and no `word_centroid_links_*`.

The agent is now trapped between three inviolable rules: implement the documented behaviour, do not `CREATE TABLE` at runtime (Root Invariant #4), and do not write to a table that does not exist. Note the extra trap: because the VL model is also 1024-dimensional, a plausible-looking "fix" is to write VL vectors into `embeddings_1024_qwen` (the PK `(version_id, model_id)` permits it) — which is precisely the Invariant #5 violation ADR-49 warns against, and would silently poison the 1024 centroid graph with image vectors.

**Fix:** add `embeddings_1024_qwen_vl`, `concept_centroids_1024_qwen_vl`, `centroid_members_1024_qwen_vl` and `word_centroid_links_1024_qwen_vl` to migration 005 (or a new 007), or explicitly demote the image-embedding feature to post-MVP and strike it from the README delta table and `docs/crates/smartfs-ai.md`.

---

### B-07 — Nothing in the build ever populates `consolidation_thresholds`, so the entire consolidation layer is dead on arrival

**Files:** `docs/03-consolidation-design.md` §6; `docs/crates/_unchanged.md` (smartfs-cli entry); `SmartFS_Architecture_v4_5.md` §3.10; `migrations/005_semantic_consolidation.sql` §4

Migration 005 §4 is deliberately fail-safe:

> "Wiersz w tej tabeli jest WARUNKIEM koniecznym do uruchomienia supervisora dla danej kombinacji … Brak wiersza = supervisor się nie odpala"

and `join_threshold DOUBLE PRECISION NOT NULL` has no default, by design. The only documented way to create a row is `docs/03` §6:

> "`smartfs-cli init` (albo nowa podkomenda `smartfs-cli calibrate --plugin-type X --model Y`) wyznacza `join_threshold` empirycznie…"

But `docs/crates/_unchanged.md` — the sole v6.0 document describing `smartfs-cli` — says:

> "**smartfs-cli** (§3.10) — delta **kosmetyczna**: nowa podkomenda `smartfs-cli concepts [--plugin-type X]` … **Poza tym bez zmian.**"

and base §3.10's command list is `write | cat | history | search | import | diff`. There is no `init`, no `calibrate`, no `reembed`. Under Hard Rule 8 ("Nie zgaduj … Jeśli `docs/crates/<crate>.md` nie specyfikuje czegoś potrzebnego do kompilacji") the agent will build exactly those seven subcommands and stop.

**Consequence:** the delivered MVP compiles, passes clippy, runs — and `smartfs-semantic`, the stated "serce v6.0", never processes a single vector, because `fetch_calibrated_combinations` always returns an empty list. This failure is silent: no error, no log line, just a supervisor that never spawns.

**Fix:** add `calibrate` (and the ADR-49 `reembed`) to `docs/crates/_unchanged.md`'s smartfs-cli entry, or promote `smartfs-cli` to its own `docs/crates/smartfs-cli.md` with the full v6.0 command list, argument shapes, and delegation targets.

---

### B-08 — `search_by_concept` has four mutually incompatible signatures, and its advertised "no-query mode" has no backing function

**Files:** `README.md` line 52; `docs/02-crates.md` line 43; `docs/crates/smartfs-mcp.md` lines 7-14; `docs/03-consolidation-design.md` §7; `docs/crates/smartfs-semantic.md` (search table); `docs/crates/_unchanged.md` line 11

| Source | Signature |
|---|---|
| `README.md` | `search_by_concept(query, plugin_type, limit)` — `plugin_type` **required** |
| `docs/02-crates.md` | `search_by_concept(query, plugin_type?, limit)` — optional |
| `docs/crates/smartfs-mcp.md` | `search_by_concept(query, plugin_type?, **model_id?**, limit)` — extra param |
| `docs/03` §7 / `smartfs-semantic.md` | `(&Pool, &[f32], &str, Uuid, usize)` — vector, **both required**, no optionals |

Four spellings for one tool. Worse, `docs/crates/smartfs-mcp.md` then specifies a second calling mode:

> "nowy tryb wywołania **bez `query`**, tylko z `plugin_type`, zwracający listę centroidów posortowaną wg `member_count`" → `{ id, label, member_count, sample_names: [...] }`

and `_unchanged.md` makes `smartfs-cli concepts` "cienka delegacja do `smartfs_semantic::search_by_concept` **bez `query`**". But `smartfs_semantic::search_by_concept` takes `query_vector: &[f32]` as a non-optional positional parameter and returns `Vec<ConceptSearchHit>` (`id, dystans, źródło`) — it cannot express "no query", and `ConceptSearchHit` has no `label`, `member_count` or `sample_names`. **There is no `list_centroids`-style function anywhere in `docs/crates/smartfs-semantic.md`.** Two documented features (the MCP browse mode and `smartfs-cli concepts`) delegate to a function that cannot serve them.

**Fix:** fix the four signatures to one; add an explicit `list_centroids(&Pool, &str, Option<Uuid>, usize) -> Result<Vec<CentroidSummary>>` plus the `CentroidSummary` struct to `docs/crates/smartfs-semantic.md`, and repoint the MCP browse mode and `smartfs-cli concepts` at it.

---

### B-09 — `smartfs-db::count_all_unconsolidated` cannot implement `smartfs-semantic::count_unconsolidated`

**Files:** `docs/crates/smartfs-db.md` lines 8-14; `docs/crates/smartfs-semantic.md` (worker table); `docs/03-consolidation-design.md` §2

`smartfs-db.md`:

```rust
/// Zwraca sumę wierszy `consolidated = FALSE` po WSZYSTKICH tabelach
/// embeddingów naraz … Używane przez smartfs-semantic::count_unconsolidated
/// jako pojedyncze wywołanie zamiast czterech osobnych zapytań
pub async fn count_all_unconsolidated(db: &Pool) -> Result<i64>
```

`smartfs-semantic.md`:

| `count_unconsolidated` | `async fn(&Pool, &str, Uuid) -> Result<i64>` | **Teraz sparametryzowane per kombinacja** (wcześniej: globalna suma) |

A global scalar sum across four tables and all plugin types cannot be used to compute a per-`(plugin_type, model_id)` backlog. And the value is load-bearing: `docs/03` §2 gates the whole consolidation cycle on `backlog >= cfg.backlog_threshold`, where `cfg` is per-combination. Wiring the global count in (as `smartfs-db.md` instructs) makes every supervisor fire whenever *any* combination is busy, defeating both the per-combination thresholds and the "lokalnie, nad ograniczonym batchem" property of Invariant #7.

**Fix:** change `count_all_unconsolidated` to `count_unconsolidated(db, plugin_type, model_id) -> Result<i64>` (and, if a global figure is genuinely wanted for metrics, keep the aggregate under a separate name with no claim of being the supervisor's input).

---

### B-10 — `docs/adr/` is absent from the prompt's mandatory reading order

**Files:** `docs/06-agentic-execution-plan.md` (the pasteable prompt, "Przeczytaj w tej kolejności")

The prescribed order is: README → 00 → 01 → 02 → base architecture → fixes → 03 → 04 → 05 → 06 → `migrations/*.sql` → `docs/crates/*.md`. **`docs/adr/*.md` never appears.**

Yet the ADRs carry normative content that exists nowhere else:

- ADR-49 §4 is the *only* place the cutover procedure and `smartfs-cli reembed --model … --all` are specified.
- ADR-49 §5 is the *only* place the "no silent fallback to the old model; default to `is_default=TRUE`" rule for `search_semantic`/`search_by_concept` is stated.
- ADR-54's "Co dokładnie się indeksuje" is the fullest statement of the `search_text` contract.
- Hard Rule 9 in the same prompt tells the agent Phase 7 is "ścieżka GPU/Vulkan w smartfs-ai, **ADR-55**" — a file it was never told to read.
- Hard Rule 4 and `docs/05` §6 require commit messages and comments to reference ADR numbers with a summary — impossible for ADRs never read.

The 01-architecture table gives one-line summaries for ADR-49..55, which is not enough to implement the cutover.

**Fix:** insert `docs/adr/*.md` into the reading order (after `docs/02-crates.md` is the natural spot, since 01 introduces them), and state explicitly which ADRs are post-MVP (56, 57, and 55's Phase 7 scope) so the agent doesn't build them early.

---

### B-11 — Duplicate `@id` UUID on two contradictory `consolidation_supervisor` signatures

**Files:** `docs/03-consolidation-design.md` §2; `docs/04-uuid-doc-linking.md` (the convention example); `docs/symbol_registry.schema.json` (the example block); `docs/crates/smartfs-docgen.md`

`6b2d4e18-3f77-4a90-9c11-8a5f0d2e7c44` is the only UUID that appears twice in the spec, and the two occurrences disagree about the function it names:

- `docs/03` §2: `pub async fn consolidation_supervisor(db: Pool, combo: (String, Uuid))`
- `docs/04`: `pub async fn consolidation_supervisor(db: Pool, cfg: ConsolidationConfig)`

The second is the pre-revision signature that `docs/03` §3 explicitly abolished (*"`ConsolidationConfig` jest teraz jawnie ładowana per `(plugin_type, model_id)` … nie jest globalną stałą aplikacji"*). `docs/crates/smartfs-semantic.md` confirms `async fn(Pool, (String, Uuid))`.

Under Hard Rule 8 the agent must not guess between two stated signatures. And `docs/crates/smartfs-docgen.md`'s own "Nigdy" list makes this an outright build failure once docgen runs: *"Nie zgaduje UUID przy konflikcie — przy dwóch identycznych `@id` w różnych miejscach: **hard error**"* — Phase 6's mandated `smartfs-docgen check` will fail on the spec's own example if the agent transcribes both.

**Fix:** update `docs/04`'s example to the current signature (it is only an illustration of the `@id` convention), or give the illustration a different, clearly-fictional UUID.

---

### B-12 — `claim_unconsolidated_batch`'s SQL is not valid PostgreSQL

**Files:** `docs/03-consolidation-design.md` §4

```sql
SELECT ast_node_id, embedding, plugin_type, model_id
FROM ast_embeddings_1536
WHERE consolidated = FALSE AND is_current = TRUE
  AND plugin_type = $1 AND model_id = $2
ORDER BY created_at ASC
FOR UPDATE SKIP LOCKED
LIMIT $3
```

PostgreSQL's `SELECT` grammar fixes the clause order as `… ORDER BY … LIMIT … FOR UPDATE [SKIP LOCKED]`. `LIMIT` after the locking clause is a syntax error, and `sqlx::query_as!` validates against a live database at compile time, so this fails the build rather than misbehaving at runtime. Since `docs/03` §4 also says the three sibling queries "różnią się tylko nazwą tabeli", the same error propagates to four call sites.

Second, subtler problem in the same query: `ast_embeddings_1536`'s primary key is `(ast_node_id, model_id)` and the batch is claimed with `FOR UPDATE SKIP LOCKED` *without* a `pending/processing` state machine, on the stated grounds that "krok jest czystym SQL … w jednej krótkiej transakcji". But `consolidate_batch` then runs `nearest_centroid` (an ANN query), `attach_to_centroid`, and possibly `split_centroid` → `kmeans2` (Lloyd's algorithm over up to `max_members_per_centroid` = 5000 vectors) **inside that same transaction**, holding row locks the whole time. That is not a "krótka transakcja SQL-only", and it is the same shape of problem ADR-43 solved for tree-sitter ("parse PRZED BEGIN … C-parser w spawn_blocking trzymałby row lock przez cały czas parsowania"). The spec should say explicitly whether `kmeans2` runs inside the transaction and what the expected lock duration is.

**Fix:** reorder to `… ORDER BY created_at ASC LIMIT $3 FOR UPDATE SKIP LOCKED`, and add a sentence to §5 addressing lock hold time across `kmeans2` (or move the split out of the claiming transaction, mirroring ADR-43).

---

### B-13 — `activity_monitor` is used but never defined, and the "write silence" clock is unspecified

**Files:** `docs/03-consolidation-design.md` §2; `docs/adr/ADR-50-working-memory-consolidation.md`; `docs/crates/smartfs-semantic.md`

Inside `consolidation_supervisor(db: Pool, combo: (String, Uuid))`:

```rust
let should_run = backlog >= cfg.backlog_threshold
    || activity_monitor.idle_for(cfg.idle_before_sleep).await
    || last_run.elapsed() >= cfg.max_wait;
```

`activity_monitor` is a free variable: not a parameter, not a field, not a `static`, not defined anywhere in `docs/03`, and absent from `docs/crates/smartfs-semantic.md`'s struct and function tables. It will not compile.

More importantly the *concept* is unspecified. "Cisza zapisu" is one of the two headline triggers of ADR-50 ("odpowiednik snu"), but nothing says what it observes or how `smartfs-semantic` — which by rule imports only `smartfs-schema` and `smartfs-db` and must never touch `smartfs-ai` or FUSE — can observe filesystem write activity at all. The base project has a same-named `activity_monitor` living inside `smartfs-ai`'s worker loop (§3.7: `activity_monitor.wait_for_idle(500ms)`), which `smartfs-semantic` is forbidden from importing. Plausible implementations (poll `MAX(created_at)` on the embedding tables; a Postgres `LISTEN/NOTIFY` channel; a shared in-process handle passed by the daemon) differ substantially in behaviour and in what `smartfs-semantic`'s public API must look like.

**Fix:** specify the mechanism, the type, and how it reaches the supervisor — most likely `count_unconsolidated`-adjacent: `async fn seconds_since_last_unconsolidated_insert(db, plugin_type, model_id) -> Result<i64>` in `smartfs-db`, which keeps `smartfs-semantic` dependency-clean. Then change the supervisor signature to match.

---

### B-14 — `create_centroid_from_cluster` cannot satisfy the `NOT NULL` columns it must write, and is missing from the crate's symbol table

**Files:** `docs/03-consolidation-design.md` §5, §4b; `docs/crates/smartfs-semantic.md`; `migrations/005_semantic_consolidation.sql` §5

`split_centroid` and `merge_centroids` both call:

```rust
let new_a = create_centroid_from_cluster(tx, &cluster_a).await?;
```

with `Cluster { mean, m2, count, member_ids, member_distances }` (docs/03 §5). Every `concept_centroids_*` table declares `plugin_type TEXT NOT NULL` and `model_id UUID NOT NULL REFERENCES embedding_models(id)`. Neither is derivable from `Cluster`, neither is a parameter, and the function must additionally choose *which of the four tables* to insert into — information that is likewise absent from both `Cluster` and the argument list.

Compounding it: `create_centroid_from_cluster` appears **nowhere** in `docs/crates/smartfs-semantic.md`'s function tables, which are the authoritative per-crate contract the subagent is handed. Nor do: `update_centroid`, `insert_centroid_member`, `deactivate_centroid`, `reassign_members`, `reparent_members`, `find_mergeable_pairs`, `fetch_centroid_members_with_vectors`, `mark_consolidated`, or the type `CentroidMemberWithVector` (used in `kmeans2`'s published signature). That is ten undocumented symbols on the critical path of the crate the docs call "najbardziej szczegółowo rozpisany crate w tym dokumencie". Hard Rule 8 tells the agent to stop rather than invent them; it will stop ten times in one crate.

**Fix:** add `plugin_type: &str, model_id: Uuid` to `create_centroid_from_cluster` (or to `Cluster`), state the table-routing rule (see S-16), and add the ten missing symbols to `docs/crates/smartfs-semantic.md`.

---

# 2. SIGNIFICANT

Real errors or inconsistencies that a careful agent might route around, but which will produce wrong or degraded behaviour.

---

### S-01 — The default code-embedding model points at a table that does not exist, making the AST centroid graph unreachable offline

**Files:** `SmartFS_Architecture_v4_5.md` §10.3; `migrations/004_ast_nodes.sql`; `migrations/005_semantic_consolidation.sql` §5a

Base §10.3, "Model dla `ast_embeddings_1536`":

> - Domyślnie: `all-MiniLM-L6-v2` (384d w tabeli **`ast_embeddings_384`**) — offline, zawsze działa
> - Opt-in: `text-embedding-3-large` (1536d) — wymaga `OPENAI_API_KEY`

`ast_embeddings_384` is created by no migration and appears in no other document. So in a default offline install, the *only* AST embedding table is `ast_embeddings_1536`, whose model (`text-embedding-3-large`) is registered with `is_local = FALSE` and requires a paid OpenAI key.

v6.0 builds on top of this without noticing: migration 005 creates `concept_centroids_1536` / `centroid_members_1536` keyed to `ast_nodes`, and `docs/03` §4 makes `ast_embeddings_1536` the worked example for batch claiming, with `label_centroid_from_members` step 1 reading `ast_nodes.name` "dla 1536". The entire per-function concept graph — arguably the most valuable part of the feature for a code repository — is unreachable without a remote API key, in a project whose ADR-49/ADR-55 thrust is uncompromisingly local-first.

**Fix:** ADR-49 should extend the Qwen cutover to the code model (e.g. a local 1024d code embedding into a new `ast_embeddings_1024_qwen` with matching centroid tables), or the spec should state plainly that the 1536 consolidation path is opt-in and requires `OPENAI_API_KEY`. Either way `ast_embeddings_384` should be struck from §10.3 as a phantom table.

---

### S-02 — `smartfs-mcp.md` declares `search_semantic` unchanged; ADR-49 requires it to change

**Files:** `docs/crates/smartfs-mcp.md` line 21; `docs/adr/ADR-49-qwen-default-model.md` §5; `SmartFS_Architecture_v4_5.md` §3.8, §13.1

`smartfs-mcp.md`: *"`search_semantic`, `search_functions`, … — wszystkie z v4.5, **bez modyfikacji**."*

But base §3.8/§13.1 define `search_semantic(query, model?, limit, type_filter?)` as *"Cosine search w `embeddings_384` lub `embeddings_768`"* — a hard-coded two-table universe that predates `embeddings_1024_qwen`. ADR-49 §5 then requires:

> "`search_semantic`/`search_by_concept` bez jawnie podanego `model_id` powinno paść na `is_default=TRUE` (nowy model) … **nie** fallbackować cicho do starego modelu … Jawny parametr `model_id` w MCP pozwala nadal odpytać stary korpus"

An unchanged `search_semantic` searches only 384/768 and will return *zero* results for every file written after the cutover, since new files no longer get `embeddings_384` at all (per `smartfs-ai.md`). Note also the parameter renaming: base says `model?`, ADR-49 says `model_id`.

**Fix:** `docs/crates/smartfs-mcp.md` must carry a `search_semantic` delta: table selection driven by resolved `model_id` (defaulting to `is_default=TRUE`), the no-silent-fallback rule, and the `model?` → `model_id?` rename.

---

### S-03 — ADR-39's offline fallback is silently broken by the ADR-49 cutover

**Files:** `SmartFS_Architecture_v4_5.md` §19 (ADR-39), §10.3; `docs/crates/smartfs-ai.md` lines 18-20

ADR-39: *"`search_functions` fallback do `embeddings_384` offline | Bez `code_model` zwraca file-level zamiast nic"*.

`docs/crates/smartfs-ai.md`: *"nowe pliki **NIE** dostają już `embeddings_384` domyślnie, chyba że jawnie skonfigurowano `legacy_model` w `smartfs.toml`"*.

So the fallback that ADR-39 exists to guarantee ("Funkcja działa od razu offline") now returns nothing for any file written after the upgrade. Nothing in v6.0 marks ADR-39 as superseded or redirects the fallback to `embeddings_1024_qwen`.

**Fix:** state in `docs/crates/smartfs-ai.md` or a short ADR-49 addendum that ADR-39's fallback target becomes `embeddings_1024_qwen`.

---

### S-04 — ADR-55 attributes the ONNX Runtime CPU baseline to ADR-49, which never mentions it

**Files:** `docs/adr/ADR-55-gpu-acceleration.md` Decyzja §3 and Otwarte pytanie

> "`ort` (ONNX Runtime, silnik **CPU-baseline z ADR-49**) też jest bindingiem Rust do biblioteki C++"
> "**ADR-49 już ustalił ONNX Runtime jako fundament CPU-baseline**"

ADR-49 is entirely about model selection (Qwen 0.6B / 4B / VL, dimensions, cutover procedure). It contains no reference to ONNX, `ort`, or any inference runtime. ONNX Runtime comes from base §3.7 (`smartfs-ai — Embedding Worker + Tree-sitter + ONNX`) and base §18's dependency list; there is no ADR for it. Since `docs/05` §6 requires ADR references to be load-bearing and durable, a citation pointing at the wrong document is exactly the failure mode that rule guards against — and this repo has a documented history of one such misattribution.

Related and worth checking in the same pass: ADR-49 never marks **ADR-26** (`all-MiniLM-L6-v2` as `is_default=TRUE`, BGE-M3 opt-in) as superseded, even though that is precisely what it does. Compare FIX-09, which treated exactly this situation for ADR-04 as a defect worth fixing.

**Fix:** change the ADR-55 citations to "base v4.5 §3.7 / §18" (or write the missing ADR), and add a "Supersedes ADR-26" line to ADR-49.

---

### S-05 — `Qwen3-Embedding-4B` is configured as `accurate_model` but has no storage table, and the "BGE-M3 slot" claim is dimensionally impossible

**Files:** `docs/adr/ADR-49-qwen-default-model.md` Decyzja §2; `migrations/005_semantic_consolidation.sql` §1; `docs/crates/smartfs-ai.md` lines 7-12

ADR-49: *"Warianty 4B/2560d i 8B/4096d tej samej rodziny **zajmują slot dotychczas zajmowany przez BGE-M3**"*, and `smartfs-ai.md` sets `accurate_model = "qwen3-embedding-4b"`.

Migration 005 registers `('Qwen3-Embedding-4B', 2560, …)` — with a comment that literally repeats the claim: `-- opt-in "accurate", zajmuje slot BGE-M3`. But BGE-M3's "slot" is the table `embeddings_768 (embedding vector(768))`. A 2560-dimensional vector cannot go there; Postgres will reject it, and doing so would violate Invariant #5 anyway. No `embeddings_2560*` table exists in any migration, nor a corresponding `concept_centroids_2560`.

So `accurate_model` is configurable but unusable: any user who enables it gets a runtime insert failure. (This is the same defect class as B-06, one severity lower only because `accurate_model` is opt-in and not on the README front page.)

**Fix:** either add the 2560 tables, or state that the "accurate" tier is deferred and remove `accurate_model` from `smartfs-ai.md`'s config block and the 4B row from migration 005.

---

### S-06 — `merge_centroids` is specified but never invoked, and its two tuning parameters exist nowhere

**Files:** `docs/03-consolidation-design.md` §2 and §4b; `migrations/005_semantic_consolidation.sql` §4; `docs/crates/smartfs-semantic.md`

`docs/03` §2 states the intent in prose:

> "Okresowy backstop mergowania (§4b) jest osobnym, dużo rzadszym zegarem **w tej samej pętli nadzorcy** — nie osobnym `tokio::task` — żeby dzielić ten sam advisory lock"

but the `consolidation_supervisor` code block immediately above contains no merge branch, no counter, and no call to `merge_centroids`. Neither of the two constants it needs exists in any concrete artefact:

- `MERGE_CHECK_INTERVAL` ("domyślnie raz na 20 cykli konsolidacji tej samej kombinacji") is not a field of `ConsolidationConfig`, not a column of `consolidation_thresholds`, and not declared as a `const` anywhere.
- `merge_threshold` is a parameter of `merge_centroids(tx, plugin_type, model_id, merge_threshold)`; the only guidance is a doc-comment aside — "domyślnie: połowa `join_threshold`" — and it is likewise absent from both the config struct and the table.

Since `join_threshold` is calibrated per combination and deliberately has no default, deriving `merge_threshold` from it needs to be stated as a rule, not an aside.

**Fix:** add the merge branch (with cycle counter) to the supervisor code block, add `merge_threshold DOUBLE PRECISION` and `merge_check_every_n_cycles INT NOT NULL DEFAULT 20` to `consolidation_thresholds` and `ConsolidationConfig`, or state explicitly that `merge_threshold = join_threshold / 2.0` is computed rather than stored.

---

### S-07 — Ten symbols on `smartfs-semantic`'s critical path are absent from its own contract document

See B-14 for the list and consequence. Called out separately here because the aggregate problem — `docs/crates/smartfs-semantic.md` presents itself as complete ("Najbardziej szczegółowo rozpisany crate w tym dokumencie — patrz tam funkcja po funkcji") while omitting roughly a third of the functions its own design document calls — will interact badly with Hard Rule 8 across the whole of Phase 3.

---

### S-08 — The prompt says the Root Invariants are in the README; they are not

**Files:** `docs/06-agentic-execution-plan.md` (subagent rules bullet 3, and prompt Hard Rule 6); `README.md`; `docs/01-architecture.md`

Both the bullet and the rule say *"Root Invariants (**README** + 01-architecture.md, 8 sztuk)"*. `README.md` contains no invariant list at all — its closest content is the philosophy quote ("Plik nie jest ścieżką…"). All eight live only in `docs/01-architecture.md`.

Since Hard Rule 6 declares the invariants inviolable and instructs the agent to halt on any apparent conflict, sending it to a document that does not contain them is a live risk of either a spurious halt or a subagent proceeding with five invariants (from base §3.1) instead of eight.

Two related points on the same passage:

- `docs/01-architecture.md` says invariants 1-5 are *"zacytowane tu **dosłownie**"* from base §3.1. They are not verbatim — base §3.1 is in English ("content_hash = SHA-256(original bytes BEFORE compression) — never after") and `docs/01` renders them in Polish. The *content* matches faithfully on all five; the claim of verbatim quotation does not. Given `docs/05` §7's rule that code/doc-comments are English while `docs/` is Polish, the translation is defensible — but "dosłownie" should be softened to "przetłumaczone" so an agent doing a literal diff doesn't flag it.
- No document contradicts any of the eight invariants substantively, with one exception already covered: B-06's implicit invitation to violate Invariant #5 or #4.

**Fix:** drop "README +" from both places, or copy the eight invariants into the README.

---

### S-09 — Base ROOT CLAUDE.md's crate map is stale (9 crates, missing `smartfs-semantic` and `smartfs-docgen`)

**Files:** `SmartFS_Architecture_v4_5.md` §3.1; `docs/02-crates.md`

§3.1's `## Crate map` lists schema, store, db, compress, fuse, ai, mcp, ipfs, cli. v6.0 adds `smartfs-semantic` and `smartfs-docgen` (11 total, as ADR-56 correctly counts). Since §3.1 is the document the agent is told to treat as ROOT CLAUDE.md and will plausibly transcribe into the repo's actual root `CLAUDE.md`, the committed root context will be missing the two new crates — including the one described as "serce v6.0".

**Fix:** `docs/01-architecture.md` already extends §3.1's invariants section; extend the crate map in the same place with the same "rozszerzenie ROOT CLAUDE.md" framing.

---

### S-10 — Hard Rule 5 requires `smartfs-docgen` at the end of every phase; `smartfs-docgen` is built in Phase 5

**Files:** `docs/06-agentic-execution-plan.md` (Hard Rule 5, Phase 5 row, Phase 6 row)

Rule 5: *"Każda publiczna funkcja/struct/enum/impl dostaje `@id` … generowane przez smartfs-docgen **na końcu każdej fazy**, nigdy ręcznie wpisywane na sztywno."*

`smartfs-docgen` is Phase 5. At the end of Phases 0-4 it does not exist. Phase 5's own note ("może iść równolegle z fazami 1-4 od początku") does not cover Phase 0, and "may run in parallel" is not "must be completed first".

Second contradiction in the same rule: `docs/03` and `docs/04` contain roughly twenty hard-coded `@id:` UUIDs in their code samples (e.g. `consolidate_batch` = `8d3a6f01-2c59-4d77-b6e4-1f8a3c5d9e02`). The agent transcribing those samples *is* writing UUIDs by hand — which Rule 5 forbids — and if it instead lets docgen generate fresh ones, every `symbol://` reference and every `@id` in the specification becomes wrong on day one.

**Fix:** move `smartfs-docgen` to Phase 0.5 (it has no smartfs dependencies, so nothing prevents it), and change Rule 5 to: "use the `@id` given in the specification where one is given; generate via `backfill_missing_ids` for everything else."

---

### S-11 — The fixes doc's own verification checklist mandates partitioning that migrations 003/004 deliberately omit

**Files:** `SmartFS_v4.5_to_v5.0_fixes.md` (Checklist weryfikacyjny, FIX-07, ADR-48); `migrations/003_embeddings.sql` header; `migrations/004_ast_nodes.sql` header

The fixes doc's final checklist includes:

> `[ ] §6 ast_embeddings_1536 spójne z partycjonowaniem z roadmap §3b (kolumna plugin_type, PARTITION BY LIST)`

and ADR-48 records LIST partitioning per `plugin_type` as an accepted decision. Migrations 003 and 004 explicitly decline to implement it, with a well-argued header comment (see the SOLID section — this is good work). But the two documents now disagree, and the fixes doc closes by asserting *"Po przejściu całego checklisty dokument jest v5.0 — bez znanych błędów poprawności"* — i.e. the build cannot satisfy its own predecessor's completion criterion.

There is a real technical consequence beyond bookkeeping: migration 005 adds `plugin_type` as a plain column to tables that ADR-48 says should be *partitioned by* that column. Retrofitting LIST partitioning later will require rewriting all four embedding tables plus their HNSW indexes.

**Fix:** add one line to the fixes-doc checklist (or a v6.0 note beside it) recording that FIX-06/07 are roadmap §3b post-MVP and intentionally not in migrations 003/004/005, cross-referencing the 003 header comment.

---

### S-12 — Two config files, no rule for which key lives where

**Files:** `docs/adr/ADR-55-gpu-acceleration.md` Decyzja §7; `README.md` line 48; `docs/01-architecture.md` line 54; `docs/crates/smartfs-ai.md`; `docs/adr/ADR-49-qwen-default-model.md`; `SmartFS_Architecture_v4_5.md` §19 (ADR-23)

`gpu_acceleration = "vulkan" | "cpu"` is placed in **`koval.toml`** by three documents. Every other runtime setting — `default_model`, `accurate_model`, `image_model`, `legacy_model`, `code_model`, `max_vectors` — lives in **`smartfs.toml`**.

`koval.toml` is not invented: ADR-23 defines it as "KOVAL compatibility … kompilacja optymalna per-maszyna", and the roadmap uses it for build-time feature rules (`require_io_uring`, `min_gpu_vram_gb`). So placing a *build/hardware* toggle there is arguably coherent — but the spec never states the division of responsibility, and `gpu_acceleration` reads as a runtime engine selector consumed by `smartfs-ai`, which otherwise reads `smartfs.toml`. An autonomous agent will either create two loaders or, more likely, put everything in one file and quietly diverge from ADR-55.

**Fix:** one sentence somewhere authoritative: "`koval.toml` = build/hardware capability rules; `smartfs.toml` = runtime configuration" — and confirm which one `smartfs-ai` actually reads at startup for `gpu_acceleration`.

---

### S-13 — Nothing enforces "exactly one `is_default=TRUE`", and migration 005 transiently has two

**Files:** `migrations/002_embedding_models.sql`; `migrations/005_semantic_consolidation.sql` §1; `docs/00-overview.md` §3

Both migrations end with a verification checkbox asserting *"dokładnie jeden wiersz is_default=TRUE"*, but `embedding_models` has no constraint enforcing it — only `name TEXT NOT NULL UNIQUE`. Migration 005 §1 inserts Qwen with `is_default = TRUE` **before** running `UPDATE embedding_models SET is_default = FALSE WHERE name = 'all-MiniLM-L6-v2'`, so the table legitimately holds two defaults between the two statements, and would hold them permanently if the migration were interrupted between them.

This sits awkwardly with the thesis `docs/00-overview.md` §3 and ADR-54 both advance — that SmartFS's correctness wins come from pushing invariants into the schema rather than trusting prose. This is a checklist item that could be a constraint and is not.

**Fix:** add `CREATE UNIQUE INDEX one_default_model ON embedding_models ((is_default)) WHERE is_default;` in 002 (deferrable, or with the UPDATE reordered before the INSERT in 005), turning the checkbox into an enforced invariant.

---

### S-14 — `smartfs-docgen`'s error type is undefined and cannot be `SmartFsError`

**Files:** `docs/04-uuid-doc-linking.md` (`resolve_symbol_link`); `docs/02-crates.md` (dependency graph); `docs/crates/smartfs-docgen.md`

`docs/04` uses `Result<...>` throughout and `Error::UnknownSymbol(id)` in one place. Neither `Error` nor the `Result` alias is defined. And `docs/02-crates.md` forecloses the obvious answer:

> "`smartfs-docgen` ← samodzielny dev-tool, importuje tylko `tree-sitter-rust`; **NIE importuje żadnego innego crate'a smartfs**"

so it cannot use `smartfs_schema::SmartFsError`, which base §3.2 designates the single project-wide error type ("SmartFsError enum (top-level error type, **all crates use this**)") and which `docs/03` §9 reaffirms as a Root Invariant. `smartfs-docgen` is therefore a documented, deliberate exception to that invariant that the spec never acknowledges as one.

`docs/crates/smartfs-docgen.md` lists five functions returning `Result<...>` and an unrelated `ConsistencyIssue` enum, but no error enum.

**Fix:** state the exception explicitly and define `DocgenError` (variants: `UnknownSymbol`, `DuplicateId`, `Io`, `Parse`) in `docs/crates/smartfs-docgen.md`. Also note that `uuid` and `serde`/`serde_json` are needed beyond `tree-sitter-rust`, since Rule 8 discourages guessing dependencies.

---

### S-15 — Phase 3's stated rationale is factually wrong about `smartfs-fuse`

Covered under B-05. Separately flagged because the *ordering* happens to be safe while the *reason given* is false, and an agent reasoning from the reason (e.g. deciding it can move `smartfs-fuse` earlier) would break the build.

---

### S-16 — The mapping from `(plugin_type, model_id)` to a concrete embedding/centroid table pair is never stated

**Files:** `docs/03-consolidation-design.md` §4, §5; `docs/crates/smartfs-semantic.md`; `migrations/005_semantic_consolidation.sql` §1, §5

Every supervisor is keyed on `(plugin_type, model_id)` and must operate on one of four embedding tables and its matching centroid/member pair. No document states the routing rule. The natural rule (`model_id` → `embedding_models.dimensions` → table) is *ambiguous by construction* after migration 005, because two registered models share 1024 dimensions:

```sql
('Qwen3-Embedding-0.6B', 1024, …, TRUE),   -- → embeddings_1024_qwen
('Qwen3-VL-Embedding-2B', 1024, …, FALSE); -- → embeddings_1024_qwen_vl (which doesn't exist, B-06)
```

and 384/1536 are ambiguous in a different way: `all-MiniLM-L6-v2` (384) feeds `embeddings_384` for file-level content but, per base §10.3, is *also* the default AST model. `text-embedding-3-large` (1536) is unambiguous only because `ast_embeddings_384` was never created.

`BufferedVector` carries no discriminator either — `docs/03` §4 says it has `ast_node_id`/`version_id` as `Option<Uuid>` with "dokładnie jedno `Some`", which distinguishes AST from file-level but not 384 from 768 from 1024.

**Fix:** add an explicit table-routing table to `docs/crates/smartfs-semantic.md` (model name → embedding table → centroid table → member table → member key column), or add a `target_table TEXT NOT NULL` / `dimensions INT` discriminator to `consolidation_thresholds` so the supervisor's scope is data-driven rather than inferred.

---

### S-17 — `SymbolRecord` and `symbol_registry.schema.json` disagree on tombstone fields

**Files:** `docs/04-uuid-doc-linking.md` (`SymbolRecord`); `docs/symbol_registry.schema.json`; `docs/crates/smartfs-docgen.md`

The JSON schema declares `tombstoned_reason: ["string","null"]`. The Rust struct in `docs/04` has `tombstoned: bool` and `doc_summary` but **no** `tombstoned_reason` field. `docs/crates/smartfs-docgen.md` specifies `tombstone_symbol(&mut SymbolRegistry, Uuid, reason: &str)` — so a reason is captured, but has nowhere to live in the struct that gets serialized.

Compounding it, `resolve_symbol_link` returns `ResolvedLocation::Tombstoned { removed_summary: record.doc_summary.clone() }` — i.e. it surfaces the *old doc summary* rather than the tombstone reason, while `docs/04`'s prose promises the link resolves to *"jawny komunikat 'symbol usunięty w commicie X'"*, which is neither field.

**Fix:** add `tombstoned_reason: Option<String>` to `SymbolRecord`, have `ResolvedLocation::Tombstoned` carry it, and decide whether "commit X" is captured (which would require git access `smartfs-docgen` is not otherwise given).

---

### S-18 — Roadmap §7c prescribes `CUDAExecutionProvider`, which ADR-55 forbids absolutely

**Files:** `SmartFS_Known_Limitations_Roadmap.md` §7c; `docs/adr/ADR-55-gpu-acceleration.md` Decyzja §5

Roadmap §7c: *"ONNX Runtime z `CUDAExecutionProvider` — bez zmian w kodzie embeddingu. Inference BGE-M3 na GPU: ~100ms zamiast ~5s na CPU."*

ADR-55 §5: *"CUDA i ROCm/HIP pozostają **całkowicie wykluczone** … żaden zamknięty, jednowendorowy stack obliczeniowy."*

The roadmap is a live document in the build tree (README links it, `docs/06` cites it for what is deliberately deferred), and ADR-55 does not mark §7c as superseded. Since ADR-55's whole framing is that the exclusion is a *principle* rather than a limitation, leaving a contradicting recommendation in the roadmap undermines it — and Phase 7 is exactly where an agent would go looking for GPU guidance.

**Fix:** strike or annotate roadmap §7c ("superseded by ADR-55 — Vulkan only"). Same document, §3a, also states `png.json → embedding: null → brak indeksu`, which ADR-49 reverses; worth the same treatment.

---

### S-19 — The lexical graph's link population is unspecified

**Files:** `migrations/005_semantic_consolidation.sql` §6; `docs/03-consolidation-design.md` §8; `docs/crates/smartfs-semantic.md`

Migration 005 creates four `word_centroid_links_*` tables with `weight DOUBLE PRECISION NOT NULL`. `docs/03` §8 fully specifies `import_wordnet` (good — see SOLID) and `label_centroid_from_members` (good), but nothing specifies how a lemma gets *linked* to a centroid or what `weight` means. The only related function is:

| `link_word_to_centroid` | `async fn(&Pool, Uuid, Uuid, f64) -> Result<()>` | **Ręczne/półautomatyczne** powiązanie |

"Ręczne/półautomatyczne" is not implementable — there is no CLI subcommand, no MCP tool, and no automatic path. Meanwhile `docs/crates/smartfs-mcp.md` promises `search_by_concept` returns centroid labels "jeśli już przypisaną przez **graf leksykalny** lub `label_centroid_from_members`", implying the lexical route is expected to work.

**Fix:** either specify the automatic linking rule and `weight` semantics (e.g. cosine of the lemma's embedding to the centroid, thresholded), or mark the lexical→centroid linking explicitly post-MVP and remove it from `search_by_concept`'s described behaviour.

---

### S-20 — ADR-49 strands the pre-computed Wikipedia corpus that ADR-06 is built on

**Files:** `SmartFS_Architecture_v4_5.md` §8.1, §19 (ADR-06); `docs/adr/ADR-49-qwen-default-model.md`

ADR-06 ("Pre-computed Wikipedia embeddings | 144M wektorów gotowych; nie embeddingujemy sami") rests on §8.1's two named corpora: `Cohere/wikipedia-22-12` (768d) and `Upstash/wikipedia-2024-06-bge-m3` (**BGE-M3, 768d**, 11 languages incl. Polish). Those vectors live in `embeddings_768` and are only comparable to other BGE-M3 vectors.

ADR-49 demotes BGE-M3 out of the `accurate_model` slot in favour of Qwen 4B (2560d) and makes Qwen 0.6B (1024d) the default. Under Invariant #5 the Wikipedia corpus becomes an island: searchable only by explicitly passing the BGE-M3 `model_id`, never reachable from the default search path, and never re-embeddable (144M vectors is far beyond `smartfs-cli reembed`'s scope).

This may well be an acceptable trade, but it is a consequence of ADR-49 that ADR-49 does not mention, and the virtual-adapter feature (§8) is a headline capability of the project.

**Fix:** add a paragraph to ADR-49 stating that BGE-M3/`embeddings_768` is retained as a first-class *queryable* tier specifically for pre-computed external corpora (ADR-06), even though it is no longer the "accurate" tier for locally-written files.

---

### S-21 — `docs/03`'s `attach_to_centroid` never persists the recomputed label, and the 20% rule has no state to compare against

**Files:** `docs/03-consolidation-design.md` §5 and §8 step 5

§8 step 5: *"Wynik jest cache'owany w `concept_centroids_*.label` i przeliczany tylko wtedy, gdy `member_count` zmieni się o **więcej niż 20%** od ostatniego przeliczenia."*

`concept_centroids_*` has `label TEXT` but no column recording the `member_count` at which the label was last computed — so "od ostatniego przeliczenia" is not computable from the schema. And `attach_to_centroid` (which is the only place `member_count` changes during normal operation) never calls `label_centroid_from_members`, so labels are only ever set at centroid creation, if then.

**Fix:** add `label_member_count BIGINT` (or `labeled_at`) to `concept_centroids_*` and add the conditional relabel call to `attach_to_centroid`.

---

# 3. MINOR

Worth fixing, low risk of derailing the build.

- **M-01 — Dangling `§2b` reference.** `migrations/005_semantic_consolidation.sql` §4 comment: *"patrz docs/03 §2b"*. `docs/03-consolidation-design.md` has §2 but no §2b. (The intended target is §2's last paragraph plus §6.)

- **M-02 — Wrong section in the `join_threshold` comment.** Same file: *"join_threshold … BEZ sensownej wartości domyślnej — patrz docs/03 §5, wymaga kalibracji"*. Calibration is `docs/03` §6; §5 is the join/split algorithm.

- **M-03 — `docs/03` section numbering is out of order.** Sections run §1, §2, §3, §4, §5, **§4b**, §6, §7, §8, §9, §10 — `§4b` (merge) is physically placed after `§5`. Since eight other documents cross-reference "docs/03 §4b", an agent scanning sequentially may stop looking before reaching it.

- **M-04 — Unsuffixed table names in the crate map.** `docs/02-crates.md` line 39 and `docs/crates/smartfs-semantic.md`'s "Owns exclusively" name `concept_centroids` / `centroid_members`; the actual tables are always dimension-suffixed (`_1536`, `_384`, `_768`, `_1024_qwen`). Harmless in prose, but it is the phrasing that would make an agent think one universal table exists — precisely what ADR-51 says must not happen.

- **M-05 — `ast_nodes.source` attributed to §3.7.** `docs/adr/ADR-54-fulltext-search-backend.md` ("`ast_nodes.source` … już istnieje w schemacie (v4.5 §3.7)") and `migrations/006_fulltext_search.sql` line 39 ("source już istnieje od v4.5 §3.7"). §3.7 is `smartfs-ai/CLAUDE.md`; the `source` column is defined in §6 (and §11).

- **M-06 — `symbol_registry.schema.json` uses a non-keyword.** `"example": "smartfs-semantic"` inside property objects is not a JSON Schema 2020-12 keyword (`examples`, an array, is). Also no top-level `required` for `generated_at`/`symbols`, and no `additionalProperties: false`, so the schema will validate almost anything.

- **M-07 — `consolidation_thresholds.updated_at` never updates.** Declared `NOT NULL DEFAULT NOW()` with no trigger and no mention in `calibrate_join_threshold`'s described `INSERT … ON CONFLICT DO UPDATE`. `calibrated_at` is nullable and written by nothing.

- **M-08 — Partial BM25 index may not be supported.** `migrations/006_fulltext_search.sql` creates `... USING bm25 (id, search_text) WITH (key_field='id') WHERE search_text IS NOT NULL`. `WHERE` on a `bm25` index is not guaranteed across pg_search versions. The migration's own header commendably warns to verify syntax against the installed version — but the verification checklist at the bottom does not include "confirm the partial index was actually created as partial", and `docs/crates/smartfs-ai.md` relies on that filtering ("partial index BM25 z migracji 006 to filtruje").

- **M-09 — Near-identical `@id`s invite a copy-paste error.** `kmeans2` = `5c8e2a94-1f6d-4b37-a8c0-3e7f9d2b6a15` and `split_centroid` = `5c8e2a94-1f6d-4b37-a8c0-3e7f9d2b6a16` differ only in the final character. They are distinct, so not a duplicate — but given that docgen hard-errors on collisions, two UUIDs one character apart are worth regenerating.

- **M-10 — No index supporting `reparent_members`/`reassign_members`.** `centroid_members_*` has `PRIMARY KEY (centroid_id, <member>)`, which serves lookup-by-centroid. `merge_centroids` and `split_centroid` update rows by centroid, so this is adequate; but there is no index on the member column, so any future "which centroid does this node belong to" query (plausible for `search_by_concept`'s result grouping) is a full scan.

- **M-11 — README's delta table makes `plugin_type` non-optional.** `search_by_concept(query, plugin_type, limit)` vs `plugin_type?` in `docs/02` and `docs/crates/smartfs-mcp.md`. Subsumed by B-08, listed here for the fix checklist.

- **M-12 — `smartfs-docgen`'s stated dependency list is too narrow.** `docs/02-crates.md`: "importuje tylko `tree-sitter-rust`". It also needs `uuid` (for `Uuid::new_v4()` in `backfill_missing_ids`) and `serde`/`serde_json` (to emit `symbol_registry.json`). Trivial, but Rule 8 tells the agent not to guess dependencies.

- **M-13 — `docs/06` never links forward.** Every other doc in the chain ends with "Dalej: …"; `docs/06-agentic-execution-plan.md` ends without one, and nothing points the reader to `docs/adr/` (related to B-10).

- **M-14 — Vision doc asserts a complete ADR range.** `docs/vision/north-star-kernel-native.md` line 21: *"Wszystkie ADR-y 01-57 przeżywają to przejście praktycznie nietknięte."* Consistent with `docs/01-architecture.md`, but compounds B-03 by implying 51 and 52 exist as documents. Otherwise the vision doc is correctly and thoroughly fenced (see SOLID).

---

# 4. THINGS THAT ARE ACTUALLY SOLID

Checked carefully, found genuinely well-specified.

**1. Migration 001's handling of the `storage_backends` gap, and FIX-01's integration.** The header comment does exactly what a good migration comment should: it names a real defect in the source document rather than papering over it — *"§4.2 oryginalnego dokumentu architektury nie przypisuje `storage_backends` do żadnej konkretnej migracji — trafia tu, bo `inode_registry.backend_id` i `blobs.backend_id` są do niej FK, więc musi istnieć pierwsza."* (Verified: base §6 indeed defines `inode_registry` with `backend_id UUID REFERENCES storage_backends(id)` while never creating that table — 001 catches a genuine bug in v4.5.) The FIX-01 refcount removal is baked into the DDL rather than described, and the verification checkbox — *"blobs NIE MA kolumny refcount (FIX-01 — jeśli jest, migracja jest z przedwersji v4.5, nie v5.0)"* — is a genuinely clever tripwire that would catch an agent regressing to the base document. The `setval('inode_registry_ino_seq', 1)` root-inode seed and its explanatory comment (ADR-21) are correct.

**2. Migration 003's explicit adjudication of the FIX-07 discrepancy.** Rather than silently picking a side, the header lays out both readings — FIX-07's prose assigns LIST partitioning to `003_embeddings.sql`, but FIX-06/07 actually target roadmap §3b post-MVP — states which one this migration implements and why, and notes the forward consequence (*"Migracja 005 … już zakłada TĘ, niespartycjonowaną wersję (dodaje `plugin_type` przez `ALTER TABLE`, co nie miałoby sensu, gdyby kolumna już istniała)"*). This is exactly the reasoning an autonomous agent cannot do for itself, written down at the point of use. (S-11 asks only that the fixes-doc checklist be annotated to match.)

**3. Migration 005 §5's four explicit DDL blocks with correct FK asymmetry.** The comment says why the shortcut was refused — *"poprzednia wersja migracji miała pełne DDL tylko dla 1536 i komentarz 'replikować analogicznie' … żeby różnice w kluczu obcym (`ast_node_id` vs `version_id`) nie zostały zgadnięte niespójnie"* — and the four blocks are in fact correct and consistent: `centroid_members_1536` keys to `ast_nodes(id)` (matching ADR-19's rule that AST embeddings never FK to `file_versions`), the other three key to `file_versions(id)`, all four `concept_centroids_*` carry a self-referential `merged_into`, and all four HNSW indexes are correctly partial on `WHERE is_active = TRUE`, which is the right choice given `search_via_centroids` filters on exactly that predicate. The `word_centroid_links_*`-per-dimension / `lexical_nodes`-dimension-independent split is also correctly reasoned.

**4. The fail-safe design of `consolidation_thresholds`.** `join_threshold DOUBLE PRECISION NOT NULL` with deliberately *no* default, plus "no row ⇒ no supervisor", is the right shape: it makes an uncalibrated system inert rather than silently wrong, and the migration comment states the principle plainly (*"fail-safe, nie fail-open: lepiej nic nie konsolidować niż konsolidować z niewykalibrowanym `join_threshold=0`"*). Every other threshold in the table has a sensible default; only the one that genuinely cannot have a universal value is left unset. B-07 is a wiring gap in how the row gets created — the design itself is correct.

**5. ADR-55's intellectual honesty about its own cost, and its Phase-7 fencing.** An ADR that argues for a principled constraint and then documents, with sources, that the constraint costs ~44%/~202% against ROCm on desktop AMD and produces a **~15× regression versus CPU** on Adreno/Mali phones — and explains *why* the upstream bug is unlikely to be fixed (closed driver blobs, no reverse-engineering capacity in a volunteer project) — is doing the job an ADR exists to do. Combined with the explicit Phase 7 gating in `docs/06` (both the phase table and Hard Rule 9), this is one of the cleanest post-MVP fences in the repo.

**6. The post-MVP quarantine holds.** I specifically checked whether `docs/vision/north-star-kernel-native.md`, ADR-56 or ADR-57 leak into the Phase 0-6 requirements. They do not. The vision doc opens with *"To NIE jest ADR … nie po to, żeby cokolwiek z tego wchodziło do Faz 0-6 albo do promptu dla Antigravity"*; README §"Wizja daleka" repeats *"nic stąd nie wchodzi do promptu dla Antigravity"*; `docs/01-architecture.md` gives ADR-56/57 their own "Post-MVP, poza zakresem Faz 0-6" block; both ADRs open with a Status line saying so and close with "Zero zmian w Fazach 0-6". No migration, crate doc, or phase row references any of the three as a build requirement. Their internal technical claims also check out on inspection — ADR-56's `special_data.agent` genuinely needs no migration (`file_versions.special_data JSONB` with `idx_versions_special` GIN already exists in migration 001), ADR-57's `describe_plugin_type`/`list_plugin_types` genuinely need only read access to `plugins/*.json`, and the vision doc's `switch_root`/early-userspace analysis is technically sound. The only correction needed anywhere in this group is M-14.

---

## Suggested order of remediation

1. **B-01** (subagent context / apply FIX-01..10 to the base doc) — largest blast radius, cheapest fix.
2. **B-02** (`pg_try_advisory_xact_lock`) — a genuine correctness bug, not a documentation one.
3. **B-03, B-10** (missing ADR files; ADRs missing from the reading order) — pure editing.
4. **B-04, B-08, B-09, B-14, S-07, S-16** — one focused pass over `docs/crates/smartfs-semantic.md` + `docs/crates/smartfs-mcp.md` resolving signatures, the query-vector path, table routing, and the ten missing symbols.
5. **B-06, B-07, S-05** — decide whether the VL/4B tiers and the `calibrate`/`reembed` CLI subcommands are in or out of MVP, then make the migrations and crate docs agree.
6. **B-05, B-12, B-13, B-11, S-10** — mechanical corrections.
7. Everything in SIGNIFICANT / MINOR as a final consistency sweep.

Nothing above requires revisiting an architectural decision. The design is sound; the specification is not yet self-consistent enough to be executed without a human in the loop.
