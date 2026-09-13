-- ─────────────────────────────────────────────────────────────────
-- migrations/008_posix_timestamps.sql
-- Wymaga: 001_core_schema.sql
--
-- Patrz: docs/adr/ADR-61-posix-timestamps-and-atime-policy.md
--
-- Rozdziela znaczniki czasu POSIX, które dotąd były zwinięte w jedną kolumnę.
-- inode_to_file_attr podawał `updated_at` jednocześnie jako atime, mtime i
-- ctime, a setattr ignorował swoje argumenty czasowe — utimensat był cichym
-- no-opem, co jest gorsze niż jawny brak wsparcia.
--
-- Dodajemy TYLKO atime i mtime. ctime zostaje jako updated_at, a crtime jako
-- created_at, i to nie jest oszczędność miejsca: POSIX definiuje ctime jako
-- czas ostatniej zmiany metadanych inode'a, czyli dokładnie to, czym
-- updated_at już jest. Osobna kolumna byłaby duplikatem, który może się
-- rozjechać (ADR-61 §Decyzja punkt 2).
--
-- Wiersze sprzed tej migracji dostają NOW(): czasów, których nikt nigdy nie
-- zapisał, nie da się odtworzyć, a NOW() jest jedyną nieszkodliwą odpowiedzią.
--
-- Idempotentna: etapy 3 i 4 planu testowego stosują migracje na świeżo
-- odtworzonych bazach przy każdym _scratch_mkfs.
-- ─────────────────────────────────────────────────────────────────

ALTER TABLE inode_registry
    ADD COLUMN IF NOT EXISTS atime TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW();

ALTER TABLE inode_registry
    ADD COLUMN IF NOT EXISTS mtime TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW();

COMMENT ON COLUMN inode_registry.atime IS
    'Czas ostatniego dostępu. Aktualizowany w trybie relatime, nie strictatime: '
    'w SmartFS metadane leżą w Postgresie, więc ścisły atime zamieniłby każdy '
    'odczyt w zapis do bazy (ADR-61 punkt 3).';

COMMENT ON COLUMN inode_registry.mtime IS
    'Czas ostatniej zmiany TREŚCI. Zmiana samych metadanych rusza updated_at '
    '(czyli ctime), nigdy mtime (ADR-61 punkt 5).';

COMMENT ON COLUMN inode_registry.updated_at IS
    'Pełni rolę POSIX-owego ctime: czas ostatniej zmiany metadanych inode-a. '
    'Niemodyfikowalny z zewnątrz — POSIX zabrania ustawiania ctime przez '
    'utimensat, co wychodzi za darmo z tego, że nikt go nie przyjmuje jako '
    'argumentu (ADR-61 punkt 2).';
