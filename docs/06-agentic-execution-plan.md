# Poziom 1.5b — Plan wykonania agentowego (Antigravity 2.0 / Gemini 3.8 Flash)

← [Metodyka komentarzy](05-code-comments.md)

Ten dokument różni się od docs/00-05: tamte opisują **co** zbudować. Ten opisuje **jak** to zlecić konkretnemu narzędziu agentowemu (stan na wrzesień 2026: Google Antigravity 2.0, domyślny model Gemini 3.8 Flash) tak, żeby nie ucierpiała spójność między crate'ami. Jeśli w chwili wykonania używane jest inne narzędzie/model, sekcja "Dlaczego" niżej i tak zostaje aktualna — tylko konkretne liczby (kontekst, limit outputu, zachowanie subagentów) trzeba zweryfikować na nowo.

## Dlaczego nie "trzy weekendy"

Podział z oryginalnej roadmapy (weekend 1, 2, 3...) był tempem dla człowieka. Nie mapuje się na orkiestrację agentową — tam realnymi ograniczeniami są:

- **Gemini 3.8 Flash: 1M tokenów kontekstu wejściowego, ale tylko 64k tokenów outputu na turę.** Cała specyfikacja v6.0 plus oryginalne dokumenty v4.5/v5.0 mieszczą się wygodnie w kontekście wejściowym (rząd wielkości: dziesiątki tysięcy linii, dużo poniżej 1M tokenów). Ale nawet gdy agent "rozumie" cały projekt naraz, fizycznie nie może wypisać całego wielocratowego kodu Rust w jednej turze — 64k tokenów outputu to za mało na cały workspace.
- **Subagenty w Antigravity są od siebie izolowane** — równoległy subagent nie widzi zmian plików innego równoległego subagenta, chyba że są jawnie przepuszczone przez wspólną warstwę. Naiwne "odpal 10 subagentów, niech każdy pisze jeden crate równolegle od zera" ryzykuje rozjazd — dwa subagenty niezależnie wymyślające niezgodne sygnatury tam, gdzie crate'y faktycznie na sobie zależą.
- **Sesje agentowe (Managed Agent Sessions) mogą wygasnąć w trakcie tury.** Długa, nieprzerywana sesja budująca wszystko naraz jest krucha — plan potrzebuje naturalnych punktów kontrolnych (commit po każdym ukończonym kawałku).

## Oś dekompozycji: graf zależności crate'ów, nie kalendarz

[docs/02-crates.md](02-crates.md) już podaje dokładny graf zależności między crate'ami — to jest właściwa oś sekwencjonowania, nie tydzień/weekend:

