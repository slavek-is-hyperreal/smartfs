# ADR-50 — Konsolidacja semantyczna: pamięć robocza + akt scalenia zamiast streaming clustering

← [Mapa ADR](../01-architecture.md) | Pełny mechanizm: [docs/03-consolidation-design.md](../03-consolidation-design.md)

**Status:** Przyjęty do implementacji w v6.0

## Kontekst

v5.0 rozwiązuje wyszukiwanie wyłącznie jako płaskie `ORDER BY embedding <=> query`. Brakuje warstwy pojęciowej wyższego rzędu (klastrów tematycznych, powiązania słowo→znaczenie). Naiwna droga do takiej warstwy — utrzymywanie centroidów w spójności ciągłej, aktualizowanych przy każdym zapisie — powtarza klasę błędu współbieżności, którą projekt już raz napotkał i naprawił dla `is_current` (FIX-02): stan pochodny liczony asynchronicznie ściga się z zapisem.

## Decyzja

Rozdzielamy dwa reżimy czasowe zamiast utrzymywać jeden spójny stan w czasie rzeczywistym:

- **Pamięć robocza** — bufor nieskonsolidowanych embeddingów (`consolidated=FALSE`), zawsze przeszukiwalny wprost (brute-force, tani bo ograniczony), nigdy nie blokujący zapisu.
- **Akt konsolidacji** — wsadowy proces wyzwalany przez dwa niezależne warunki: przekroczenie limitu bufora (`backlog_threshold`) LUB długą ciszę zapisu (`idle_before_sleep`), z twardym sufitem (`max_wait`) gwarantującym postęp nawet pod nieprzerwaną aktywnością.
- **Pamięć skrystalizowana** — graf centroidów, aktualizowany wyłącznie przez akt konsolidacji, lokalnie (dołączanie do najbliższego istniejącego centroidu + rozszczepianie po przekroczeniu progu wariancji/liczności), nigdy globalnym przeliczeniem.

Zapytanie zawsze scala obie warstwy (wzorzec memtable+SSTable z baz LSM-tree) — nic nie jest niewidoczne, tylko świeże dane nie mają jeszcze etykiety tematycznej do czasu konsolidacji.

## Odrzucone alternatywy

**Streaming k-means (aktualizacja centroidu przy każdym zapisie, w tej samej transakcji co `cow_commit`).** Odrzucone: wymagałoby trzymania blokady na centroidzie przez czas obliczenia przypisania, na krytycznej ścieżce zapisu — dokładnie ten sam błąd co próba ustawiania `is_current` w `cow_commit` przed FIX-02, tylko na trudniejszym obiekcie.

**Globalne przeliczenie k-means po każdym batchu embeddingów.** Odrzucone: koszt O(wszystkie wektory) przy każdym przeliczeniu nie skaluje się; przy dużym korpusie każda konsolidacja byłaby droższa niż sam zysk z niej.

**HDBSCAN / density-based clustering zamiast IVF-style dołączania z progiem.** Rozważone, nie odrzucone na stałe — semantycznie ładniejsze (liczba klastrów wyłania się sama, nie jest parametrem `k`), ale brak gotowego wsparcia inkrementalnego w Postgresie czyni to zadaniem wsadowym offline, nie mechanizmem pasującym do architektury "worker działający ciągle w tle". Zostawione jako możliwy tryb `clustering_strategy = "hdbscan_batch"` post-v6.0, nie domyślny.

## Konsekwencje

- Nowy crate `smartfs-semantic`, nowa migracja `005_semantic_consolidation.sql`.
- `smartfs-mcp` zyskuje `search_by_concept` obok istniejącego `search_semantic`.
- Wynik wyszukiwania przez pojęcia ma opóźnienie względem zapisu rzędu `idle_before_sleep` lub `backlog_threshold` embeddingów (cokolwiek nastąpi pierwsze) — świadomie akceptowane, bo dotyczy tylko warstwy grupowania tematycznego, nie samego wyszukiwania punktowego, które pozostaje natychmiastowe przez istniejące `search_semantic`.
