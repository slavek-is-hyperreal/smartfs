# Crate'y bez zmian merytorycznych w v6.0

← [Mapa crate'ów](../02-crates.md)

Poniższe crate'y dziedziczą swoje CLAUDE.md wprost z [SmartFS_Architecture_v4_5.md](../base-v4.5-v5.0/SmartFS_Architecture_v4_5.md) (sekcja 3.x, numery §3.x przy każdym crate niżej) i z poprawek w [SmartFS_v4.5_to_v5.0_fixes.md](../base-v4.5-v5.0/SmartFS_v4.5_to_v5.0_fixes.md) — nic w v6.0 ich nie dotyka:

- **smartfs-store** (§3.3) — `BlobStore` trait, `LocalDiskStore`, bez zmian. (Uwaga poza zakresem v6.0, ale warto pamiętać przy przyszłej pracy: krytyczna ocena v5.0 wskazała, że `store.exists()` z FIX-03 dodaje round-trip na gorącej ścieżce dedupu dla backendów sieciowych — nie rozwiązywane w tej wersji.)
- **smartfs-compress** (§3.4) — pipeline hash+zstd, bez zmian.
- **smartfs-fuse** (§3.6) — operacje FUSE, bufor RAM (ADR-16), bez zmian. Limit rozmiaru pliku jako zabezpieczenie przed OOM pozostaje otwartym tematem spoza zakresu v6.0.
- **smartfs-ipfs** (§3.9) — `IpfsStore`, CID <256KB, bez zmian.
- **smartfs-cli** (§3.10) — delta kosmetyczna: nowa podkomenda `smartfs-cli concepts [--plugin-type X]`, cienka delegacja do `smartfs_semantic::search_by_concept` bez `query` — przegląd grafu centroidów z terminala. Poza tym bez zmian.

  **§calibrate** — nowa podkomenda `calibrate`:

  ```
  cargo run -p smartfs-cli -- calibrate --plugin-type <type> --model <model-name>
  ```

  Działanie: próbkuje istniejące wektory dla danej kombinacji `(plugin_type, model_id)`, oblicza empiryczny `join_threshold` (percentyl p75 rozkładu dystansów k-NN), i upsertuje wiersz do `consolidation_thresholds`. **Musi być uruchomiona przed** `spawn_all_consolidation_supervisors` — jeśli wiersz w `consolidation_thresholds` nie istnieje, supervisor dla tej kombinacji nie jest uruchamiany (fail-safe).

Jeśli któryś z tych crate'ów wymaga zmiany w przyszłości, odpowiedni plik powinien zostać wydzielony tutaj analogicznie do `smartfs-db.md`/`smartfs-ai.md`/`smartfs-mcp.md` w tym katalogu, nie dopisywany do tego zbiorczego pliku.