| Faza | Crate'y | Tryb | Thinking level | Powód |
|---|---|---|---|---|
| 0 | `smartfs-schema` + migracje 001→006 po kolei | sekwencyjnie, jeden agent | **high** | Kręgosłup — wszystko inne to importuje; błąd tu nie jest lokalny |
| 1 | `smartfs-store`, potem `smartfs-compress`, potem `smartfs-ipfs` | **sekwencyjnie** (store najpierw, compress po store — compress importuje ze store) | medium | `smartfs-compress` importuje `smartfs-store`; równoległość store/compress byłaby błędem (compress nie skompiluje się bez gotowego store API). `smartfs-ipfs` może iść po compress. |
| 2 | `smartfs-db`, potem `smartfs-ai` (**tylko ścieżka CPU/ONNX Runtime, ADR-49** — patrz Faza 7) | sekwencyjnie | **high** dla fragmentów dotykających FIX-01..10 i migracji 005/006; medium reszta | `smartfs-ai` zależy od `smartfs-db`+`smartfs-store`; to tu już raz złapano prawdziwy bug (plugin_type) |
| 3 | `smartfs-semantic`, `smartfs-fuse` | równolegle (2 subagenty) | medium | Oba zależą tylko od db+schema, nie od siebie nawzajem (graf w 02-crates.md). Uwaga: `smartfs-fuse` zależy od `smartfs-db`, **nie** od `smartfs-ai` — może biec równolegle z `smartfs-ai` (Faza 2b), nie blokuje go. |
| 4 | `smartfs-mcp`, potem `smartfs-cli` | sekwencyjnie | medium | Zależą od prawie wszystkiego powyżej |
| 5 | `smartfs-docgen` — **BUDOWANY** w tej fazie | niezależnie, może iść równolegle z fazami 1-4 od początku | medium | Dev-tool skanujący pliki jako tekst — nie linkuje się z resztą crate'ów. Faza 5 = build + unit testy `smartfs-docgen`. |
| 6 | Integracja | sekwencyjnie, jeden agent | **high** | `cargo check --workspace`, `clippy --workspace`, backfill `@id` przez `smartfs-docgen` (**WERYFIKACJA** — `smartfs-docgen check`, nie build), weryfikacja `docs/symbol_registry.json`, testy per crate |
| 7 | **Post-MVP, opcjonalna** — ścieżka GPU w `smartfs-ai` (bindingi FFI do `ggml`/`llama.cpp` zbudowanego wyłącznie z backendem Vulkan, ADR-55) | sekwencyjnie, jeden agent, **dopiero po zakończeniu Fazy 6** | high | Osobny silnik inferencji, zależność zewnętrzna (C++ przez FFI) — nigdy nie blokuje pierwszego działającego demona na CPU (ADR-55, Konsekwencje) |

## Zasady dla każdego subagenta (żeby nie kolidowały)

- Jeden subagent = jeden katalog `crates/<nazwa>/`. Zero współdzielonych plików z innym równoległym subagentem w tej samej fazie.
- Maksymalna głębokość rekursji subagentów: **1** (menedżer → subagent per crate; subagent nie spawnuje dalszych subagentów). Graf zależności już daje całą potrzebną dekompozycję — głębsza rekursja to tylko ryzyko przekroczenia budżetu bez korzyści.
- Każdy subagent dostaje **wyłącznie**: plik dokumentacji swojego crate'a (`docs/crates/<crate>.md` albo odpowiednią sekcję §3.x [`docs/base-v4.5-v5.0/SmartFS_Architecture_v4_5.md`](base-v4.5-v5.0/SmartFS_Architecture_v4_5.md) dla crate'ów "bez zmian", wskazaną w [`docs/crates/_unchanged.md`](crates/_unchanged.md)), Root Invariants (README + 01-architecture.md, 8 sztuk), i już scommitowany `smartfs-schema` — nic więcej, żeby nie zgadywał cudzych API.
- Commit po każdym ukończonym crate'cie osobno — naturalny punkt kontrolny, gdyby sesja wygasła w trakcie.
- Komentarze w kodzie: po angielsku, zgodnie z [docs/05](05-code-comments.md). Wiadomości commitów odnoszą się do numeru ADR/invariantu/migracji, nigdy do treści tego promptu ani rozmowy, w której powstał (ta sama zasada nr 2 z docs/05, zastosowana do commit message).

## Prompt (do wklejenia w Antigravity Agent Manager)

