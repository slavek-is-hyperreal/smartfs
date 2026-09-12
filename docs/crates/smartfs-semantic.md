# smartfs-semantic — Centroidy, Bufor Roboczy, Konsolidacja, Graf Leksykalny

← [Mapa crate'ów](../02-crates.md) | Mechanizm: [docs/03-consolidation-design.md](../03-consolidation-design.md)

**Poziom 4 dokumentacji — najwyższa granularność w tym dokumencie.** Każda pozycja niżej ma `@id` w kodzie; po zaimplementowaniu i przepuszczeniu przez `smartfs-docgen scan`, linki `symbol://` w tym pliku będą rozwiązywalne.

**Rewizja po review:** dodane `merge_centroids`, `Cluster`, funkcje topologii supervisorów (`spawn_all_consolidation_supervisors`, advisory lock), `consolidation_thresholds` jako źródło configu. Poprawiony błąd `plugin_type` w `claim_unconsolidated_batch` (patrz [docs/03](../03-consolidation-design.md) §4).

## CLAUDE.md dla tego crate'a

```markdown
# smartfs-semantic — Konsolidacja Semantyczna

## Owns exclusively
Graf centroidów (concept_centroids_*, centroid_members_*), graf leksykalny
(lexical_nodes, lexical_edges, word_centroid_links_*), tabelę
consolidation_thresholds, worker konsolidacji i merge'a.
Czyta embeddingi zapisane przez smartfs-ai (w tym denormalizowaną kolumnę
plugin_type — patrz smartfs-ai.md), nigdy ich nie generuje sam.

## Typ błędu
Result<T> = smartfs_schema::Result<T> (SmartFsError). Nie definiuje własnego
typu błędu — patrz Root Invariant "jeden SmartFsError dla wszystkich crate'ów".

## Nigdy
- Nie importuje smartfs-ai (patrz docs/02-crates.md, uzasadnienie rozdzielenia)
- Nie zapisuje do embeddings_*/ast_embeddings_* poza kolumną `consolidated`
- Nie przelicza centroidu globalnie — tylko lokalnie, w obrębie jednego batcha
- Nie trzyma transakcji SQL otwartej przez czas dłuższy niż jeden batch
- Nie generuje embeddingów — do wektora zapytania woła publiczne API smartfs-ai
- Nie USUWA wierszy concept_centroids_* (DELETE) — tylko is_active=FALSE +
  merged_into, nawet przy split i merge (ADR-53)
- Nie uruchamia consolidate_batch ani merge_centroids bez trzymanego
  pg_try_advisory_lock dla danej (plugin_type, model_id)

## Commands
cargo test -p smartfs-semantic   (wymaga Docker PostgreSQL, jak smartfs-db)
cargo clippy -p smartfs-semantic
```

## Struktury danych

| Symbol | Kind | Plik (docelowy) | Opis |
|---|---|---|---|
| `ConceptCentroid` | Struct | `src/types.rs` | id, plugin_type, model_id, centroid, m2, member_count, label, is_active, merged_into |
| `CentroidMember` | Struct | `src/types.rs` | centroid_id, ast_node_id/version_id (dokładnie jedno Some), distance |
| `Cluster` | Struct | `src/kmeans.rs` | Wynik `kmeans2`: mean, m2, count, member_ids, member_distances — **nowe, wcześniej niezdefiniowane** |
| `LexicalNode` | Struct | `src/lexical.rs` | id, lemma, pos, language, external_ref |
| `LexicalRelation` | Enum | `src/lexical.rs` | `Synonym \| Hypernym \| Hyponym \| Meronym` |
| `WordCentroidLink` | Struct | `src/lexical.rs` | lexical_node_id, centroid_id, weight |
| `WordnetImport` | Struct | `src/lexical.rs` | Deserializowany kontrakt JSON dla `import_wordnet` — **nowe** | 
| `ConsolidationConfig` | Struct | `src/config.rs` | Ładowana per `(plugin_type, model_id)` z `consolidation_thresholds`, nie globalna — **poprawione skalowanie** |
| `BufferedVector` | Struct | `src/types.rs` | Wiersz z bufora; `ast_node_id: Option<Uuid>`, `version_id: Option<Uuid>`, `plugin_type: String` (teraz zawsze obecne, patrz smartfs-ai.md) |
| `ConceptSearchHit` | Struct | `src/search.rs` | id, dystans, źródło (`Crystallized \| Buffered`) |

## Funkcje — topologia supervisorów (nowa sekcja)

| Symbol | Sygnatura (skrót) | Opis | Szczegóły |
|---|---|---|---|
| `spawn_all_consolidation_supervisors` | `async fn(Pool) -> Result<()>` | Odkrywa skalibrowane kombinacje, utrzymuje po jednym tasku na każdą | docs/03 §2 |
| `fetch_calibrated_combinations` | `async fn(&Pool) -> Result<Vec<(String, Uuid)>>` | `SELECT DISTINCT plugin_type, model_id FROM consolidation_thresholds` | — |
| `load_consolidation_config` | `async fn(&Pool, &str, Uuid) -> Result<ConsolidationConfig>` | Błąd `MissingCalibration`, jeśli brak wiersza | docs/03 §3 |
| `advisory_lock_key` | `fn(&str, Uuid) -> i64` | `hashtext(plugin_type \|\| model_id)`, deterministyczne | docs/03 §2 |
| `try_advisory_lock` / `release_advisory_lock` | `async fn(&Pool, i64) -> Result<bool>` / `async fn(&Pool, i64) -> Result<()>` | Cienki wrapper na `pg_try_advisory_lock`/`pg_advisory_unlock` | docs/03 §2 |

## Funkcje — worker i pętla nadzorcy

| Symbol | Sygnatura (skrót) | Opis | Szczegóły |
|---|---|---|---|
| `consolidation_supervisor` | `async fn(Pool, (String, Uuid))` | Pętla nadzorcy jednej kombinacji — dwa zegary + advisory lock | docs/03 §2 |
| `count_unconsolidated` | `async fn(&Pool, &str, Uuid) -> Result<i64>` | Teraz sparametryzowane per kombinacja (wcześniej: globalna suma) | — |
| `claim_unconsolidated_batch` | `async fn(&mut Transaction, &str, Uuid, i64) -> Result<Vec<BufferedVector>>` | `FOR UPDATE SKIP LOCKED`; **poprawiony błąd `plugin_type`** | docs/03 §4 |
| `consolidate_batch` | `async fn(&Pool, &str, Uuid, &ConsolidationConfig) -> Result<usize>` | Roszczenie → dołączenie/utworzenie → commit | docs/03 §5 |
| `nearest_centroid` | `async fn(&mut Transaction, &str, Uuid, &BufferedVector) -> Result<Option<(ConceptCentroid, f64)>>` | ANN po `concept_centroids_*`, filtr `is_active=TRUE` | — |
| `attach_to_centroid` | `async fn(&mut Transaction, &ConceptCentroid, &BufferedVector, f64, &ConsolidationConfig) -> Result<()>` | Welford update + decyzja o rozszczepieniu | docs/03 §5 |
| `create_centroid_from` | `async fn(&mut Transaction, &str, Uuid, &BufferedVector) -> Result<ConceptCentroid>` | Nowy centroid | — |
| `welford_update` | `fn(&mut Vec<f32>, &mut f64, &mut i64, &[f32])` | Przyrostowa średnia/wariancja | docs/03 §5 |
| `variance_from_m2` | `fn(f64, i64) -> f64` | — | docs/03 §5 |
| `split_centroid` | `async fn(&mut Transaction, Uuid, &ConsolidationConfig) -> Result<()>` | Lokalny k-means (k=2); dezaktywuje stary zamiast `DELETE` | docs/03 §5 |
| `kmeans2` | `fn(&[CentroidMemberWithVector]) -> Result<(Cluster, Cluster)>` | Czysta funkcja, testowalna bez bazy | docs/03 §5 |
| `merge_centroids` | `async fn(&mut Transaction, &str, Uuid, f64) -> Result<usize>` | **Nowe** — backstop przeciw dryfującym duplikatom | docs/03 §4b |
| `combine_centroids` | `fn(&ConceptCentroid, &ConceptCentroid) -> Cluster` | **Nowe** — wzór Chan et al. na łączenie wariancji dwóch zbiorów | docs/03 §4b |
| `calibrate_join_threshold` | `async fn(&Pool, &str, Uuid, f64) -> Result<f64>` | Wywoływane przez `smartfs-cli calibrate`; zapisuje do `consolidation_thresholds` | docs/03 §6 |

## Funkcje — wyszukiwanie

| Symbol | Sygnatura (skrót) | Opis |
|---|---|---|
| `search_by_concept` | `async fn(&Pool, &[f32], &str, Uuid, usize) -> Result<Vec<ConceptSearchHit>>` | Scala warstwę skrystalizowaną i roboczą |
| `search_via_centroids` | `async fn(&Pool, &[f32], &str, Uuid, usize) -> Result<Vec<ConceptSearchHit>>` | Routing IVF-style, filtr `is_active=TRUE` |
| `brute_force_unconsolidated` | `async fn(&Pool, &[f32], &str, Uuid, usize) -> Result<Vec<ConceptSearchHit>>` | Skan bufora |
| `merge_by_similarity` | `fn(Vec<ConceptSearchHit>, Vec<ConceptSearchHit>, usize) -> Vec<ConceptSearchHit>` | Scalanie i deduplikacja |

## Funkcje — graf leksykalny

| Symbol | Sygnatura (skrót) | Opis |
|---|---|---|
| `import_wordnet` | `async fn(&Pool, &Path, language: &str) -> Result<usize>` | Kontrakt wejścia (JSON pośredni) doprecyzowany w docs/03 §8 |
| `link_word_to_centroid` | `async fn(&Pool, Uuid, Uuid, f64) -> Result<()>` | Ręczne/półautomatyczne powiązanie |
| `label_centroid_from_members` | `async fn(&Pool, Uuid) -> Result<Option<String>>` | Algorytm TF-IDF z tokenizacją camelCase/snake_case — pełny opis w docs/03 §8 |

## Uwaga o testowalności

`welford_update`, `variance_from_m2`, `kmeans2` i `combine_centroids` są celowo czystymi funkcjami bez zależności od `sqlx`/`Pool` — testowalne jednostkowo bez Dockera z PostgreSQL. Sugerowane przypadki testowe: pusty bufor, pojedynczy punkt (wariancja=0, brak rozszczepienia), dystans dokładnie równy `join_threshold` (rozstrzygnięcie: `<=` dołącza, patrz docs/03 §5 — graniczny przypadek trzeba testować jawnie, nie zakładać), `kmeans2` na dwóch identycznych punktach (zdegenerowany przypadek — oba klastry powinny wyjść identyczne, nie crashować).

## Symbole niezadokumentowane wcześniej (B-14, S-07, S-16)

| Symbol | Sygnatura (skrót) | Opis |
|---|---|---|
| `list_active_centroids` | `async fn(pool, plugin_type, model_id, limit) -> Result<Vec<CentroidSummary>>` | Zwraca aktywne (nie-tombstoned, `is_active=TRUE`) centroidy dla danej kombinacji `(plugin_type, model_id)` |
| `CentroidSummary` | Struct | `id: Uuid`, `label: Option<String>`, `member_count: i64`, `centroid: Vec<f32>` |
| `create_centroid_from` | `async fn(tx, plugin_type, model_id, item) -> Result<Uuid>` | Tworzy nowy centroid z pojedynczego buforowanego wektora |
| `create_centroid_from_cluster` | `async fn(tx, cluster) -> Result<Uuid>` | Tworzy centroid z wyliczonej struktury `Cluster` |
| `deactivate_centroid` | `async fn(tx, centroid_id) -> Result<()>` | Ustawia `is_active=FALSE`; nigdy nie wykonuje `DELETE` |
| `reparent_members` | `async fn(tx, old_a, old_b, new_id) -> Result<()>` | Przepisuje wiersze członków po scaleniu — `old_a` i `old_b` → `new_id` |
| `fetch_centroid_members_with_vectors` | `async fn(tx, centroid_id) -> Result<Vec<CentroidMemberWithVector>>` | Pobiera członków centroidu wraz z ich wektorami embeddingów |
| `find_mergeable_pairs` | `async fn(tx, plugin_type, model_id, threshold) -> Result<Vec<(ConceptCentroid, ConceptCentroid)>>` | Wyszukuje pary aktywnych centroidów bliższych niż `threshold` |
| `label_centroid_from_members` | `async fn(tx, centroid_id) -> Result<Option<String>>` | Etykietowanie TF-IDF; uruchamiane gdy `member_count` zmieni się o >20% od ostatniego przeliczenia |
| `import_wordnet` | `async fn(db, path, language) -> Result<usize>` | Import z ustandaryzowanego formatu pośredniego JSON; patrz docs/03 §8 |

## SchemaFamily — routing tabel per wymiar

Kombinacja `(plugin_type, model_id)` mapuje się na konkretną parę tabel centroidów/członków w zależności od wymiaru wektora:

| Wymiar | Tabela centroidów | Tabela członków | Member ref |
|---|---|---|---|
| 1536d | `concept_centroids_1536` | `centroid_members_1536` | `ast_nodes` |
| 1024d | `concept_centroids_1024_qwen` | `centroid_members_1024_qwen` | `file_versions` |
| 768d | `concept_centroids_768` | `centroid_members_768` | `file_versions` |
| 384d | `concept_centroids_384` | `centroid_members_384` | `file_versions` |

Routing odbywa się przez `SchemaFamily` — enum lub lookup na podstawie `model_id.dimensions` z `embedding_models`. Zgodnie z Invariant #5 (nigdy nie mieszaj wymiarów) żadna funkcja nie przeszukuje więcej niż jednej rodziny naraz.

