# SmartFS — Specyfikacja Napraw v4.5 → v5.0
## Patch-spec do zaaplikowania przez agenta (Claude Code / Gemini)

**Cel:** Zamknąć wszystkie błędy poprawności znalezione w peer review v4.5. Po zaaplikowaniu wszystkich napraw dokument architektoniczny jest kompletny i wolny od sprzeczności międzysekcyjnych.

**Jak używać:** Aplikuj naprawy w kolejności FIX-01 → FIX-10. Każda ma sekcję docelową, opis problemu w jednym zdaniu, oraz dokładny blok `USUŃ` / `WSTAW`. Kod, identyfikatory i komentarze po angielsku (zgodnie z konwencją projektu); proza wyjaśniająca po polsku. Na końcu jest checklist weryfikacyjny.

**Zasada nadrzędna przy aplikowaniu:** nie zmieniaj niczego poza tym, co wskazane. Nie „ulepszaj" schematu przy okazji. Jeśli fragment do usunięcia nie istnieje dokładnie tak jak podano, zatrzymaj się i zgłoś rozbieżność zamiast zgadywać.

---

## Tabela zbiorcza

| Fix | Sekcja | Severity | Problem w jednym zdaniu |
|-----|--------|----------|--------------------------|
| FIX-01 | §6 (`blobs`), §7.3, §17 Etap 8 | HIGH | `refcount` rośnie nieatomowo względem referencji → dryf w górę, GC `WHERE refcount=0` nigdy nie sprząta; dwie sprzeczne strategie GC |
| FIX-02 | §11.1, §12.1, §12.2, §6 (`ast_embeddings_1536`) | HIGH | `is_current` ustawiane w `cow_commit` ściga się z async workerem → dwie wersje `is_current=TRUE` |
| FIX-03 | §7.3, §11.1 (`cow_commit`) | MED | `xmax=0` bywa false-negative pod współbieżnością → pominięty `store.put`, martwy blob |
| FIX-04 | §7.3, §11.1 (`cow_commit`) | MED | błąd `store.put` (nie crash) zostawia zatruty wiersz `blobs` referencjonowany później |
| FIX-05 | §12.2 (worker) | MED | anti-starvation valve odblokowuje start, ale preempcja w trakcie daje livelock zamiast pracy |
| FIX-06 | Roadmap §3b | HIGH | `one_active_per_type UNIQUE (…, is_active)` blokuje wiele nieaktywnych shardów |
| FIX-07 | Roadmap §3b | MED | runtime `CREATE TABLE` per shard łamie Invariant #4 (no DDL in runtime) |
| FIX-08 | nagłówek | LOW | wersja/changelog nadal mówi „3.5 / względem v3.0" |
| FIX-09 | ADR-04 | LOW | ADR-04 opisuje advisory lock jako aktualną decyzję, choć ADR-40 go zastępuje |
| FIX-10 | §17 Etap 1, §3.5 nagłówek CLAUDE.md | LOW | deliverable i komentarz mówią „advisory lock dedup" |

---

## FIX-01 — `refcount` przestaje być autorytetem; GC-by-scan jest jedyną strategią

**Lokalizacja:** §6 (`CREATE TABLE blobs`), §7.3 (flow dedupu), §17 Etap 8 (GC).

**Problem:** `ON CONFLICT DO UPDATE SET refcount+1` inkrementuje licznik w kroku 1 (poza transakcją), a wiersz `file_versions` — jedyną prawdziwą referencję — tworzysz w kroku 2 (osobna transakcja). Nie są atomowe. Gdy krok 2 padnie, `refcount` już podbity, referencji nie ma, a nic nigdzie licznika nie dekrementuje (brak dekrementu przy unlink/usunięciu wersji w całym dokumencie). `refcount` jest write-only i dryfuje w górę → GC `WHERE refcount=0` nigdy nie zadziała. Dodatkowo §17 Etap 8 opisuje **inną** strategię (scan po `file_versions`), sprzeczną z §7.3.

**Zmiana 1 — schema tabeli `blobs` (§6):**

USUŃ kolumnę `refcount` i komentarz o niej. WSTAW wersję bez refcountu:

