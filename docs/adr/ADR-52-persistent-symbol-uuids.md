# ADR-52 — Trwałe UUID symboli kodu, skaner docgen i schemat symbol://

← [Mapa ADR](../01-architecture.md) | Specyfikacja: [docs/04-uuid-doc-linking.md](../04-uuid-doc-linking.md) | Standard komentarzy: [docs/05-code-comments.md](../05-code-comments.md)

**Status:** Przyjęty do implementacji w v6.0

## Kontekst

W dużych projektach z rozbudowaną dokumentacją architektoniczną referencje z dokumentacji do kodu źródłowego (`crates/smartfs-semantic/src/worker.rs:142` lub `file:///...`) ulegają szybkiej degradacji pod wpływem refaktorów, przenoszenia plików i zmian liczby linii. Dokumentacja staje się pełna martwych lub fałszywych odnośników, a odtworzenie intencji architektonicznej wymaga czasochłonnego przeszukiwania historii git.

Potrzebny był mechanizm trwałego wiązania pojęć architektonicznych i specyfikacji z konkretnymi implementacjami w kodzie, odporny na refaktoryzacje i weryfikowalny w potoku CI.

## Decyzja

1. **Adnotacje `@id: <uuid>` w kodzie źródłowym:**
   Każdy publiczny symbol w workspace (`pub fn`, `pub struct`, `pub enum`, `pub trait`, bloki `impl`) zostaje opatrzony unikalnym identyfikatorem UUID v4 w bloku komentarza dokumentacyjnego:
   ```rust
   /// @id: 7e2b1c43-98a5-4f01-b36e-561278a9c3d4
   /// Opis funkcji...
   pub async fn consolidate_batch(...) -> Result<usize> { ... }
   ```

2. **Dedykowany crate narzędziowy `smartfs-docgen`:**
   Samodzielne narzędzie deweloperskie (bez zależności od Tokio, SQLx czy FUSE), które:
   - Skanuje pliki źródłowe `.rs` i buduje rejestr symboli (`docs/symbol_registry.json`) zgodny ze schematem `docs/symbol_registry.schema.json`.
   - Zapewnia polecenie `smartfs-docgen backfill` do idempotentnego nadawania UUID brakującym symbolom.
   - Weryfikuje spójność (`smartfs-docgen check`): wykrywa brakujące ID, zduplikowane UUID, martwe odnośniki w dokumentacji markdown oraz rozbieżności lokalizacji.
   - Umożliwia rozwiązywanie odnośników (`smartfs-docgen resolve <uuid>`) do aktualnego pliku i linii.

3. **Schemat `symbol://<uuid>` w dokumentacji:**
   Wszystkie odnośniki w dokumentacji architektonicznej referujące symbole kodu używają formatu `[NazwaSymbolu](symbol://<uuid>)`.

4. **Tombstoning zamiast cichego usuwania:**
   Gdy symbol zostaje usunięty lub zastąpiony, jego wpis w rejestrze jest oznaczany jako `tombstoned = true` z podaniem przyczyny (`tombstoned_reason`), co zapobiega powstawaniu cichych martwych linków i zachowuje ciągłość audytową.

## Odrzucone alternatywy

**Ścieżki relatywne plik:linia w markdown.** Odrzucone: jakakolwiek zmiana kodu (dodanie importu, podział modułu) unieważnia numery linii lub ścieżki, czyniąc dokumentację niewiarygodną.

**Makra proceduralne Rust (np. `#[symbol_id("...")]`).** Odrzucone: spowalniają czas kompilacji, zaciemniają kod źródłowy i utrudniają analizę przez proste narzędzia tekstowe bez pełnego rozwijania makr i środowiska kompilatora.

**Czysty git blame / ctags / LSP.** Odrzucone: ctags/LSP opierają się na nazwie symbolu, co nie chroni przed kolizjami nazw, zmianami nazw w trakcie refaktoryzacji ani nie pozwala na tombstoning z opisem architektonicznym w rejestrze.

## Konsekwencje

- Powstanie crate'a `smartfs-docgen` oraz rejestru `docs/symbol_registry.json`.
- Wprowadzenie twardej bramki jakościowej w CI: `cargo run -p smartfs-docgen -- check ./crates ./docs` nie dopuszcza do repozytorium kodu bez identyfikatorów ani dokumentacji z martwymi linkami.
- Pełna identyfikowalność i synchronizacja specyfikacji z implementacją kodu w całym projekcie.
