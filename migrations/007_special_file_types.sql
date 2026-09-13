-- ─────────────────────────────────────────────────────────────────
-- migrations/007_special_file_types.sql
-- Wymaga: 001_core_schema.sql
--
-- Patrz: docs/adr/ADR-59-posix-special-file-types.md
--
-- Dodaje `rdev` do inode_registry — jedyna zmiana schematu potrzebna do
-- obsługi pełnego zestawu typów POSIX (FIFO, gniazda, węzły urządzeń).
--
-- Typ pliku NIE dostaje własnej kolumny. `mode` już dziś trzyma pełny mode
-- z bitami S_IFMT — root inode jest zasiany jako 16877 = 0o40755 — a POSIX
-- definiuje typ właśnie jako bity w mode. Osobna kolumna byłaby drugim
-- źródłem prawdy o tym samym fakcie (ADR-59 §Decyzja punkt 2). Błąd, który
-- naprawiamy, jest w kodzie: `inode_create` i `setattr` maskowały mode do
-- 0o7777 i gubiły typ.
--
-- Idempotentna: etapy 3 i 4 planu testowego stosują migracje na świeżo
-- odtworzonych bazach przy każdym _scratch_mkfs.
-- ─────────────────────────────────────────────────────────────────

-- BIGINT, nie INT: dev_t w Linuksie jest 64-bitowe, a makedev() przy dużym
-- numerze pobocznym nie mieści się w 32 bitach.
ALTER TABLE inode_registry
    ADD COLUMN IF NOT EXISTS rdev BIGINT NOT NULL DEFAULT 0;

COMMENT ON COLUMN inode_registry.rdev IS
    'Numer urządzenia dla S_IFCHR/S_IFBLK; 0 dla wszystkich pozostałych typów (ADR-59).';

COMMENT ON COLUMN inode_registry.mode IS
    'Pełny mode POSIX: bity typu S_IFMT OR uprawnienia. Wiersze sprzed migracji 007 '
    'mogą mieć zamaskowane bity typu — czytający wraca wtedy do is_dir (ADR-59 punkt 7).';
