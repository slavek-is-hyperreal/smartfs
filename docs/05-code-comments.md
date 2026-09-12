# Poziom 1.5 — Metodyka komentarzy w kodzie

← [UUID i linkowanie dokumentacji](04-uuid-doc-linking.md) | → [Plan wykonania agentowego](06-agentic-execution-plan.md)

Ten dokument uzupełnia [docs/04](04-uuid-doc-linking.md): tamten opisuje *mechanizm* trwałego linkowania (UUID, `symbol://`), ten opisuje *treść* — co w ogóle powinno się znaleźć w komentarzu, żeby przeżył lata edycji, a nie tylko przeżył zmianę numeru linii.

## Zasada nadrzędna: komentarz przeżywa kontekst, w którym powstał

Modele językowe (w tym te, które współtworzyły ten projekt) mają nawyk pisania komentarzy w stylu "rozwiązuje problem z promptu" albo "poprawka po Twojej uwadze o X" — sensownych w chwili pisania, bezsensownych dwa lata później, gdy nikt już nie pamięta, czym było "X" ani jaki prompt to był. Zasada, z której wynika reszta tego dokumentu: **komentarz musi być zrozumiały dla kogoś, kto nie ma dostępu do historii rozmowy, w której kod powstał — łącznie z przyszłą wersją nas samych.**

## Zasady

1. **Komentarz opisuje "dlaczego", nie "co"** (McConnell, *Code Complete*; Martin, *Clean Code*). Kod już mówi, co robi — komentarz wart napisania to decyzja projektowa, ograniczenie zewnętrzne, invariant, albo powód, dla którego odrzucono inne podejście. Komentarz powtarzający sygnaturę funkcji słowami to szum, nie dokumentacja.

2. **Zero odniesień do efemerycznego kontekstu.** Nigdy: nazwa promptu, numer wiadomości w czacie, "jak prosiłeś", "zgodnie z Twoją sugestią", identyfikator istniejący wyłącznie w historii rozmowy. Zamiast tego: odniesienie do trwałego artefaktu — numeru ADR, nazwy invariantu, numeru migracji, linku `symbol://<uuid>` (docs/04). Trwały artefakt da się sprawdzić za rok bez dostępu do rozmowy, w której powstał; efemeryczny — nie.

3. **Anti-pattern "journal comment"** (Martin, *Clean Code*). Komentarz-dziennik zmian ("2026-03: dodano X", "poprawka Y po review") należy do `git log`/`git blame`, nie do treści pliku. Kod źródłowy opisuje stan obecny, nie swoją historię — historia już jest zapisana gdzie indziej, w systemie, który jest do tego zaprojektowany.

4. **Komentarz, który kłamie, jest gorszy niż jego brak** (Kernighan & Pike, *The Practice of Programming*). Komentarz nieaktualizowany razem ze zmianą logiki, którą opisuje, staje się aktywnie mylący. Zasada praktyczna: zmiana sygnatury lub zachowania funkcji i aktualizacja komentarza nad nią to jedna zmiana, nie dwa osobne kroki, z których drugi łatwo pominąć.

5. **Używaj wbudowanej struktury rustdoc zamiast wolnej prozy tam, gdzie pasuje**: `# Examples`, `# Errors`, `# Panics`, `# Safety` (Rust API Guidelines, konwencje C-FAILURE/C-EXAMPLE). To jest znormalizowany, przeszukiwalny i renderowalny (`cargo doc`) format — nie tylko kwestia stylu. Funkcja `unsafe` bez sekcji `# Safety` tłumaczącej, jakiego niezmiennika pilnuje wywołujący, jest w tym projekcie traktowana jak brakujący test.

6. **Link do ADR/invariantu zawsze z krótkim streszczeniem obok, nigdy samym numerem.** "Patrz ADR-53" bez ani słowa treści jest kruche — plik może zostać przeniesiony, ADR zarchiwizowany, link się urwie. "Advisory lock per (plugin_type, model_id), patrz ADR-53" zostaje czytelne nawet wtedy, gdy link już nie działa. Link ma być tak trwały, jak się da (numer ADR, nie numer linii) — ale nigdy jedynym nośnikiem znaczenia.

7. **Język: kod i doc-komentarze w Rust — po angielsku.** To konwencja ekosystemu (docs.rs, discoverability, kontrybutorzy spoza Polski) i jest spójna z celem projektu, żeby SmartFS pozostał dostępny szerzej niż tylko lokalnie. Dokumentacja architektoniczna w `docs/` (ten plik włącznie) zostaje po polsku — to jest warstwa decyzyjna projektu, nie jego publiczny interfejs.

## Co nie jest komentarzem, tylko powinno być czymś innym

- **Wyłączony/zakomentowany kod** → usuń. Jest w historii `git`, jeśli będzie kiedyś potrzebny.
- **TODO bez właściciela i bez warunku, kiedy przestaje być aktualny** → albo issue w trackerze projektu, albo w ogóle nie pisz — TODO, który nikt nigdy nie zamknie, jest gorszy niż jego brak.
- **Parafraza typu/sygnatury słowami** → usuń, to czysta redundancja, którą kompilator i tak weryfikuje.

## Lint w CI (propozycja, proporcjonalna do skali projektu)

Prosty grep/regex w CI, nie pełny analizator statyczny, odrzucający wzorce w stylu: `fixes.*prompt`, `as (you|I) (asked|said|suggested)`, `per (our|the) conversation`, `id\d+.*prompt`, `zgodnie z (Twoją|twoją) (prośbą|sugestią)`. Tani check, łapiący dokładnie tę klasę błędu, o którą chodziło w tej decyzji — nie pretenduje do wykrycia każdego złego komentarza.

## Przykład

Źle (odniesienie efemeryczne, "co" zamiast "dlaczego"):

```rust
// Poprawka po Twojej uwadze — id5 z promptu. Zwraca teraz Option.
pub fn find_centroid(...) -> Option<Uuid> { ... }
```

Dobrze (trwały artefakt, "dlaczego", struktura rustdoc):

```rust
/// Returns `None` when no centroid is within `join_threshold` (ADR-50,
/// docs/03 §5) — the caller is expected to create a new centroid in that
/// case, never to treat `None` as an error.
///
/// # Errors
/// Propagates `SmartFsError::MissingCalibration` if no calibrated
/// threshold exists yet for this `(plugin_type, model_id)` (ADR-53).
pub fn find_centroid(...) -> Result<Option<Uuid>> { ... }
```

Dalej: [docs/06-agentic-execution-plan.md](06-agentic-execution-plan.md)