```
Jesteś agentem kodującym pracującym w repozytorium SmartFS. Pełna specyfikacja
leży w bieżącym katalogu roboczym. Przeczytaj w tej kolejności, zanim
napiszesz jakikolwiek kod: README.md → docs/00-overview.md →
docs/01-architecture.md (zawiera pełny tekst wszystkich 8 Root Invariants) →
docs/02-crates.md → docs/base-v4.5-v5.0/SmartFS_Architecture_v4_5.md (pełna
architektura bazowa v4.5, na której v6.0 jest przyrostem — cytowana przez
docs/crates/*.md per numer §3.x) → docs/base-v4.5-v5.0/SmartFS_v4.5_to_v5.0_fixes.md
(FIX-01..10) → docs/03-consolidation-design.md → docs/04-uuid-doc-linking.md
→ docs/adr/*.md (w kolejności numerycznej: ADR-49, ADR-50, ADR-51, ADR-52,
ADR-53, ADR-54, ADR-55, ADR-56, ADR-57)
→ docs/05-code-comments.md → docs/06-agentic-execution-plan.md (ten plik —
zawiera fazy i twarde zasady niżej) → migrations/*.sql w kolejności numerycznej
(001 do 006) → docs/crates/*.md (w tym docs/crates/_unchanged.md dla crate'ów
bez zmian merytorycznych).

Zbuduj cały workspace Cargo zgodnie z tą specyfikacją, w kolejności Faza 0 →
Faza 6 opisanej w docs/06-agentic-execution-plan.md.

TWARDE ZASADY:
1. Jeden subagent = jeden katalog crates/<nazwa>/. Nigdy dwóch subagentów nie
   pisze do tego samego katalogu w tej samej fazie.
2. Maksymalna głębokość rekursji subagentów: 1. Nie spawnuj subagenta z
   subagenta.
3. Commit po każdej ukończonej migracji i po każdym ukończonym crate'cie
   osobno — to są punkty kontrolne na wypadek wygaśnięcia sesji.
4. Kod i komentarze w kodzie: po angielsku (docs/05). Wiadomości commitów:
   krótkie, odnoszą się do numeru ADR/invariantu/migracji, nigdy do treści
   tego promptu ani rozmowy, w której powstał.
5. Każda publiczna funkcja/struct/enum/impl dostaje `@id: <uuid v4>` w
   komentarzu dokumentującym (docs/04) — generowane przez smartfs-docgen na
   końcu każdej fazy, nigdy ręcznie wpisywane na sztywno.
6. Root Invariants (README + 01-architecture.md, 8 sztuk) są NIENARUSZALNE.
   Jeśli implementacja invariantu wydaje się niemożliwa albo sprzeczna z
   czymkolwiek innym w specyfikacji — ZATRZYMAJ SIĘ i zgłoś sprzeczność
   zamiast cicho ją obchodzić.
7. `cargo check --workspace` i `cargo clippy --workspace --all-targets` muszą
   przechodzić czysto na końcu KAŻDEJ fazy, nie tylko na końcu całości.
8. Nie zgaduj sygnatur funkcji, których dokumentacja nie podaje. Jeśli
   docs/crates/<crate>.md nie specyfikuje czegoś potrzebnego do kompilacji —
   zatrzymaj się i zapytaj, zamiast wymyślać.
9. Faza 7 (ścieżka GPU/Vulkan w smartfs-ai, ADR-55) jest POST-MVP. Nie
   zaczynaj jej, dopóki Fazy 0-6 (ścieżka CPU) nie przechodzą czysto
   cargo check/clippy/testów w całości. GPU jest bonusem szybkości, nigdy
   warunkiem pierwszego działającego demona.

Zacznij od Fazy 0: smartfs-schema, potem migracje 001-006 w kolejności
numerycznej. Po każdej migracji uruchom listę weryfikacyjną z komentarza na
końcu jej pliku SQL, zanim przejdziesz dalej.
```

## Co zostaje "na weekendy" (świadomie odłożone, nie z powodu tempa)

Pozycje z [`docs/base-v4.5-v5.0/SmartFS_Known_Limitations_Roadmap.md`](base-v4.5-v5.0/SmartFS_Known_Limitations_Roadmap.md) oznaczone tam jako post-MVP (io_uring dla FUSE, seccomp sandboxing dla tree-sitter, FastCDC dla plików >RAM) zostają odłożone — ale dlatego, że są jawnie oznaczone jako hardening poza zakresem MVP, nie dlatego, że agentowi "zabrakłoby weekendu". Wszystko, co jest w zakresie v6.0 (Fazy 0-6 powyżej), idzie w jednym ciągłym przebiegu agentowym.