```sql
-- ─────────────────────────────────────────────────────────
-- Tabela blobs — punkt serializacji dedupu (content-addressed)
-- GC: scan po file_versions (patrz §7.5), NIE refcount.
-- Refcount celowo usunięty: nie da się go utrzymać atomowo względem
-- referencji tworzonej w osobnej transakcji (krok 2 cow_commit).
-- ─────────────────────────────────────────────────────────
CREATE TABLE blobs (
    content_hash    TEXT PRIMARY KEY,          -- full SHA-256 hex (256-bit, zero collisions)
    blob_id         UUID NOT NULL,             -- uuid in blob store
    backend_id      UUID REFERENCES storage_backends(id),
    size            BIGINT NOT NULL,           -- original size (pre-compression)
    compressed_size BIGINT,                    -- set after store.put succeeds
    created_at      TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
```

**Zmiana 2 — flow dedupu (§7.3):**

USUŃ `ON CONFLICT (content_hash) DO UPDATE SET refcount = blobs.refcount + 1`. WSTAW no-op UPDATE, który jedynie umożliwia `RETURNING` przy konflikcie:

```sql
-- Atomic serialization point. The DO UPDATE is a no-op whose only purpose
-- is to make RETURNING return the existing row on conflict.
INSERT INTO blobs (content_hash, blob_id, backend_id, size, compressed_size)
VALUES ($hash, $uuid, $backend_id, $orig_size, NULL)
ON CONFLICT (content_hash) DO UPDATE
    SET content_hash = blobs.content_hash        -- no-op touch
RETURNING blob_id, (xmax = 0) AS inserted;
-- inserted=TRUE  → you own this content → store.put (see FIX-03/FIX-04)
-- inserted=FALSE → reuse blob_id (but verify it physically exists, FIX-03)
```

**Zmiana 3 — dodaj nowy podrozdział §7.5 (GC-by-scan jako jedyna prawda):**

```markdown
### 7.5. Garbage Collection (post-MVP, refcount-free)

Blob jest osierocony, gdy żadna wersja go nie referencjonuje. GC jest okresowym
skanem, nie licznikiem — dzięki temu nie zależy od atomowości, której między
krokiem 1 a 2 cow_commit nie ma.

    -- 1. Znajdź osierocone bloby (grace window chroni świeże, jeszcze niescommitowane)
    DELETE FROM blobs b
    WHERE NOT EXISTS (
        SELECT 1 FROM file_versions fv WHERE fv.blob_id = b.blob_id
    )
    AND b.created_at < now() - interval '1 hour'   -- grace: in-flight two-step commit
    RETURNING blob_id, backend_id;

    -- 2. Dla każdego zwróconego (blob_id, backend_id): store.delete(blob_id)

Grace window (1h) pokrywa okno między INSERT INTO blobs (krok 1) a INSERT file_versions
(krok 2). Bloby z external_path NIE mają wiersza w blobs (wskazują na plik hosta),
więc GC ich nie dotyczy — store.delete nigdy nie tyka external_path (ADR-38).

Po wejściu FastCDC (roadmap §1): każdy chunk to osobny blob w tej samej tabeli,
więc ten sam scan działa per-chunk bez zmian — kolejny argument za GC-by-scan.
```

**Zmiana 4 — §17 Etap 8:** USUŃ opis GC oparty na refcount. WSTAW odwołanie do §7.5:

```markdown
- GC blobów — scan po file_versions (patrz §7.5, refcount-free)
  - Wersje z external_path != NULL wskazują na pliki hosta — store.delete ich nie tyka
  - Grace window 1h chroni świeże bloby w trakcie two-step commit
```

---

## FIX-02 — `is_current` należy do workera, nie do `cow_commit`

**Lokalizacja:** §11.1 i §12.1 (krok 2 `cow_commit`), §12.2 (worker), §6 (komentarz przy `ast_embeddings_1536`).

**Problem:** `UPDATE ast_embeddings_1536 SET is_current=FALSE WHERE ast_node_id IN (SELECT id FROM ast_nodes WHERE version_id = prev_version)` biegnie w `cow_commit`, ale wektory generuje **później** worker. W chwili commitu wektory poprzedniej wersji często jeszcze nie istnieją → UPDATE trafia 0 wierszy → dwie (lub więcej) wersje kończą z `is_current=TRUE` → `search_functions` zwraca nieaktualne funkcje obok aktualnych.

