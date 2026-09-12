# Głęboki nurek — trwałe UUID dla symboli i linkowanie dokumentacji

← [Konsolidacja semantyczna](03-consolidation-design.md) | Implementacja: [`smartfs-docgen`](crates/smartfs-docgen.md)

## Problem

Dokumentacja, która linkuje "funkcja X jest w pliku Y, linia Z", psuje się przy pierwszej edycji, która przesuwa linie — czyli praktycznie przy każdym commicie. To jest dokładnie ten sam problem, który SmartFS już raz rozwiązał dla *danych użytkownika*: plik nie jest swoją ścieżką ani swoim numerem linii, jest swoją tożsamością (hashem). v6.0 stosuje dokładnie tę samą zasadę do *własnego kodu źródłowego* SmartFS: funkcja/struct/impl nie jest swoim adresem w pliku, jest swoim UUID.

## Konwencja: `@id` w komentarzu dokumentującym

Bezpośrednio nad każdym publicznym elementem (`pub fn`, `pub struct`, `pub enum`, `pub trait`, blok `impl`), który ma być adresowalny z dokumentacji, umieszczamy znacznik:

```rust
/// @id: 6b2d4e18-3f77-4a90-9c11-8a5f0d2e7c44
/// Pętla nadzorcy konsolidacji — patrz docs/03-consolidation-design.md §2.
pub async fn consolidation_supervisor(db: Pool, cfg: ConsolidationConfig) {
    // ...
}
```

Zasady:

- `@id` jest nadawany **raz**, przy pierwszym wprowadzeniu symbolu, i **nigdy się nie zmienia** — nawet gdy funkcja jest przenoszona do innego pliku, przemianowywana, czy przenoszona między crate'ami. Tożsamość jest niezależna od lokalizacji, dokładnie jak `content_hash` blobu.
- Jeśli funkcja jest usuwana, jej `@id` jest odnotowywany jako *tombstone* w rejestrze (nie jest nigdy reużywany dla innego symbolu) — link w starej dokumentacji rozwiązuje się wtedy na jawny komunikat "symbol usunięty w commicie X", zamiast fałszywie wskazywać na coś innego.
- Elementy prywatne (nie-`pub`) nie muszą mieć `@id` — dokumentacja poziomu "dla użytkownika crate'a" nie ma potrzeby ich adresować. `smartfs-docgen` może być uruchomiony też z flagą obejmującą elementy prywatne, do dokumentacji wewnętrznej danego crate'a.

## Ekstrakcja: dlaczego przez tree-sitter, nie przez `syn`/makra proceduralne

SmartFS **już ma** zależność `tree-sitter` + `tree-sitter-rust` w workspace (używaną w `smartfs-ai` do parsowania kodu *użytkownika*). `smartfs-docgen` reużywa tę samą zależność do parsowania *własnego* kodu SmartFS — nie dodajemy nowego parsera ani nie próbujemy tego robić przez `syn` + makra proceduralne (co wymagałoby kompilacji, a `smartfs-docgen` ma działać też na kodzie, który się aktualnie nie kompiluje, np. w trakcie edycji). To jest deliberatywnie ten sam wybór technologiczny, żeby nie utrzymywać dwóch niezależnych parserów Rusta w jednym repozytorium.

```rust
/// @id: d9a4f2e1-3b76-4c88-a0f5-1e6d9c3a7b02
pub struct SymbolRecord {
    pub id: Uuid,
    pub crate_name: String,
    pub kind: SymbolKind,
    pub name: String,
    /// Ostatnia ZNANA lokalizacja — cache do szybkiego przejścia, NIGDY
    /// nie jest traktowana jako źródło prawdy. resolve_symbol_link zawsze
    /// weryfikuje ją ponownym skanem przed zwróceniem wyniku.
    pub last_known_file: PathBuf,
    pub last_known_line: u32,
    pub doc_summary: Option<String>,
    pub tombstoned: bool,
}

/// @id: b7e1c3a9-5f28-4d64-9a1e-8c0b6d2f4a91
pub enum SymbolKind { Function, Struct, Enum, Trait, ImplBlock }

/// @id: 4f8a2d6e-1c93-4b57-8e0a-3d7f9b1c5a44
/// Przechodzi drzewo źródeł jednego crate'a i zwraca wszystkie symbole
/// oznaczone @id (oraz — z flagą include_unlabeled — te bez znacznika,
/// do wykrycia brakujących).
pub fn scan_crate(path: &Path, include_unlabeled: bool) -> Result<Vec<SymbolRecord>> {
    // tree-sitter-rust: query po node.kind() in
    // {"function_item", "struct_item", "enum_item", "trait_item", "impl_item"},
    // odczyt poprzedzającego line_comment / doc_comment w poszukiwaniu "@id:"
    ...
}

/// @id: e2c6a8f4-9d31-4b70-a5c8-2f0e6d1b3a77
/// Idempotentny codemod: dla elementów pub bez @id wstawia nowy Uuid::new_v4()
/// bezpośrednio nad definicją, jako nowa linia doc-comment. Bezpieczne do
/// uruchamiania wielokrotnie — nigdy nie dotyka elementów, które już mają @id.
pub fn backfill_missing_ids(path: &Path) -> Result<usize> {
    ...
}
```

