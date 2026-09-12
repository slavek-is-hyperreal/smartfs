# smartfs-schema — delta w v6.0

← [Mapa crate'ów](../02-crates.md) | Baza: CLAUDE.md z v4.5 §3.2 (bez zmian poza rozszerzeniem `SmartFsError`)

Jedyna zmiana: `SmartFsError` (jedyny, współdzielony typ błędu wszystkich crate'ów — Root Invariant) zyskuje dwa warianty potrzebne przez `smartfs-semantic` (patrz [docs/03-consolidation-design.md](../03-consolidation-design.md) §9 i [ADR-53](../adr/ADR-53-consolidation-concurrency.md)):

```rust
pub enum SmartFsError {
    // ...warianty istniejące z v4.5, bez zmian...

    /// Brak wiersza w consolidation_thresholds dla (plugin_type, model_id) —
    /// supervisor dla tej kombinacji świadomie się nie odpala, dopóki
    /// smartfs-cli calibrate nie zostanie uruchomione.
    MissingCalibration { plugin_type: String, model_id: Uuid },

    /// pg_try_advisory_lock nie przyznał blokady — inna instancja już
    /// pracuje nad tą samą kombinacją. NIE jest logowane jako error na
    /// poziomie wywołującym (to normalny, oczekiwany stan pod współbieżnością),
    /// tylko jako informacja o pominięciu cyklu.
    AdvisoryLockUnavailable,
}
```

Zgodnie z zasadą "no logic" dla tego crate'a — same warianty enuma, żadnej logiki obsługi błędu (ta żyje w `smartfs-semantic`). Reszta crate'a (typy `Uuid`, `ContentHash`, `BlobId` itd.) bez zmian.
