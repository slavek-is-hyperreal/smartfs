# ADR-49 — Qwen3-Embedding-0.6B jako domyślny model; Qwen3-VL-Embedding-2B dla obrazów

← [Mapa ADR](../01-architecture.md)

**Status:** Przyjęty do implementacji w v6.0

## Decyzja

- `is_default=TRUE` w `embedding_models` przechodzi z `all-MiniLM-L6-v2` (384d) na **Qwen3-Embedding-0.6B** (1024d, Apache 2.0, MRL, ~100+ języków w tym polski).
- Warianty 4B/2560d i 8B/4096d tej samej rodziny zajmują slot dotychczas zajmowany przez BGE-M3 jako opt-in "dokładny" tier w `smartfs.toml` (`accurate_model`).
- Nowy, osobny slot opt-in: **Qwen3-VL-Embedding-2B** dla pluginów plikowych bez `ast=true` i z rozszerzeniami obrazowymi (`png.json`, `jpg.json`, `pdf.json` traktowany jako obraz strony) — pierwszy raz w historii projektu te pliki dostają embedding zamiast `"embedding": null`.

## Dlaczego 0.6B, nie 4B/8B, jako *domyślny*

Anti-starvation valve (ADR-42, FIX-05) czyni pierwszy element batcha konsolidacji-embeddingowej nieprzerywalnym, żeby zagwarantować postęp pod ciągłym zapisem. Ten mechanizm zakłada, że pojedyncza inferencja jest krótka (rzędu dziesiątek-set milisekund, jak dla `all-MiniLM-L6-v2`). Model 8B na CPU to realnie sekundy na plik — jako model *domyślny*, uruchamiany po każdym zapisie, odtworzyłby dokładnie ten livelock, który FIX-05 miał zamknąć, tylko na innym poziomie (nieprzerywalny, ale wielosekundowy pierwszy element = worker efektywnie zamrożony pod ciągłą edycją). 0.6B ma profil kosztowy tego samego rzędu co dzisiejszy domyślny model — podmieniamy "coś działającego" na "coś lepszego o tym samym profilu ryzyka", nie robimy jednocześnie dwóch zmian (jakość + koszt operacyjny).

## Konsekwencja schematowa — to NIE jest podmiana pliku modelu

Zmiana wymiaru (384→1024) oznacza inną przestrzeń wektorową — stare i nowe wektory nie są ze sobą porównywalne. To wymaga:

1. Nowej tabeli `embeddings_1024_qwen` (analogicznej do `embeddings_384`/`embeddings_768`), z własnym indeksem HNSW.
2. Nowego wiersza w `embedding_models` (`Qwen3-Embedding-0.6B`, `dimensions=1024`).
3. Przestawienia `is_default` na nowy wiersz, przy zachowaniu starego (`all-MiniLM-L6-v2`) jako `is_default=FALSE` — nie usuwamy starych embeddingów, `search_semantic` nadal potrafi je odpytać jawnie podając `model_id`.
4. **Procedury cutover**, jawnie opisanej tutaj, bo v4.5/v5.0 nie miały żadnej: worker po starcie z nową konfiguracją zaczyna generować `embeddings_1024_qwen` dla *nowych* zapisów natychmiast; pełne pokrycie istniejącego korpusu wymaga jednorazowego zadania wsadowego (`smartfs-cli reembed --model qwen3-embedding-0.6b --all`), które przechodzi przez wszystkie `file_versions WHERE status='clean'` i dogenerowuje brakujące wiersze w nowej tabeli, w tempie ograniczonym tą samą logiką backpressure co worker (nie wszystko naraz).
5. Dopóki reembedding wsadowy nie pokryje całego korpusu: `search_semantic`/`search_by_concept` bez jawnie podanego `model_id` powinno paść na `is_default=TRUE` (nowy model) i zwracać wyniki tylko z tego, co już przeembedowane — **nie** fallbackować cicho do starego modelu (to zaciemniłoby, które wyniki są z jakiego modelu). Jawny parametr `model_id` w MCP pozwala nadal odpytać stary korpus w całości w międzyczasie.

## Qwen3-VL-Embedding dla obrazów — zakres decyzji

Traktowane jako osobny, dodatkowy embedding (nowa tabela, nowy `plugin_type` w sensie partycjonowania z FIX-07), **nie** jako embedding domyślny dla wszystkich plików — mieszanie przestrzeni wektorowej obrazu i tekstu w jednej tabeli łamałoby Invariant #5. `png.json` zyskuje pole:

```json
"embedding": {
    "model": "qwen3-vl-embedding-2b",
    "dimensions": 1024,
    "description": "Multimodalny embedding percepcyjny obrazu."
}
```

## Otwarte pytanie, świadomie nierozwiązane w tej wersji

Rodzina Qwen (stan na moment pisania) nie ma dojrzałego, kontrastywnie trenowanego modelu embeddingowego dla audio analogicznego do CLAP. Jeśli SmartFS ma docelowo objąć też pliki muzyczne/dźwiękowe jako „sygnał", to wymaga osobnej decyzji o modelu spoza rodziny Qwen — nie rozstrzygamy tego w ADR-49.

## Addendum (po implementacji v6.0)

- **Qwen3-VL-Embedding-2B** (embedding obrazów) jest **post-MVP** — jawnie zdegradowany per B-06. Brak tabeli `embeddings_1024_qwen_vl` w migracjach 001-006. Zostanie dodany w przyszłej migracji, gdy pipeline embeddingowy dla obrazów zostanie zaimplementowany.
- **BGE-M3** (768d) jest **zachowany** dla zewnętrznych, prekalkulowanych korpusów (Wikipedia, encyklopedie — ADR-06). Tabele (`embeddings_768`, `concept_centroids_768`) pozostają w schemacie.
- **Qwen3-Embedding-4B** (2560d) to przyszły tier dokładnościowy, post-MVP — brak tabeli `embeddings_2560` w aktualnym schemacie.
- **Fallback offline:** jeśli domyślny model nie jest dostępny w czasie uruchomienia, fallback idzie na aktywny model `is_default=TRUE` w chwili startu — nie na zahardkodowaną stałą MiniLM.
