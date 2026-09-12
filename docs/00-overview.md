# Poziom 1 — Filozofia i warstwy

← [README](../README.md) | → [Architektura systemu](01-architecture.md)

## Trzy warstwy danych (bez zmian od v4.5)

- **Tożsamość danych** — co to jest, co znaczy, jak się zmieniało (PostgreSQL)
- **Fizyczne bajty** — gdzie leżą i w jakiej formie (Storage Backend)
- **Interfejs dostępu** — jak aplikacje i AI mogą to konsumować (FUSE + MCP)

v6.0 dodaje pod spód czwartą, ukrytą warstwę, o której żadna z powyższych trzech nic nie wie wprost:

- **Konsolidacja znaczenia** — jak pojedyncze, świeże embeddingi stają się z czasem częścią nawigowalnego grafu pojęć, zamiast zostać na zawsze rozproszonym chmurą punktów w przestrzeni wektorowej

## Skąd wzięła się warstwa konsolidacji

Dwa fakty, które się ze sobą nie zgadzały w v5.0:

1. Wyszukiwanie semantyczne w v5.0 to zawsze płaski `ORDER BY embedding <=> $query LIMIT N` po HNSW. Działa dobrze punktowo ("znajdź mi coś podobnego do X"), ale nie daje żadnej odpowiedzi na pytanie "z czego w ogóle składa się ta baza wiedzy" — nie ma pojęcia wyższego rzędu niż pojedynczy wektor.
2. Każda próba utrzymywania takiego pojęcia wyższego rzędu (np. klastra tematycznego) *w locie*, współbieżnie z ciągłymi zapisami FUSE, powtarza dokładnie tę klasę błędu, którą FIX-02 już raz musiał naprawić dla `is_current`: stan pochodny licząc się asynchronicznie ściga się z zapisem i psuje się pod współbieżnością.

Rozwiązanie przyjęte w v6.0 nie próbuje utrzymywać klastrów w spójności ciągłej. Zamiast tego rozdziela dwa reżimy czasowe, świadomie analogicznie do tego, jak w poznaniu współistnieje pamięć robocza (mała, szybka, tymczasowo niespójna z resztą) i pamięć skrystalizowana (duża, wolno aktualizowana, ale stabilna i przeszukiwalna strukturalnie):

- **Pamięć robocza** — każdy nowy embedding jest natychmiast przeszukiwalny wprost (dokładny cosine / HNSW, jak dziś), ale jeszcze nie ma miejsca w grafie centroidów. To jest ograniczony, tani do przeszukania zbiór.
- **Akt konsolidacji** — okresowy, wsadowy proces, wyzwalany albo przekroczeniem limitu pamięci roboczej, albo długą ciszą zapisu (odpowiednik snu), który dogrywa świeże wektory do istniejącego grafu centroidów, lokalnie, bez globalnego przeliczania wszystkiego od zera.
- **Pamięć skrystalizowana** — graf centroidów, opcjonalnie zakotwiczony w graf leksykalny (słowo → centroid), read-mostly, tani i stabilny do przeszukiwania strukturalnego ("pokaż mi tematy w tym repozytorium").

Zapytanie użytkownika zawsze przeszukuje obie warstwy naraz i scala wynik (wzorzec identyczny jak memtable + SSTable w bazach LSM-tree) — nic nigdy nie jest niewidoczne, tylko świeżo zapisane pliki nie mają jeszcze etykiety tematycznej, dopóki nie przejdą konsolidacji.

Pełny mechanizm: [docs/03-consolidation-design.md](03-consolidation-design.md).

## Druga zmiana filozoficzna: model embeddingowy jako wymienna, nie wbudowana część

v4.5 traktował `all-MiniLM-L6-v2` jako praktycznie stały fundament (`is_default=TRUE`, bez opisanej ścieżki zmiany). v6.0 traktuje wybór domyślnego modelu jako decyzję, która *będzie* się zmieniać w cyklu życia projektu (nowe modele, nowe wymiary, nowe modalności), i wymaga, żeby każda taka zmiana miała jawną, opisaną procedurę migracji korpusu — nie tylko podmianę pliku modelu. Pierwsza taka zmiana opisana jest w [ADR-49](adr/ADR-49-qwen-default-model.md).

## Trzecia zmiana filozoficzna: SQL jako narzędzie wyszukiwania, nie tylko magazyn

v6.0 dodaje wyszukiwanie pełnotekstowe ([ADR-54](adr/ADR-54-fulltext-search-backend.md)) i przy tej okazji warto nazwać prawidłowość widoczną w projekcie od początku: większość twardych zwycięstw poprawnościowych SmartFS (FIX-01..10, partial unique index, `is_active`/`merged_into` zamiast `DELETE`, `NOT NULL`, które złapało błąd `plugin_type` przy review konsolidacji) została wymuszona na poziomie SQL/schematu, nie w kodzie Rust. Wybór `pg_search` zamiast osobnego silnika wyszukiwania kontynuuje ten wzorzec świadomie: Postgres-jako-platforma dostaje więcej odpowiedzialności, nie mniej, bo konsekwentnie okazuje się, że robi ją porządniej niż bespoke'owy kod utrzymywany osobno.

Dalej: [docs/01-architecture.md](01-architecture.md)
