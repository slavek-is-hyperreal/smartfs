# ADR-57 — Słownik pluginów jako dane, żywy przez MCP (inspiracja Pick/MultiValue)

**Status:** Post-MVP, opt-in. Nie zmienia Faz 0-6, nie wymaga nowej migracji na start. Rozszerza ten sam wątek "ergonomia dla agentów" co [ADR-56](ADR-56-agent-continuity.md).

## Kontekst — co Pick naprawdę robił i dlaczego naprawdę umarł

Pick OS (Dick Pick, przełom lat 60./70.) łączył system operacyjny i bazę danych w jedno: dane trzymane były w plikach haszowanych z dynamicznym podziałem grup przy przepełnieniu, a **słownik opisujący pola pliku (`D-type`) był sam plikiem tego samego rodzaju co dane (`F-type`)** — nie osobną, uprzywilejowaną warstwą metadanych. Rekordy były z natury wielowartościowe (`multivalue`/`subvalue`) — jeden rekord mógł zawierać powtarzającą się grupę atrybutów bez rozbijania jej na osobną tabelę i JOIN-a. To jest dokładnie to, co dziś nazywamy "non-first-normal-form"/document store — tylko 20-30 lat wcześniej niż MongoDB.

Ważna korekta względem intuicji "umarło, bo było za drogie/za trudne algorytmicznie": to nieprawda dla Picka konkretnie. Źródła (Wikipedia, `MultiValue database`/`Pick operating system`) wskazują na przyczyny **biznesowe i ekosystemowe**, nie techniczne: fragmentacja licencyjna (wielu vendorów sprzedawało niekompatybilne "smaki" Picka), spory sądowe o licencje, słaby marketing, i w końcu — standaryzacja Unixa (ANSI, wspólny front wielu producentów) rozjechała rozproszony, nieskoordynowany ekosystem Picka mimo że technicznie Pick był w latach 80. postrzegany jako *silny* konkurent Unixa. Prawdziwe ograniczenie sprzętowe istniało tylko częściowo i wcześnie: pierwsze wersje miały sztywne limity rozmiaru rekordu, co pchnęło późniejsze wdrożenia w stronę B-tree obok haszowania — czyli tam, gdzie w końcu wylądowała też reszta branży.

To wzmacnia Twoją intuicję, tylko z innego powodu niż "zbyt kosztowne": model danych (słownik jako dane, rekord wielowartościowy bez JOIN-a) nie przegrał merytorycznie — przegrał firmowo. Sam pomysł żyje dziś jako JSONB/tablice w Postgresie i jako bazy dokumentowe — my już go używamy (`special_data JSONB`), tylko nie nazwaliśmy go po imieniu.

## Obserwacja: SmartFS już ma połowę słownika Picka, tylko nie jest on danymi

**Korekta pochodzenia (bezpośrednio od autora projektu):** system pluginów nie jest przypadkową zbieżnością z Pickiem odkrytą post factum — powstał wprost po obejrzeniu materiału o Pick OS (kanał Asianometry) i został świadomie zaprojektowany na wzór jego słowników. Nie istniał we wcześniejszych wersjach specyfikacji (przed v3.0). To, czego brakowało do tej pory, to nie inspiracja, tylko jej dokończenie: sam słownik (`plugins/*.json`) już jest Pickowy, ale nigdy nie stał się odpytywalny w runtime tym samym kanałem co dane — dokładnie tę drugą połowę dokłada ten ADR.

`plugins/<type>.json` (v4.5 §10) jest strukturą dokładnie w duchu `D-type`:

```json
{
  "type": "png",
  "description": "PNG image file containing raster graphics data...",
  "schema": {
    "width": { "type": "integer", "description": "Image width in pixels." },
    "text_metadata": { "type": "object", "description": "Key-value pairs from PNG tEXt chunks..." }
  },
  "ast": false,
  "embedding": null
}
```

