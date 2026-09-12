# smartfs-docgen — UUID Symboli i Rejestr Dokumentacji (dev-tool)

← [Mapa crate'ów](../02-crates.md) | Mechanizm: [docs/04-uuid-doc-linking.md](../04-uuid-doc-linking.md)

## CLAUDE.md dla tego crate'a

```markdown
# smartfs-docgen — Symbol Registry Generator

## Owns exclusively
Ekstrakcja @id z kodu źródłowego crates/*/src przez tree-sitter-rust.
Generowanie i utrzymywanie docs/symbol_registry.json.
Rozwiązywanie linków symbol://<uuid> w plikach docs/*.md.

## To jest dev-tool, NIE wchodzi do binarki smartfs-daemon
Uruchamiany ręcznie lub w CI/pre-commit, nigdy przez daemon w runtime.
Brak zależności od tokio/sqlx/fuser — czysto synchroniczny, operuje na plikach.

## Nigdy
- Nie zmienia @id istniejącego symbolu
- Nie reużywa @id po tombstone
- Nie zgaduje UUID przy konflikcie — przy dwóch identycznych @id w różnych
  miejscach: hard error, nie "wybierz pierwszy"
- Nie modyfikuje kodu poza wstawianiem BRAKUJĄCYCH @id (backfill_missing_ids)

## Commands
cargo run -p smartfs-docgen -- scan ./crates > docs/symbol_registry.json
cargo run -p smartfs-docgen -- backfill ./crates
cargo run -p smartfs-docgen -- check ./crates ./docs   # CI hook
cargo run -p smartfs-docgen -- resolve <uuid>
```

## Funkcje

| Symbol | Sygnatura (skrót) | Opis |
|---|---|---|
| `scan_crate` | `fn(&Path, bool) -> Result<Vec<SymbolRecord>>` | Ekstrakcja przez tree-sitter-rust; `bool` = uwzględnić elementy bez `@id` |
| `backfill_missing_ids` | `fn(&Path) -> Result<usize>` | Idempotentny codemod — wstawia nowe UUID tam, gdzie brak |
| `resolve_symbol_link` | `fn(&SymbolRegistry, Uuid, &Path) -> Result<ResolvedLocation>` | Rozwiązanie `symbol://` z re-weryfikacją, nie tylko odczytem cache |
| `check_registry_consistency` | `fn(&Path, &Path) -> Result<Vec<ConsistencyIssue>>` | CI hook — brakujące `@id`, martwe linki, zduplikowane UUID |
| `tombstone_symbol` | `fn(&mut SymbolRegistry, Uuid, reason: &str)` | Oznacza usunięty symbol bez usuwania go z rejestru |

## Struktury

| Symbol | Kind | Opis |
|---|---|---|
| `SymbolRecord` | Struct | Jeden wiersz rejestru — patrz [schemat](../symbol_registry.schema.json) |
| `SymbolKind` | Enum | `Function \| Struct \| Enum \| Trait \| ImplBlock` |
| `SymbolRegistry` | Struct | In-memory reprezentacja `symbol_registry.json`, indeksowana po `Uuid` |
| `ResolvedLocation` | Enum | `Found { file, line } \| Tombstoned { removed_summary }` |
| `ConsistencyIssue` | Enum | `MissingId { file, line } \| DuplicateId { id, locations } \| DeadLink { doc_file, id } \| StaleCache { id, expected, actual }` |

## Dlaczego to jest osobny crate, nie moduł w `smartfs-cli`

`smartfs-cli` deleguje do `smartfs-db` i (od v6.0) `smartfs-semantic` — operuje na *danych użytkownika* przechowywanych przez system. `smartfs-docgen` operuje na *kodzie źródłowym samego SmartFS-a* jako plikach tekstowych na dysku deweloperskim, bez żadnej zależności od uruchomionego demona czy bazy danych. Mieszanie tych dwóch w jednym binarnym pyłoby niepotrzebnie łączyło ścieżkę "użytkownik odpytuje swoje pliki" ze ścieżką "deweloper utrzymuje dokumentację" — rozdzielenie jest tanie (to mały crate) i eliminuje ryzyko, że przyszła zmiana w jednym przypadkowo wpłynie na drugi.
