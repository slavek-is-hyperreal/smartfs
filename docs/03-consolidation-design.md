# Głęboki nurek — pamięć robocza i konsolidacja semantyczna

← [Mapa crate'ów](02-crates.md) | Implementacja: [`smartfs-semantic`](crates/smartfs-semantic.md) | ADR: [ADR-50](adr/ADR-50-working-memory-consolidation.md), [ADR-53](adr/ADR-53-consolidation-concurrency.md)

**Rewizja po review:** ta wersja zamyka błąd (`plugin_type` odczytywany z kolumny, która nie istniała) i cztery luki (topologia supervisorów, brak operacji merge, skalowanie configu, niedookreślone typy/algorytmy) zgłoszone przy pierwszym przeczytaniu specyfikacji. Zmiany są odnotowane przy każdej sekcji, której dotyczą.

## 1. Model mentalny (bez zmian)

Dwa reżimy, dwie różne gwarancje:

| | Pamięć robocza (bufor) | Pamięć skrystalizowana (graf centroidów) |
|---|---|---|
| Rozmiar | Ograniczony (`backlog_threshold`, domyślnie 500) | Nieograniczony, rośnie wolno |
| Spójność | Zawsze aktualna, ale nieustrukturyzowana | Ustrukturyzowana, ale opóźniona o do jednego cyklu konsolidacji |
| Koszt przeszukania | Brute-force (tanie, bo małe) | Routing przez centroid (IVF-style) |
| Kto zapisuje | `smartfs-ai` po każdym embeddingu (`consolidated=FALSE`) | Wyłącznie `consolidation_supervisor` |

Zapytanie zawsze odpytuje obie warstwy i scala wynik (wzorzec memtable+SSTable z baz LSM-tree).

## 2. Topologia supervisorów — ROZSTRZYGNIĘTE (wcześniej niejasne)

**Decyzja:** jeden `consolidation_supervisor` **per `(plugin_type, model_id)`**, nie jeden globalny proces obsługujący wszystko. Powód: progi (`join_threshold` itd.) są per kombinacja (patrz §5), więc supervisor obsługujący wiele kombinacji naraz musiałby przełączać kontekst configu przy każdym batchu — prościej i bezpieczniej mieć jedną pętlę na kombinację.

```rust
/// @id: 0f4a8e21-6d53-4b19-9c72-1a5e8d3f6b04
/// Odkrywa wszystkie kombinacje (plugin_type, model_id), dla których istnieje
/// skalibrowany wiersz w consolidation_thresholds (brak wiersza = kombinacja
/// świadomo pomijana — fail-safe, patrz migracja 005 §4), i utrzymuje po
/// jednym tokio::task per kombinacja. Wywoływane raz przy starcie demona,
/// oraz okresowo (co REDISCOVER_INTERVAL, domyślnie 10 min) żeby podłapać
/// nowe kombinacje bez restartu.
pub async fn spawn_all_consolidation_supervisors(db: Pool) -> Result<()> {
    let mut running: HashMap<(String, Uuid), JoinHandle<()>> = HashMap::new();
    loop {
        let combos = fetch_calibrated_combinations(&db).await?;
        for combo in &combos {
            running.entry(combo.clone()).or_insert_with(|| {
                let db = db.clone();
                let combo = combo.clone();
                tokio::spawn(async move { consolidation_supervisor(db, combo).await })
            });
        }
        tokio::time::sleep(REDISCOVER_INTERVAL).await;
    }
}
```

**Współbieżność między wieloma instancjami demona (jeśli kiedyś uruchamiane tak, dziś poza zakresem MVP, ale zabezpieczone już teraz):** każdy `consolidation_supervisor` bierze `pg_try_advisory_lock(hashtext(plugin_type || model_id))` na czas jednego batcha. To jest jedyne miejsce w v6.0, gdzie sięgamy po advisory lock — i celowo, bo tu (w przeciwieństwie do dedupu blobów, ADR-40) nie ma okna I/O między zwolnieniem locka a zapisem: cała operacja mieści się w jednej krótkiej transakcji SQL, więc żaden z problemów, które zdyskwalifikowały advisory lock w ADR-40, tu nie występuje. To zamyka lukę "dwie współbieżne instancje tworzą prawie identyczne centroidy dla tego samego wektora" u źródła, zamiast tylko sprzątać po fakcie.

```rust
/// @id: 6b2d4e18-3f77-4a90-9c11-8a5f0d2e7c44
pub async fn consolidation_supervisor(db: Pool, combo: (String, Uuid)) {
    let (plugin_type, model_id) = combo;
    let mut last_run = Instant::now();
    loop {
        let cfg = match load_consolidation_config(&db, &plugin_type, model_id).await {
            Ok(c) => c,
            Err(e) => { tracing::error!("no consolidation config for {plugin_type}/{model_id}: {e}"); return; } // brak configu = kombinacja jeszcze nieskalibrowana; zakończ, spawn_all_consolidation_supervisors spróbuje ponownie po następnej kalibracji
        };
        let backlog = count_unconsolidated(&db, &plugin_type, model_id).await.unwrap_or(0);

        let should_run = backlog >= cfg.backlog_threshold
            || activity_monitor.idle_for(cfg.idle_before_sleep).await
            || last_run.elapsed() >= cfg.max_wait;

        if should_run {
            let lock_key = advisory_lock_key(&plugin_type, model_id);
            if try_advisory_lock(&db, lock_key).await.unwrap_or(false) {
                match consolidate_batch(&db, &plugin_type, model_id, &cfg).await {
                    Ok(n) => { tracing::info!("consolidated {n} vectors for {plugin_type}/{model_id}"); last_run = Instant::now(); }
                    Err(e) => tracing::error!("consolidation batch failed: {e}"),
                }
                release_advisory_lock(&db, lock_key).await.ok();
            }
            // lock niedostępny = inna instancja już pracuje nad tą kombinacją; nic nie rób, spróbuj następnym razem
        } else {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }
}
```

Okresowy backstop mergowania (§4b) jest osobnym, dużo rzadszym zegarem w tej samej pętli nadzorcy — nie osobnym tokio::task — żeby dzielić ten sam advisory lock i nigdy nie biec równolegle z `consolidate_batch` dla tej samej kombinacji.

## 3. Konfiguracja — ROZSTRZYGNIĘTE (wcześniej: jedna globalna struktura, niespójne z per-model kalibracją)

`ConsolidationConfig` jest teraz jawnie ładowana **per `(plugin_type, model_id)`** z tabeli `consolidation_thresholds` (migracja 005 §4), nie jest globalną stałą aplikacji:

```rust
/// @id: 1a7f3c02-9e44-4b1a-8f0a-2d6e5c9a7b11
pub struct ConsolidationConfig {
    pub backlog_threshold: i64,
    pub idle_before_sleep: Duration,
    pub max_wait: Duration,
    pub join_threshold: f64,
    pub split_variance_threshold: f64,
    pub max_members_per_centroid: i64,
    pub batch_size: i64,   // było nieużywaną stałą globalną BATCH_SIZE — teraz pole configu, per kombinacja
}

/// @id: 9c3e7a15-4b82-4d06-a9f1-3e8c5d2b7a44
async fn load_consolidation_config(db: &Pool, plugin_type: &str, model_id: Uuid) -> Result<ConsolidationConfig> {
    // SELECT * FROM consolidation_thresholds WHERE plugin_type=$1 AND model_id=$2
    // Brak wiersza -> Err(SmartFsError::MissingCalibration { plugin_type, model_id })
    ...
}
```

## 4. Roszczenie batcha (poprawiony błąd `plugin_type`)

**Błąd w poprzedniej wersji:** zapytanie selekcjonowało `plugin_type` bezpośrednio z `ast_embeddings_1536`, która tej kolumny nie miała. **Poprawka:** migracja 005 §2 denormalizuje `plugin_type` na wszystkie cztery tabele embeddingów (wypełniane przez `smartfs-ai` przy INSERT, patrz [`smartfs-ai.md`](crates/smartfs-ai.md)), więc zapytanie jest teraz poprawne bez JOIN-a:

```rust
/// @id: c4e19a7d-5b2f-4e88-a1c3-9f6d0b8e2a55
async fn claim_unconsolidated_batch(
    tx: &mut Transaction<'_, Postgres>,
    plugin_type: &str,
    model_id: Uuid,
    limit: i64,
) -> Result<Vec<BufferedVector>> {
    sqlx::query_as!(
        BufferedVector,
        r#"
        SELECT ast_node_id, embedding, plugin_type, model_id
        FROM ast_embeddings_1536
        WHERE consolidated = FALSE AND is_current = TRUE
          AND plugin_type = $1 AND model_id = $2
        ORDER BY created_at ASC
        FOR UPDATE SKIP LOCKED
        LIMIT $3
        "#,
        plugin_type, model_id, limit
    ).fetch_all(&mut **tx).await.map_err(SmartFsError::from)
}
```

Analogiczne zapytania dla `embeddings_384`/`embeddings_768`/`embeddings_1024_qwen` różnią się tylko nazwą tabeli i tym, że zwracają `version_id` zamiast `ast_node_id` — `BufferedVector` ma oba pola jako `Option<Uuid>`, dokładnie jedno z nich jest `Some` w zależności od tabeli źródłowej.

Roszczenie nadal nie potrzebuje maszyny stanów `pending/processing` z `smartfs-ai` — uzasadnienie bez zmian: krok jest czystym SQL, `FOR UPDATE SKIP LOCKED` w jednej krótkiej transakcji wystarcza, crash wycofuje transakcję sam.

## 5. Algorytm dołączania i rozszczepiania (bez zmian algorytmicznych, typy doprecyzowane)

```rust
/// @id: 8d3a6f01-2c59-4d77-b6e4-1f8a3c5d9e02
pub async fn consolidate_batch(
    db: &Pool,
    plugin_type: &str,
    model_id: Uuid,
    cfg: &ConsolidationConfig,
) -> Result<usize> {
    let mut tx = db.begin().await?;
    let batch = claim_unconsolidated_batch(&mut tx, plugin_type, model_id, cfg.batch_size).await?;
    let mut n = 0;

    for item in &batch {
        match nearest_centroid(&mut tx, plugin_type, model_id, item).await? {
            Some((centroid, dist)) if dist <= cfg.join_threshold => {
                attach_to_centroid(&mut tx, &centroid, item, dist, cfg).await?;
            }
            _ => {
                create_centroid_from(&mut tx, plugin_type, model_id, item).await?;
            }
        }
        mark_consolidated(&mut tx, item).await?;
        n += 1;
    }

    tx.commit().await?;
    Ok(n)
}
```

Welford i rozszczepianie — algorytm identyczny jak w poprzedniej wersji, teraz z jawnie zdefiniowanym typem `Cluster` (wcześniej brakującym):

```rust
/// @id: f0b7d2e4-8a3c-4f19-9d5e-6c1a0b7f3d88
fn welford_update(mean: &mut Vec<f32>, m2: &mut f64, count: &mut i64, new_point: &[f32]) {
    *count += 1;
    let n = *count as f32;
    let mut sq_delta_sum = 0.0f64;
    for (m, x) in mean.iter_mut().zip(new_point) {
        let delta = x - *m;
        *m += delta / n;
        let delta2 = x - *m;
        sq_delta_sum += (delta * delta2) as f64;
    }
    *m2 += sq_delta_sum;
}

/// @id: 2e9c5a17-4d80-4b3e-8f21-7a6d0c9b5e33
fn variance_from_m2(m2: f64, count: i64) -> f64 {
    if count < 2 { 0.0 } else { m2 / (count as f64 - 1.0) }
}

/// @id: d1f6a3c8-7e02-4b95-a3d1-6f8c2e0b9a55
/// Wynik lokalnego k-means(k=2). Zawiera WSZYSTKO potrzebne do założenia
/// nowego centroidu bez ponownego przeliczania od zera (mean/m2/count
/// liczone raz, w kmeans2, nie osobno po fakcie).
pub struct Cluster {
    pub mean: Vec<f32>,
    pub m2: f64,
    pub count: i64,
    pub member_ids: Vec<Uuid>,          // ast_node_id LUB version_id, zależnie od tabeli
    pub member_distances: Vec<f64>,     // dystans każdego członka do NOWEGO mean tego klastra
}

/// @id: 5c8e2a94-1f6d-4b37-a8c0-3e7f9d2b6a15
fn kmeans2(members: &[CentroidMemberWithVector]) -> Result<(Cluster, Cluster)> {
    // Lloyd's algorithm, k=2, inicjalizacja: dwa najbardziej odległe punkty
    // (2-approximation, tanie dla małego, ograniczonego zbioru --
    // max_members_per_centroid z założenia ogranicza |members|).
    // Iteruje do zbieżności przypisań (typowo <10 iteracji dla k=2).
    ...
}
```

```rust
/// @id: a3f8d1c6-7e29-4a55-b0d4-9c2e5f8a1b77
async fn attach_to_centroid(
    tx: &mut Transaction<'_, Postgres>,
    centroid: &ConceptCentroid,
    item: &BufferedVector,
    dist: f64,
    cfg: &ConsolidationConfig,
) -> Result<()> {
    let mut mean = centroid.centroid.clone();
    let mut m2 = centroid.m2;
    let mut count = centroid.member_count;

    welford_update(&mut mean, &mut m2, &mut count, &item.embedding);
    let variance = variance_from_m2(m2, count);

    update_centroid(tx, centroid.id, &mean, m2, count).await?;
    insert_centroid_member(tx, centroid.id, item, dist).await?;

    if variance > cfg.split_variance_threshold || count > cfg.max_members_per_centroid {
        split_centroid(tx, centroid.id, cfg).await?;
    }
    Ok(())
}

/// @id: 5c8e2a94-1f6d-4b37-a8c0-3e7f9d2b6a16
async fn split_centroid(tx: &mut Transaction<'_, Postgres>, centroid_id: Uuid, cfg: &ConsolidationConfig) -> Result<()> {
    let members = fetch_centroid_members_with_vectors(tx, centroid_id).await?;
    let (cluster_a, cluster_b) = kmeans2(&members)?;
    let new_a = create_centroid_from_cluster(tx, &cluster_a).await?;
    let new_b = create_centroid_from_cluster(tx, &cluster_b).await?;
    reassign_members(tx, centroid_id, &cluster_a, new_a, &cluster_b, new_b).await?;
    // Stary centroid NIE jest usuwany (DELETE) — jest oznaczany is_active=FALSE,
    // spójnie z tym, jak merge_centroids (§4b) też nie usuwa, tylko dezaktywuje.
    // Historia "co się z czego rozpadło" zostaje w bazie do audytu.
    deactivate_centroid(tx, centroid_id).await?;
    Ok(())
}
```

## 4b. Merge centroidów — NOWA sekcja (zamyka lukę "brak operacji odwrotnej do split")

Advisory lock z §2 zamyka najczęstszą przyczynę powstawania prawie identycznych centroidów (dwie współbieżne konsolidacje tej samej kombinacji). Nie zamyka drugiej: powolny dryf, w którym dwa centroidy powstałe w różnym czasie stają się z czasem bliskie sobie (np. baza kodu ujednolica styl, dwie wcześniej odrębne konwencje nazewnictwa zlewają się semantycznie). Dlatego `consolidation_supervisor` uruchamia dodatkowo, dużo rzadziej (co `MERGE_CHECK_INTERVAL`, domyślnie raz na 20 cykli konsolidacji tej samej kombinacji), przegląd par centroidów pod kątem scalenia:

```rust
/// @id: 7a2c9e4f-3b86-4d17-9f0a-5e8c2d6b3a71
/// Skanuje pary centroidów AKTYWNYCH (is_active=TRUE) tej samej kombinacji
/// (plugin_type, model_id) w poszukiwaniu par bliższych niż merge_threshold
/// (domyślnie: połowa join_threshold — scalamy tylko wyraźnie zdublowane,
/// nie tylko "podobne"). Dla znalezionej pary: nowy centroid = ważona średnia
/// (ważona member_count), m2 przybliżone przez połączenie wariancji obu
/// (Chan et al. parallel variance combination — dokładny wzór, nie ponowne
/// przejście po wszystkich historycznych punktach), obie strony oznaczone
/// is_active=FALSE z merged_into wskazującym na nowy.
pub async fn merge_centroids(tx: &mut Transaction<'_, Postgres>, plugin_type: &str, model_id: Uuid, merge_threshold: f64) -> Result<usize> {
    let candidates = find_mergeable_pairs(tx, plugin_type, model_id, merge_threshold).await?;
    let mut n = 0;
    for (a, b) in candidates {
        let merged = combine_centroids(&a, &b); // Chan et al. parallel variance formula
        let new_id = create_centroid_from_cluster(tx, &merged).await?;
        reparent_members(tx, a.id, b.id, new_id).await?;
        deactivate_centroid(tx, a.id).await?;
        deactivate_centroid(tx, b.id).await?;
        n += 1;
    }
    Ok(n)
}
```

`merge_centroids` biegnie pod tym samym advisory lockiem co `consolidate_batch` dla tej kombinacji (§2) — nigdy nie działa równolegle z bieżącą konsolidacją tej samej pary `(plugin_type, model_id)`.

## 6. Dobór `join_threshold` — bez zmian merytorycznych, doprecyzowane miejsce zapisu

`smartfs-cli init` (albo nowa podkomenda `smartfs-cli calibrate --plugin-type X --model Y`) wyznacza `join_threshold` empirycznie (percentyl rozkładu dystansów k-NN na próbce istniejących wektorów) i zapisuje wynik jako `INSERT ... ON CONFLICT (plugin_type, model_id) DO UPDATE` do `consolidation_thresholds` — nie do `smartfs.toml`. Dopóki wiersz nie istnieje, `spawn_all_consolidation_supervisors` świadomie nie odpala supervisora dla tej kombinacji (fail-safe, patrz migracja 005 §4).

## 7. Zapytanie — scalanie warstwy roboczej i skrystalizowanej (bez zmian)

```rust
/// @id: 7d1f4b29-6a83-4c05-9e17-2b8d5f0a3c66
pub async fn search_by_concept(
    db: &Pool,
    query_vector: &[f32],
    plugin_type: &str,
    model_id: Uuid,
    limit: usize,
) -> Result<Vec<ConceptSearchHit>> {
    let crystallized = search_via_centroids(db, query_vector, plugin_type, model_id, limit).await?;
    let buffered = brute_force_unconsolidated(db, query_vector, plugin_type, model_id, limit).await?;
    Ok(merge_by_similarity(crystallized, buffered, limit))
}
```

`search_via_centroids` filtruje `WHERE is_active = TRUE` — scaleni/rozszczepieni przodkowie centroidów nigdy nie są zwracani jako trafienia, tylko zachowani do audytu (`merged_into`, historia w `centroid_members_*` przez `reparent_members`).

## 8. Graf leksykalny — kontrakt wejścia (zamyka lukę "opisane jednym zdaniem")

**`import_wordnet`** nie parsuje żadnego konkretnego formatu producenta (np. natywnego eksportu XML danego wordnetu) bezpośrednio — przyjmuje ustandaryzowany format pośredni, żeby `smartfs-semantic` nie musiał znać szczegółów formatu każdego źródła leksykalnego:

```jsonc
// oczekiwany format pliku wejściowego dla import_wordnet (jeden JSON, UTF-8)
{
  "language": "pl",
  "nodes": [ { "lemma": "funkcja", "pos": "noun", "external_ref": "plwn-synset-1234" } ],
  "edges": [ { "from": "funkcja", "to": "metoda", "relation": "hypernym" } ]
}
```

Konwersja z natywnego formatu konkretnego wordnetu (np. eksportu Słowosieci) do powyższego jest osobnym, jednorazowym skryptem narzędziowym, utrzymywanym poza `smartfs-semantic` (np. `tools/plwordnet_to_smartfs.py`) — rozdzielenie jest świadome: `import_wordnet` ma jeden, stabilny kontrakt wejścia niezależnie od tego, ile różnych źródeł leksykalnych ktoś kiedyś zechce podłączyć.

```rust
/// @id: e8f2c6a1-4d97-4b03-a5e6-2f9d7c1b8a63
pub async fn import_wordnet(db: &Pool, path: &Path, language: &str) -> Result<usize> {
    let doc: WordnetImport = serde_json::from_reader(File::open(path)?)?;
    // UPSERT do lexical_nodes po (lemma, pos, language), potem lexical_edges
    // po (from_id, to_id, relation) — idempotentne, bezpieczne do ponownego uruchomienia
    ...
}
```

**`label_centroid_from_members`** — algorytm doprecyzowany (wcześniej: "TF-IDF po nazwach", bez definicji):

1. Dla każdego członka centroidu pobierz `ast_nodes.name` (dla 1536) lub nazwę pliku (dla pozostałych).
2. Tokenizuj każdą nazwę: rozbij `camelCase` i `snake_case` na słowa składowe, zlowercase'uj (`parseConfigFile` → `["parse", "config", "file"]`).
3. Częstość termu (TF) liczona **wewnątrz** zbioru członków centroidu; częstość dokumentowa (DF) liczona po wszystkich `ast_nodes.name` tego samego `plugin_type` w całym korpusie (nie tylko tego centroidu) — standardowe TF-IDF, gdzie "dokument" to jeden centroid, a "korpus" to wszystkie centroidy danego `plugin_type`.
4. Etykietą centroidu zostaje termin o najwyższym TF-IDF; przy remisie — termin częściej występujący jako pierwszy token nazwy (heurystyka: pierwszy token częściej niesie kategorię ogólną, np. `get`/`parse`/`validate`).
5. Wynik jest cache'owany w `concept_centroids_*.label` i przeliczany tylko wtedy, gdy `member_count` zmieni się o więcej niż 20% od ostatniego przeliczenia (unika przeliczania etykiety przy każdym pojedynczym dołączeniu).

## 9. Typ błędu — ROZSTRZYGNIĘTE (wcześniej: gołe `Result<T>` bez związku ze SmartFsError)

Wszystkie funkcje w `smartfs-semantic` zwracają `smartfs_schema::Result<T>` (alias na `Result<T, SmartFsError>`), zgodnie z Root Invariant "SmartFsError enum — wszystkie crate'y go używają". `SmartFsError` zyskuje w v6.0 dwa nowe warianty:

```rust
// w smartfs-schema, nie w smartfs-semantic — zgodnie z "smartfs-schema: shared types, no logic"
pub enum SmartFsError {
    // ...warianty istniejące z v4.5...
    MissingCalibration { plugin_type: String, model_id: Uuid },
    AdvisoryLockUnavailable,   // nie jest traktowany jak błąd na górze pętli — patrz §2, po prostu "spróbuj później"
}
```

## 10. Odporność na awarię (bez zmian, uzupełnione o merge)

- Roszczenie batcha i merge biegną w jednej transakcji każde — crash wycofuje, nic nie ginie ani nie dubluje się.
- `split_centroid` i `merge_centroids` nigdy nie usuwają wierszy `DELETE` — tylko `is_active=FALSE` + `merged_into`/historia w `centroid_members_*` — więc nawet błędne scalenie jest odwracalne ręcznie (audyt), nie jest nieodwracalną utratą struktury.
- Advisory lock (§2) gwarantuje, że `consolidate_batch` i `merge_centroids` dla tej samej kombinacji nigdy nie biegną równolegle, nawet przy wielu instancjach demona.

Dalej: [docs/04-uuid-doc-linking.md](04-uuid-doc-linking.md)
