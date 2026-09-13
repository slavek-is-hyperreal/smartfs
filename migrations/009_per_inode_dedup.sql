-- ─────────────────────────────────────────────────────────────────
-- migrations/009_per_inode_dedup.sql
-- Wymaga: 001_core_schema.sql
--
-- Patrz: docs/adr/ADR-62-per-inode-dedup-and-lazy-reclaim.md (faza A)
--
-- Dedup przełączany per plik. Dwie zmiany, obie wynikające z jednej zasady:
-- KAŻDY fizyczny blob zachowuje wiersz w inwentarzu, a warunkowe jest
-- wyłącznie jego uczestnictwo w dedupie.
--
-- 1. inode_registry.dedup_enabled — trzecie rodzeństwo obok compression_level
--    i versioning_enabled, ustawiane przez wtyczkę typu pliku (ADR-60).
--
-- 2. blobs: klucz główny przenosi się z content_hash na blob_id, dochodzi
--    `shared`, a unikalność treści staje się indeksem CZĘŚCIOWYM obejmującym
--    tylko bloby współdzielone.
--
--    Dlaczego nie zostawić content_hash jako klucza: blob prywatny i
--    współdzielony o identycznej treści muszą móc współistnieć, a pod kluczem
--    głównym na content_hash nie mogą. Pierwsza wersja ADR-62 rozwiązywała to
--    przez nieprzyznawanie prywatnym wiersza w ogóle — i to było gorsze,
--    bo `check_invariant_1_crypto` (etap 4) oraz scrub sum kontrolnych robią
--    JOIN blobs i po cichu przestałyby weryfikować bloby prywatne. Sprawdzian,
--    który cicho przestaje sprawdzać, jest gorszy niż jego brak.
--
--    Semantyka dedupu z FIX-01 zostaje bez zmian — `INSERT ... ON CONFLICT
--    (content_hash) WHERE shared DO UPDATE ... RETURNING blob_id, (xmax = 0)`
--    działa na indeksie częściowym tak samo jak działał na kluczu głównym.
--    Zweryfikowane empirycznie na PostgreSQL 16 przed napisaniem tej migracji.
--
-- FIX-01 nadal obowiązuje: nigdzie tu nie ma refcountu i nie może się pojawić.
--
-- Idempotentna: etapy 3 i 4 planu testowego stosują migracje na świeżo
-- odtworzonych bazach przy każdym _scratch_mkfs.
-- ─────────────────────────────────────────────────────────────────

ALTER TABLE inode_registry
    ADD COLUMN IF NOT EXISTS dedup_enabled BOOLEAN NOT NULL DEFAULT TRUE;

COMMENT ON COLUMN inode_registry.dedup_enabled IS
    'Czy nowe wersje tego pliku wchodzą do indeksu dedupu. Domyślnie TRUE, bo '
    'dedup jest dzisiejszym zachowaniem i konstytutywny dla magazynu '
    'adresowanego treścią. FALSE daje gwarancję natychmiastowego zwolnienia '
    'miejsca przy unlink. Dotyczy wyłącznie wersji zapisanych PO zmianie: to, '
    'czy kasowanie zwolni miejsce, jest własnością bloba (blobs.shared), nie '
    'tej flagi (ADR-62 §Rozstrzygnięcia #2).';

ALTER TABLE blobs
    ADD COLUMN IF NOT EXISTS shared BOOLEAN NOT NULL DEFAULT TRUE;

COMMENT ON COLUMN blobs.shared IS
    'Czy blob uczestniczy w dedupie. Blob prywatny (FALSE) należy do dokładnie '
    'jednej wersji jednego inode-a i NIGDY nie jest celem ani źródłem dedupu — '
    'bez tej reguły gwarancja szybkiego kasowania wyparowałaby po cichu '
    '(ADR-62 §Decyzja punkt 4).';

-- Przeniesienie klucza głównego. Wykonywane warunkowo, żeby migracja dała się
-- puścić dwa razy.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'blobs_pkey'
          AND conrelid = 'blobs'::regclass
          AND (SELECT array_agg(attname::text ORDER BY attname)
               FROM pg_attribute
               WHERE attrelid = 'blobs'::regclass
                 AND attnum = ANY(conkey)) = ARRAY['content_hash']::text[]
    ) THEN
        ALTER TABLE blobs DROP CONSTRAINT blobs_pkey;
        ALTER TABLE blobs ADD CONSTRAINT blobs_pkey PRIMARY KEY (blob_id);
    END IF;
END $$;

-- Indeks dedupu: unikalność treści TYLKO wśród blobów współdzielonych.
CREATE UNIQUE INDEX IF NOT EXISTS blobs_dedup
    ON blobs (content_hash) WHERE shared;

-- Wyszukiwanie po treści (dedup_check, weryfikacja Invariantu #1) obejmuje
-- też bloby prywatne, więc potrzebuje indeksu nieczęściowego.
CREATE INDEX IF NOT EXISTS idx_blobs_content_hash ON blobs (content_hash);