**Zmiana 1 — USUŃ z kroku 2 `cow_commit` w §11.1 ORAZ §12.1** cały fragment:

```sql
      -- Zdezaktualizuj wektory poprzedniej wersji (is_current → FALSE)
      UPDATE ast_embeddings_1536 SET is_current = FALSE
          WHERE ast_node_id IN (SELECT id FROM ast_nodes WHERE version_id = prev_version.id);
```

i towarzyszący mu opis (`prev_version` służył tylko do tego — jeśli nie jest już nigdzie używany poza `parent_version_id`, zostaw wyliczenie `prev` dla `parent_version_id`, usuń tylko UPDATE).

W §11.1 w bloku „Kluczowe właściwości" USUŃ punkt o `is_current` z `cow_commit`; zastąp:

```markdown
- `is_current` NIE jest ustawiane w cow_commit — należy do workera (patrz §12.2).
  Powód: wektory poprzedniej wersji mogą jeszcze nie istnieć w chwili commitu.
```

**Zmiana 2 — WSTAW do workera (§12.2)** krok po udanym embedowaniu wersji. Reguła jest **order-independent**: po zembedowaniu dowolnej wersji danego inode przelicz `is_current` wszystkich jego wektorów względem prawdziwie najnowszej wersji. Dzięki temu nie ma znaczenia, w jakiej kolejności worker kończy wersje (SKIP LOCKED może je przestawić).

```rust
// Po udanym embed_version(version_id) i wstawieniu wektorów AST:
async fn refresh_is_current(db: &Pool, version_id: Uuid) {
    // 1. inode tej wersji
    let inode_id: Uuid = match sqlx::query_scalar!(
        "SELECT inode_id FROM file_versions WHERE id = $1", version_id
    ).fetch_one(db).await {
        Ok(v) => v,
        Err(e) => { tracing::error!("refresh_is_current inode lookup: {e}"); return; }
    };

    // 2. najnowsza wersja tego inode (jedyna, która ma być is_current)
    let latest_vid: Uuid = match sqlx::query_scalar!(
        "SELECT id FROM file_versions
         WHERE inode_id = $1 ORDER BY version_number DESC LIMIT 1", inode_id
    ).fetch_one(db).await {
        Ok(v) => v,
        Err(e) => { tracing::error!("refresh_is_current latest lookup: {e}"); return; }
    };

    // 3. przelicz is_current dla WSZYSTKICH zembedowanych wektorów tego inode.
    //    Order-independent: is_current = (wektor należy do najnowszej wersji).
    if let Err(e) = sqlx::query!(
        r#"
        UPDATE ast_embeddings_1536 ae
        SET is_current = (fv.id = $1)
        FROM ast_nodes an
        JOIN file_versions fv ON an.version_id = fv.id
        WHERE ae.ast_node_id = an.id
          AND fv.inode_id = $2
        "#,
        latest_vid, inode_id
    ).execute(db).await {
        tracing::error!("refresh_is_current update: {e}");
    }
}
```

Wywołuj `refresh_is_current(&db, version_id)` bezpośrednio po `mark_clean` w pętli workera (bez `?` — log i kontynuuj, zgodnie z ADR-34).

**Zmiana 3 — §6, komentarz pod `ast_embeddings_1536`:** USUŃ komentarz sugerujący UPDATE w commit. WSTAW:

```sql
-- is_current jest zarządzane przez workera (smartfs-ai), nie przez cow_commit.
-- Po zembedowaniu dowolnej wersji inode: is_current = TRUE tylko dla wektorów
-- najnowszej wersji tego inode; reszta = FALSE. Order-independent (patrz FIX-02).
-- Partial HNSW index (WHERE is_current=TRUE) trzyma aktywny zbiór mały.
```

---

## FIX-03 — Siatka bezpieczeństwa dla `xmax=0` (false-negative)

**Lokalizacja:** §7.3 i §11.1 (`cow_commit`, gałąź `inserted=FALSE`).