Pole → typ → opis, jeden plik JSON per typ. To jest już dictionary-as-config. Czego brakuje względem Picka: ten słownik jest widoczny tylko demonowi przy starcie (czyta pliki z dysku) — nie jest odpytywalny w runtime tym samym kanałem, którym odpytywane są dane (MCP). Agent, który chce wiedzieć "jakie pola ma plik typu `rust`", dziś musi przeczytać `plugins/rust.json` jako plik tekstowy poza SmartFS, zamiast zapytać sam system. To jest dokładnie ta sama klasa problemu co brak `@id` przed ADR-52 — metadana istnieje, ale nie jest adresowalna tym samym mechanizmem co reszta.

## Decyzja

**1. Dwa nowe narzędzia MCP, czysto do odczytu, zero nowej migracji:**

```
describe_plugin_type(type: String) -> PluginSchema
  → wprost zawartość plugins/<type>.json (description, schema, ast, embedding, match_extensions)

list_plugin_types() -> Vec<PluginSummary>
  → { type, description, match_extensions } dla każdego zarejestrowanego pluginu
```

Agent pyta system o kształt danych tym samym kanałem, którym pyta o same dane — zero potrzeby czytania plików konfiguracyjnych poza SmartFS, zero zgadywania nazw pól ze `special_data` po samych wartościach.

**2. Otwarte pytanie (świadomie nierozstrzygnięte, nie zgadujemy zakresu z wyprzedzeniem):** czy pójść krok dalej i przenieść same definicje pluginów *do* SmartFS jako zwykłe wersjonowane pliki (`special_type = "plugin_schema"`), zamiast trzymać je poza CoW jako statyczny config na dysku. To byłoby wierne Pickowi w 100% — słownik jest plikiem jak każdy inny, ma historię wersji, content_hash, można go `smartfs-cli cat`ować i `history`ować jak każdy inny plik. Nie robimy tego teraz — wymagałoby to rozstrzygnięcia, jak demon ładuje konfigurację pluginów przy starcie z bazy zamiast z dysku (kolejność bootstrapu), co jest realną zmianą, nie tylko dodaniem narzędzia MCP. Zostawiamy to jako naturalny "Otwarte pytanie" tego ADR, do podjęcia dopiero jeśli/gdy realny agent faktycznie potrzebuje edytować schemat pluginu w runtime, a nie tylko go odczytać.

## Dlaczego nie teraz (Faza 0-6)

Jak ADR-56 i ADR-55: dwa narzędzia tylko-do-odczytu nie dotykają żadnej migracji ani żadnego z 11 crate'ów wymaganych do pierwszego działającego demona. Wymagają jedynie, żeby `smartfs-mcp` umiał odczytać `plugins/*.json` (i tak czyta je demon przy starcie) i zserializować do JSON-RPC — praca rzędu jednego popołudnia po Fazie 6, nie coś co powinno blokować Antigravity teraz.

## Konsekwencje

- Zero zmian w Fazach 0-6. Zero nowej migracji.
- Agent (i człowiek przez `smartfs-cli`, gdyby dodać tam cienką komendę `describe <type>`) odkrywa kształt danych przez ten sam interfejs co dane, zamiast przez pliki konfiguracyjne poza systemem — mniejsze ryzyko, że model zgadnie nieistniejące pole `special_data` z dużą pewnością siebie.
- Nazywamy po imieniu wzorzec, który SmartFS już nieświadomie stosuje (`plugins/*.json` jako D-type) — żadna nowa koncepcja danych, tylko nowa droga dostępu do istniejącej.
- Pełne przeniesienie definicji pluginów do samego SmartFS (Otwarte pytanie wyżej) zostaje świadomie odłożone — nie dlatego, że jest za drogie, tylko dlatego, że nikt jeszcze nie potrzebuje edytować schematu w runtime.

Sources: [Pick operating system — Wikipedia](https://en.wikipedia.org/wiki/Pick_operating_system), [MultiValue database — Wikipedia](https://en.wikipedia.org/wiki/MultiValue_database)
