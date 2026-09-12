# ADR-56 — Ciągłość agenta: prowieniencja `actor`/`session` i wznawialność po utracie kontekstu

**Status:** Post-MVP, opt-in. Nie zmienia Faz 0-6 ani promptu dla Antigravity — nie wymaga nowej migracji. Kandydat na Fazę 7b (po Fazie 6, obok/po ADR-55), albo osobny follow-up po pierwszym działającym demonie.

## Kontekst

Cała reszta v6.0 jest projektowana pod dwóch odbiorców: ludzi i modele, które będą tym repo operować (piszą kod, później może odpytują SmartFS jako swoją warstwę pamięci przez MCP). Do tej pory "pod modele" oznaczało głównie: stabilne adresowanie niezależne od pozycji (`@id`, `content_hash`) i jawne "dlaczego" zamiast samego "co" (ADR, docs/05). To trafia w jedną klasę ograniczenia modelu — brak ciągłej, przeżytej historii, przez co trzeba rekonstruować kontekst z zewnętrznych źródeł zamiast pamiętać.

Jest jednak druga klasa, dotąd nieadresowana: **agent nie ma gwarancji, że dokończy zadanie w jednej, ciągłej sesji.** `docs/06-agentic-execution-plan.md` już to nazywa wprost ("Managed Agent Sessions mogą wygasnąć w trakcie tury") i odpowiada na to checkpointami na poziomie commitów Gita. To działa dla *budowy kodu*. Nie działa dla *agenta używającego SmartFS jako pamięci roboczej w runtime* — tam nie ma "commitu" jako naturalnego punktu kontrolnego, a wznowienie po utracie kontekstu dziś oznacza: człowiek ręcznie sklleja podsumowanie i wkleja je z powrotem. To jest dokładnie ten sam problem, co zgubiony kubek na kawę — agent (jak i człowiek z deficytem pamięci roboczej) nie traci zdolności do działania, traci **widoczność tego, co już zrobił**, i bez zewnętrznego śladu odtwarza to na nowo albo robi to dwa razy.

## Decyzja

**1. Prowieniencja zapisu — `special_data.agent` (bez nowej migracji)**

`file_versions.special_data` (JSONB, już indeksowane przez `idx_versions_special` GIN, §6) zyskuje opcjonalną, ale zalecaną konwencję klucza `agent`, wypełnianą przez każdego klienta MCP/CLI, który jest agentem (nie człowiekiem przy klawiaturze):

```json
{
  "agent": {
    "actor_id": "antigravity-session-7f3a",
    "task_id": "faza-2-smartfs-db",
    "why": "implementacja cow_commit wg FIX-04"
  }
}
```

Zero nowych tabel, zero nowej migracji — reuse-before-rewrite, ta sama zasada co przy GPU (ADR-55). Pole jest opcjonalne: zapis bez niego działa dokładnie jak dziś (Invariant #4 nienaruszony — brak nowego `CREATE TABLE`).

**2. Nowe narzędzie MCP: `get_actor_activity`**

```
get_actor_activity(actor_id: String, since: Option<Timestamp>, limit: usize)
  -> Vec<FileVersion>
```

Odpytuje `file_versions` po `special_data->'agent'->>'actor_id'`, sortowane po `created_at`. To jest funkcjonalny odpowiednik tego, o co proszę Cię Ty, kiedy wklejasz mi podsumowanie po ucięciu kontekstu — tylko że agent może zapytać sam, strukturalnie, zamiast czekać na relację człowieka. Świeżo wznowiona sesja pyta "co ja (albo mój poprzednik o tym `actor_id`) już zrobiłem", zamiast rekonstruować to z prozy.

**3. `task_id` jako opcjonalny łącznik przerwanych zadań**

Gdy wiele zapisów niesie ten sam `task_id`, przerwane zadanie jest odnajdywalne wprost: `find_incomplete_task(task_id)` zwraca wszystkie wersje z tym `task_id`, których `status` nie jest jeszcze `clean`. To nie zastępuje ludzkiego review — to daje agentowi (i Tobie) odpowiedź na "gdzie dokładnie stanąłem", zanim zapyta się człowieka.

**4. Nowa zasada dla przyszłych narzędzi MCP: idempotencja jako wymóg, nie przypadek**

`cow_commit` jest już idempotentny przez `content_hash` (dedup w `blobs`) — to jest przypadek szczęśliwy, nie zaprojektowany specjalnie pod agentów. Każde **nowe** narzędzie MCP, które coś zapisuje (np. przyszłe rozszerzenia `smartfs-cli calibrate` albo cokolwiek post-v6.0), musi być bezpieczne do wywołania dwa razy z tym samym inputem bez efektu ubocznego. Powód wprost: agent, który zgubił kontekst, nie wie, czy zdążył wykonać krok, zanim sesja wygasła — musi móc bezpiecznie spróbować ponownie zamiast zgadywać.

## Dlaczego nie teraz (Faza 0-6)

`docs/06-agentic-execution-plan.md` i prompt dla Antigravity są już zweryfikowane i gotowe do uruchomienia. To ADR nie dotyka żadnej migracji 001-006 ani żadnego z 11 crate'ów w zakresie MVP — dopisanie go teraz do Twardych Zasad tylko zwiększyłoby ryzyko zatrzymania na Zasadzie 8 (agent pytający o coś, co i tak nie jest wymagane do pierwszego działającego demona). Tak samo jak GPU/Vulkan (ADR-55) jest bonusem szybkości nigdy warunkiem startu, tak to jest bonusem ciągłości, nigdy warunkiem MVP.

## Otwarte pytanie

Czy `actor_id` powinien być też polem na `concept_centroids_*`/`consolidation_thresholds` (żeby agent kalibrujący progi też był odnajdywalny) — odłożone do czasu, aż realny agent faktycznie zacznie pisać do tych tabel w sposób, który wymaga takiej widoczności. Nie zgadujemy zakresu z wyprzedzeniem.

## Konsekwencje

- Zero zmian w Fazach 0-6 i w prompcie dla Antigravity.
- Zero nowej migracji na start — `special_data` już istnieje i jest indeksowane.
- Jedno nowe narzędzie MCP (`get_actor_activity`) i jedna nowa konwencja (`agent`/`task_id` w JSONB) do dopisania, gdy projekt dojdzie do punktu, w którym agent faktycznie używa SmartFS jako własnej pamięci roboczej w runtime, a nie tylko jako celu budowy kodu.