## Rejestr symboli

`smartfs-docgen scan` produkuje `docs/symbol_registry.json` — jeden plik, generowany, **nie edytowany ręcznie** (analogicznie do tego, że `ast_nodes` w bazie nigdy nie jest edytowane ręcznie, tylko wypełniane przez parser). Schemat: [`docs/symbol_registry.schema.json`](symbol_registry.schema.json).

Rejestr jest wejściem dla dwóch operacji:

```rust
/// @id: 1c9f4b2e-6a37-4d81-b0e5-9c2a7f3d6b18
/// Rozwiązuje `symbol://<uuid>` na aktualne file:line. NIGDY nie ufa
/// last_known_line z rejestru jako ostatecznej odpowiedzi bez weryfikacji —
/// zawsze re-skanuje wskazany plik i potwierdza, że symbol tam nadal jest
/// (chroni przed rejestrem, który sam jest nieaktualny względem repo).
pub fn resolve_symbol_link(registry: &SymbolRegistry, id: Uuid, crates_root: &Path) -> Result<ResolvedLocation> {
    let record = registry.get(id).ok_or(Error::UnknownSymbol(id))?;
    if record.tombstoned {
        return Ok(ResolvedLocation::Tombstoned { removed_summary: record.doc_summary.clone() });
    }
    verify_and_locate(crates_root, record) // re-skan, potwierdzenie, ewentualna korekta last_known_line
}

/// @id: 9a3e7c1f-4b58-4d02-a6f9-3c8e1d5b7a20
/// CI hook: uruchamiany w pre-commit / CI. Fails build jeśli którykolwiek
/// pub-liczny symbol nie ma @id, LUB jeśli dokumentacja w docs/ zawiera
/// symbol://<uuid> wskazujący na nieistniejący/tombstoned rekord.
pub fn check_registry_consistency(crates_root: &Path, docs_root: &Path) -> Result<Vec<ConsistencyIssue>> {
    ...
}
```

## Składnia linku w dokumentacji

W plikach `.md` pod `docs/` symbol jest linkowany tak:

```markdown
Zobacz implementację [`consolidation_supervisor`](symbol://6b2d4e18-3f77-4a90-9c11-8a5f0d2e7c44)
```

`symbol://<uuid>` nie jest realnym URI rozwiązywanym przez przeglądarkę — jest rozwiązywany przez `smartfs-docgen resolve` (CLI) albo przez wtyczkę edytora, która na żądanie zamienia go na aktualne `crates/smartfs-semantic/src/consolidate.rs:47` i tam przeskakuje. Dokumentacja w repo commituje `symbol://`, nigdy zamrożonego `plik:linia` — to jest cała różnica, która sprawia, że link przeżywa refaktoryzację.

## Docelowy stan: dogfooding przez samo SmartFS

`smartfs-docgen` jest rozwiązaniem przejściowym, potrzebnym dopóki kod źródłowy SmartFS nie jest przechowywany *w* SmartFS. SmartFS już dziś nadaje UUID każdemu węzłowi AST (`ast_nodes.id`) każdego pliku `.rs`, który przez niego przechodzi — czyli gdyby repozytorium SmartFS było zamontowane jako katalog na samym SmartFS (wizja z §22 dokumentu architektury v4.5: "SmartFS hostuje własne artefakty"), `ast_nodes.id` byłby dokładnie tym samym UUID, o który tu chodzi, nadawanym automatycznie, bez osobnego narzędzia. `smartfs-docgen` istnieje więc świadomie jako rozwiązanie na "przed self-hostingiem" — gdy self-hosting wyląduje (post-v1.0), ten crate powinien zostać usunięty na rzecz bezpośredniego odpytywania własnej bazy `ast_nodes` SmartFS-a o samego siebie.

Dalej, konkretna implementacja: [docs/crates/smartfs-semantic.md](crates/smartfs-semantic.md), [docs/crates/smartfs-docgen.md](crates/smartfs-docgen.md)