**Problem:** `(xmax = 0) AS inserted` jest standardowym idiomem, ale gdy Twój świeżo wstawiony wiersz zostanie zablokowany przez współbieżną transakcję, `xmax` bywa niezerowe → dostajesz `inserted=FALSE` mimo że to Ty wstawiłeś → pomijasz `store.put` → `file_versions` wskazuje na blob, którego w store nie ma → odczyt pada na zawsze.

**Zmiana — w gałęzi `inserted=FALSE`** dodaj weryfikację fizycznej obecności bloba (patrz też scalony `cow_commit` w FIX-04):

```rust
} else {
    // inserted=FALSE: ktoś inny (rzekomo) jest właścicielem.
    // Zweryfikuj, że blob FIZYCZNIE istnieje — chroni przed:
    //   (a) xmax false-negative (to my wstawiliśmy, ale xmax != 0),
    //   (b) zatrutym wierszem blobs po wcześniejszym błędzie put (FIX-04).
    if !store.exists(blob_id).await? {
        let compressed = compress(data, level);
        store.put(blob_id, &compressed).await?;   // best-effort heal
    }
}
```

---

## FIX-04 — Kompensacja przy błędzie `store.put` (zatruty wiersz `blobs`)

**Lokalizacja:** §7.3 i §11.1 (`cow_commit`, gałąź `inserted=TRUE`).

**Problem:** Gdy `inserted=TRUE`, wiersz `blobs` jest już zacommitowany. Jeśli potem `store.put` zwróci błąd (dysk pełny, I/O error — nie crash), zostaje wiersz `blobs` z `blob_id` bez fizycznego bloba. Następny writer tej samej treści dostanie `inserted=FALSE`, reużyje ten `blob_id`, założy `file_versions` → blob staje się referencjonowany → GC-by-scan go nie ruszy. Zatruty na stałe.

**Zmiana — scalony, kanoniczny `cow_commit` (zastępuje pseudokod z §11.1 KROK 1 oraz §12.1):**

```
cow_commit(inode_id, data, override_syntax_check):

    // ── KROK 1: poza jakąkolwiek transakcją ─────────────────────────
    hash = SHA-256(data)

    if plugin.ast and not override_syntax_check:
        ast_nodes = tree_sitter_parse(data)      // spawn_blocking, NO lock held
        if parse_error: return Err(SyntaxError)   // CLI: exit 1; FUSE: EACCES już w flush()

    uuid = Uuid::new_v4()
    row = INSERT INTO blobs (content_hash, blob_id, backend_id, size, compressed_size)
          VALUES ($hash, $uuid, $backend_id, $orig_size, NULL)
          ON CONFLICT (content_hash) DO UPDATE SET content_hash = blobs.content_hash
          RETURNING blob_id, (xmax = 0) AS inserted
    blob_id = row.blob_id

    if row.inserted:
        compressed = compress(data, level)
        match store.put(blob_id, compressed):
            Ok  => UPDATE blobs SET compressed_size = len(compressed) WHERE content_hash = $hash
            Err(e) =>
                // KOMPENSACJA: żadna file_versions jeszcze nie referuje tego blobu.
                // Usuń zatruty wiersz, żeby kolejny zapis spróbował od nowa.
                DELETE FROM blobs WHERE content_hash = $hash
                return Err(e)
    else:
        // Weryfikuj fizyczną obecność (xmax false-negative / zatruty wiersz — FIX-03)
        if not store.exists(blob_id):
            compressed = compress(data, level)
            store.put(blob_id, compressed)?

    // ── KROK 2: krótka transakcja — tylko SQL, zero I/O ─────────────
    BEGIN;
      SELECT id FROM inode_registry WHERE id = $inode_id FOR UPDATE;
      n    = SELECT COALESCE(MAX(version_number), 0) + 1
                 FROM file_versions WHERE inode_id = $inode_id;
      prev = SELECT id FROM file_versions
                 WHERE inode_id = $inode_id ORDER BY version_number DESC LIMIT 1;
      INSERT INTO file_versions
          (inode_id, version_number, blob_id, content_hash, size, status, parent_version_id)
          VALUES ($inode_id, n, $blob_id, $hash, $orig_size, 'pending', prev);
      INSERT INTO ast_nodes (...) ON CONFLICT DO NOTHING;         // jeśli ast=true
      UPDATE inode_registry
          SET current_blob_id = $blob_id, size = $orig_size, updated_at = NOW()
          WHERE id = $inode_id;
      // is_current NIE jest tu ruszane — należy do workera (FIX-02)
    COMMIT;
```

