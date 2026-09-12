# smartfs-mcp — delta w v6.0

← [Mapa crate'ów](../02-crates.md) | Baza: CLAUDE.md z v4.5 §3.8 (bez zmian poza nowym narzędziem)

## Nowe narzędzie

**`search_by_concept(query, plugin_type?, model_id?, limit)`**
Deleguje do `smartfs_semantic::search_by_concept`. W odróżnieniu od `search_semantic` (płaski cosine po jednej tabeli embeddingów) przeszukuje graf centroidów — zwraca wyniki pogrupowane wg centroidu z jego etykietą (jeśli już przypisaną przez graf leksykalny lub `label_centroid_from_members`), plus surowe trafienia. Odpowiada też na pytanie "jakie tematy/centroidy w ogóle istnieją dla tego `plugin_type`" — nowy tryb wywołania bez `query`, tylko z `plugin_type`, zwracający listę centroidów posortowaną wg `member_count`.

```jsonc
// Przykładowe wywołanie bez query — "co jest w tym repozytorium"
{ "tool": "search_by_concept", "plugin_type": "rust", "limit": 20 }
// → lista centroidów: { id, label, member_count, sample_names: [...] }
```

**`search_fulltext(query, plugin_type?, limit)`** (ADR-54)
Deleguje do `smartfs_db::search_fulltext_bm25`. Dosłowne, leksykalne dopasowanie (BM25 przez pg_search) — komplementarne do `search_semantic`/`search_by_concept`, nie ich zastępstwo. Dobre tam, gdzie użytkownik wie dokładnie, czego szuka: nazwa symbolu, treść komunikatu błędu, fraza w cudzysłowie. Zwraca trafienia zarówno z warstwy plikowej (`file_versions.search_text`), jak i z kodu per-funkcja (`ast_nodes.source`), oznaczone polem `kind`. Scalenie trzech trybów wyszukiwania w jeden ranking hybrydowy jest świadomie poza zakresem v6.0 — patrz "Otwarte pytanie" w [ADR-54](../adr/ADR-54-fulltext-search-backend.md).

## Bez zmian

`search_semantic`, `search_functions`, `get_file_history`, `query_by_metadata`, `find_by_hash`, `get_file_content`, `find_broken_files`, `diff_functions` — wszystkie z v4.5, bez modyfikacji. Zasada "MCP przyjmuje strukturę, nie surowy SQL" (ADR-07) obowiązuje też nowe narzędzie.
