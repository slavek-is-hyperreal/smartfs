# Crate'y bez zmian merytorycznych w v6.0

← [Mapa crate'ów](../02-crates.md)

Poniższe crate'y dziedziczą swoje CLAUDE.md wprost z [SmartFS_Architecture_v4_5.md](../base-v4.5-v5.0/SmartFS_Architecture_v4_5.md) (sekcja 3.x, numery §3.x przy każdym crate niżej) i z poprawek w [SmartFS_v4.5_to_v5.0_fixes.md](../base-v4.5-v5.0/SmartFS_v4.5_to_v5.0_fixes.md) — nic w v6.0 ich nie dotyka:

- **smartfs-compress** (§3.4) — pipeline hash+zstd, bez zmian.
- **smartfs-ipfs** (§3.9) — `IpfsStore`, CID <256KB, bez zmian.
- **smartfs-cli** (§3.10) — delta kosmetyczna: nowa podkomenda `smartfs-cli concepts [--plugin-type X]`, cienka delegacja do `smartfs_semantic::search_by_concept` bez `query` — przegląd grafu centroidów z terminala. Poza tym bez zmian.

  **§calibrate** — nowa podkomenda `calibrate`:

  ```
  cargo run -p smartfs-cli -- calibrate --plugin-type <type> --model <model-name>
  ```

  Działanie: próbkuje istniejące wektory dla danej kombinacji `(plugin_type, model_id)`, oblicza empiryczny `join_threshold` (percentyl p75 rozkładu dystansów k-NN), i upsertuje wiersz do `consolidation_thresholds`. **Musi być uruchomiona przed** `spawn_all_consolidation_supervisors` — jeśli wiersz w `consolidation_thresholds` nie istnieje, supervisor dla tej kombinacji nie jest uruchamiany (fail-safe).

**Wypisane z tej listy przez [ADR-58](../adr/ADR-58-two-stage-cow-commit.md):** `smartfs-store` (§3.3) i `smartfs-fuse` (§3.6) mają teraz własne pliki — [smartfs-store.md](smartfs-store.md) i [smartfs-fuse.md](smartfs-fuse.md). Dwuetapowy `cow_commit` dokłada pierwszemu kolejkę `pending/` i scrub sum kontrolnych, a drugiemu przepisuje `release()`. Uwaga o round-tripie `store.exists()` z FIX-03 przeniosła się do `smartfs-store.md` i nadal jest poza zakresem; limit rozmiaru pliku jako zabezpieczenie przed OOM w `smartfs-fuse` też pozostaje otwarty.

Jeśli któryś z pozostałych crate'ów wymaga zmiany w przyszłości, odpowiedni plik powinien zostać wydzielony tutaj analogicznie do `smartfs-db.md`/`smartfs-ai.md`/`smartfs-mcp.md` w tym katalogu, nie dopisywany do tego zbiorczego pliku.