Zaktualizuj też skrócony blok w §12.1, żeby był zgodny z powyższym (gałąź `inserted=TRUE` z kompensacją, brak UPDATE `is_current`).

---

## FIX-05 — Anti-starvation valve: wyłącz preempcję na jeden element (livelock)

**Lokalizacja:** §12.2 (supervisor loop + `should_embed_despite_activity`).

**Problem:** Valve sprawia, że worker *ruszy* mimo aktywności zapisu — ale pętla dalej robi `tokio::select!` z `write_activity_detected()`. Pod ciągłym zapisem (`cargo build`) każdy embed startuje i natychmiast dostaje preempcję → `revert_to_pending` → i w kółko. To livelock: valve dowozi start, preempcja zabija robotę, backlog nigdy nie spada.

**Zmiana — w pętli workera** wprowadź `force_mode`. Gdy valve jest aktywny, przetwarzaj co najmniej jeden element **bez** ramienia preempcji:

```rust
loop {
    let force = should_embed_despite_activity(&db).await;   // valve: oldest>60s OR backlog>1000

    if !force {
        activity_monitor.wait_for_idle(Duration::from_millis(500)).await;
    }

    let batch = claim_pending(&db, BATCH_SIZE).await;   // bez ?
    if batch.is_empty() {
        tokio::time::sleep(Duration::from_secs(2)).await;
        continue;
    }

    let mut preempted = false;
    for (i, version_id) in batch.iter().enumerate() {
        if preempted { break; }

        // W trybie force pierwszy element jest NIEPRZERYWALNY — gwarantuje postęp.
        // Bez tego cargo build daje livelock: start → preempt → revert → start...
        let allow_preempt = !force || i > 0;

        if allow_preempt {
            tokio::select! {
                result = embed_version(&db, &store, *version_id) => {
                    finish_embed(&db, *version_id, result).await;  // mark_clean/retry + refresh_is_current
                }
                _ = activity_monitor.write_activity_detected() => { preempted = true; }
            }
        } else {
            // niePRZERYWALNY embed — dowozi co najmniej jeden element pod ciągłym zapisem
            let result = embed_version(&db, &store, *version_id).await;
            finish_embed(&db, *version_id, result).await;
        }
    }

    if preempted {
        for version_id in &batch { let _ = revert_to_pending(&db, *version_id).await; }
    }
}
```

`finish_embed` = `match result { Ok => mark_clean + refresh_is_current (FIX-02); Err => increment_retry }`, wszystko z logowaniem bez `?`.

**Nit wydajnościowy (opcjonalny):** `should_embed_despite_activity` robi `COUNT(*)` na `file_versions` co iterację. Przy dużym backlogu to seq scan. `COUNT(*) ... WHERE status='pending'` korzysta z `idx_versions_pending` (partial), więc jest znośne, ale przy bardzo dużych backlogach rozważ estymatę zamiast dokładnego COUNT. Nie blokujące na MVP.

---

## FIX-06 — Roadmap §3b: `one_active_per_type` odwrócony → partial unique index

**Lokalizacja:** Roadmap, §3b (`CREATE TABLE embedding_index_shards`).

**Problem:** `UNIQUE (plugin_type, model_id, is_active)` znaczy: co najwyżej jeden `(rust, model, TRUE)` **i** co najwyżej jeden `(rust, model, FALSE)`. Z czasem masz wiele nieaktywnych shardów (s0, s1, s2 = `is_active=FALSE`) → kolidują → drugi zamknięty shard rzuca UNIQUE violation.

**Zmiana — USUŃ** constraint z ciała `CREATE TABLE`:

```sql
    CONSTRAINT one_active_per_type UNIQUE (plugin_type, model_id, is_active)
        DEFERRABLE INITIALLY DEFERRED
```

**WSTAW** partial unique index po `CREATE TABLE`:

