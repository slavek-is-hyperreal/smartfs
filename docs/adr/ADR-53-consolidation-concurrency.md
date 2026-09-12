# ADR-53 — Jeden supervisor per (plugin_type, model_id), advisory lock, merge jako backstop

← [Mapa ADR](../01-architecture.md) | Mechanizm: [docs/03-consolidation-design.md](../03-consolidation-design.md) §2, §4b

**Status:** Przyjęty do implementacji w v6.0 (domyka lukę zgłoszoną przy review pierwszej wersji specyfikacji)

## Kontekst

Pierwsza wersja specyfikacji konsolidacji (ADR-50) nie rozstrzygała: (1) czy istnieje jeden globalny worker konsolidacji, czy po jednym na kombinację `(plugin_type, model_id)`; (2) co się dzieje, gdy dwie współbieżne konsolidacje tej samej kombinacji jednocześnie nie znajdują wystarczająco bliskiego centroidu dla dwóch prawie identycznych wektorów — obie zakładają nowy centroid, i te dwa nigdy się nie łączą, bo nie istnieje operacja odwrotna do `split_centroid`.

## Decyzja

1. **Jeden `consolidation_supervisor` per `(plugin_type, model_id)`**, nie jeden globalny proces — bo progi konfiguracyjne (§3 w docs/03) są per kombinacja, więc wspólny worker musiałby przełączać kontekst configu przy każdym batchu.
2. **`pg_try_advisory_lock` na czas jednego batcha**, kluczowany hashem `(plugin_type, model_id)`. To jest jedyne miejsce w v6.0 sięgające po advisory lock — świadomie, bo operacja mieści się w całości w jednej krótkiej transakcji SQL, więc żaden z problemów, które zdyskwalifikowały advisory lock w dedupie blobów (ADR-40 — zwolnienie locka przy tymczasowym COMMIT, okno I/O) tutaj nie występuje. Zamyka współbieżne powstawanie duplikatów centroidów **u źródła** dla najczęstszego przypadku (dwie instancje demona, ta sama kombinacja, ten sam moment).
3. **`merge_centroids` jako rzadki, okresowy backstop** (nie główny mechanizm) dla drugiego przypadku: powolny semantyczny dryf, w którym dwa centroidy powstałe w różnym czasie zbliżają się do siebie bez żadnej współbieżnej kolizji w tle. Biegnie pod tym samym advisory lockiem co konsolidacja, więc nigdy nie koliduje z bieżącym batchem tej samej kombinacji.
4. **Scalanie i rozszczepianie nigdy nie usuwają wierszy** — `is_active=FALSE` + `merged_into` zamiast `DELETE`, zachowując pełną historię do audytu i czyniąc błędne scalenie odwracalnym ręcznie.

## Odrzucone alternatywy

**Brak żadnej ochrony przed duplikatami, zaakceptowanie ich jako szum.** Odrzucone: przy rosnącym korpusie liczba martwych, prawie identycznych centroidów rosłaby bez ograniczenia, degradując jakość `search_by_concept` (rozmywanie wyników pomiędzy duplikatami) bez żadnego mechanizmu naprawczego.

**Globalny mutex na cały proces konsolidacji (jeden supervisor, jedna blokada, wszystkie kombinacje szeregowo).** Odrzucone: przy wielu typach pluginów i modelach serializowałoby to konsolidację bez potrzeby — kombinacje `(rust, qwen)` i `(markdown, minilm)` nie mają ze sobą nic wspólnego i nie muszą czekać na siebie nawzajem.

## Konsekwencje

- `concept_centroids_*` zyskują kolumny `is_active`, `merged_into` (patrz migracja 005 §5).
- `SmartFsError` zyskuje warianty `MissingCalibration` i `AdvisoryLockUnavailable` (ten drugi nie jest traktowany jako błąd wymagający logowania na poziomie error — brak locka oznacza po prostu "inna instancja już pracuje", normalny stan).
- `spawn_all_consolidation_supervisors` musi okresowo re-skanować `consolidation_thresholds` w poszukiwaniu nowych, skalibrowanych kombinacji (nowy plugin type albo nowy model = nowy supervisor bez restartu demona).