```sql
-- Dokładnie jeden aktywny shard per (typ, model); dowolnie wiele nieaktywnych.
CREATE UNIQUE INDEX one_active_shard_per_type
    ON embedding_index_shards (plugin_type, model_id)
    WHERE is_active = TRUE;
```

---

## FIX-07 — Roadmap §3b: runtime `CREATE TABLE` per shard łamie Invariant #4

**Lokalizacja:** Roadmap §3b (dynamiczne tabele shardów), koliduje z ROOT CLAUDE.md Invariant #4 („no CREATE TABLE in runtime code — migrations only").

**Problem:** §3b każe demonowi tworzyć `CREATE TABLE ast_embeddings_rust_1536_s0` przy starcie sharda. To wraca do problemu wywalonego na początku projektu: rola runtime z uprawnieniem DDL na produkcyjnej bazie + race przy równoległym `CREATE TABLE IF NOT EXISTS`.

**Zmiana — zastąp app-managed sharding natywnym partycjonowaniem PostgreSQL.** Ten sam cel (mały HNSW per segment, izolacja per-typ, zawsze w RAM) bez runtime DDL i bez fan-out w kodzie (planner robi partition pruning sam).

USUŃ z roadmapy §3b całą sekcję „Tabele embeddingów tworzone dynamicznie gdy shard startuje" (blok z `CREATE TABLE ast_embeddings_rust_1536_s0` w runtime). WSTAW:

```markdown
### 3b. Izolacja per-typ przez natywne partycjonowanie (zamiast runtime DDL)

Zamiast demona tworzącego tabele shardów w runtime — jedna tabela partycjonowana
LIST po plugin_type, z osobnym indeksem HNSW per partycja. Wszystkie CREATE TABLE
są w migracjach (Invariant #4), planner robi partition pruning (zero fan-out w kodzie).

    CREATE TABLE ast_embeddings_1536 (
        ast_node_id UUID NOT NULL REFERENCES ast_nodes(id) ON DELETE CASCADE,
        model_id    UUID NOT NULL REFERENCES embedding_models(id),
        plugin_type TEXT NOT NULL,                      -- klucz partycjonowania
        embedding   vector(1536) NOT NULL,
        is_current  BOOLEAN NOT NULL DEFAULT TRUE,
        PRIMARY KEY (ast_node_id, model_id, plugin_type)
    ) PARTITION BY LIST (plugin_type);

    -- Partycje tworzone MIGRACJĄ przy dodaniu pluginu (nie w runtime):
    CREATE TABLE ast_embeddings_1536_rust
        PARTITION OF ast_embeddings_1536 FOR VALUES IN ('rust');
    CREATE INDEX ON ast_embeddings_1536_rust
        USING hnsw (embedding vector_cosine_ops) WHERE is_current = TRUE;

    CREATE TABLE ast_embeddings_1536_python
        PARTITION OF ast_embeddings_1536 FOR VALUES IN ('python');
    CREATE INDEX ON ast_embeddings_1536_python
        USING hnsw (embedding vector_cosine_ops) WHERE is_current = TRUE;

Search po kodzie Rust: WHERE plugin_type='rust' → planner tyka tylko partycję rust.
Każda partycja ma własny, mały HNSW (partial na is_current). Nowy język = nowa
migracja z partycją, nie runtime CREATE TABLE.

Jeśli pojedyncza partycja przerośnie RAM (miliony funkcji jednego języka) — DOPIERO
wtedy sub-sharding tej partycji, też migracją (RANGE po shard_index). embedding_index_shards
zostaje jako rejestr metadanych, ale NIE steruje runtime DDL.
```

**Uwaga:** tabela `ast_embeddings_1536` z §6 dokumentu głównego dostaje wtedy kolumnę `plugin_type` i klauzulę `PARTITION BY LIST (plugin_type)`. Zsynchronizuj definicję w §6 z powyższą, żeby dokument główny i roadmap się nie rozjechały. Migracja `003_embeddings.sql` tworzy tabelę partycjonowaną + partycje dla języków z `plugins/` obecnych na starcie. *(roadmap §3b — post-MVP; nie wchodzi do migracji 001-006)*

---

## FIX-08 — Nagłówek: wersja i changelog

**Lokalizacja:** blok nagłówkowy dokumentu.

USUŃ:

```markdown
**Wersja:** 3.5 — synteza v3.0 + poprawki Opusa + poprawki Gemini + własne korekty  

**Zmiany względem v3.0:**
```
(wraz z całą listą „Zmiany względem v3.0")

WSTAW:

```markdown
**Wersja:** 5.0 — po pełnym cyklu peer review (v2→v4.5) + domknięcie błędów poprawności

**Zmiany względem v4.5:**
- GC-by-scan zamiast refcount (refcount usunięty jako niemożliwy do utrzymania atomowo) — FIX-01
- is_current zarządzane przez workera, order-independent, nie przez cow_commit — FIX-02
- store.exists() safety net dla xmax false-negative — FIX-03
- Kompensacja DELETE blobs przy błędzie store.put — FIX-04
- Anti-starvation valve wyłącza preempcję na jeden element (koniec livelocku) — FIX-05
- Partial unique index dla aktywnego sharda (był odwrócony constraint) — FIX-06
- Izolacja per-typ przez natywne partycjonowanie zamiast runtime DDL — FIX-07
- ADR-04 oznaczony jako superseded przez ADR-40
```

---

## FIX-09 — ADR-04 oznacz jako superseded

**Lokalizacja:** §19, wiersz ADR-04.

USUŃ obecny wiersz ADR-04. WSTAW:

```markdown
| ADR-04 | ~~`pg_advisory_xact_lock(hashtext($hash))` dla dedupu~~ **SUPERSEDED przez ADR-40** | Advisory lock zwalniał się przy tymczasowym COMMIT (okno I/O na duplikat) i redukował 256-bit hash do 32-bit (false contention). Zastąpiony tabelą `blobs` z UNIQUE na pełnym content_hash. |
```

---

## FIX-10 — §17 Etap 1 i komentarze: „advisory lock" → „blobs"

**Lokalizacja:** §17 Weekend 1 Etap 1 (deliverable), oraz smartfs-db/CLAUDE.md jeśli gdziekolwiek został ślad.

USUŃ w §17 Etap 1:

```markdown
- `smartfs-db`: CoW write, advisory lock dedup, historia wersji
```

WSTAW:

```markdown
- `smartfs-db`: CoW write, dedup przez tabelę `blobs` (ON CONFLICT), historia wersji
```

Przeskanuj cały dokument (`grep -in "advisory"`) i upewnij się, że jedyne wystąpienia to ADR-04 (superseded) i §7.3/ADR-40 (wyjaśnienie *dlaczego* advisory lock został porzucony). Żaden aktywny opis ścieżki zapisu nie może mówić „advisory lock".

---

## Nowe / zaktualizowane ADR (§19)

Dopisz do tabeli ADR:

```markdown
| ADR-44 | GC-by-scan zamiast refcount; refcount usunięty z `blobs` | Refcount inkrementowany w kroku 1 poza transakcją, referencja tworzona w kroku 2 osobno — nieatomowe, dryf w górę, nic nie dekrementuje. Scan po file_versions + grace window 1h jest jedyną prawdą (§7.5) |
| ADR-45 | `is_current` zarządzane przez workera, przeliczane względem najnowszej wersji inode | Ustawianie w cow_commit ścigało się z async embeddingiem (wektory poprzedniej wersji jeszcze nie istniały) → wiele wersji is_current=TRUE. Worker przelicza order-independent po każdym embedzie (FIX-02) |
| ADR-46 | `store.exists()` + kompensacja `DELETE blobs` wokół `store.put` | xmax=0 bywa false-negative pod współbieżnością (pominięty put → martwy blob); błąd put zostawiał zatruty wiersz blobs referencjonowany później. Weryfikacja obecności + kompensacja domykają oba (FIX-03/04) |
| ADR-47 | Anti-starvation valve czyni pierwszy element batcha nieprzerywalnym | Sama valve dawała livelock pod ciągłym zapisem (start→preempt→revert w kółko); nieprzerywalny pierwszy element gwarantuje postęp (FIX-05) |
| ADR-48 | Izolacja per-typ wektorów przez natywne partycjonowanie LIST, nie runtime DDL | App-managed sharding wymagał CREATE TABLE w runtime (łamie Invariant #4, DDL-grant na produkcji, race). Partycje LIST per plugin_type + partial HNSW per partycja dają ten sam mały-indeks-per-segment z migracji, z partition pruning zamiast fan-out (FIX-07) |
```

---

## Zmiany w CLAUDE.md (per crate)

### smartfs-db/CLAUDE.md

W sekcji „Dedup" USUŃ opis z refcountem, WSTAW:

```markdown
## Dedup — blobs table, GC-by-scan (no refcount)
INSERT INTO blobs ... ON CONFLICT (content_hash) DO UPDATE SET content_hash=blobs.content_hash
RETURNING blob_id, (xmax=0) AS inserted.
- inserted=TRUE  → store.put; on put error DELETE the blobs row (compensation).
- inserted=FALSE → if !store.exists(blob_id): store.put (heal xmax false-negative / poison).
No refcount column. GC = periodic scan: DELETE FROM blobs WHERE NOT EXISTS
(file_versions ref) AND created_at < now()-1h. Never rely on a counter.
```

W sekcji „CoW transaction shape" USUŃ UPDATE `is_current`, dopisz:

```markdown
## is_current is NOT set here
ast_embeddings_1536.is_current is owned by smartfs-ai worker, not cow_commit.
cow_commit only inserts ast_nodes + file_versions. Worker recomputes is_current
against the inode's latest version after embedding (order-independent).
```

### smartfs-ai/CLAUDE.md

W sekcji „ast_embeddings_1536 — current version only" USUŃ opis ustawiania w commit, WSTAW:

```markdown
## is_current — worker owns it, recompute after every embed
After embedding a version's AST nodes: look up the inode's latest version_number,
then UPDATE ast_embeddings SET is_current = (version == latest) for ALL that inode's
embedded vectors. Order-independent — SKIP LOCKED may finish versions out of order,
so never assume "previous" version; always recompute vs the true latest.

## Anti-starvation — first item is non-preemptible in force mode
When should_embed_despite_activity() is true, process the first batch item WITHOUT
the write_activity_detected() select arm. Otherwise continuous writes (cargo build)
livelock the worker: start → preempt → revert forever. Valve provides the start;
non-preemptible first item provides the progress.
```

---

## Checklist weryfikacyjny (agent uruchamia po zaaplikowaniu wszystkich napraw)

```
[ ] grep -in "refcount" dokument → 0 wystąpień w schemacie/flow (dozwolone tylko w ADR-44 jako opis usunięcia)
[ ] grep -in "advisory" → tylko ADR-04 (superseded) i §7.3/ADR-40 (uzasadnienie porzucenia)
[ ] blobs CREATE TABLE nie ma kolumny refcount
[ ] §7.5 istnieje i opisuje GC-by-scan z grace window
[ ] cow_commit (§11.1, §12.1) NIE zawiera UPDATE ast_embeddings SET is_current
[ ] worker (§12.2) zawiera refresh_is_current wywoływane po mark_clean
[ ] cow_commit gałąź inserted=TRUE ma kompensację DELETE blobs przy błędzie put
[ ] cow_commit gałąź inserted=FALSE ma if !store.exists → store.put
[ ] worker ma force_mode / allow_preempt = !force || i>0
[ ] roadmap §3b: partial unique index zamiast UNIQUE(...,is_active); brak runtime CREATE TABLE
[ ] §6 ast_embeddings_1536 spójne z partycjonowaniem z roadmap §3b (kolumna plugin_type, PARTITION BY LIST)
[ ] nagłówek mówi v5.0, changelog "względem v4.5"
[ ] ADR-04 oznaczony SUPERSEDED; ADR-44..48 dodane
[ ] §17 Etap 1 deliverable mówi "dedup przez blobs", nie "advisory lock"
[ ] CLAUDE.md (smartfs-db, smartfs-ai) zsynchronizowane z FIX-01/02/03/04/05
```

Po przejściu całego checklisty dokument jest v5.0 — bez znanych błędów poprawności, spójny międzysekcyjnie, gotowy do implementacji.
