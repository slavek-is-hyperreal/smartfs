# smartfs-ai — delta w v6.0

← [Mapa crate'ów](../02-crates.md) | Baza: CLAUDE.md z v4.5 §3.7 (bez zmian poza tym, co niżej)

## Zmiany konfiguracyjne (ADR-49)

```toml
[embedding]
default_model = "qwen3-embedding-0.6b"      # było: "all-minilm-l6-v2"
accurate_model = "qwen3-embedding-4b"       # opcjonalnie; było BGE-M3
image_model = "qwen3-vl-embedding-2b"       # NOWE — obrazy, opt-in
```

## Zmiana w `## Embeddings generated per file version`

```markdown
- embeddings_1024_qwen z Qwen3-Embedding-0.6B (is_default=TRUE) — zawsze
- embeddings_384/768 z modeli starszych — tylko dla wierszy, które je już
  mają (kompatybilność wstecz); nowe pliki NIE dostają już embeddings_384
  domyślnie, chyba że jawnie skonfigurowano legacy_model w smartfs.toml
- embeddings_1024_qwen_vl z Qwen3-VL-Embedding-2B — tylko dla plików, których
  plugin ma "embedding".model wskazujący na model obrazowy (nowość — png.json
  i podobne przestają mieć "embedding": null)
```

## Nowo wstawiony wiersz przy zapisie embeddingu

Każdy INSERT do tabeli embeddingów ustawia teraz też `consolidated = FALSE` (wartość domyślna kolumny) — tego `smartfs-ai` nie musi robić jawnie, bo to `DEFAULT` w schemacie.

**To, co `smartfs-ai` MUSI robić jawnie (poprawka po review):** wypełnić kolumnę `plugin_type` przy każdym INSERT do `embeddings_384`/`embeddings_768`/`embeddings_1024_qwen`/`ast_embeddings_1536`. Kolumna nie ma wartości domyślnej (patrz [migracja 005](../../migrations/005_semantic_consolidation.sql) §2 — `NOT NULL` bez `DEFAULT`), bo `smartfs-ai` i tak zna `special_type` pliku, który właśnie embeduje (przetwarza wiersz `file_versions`, który to pole ma) — zero dodatkowego zapytania, tylko przekazanie już posiadanej wartości do `INSERT`. Bez tego insert do tabeli embeddingów zwróci błąd `NOT NULL violation`, nie zostanie po cichu pominięty.

```rust
// smartfs-ai, miejsce insertu embeddingu — jedna dodatkowa kolumna względem v5.0
sqlx::query!(
    "INSERT INTO embeddings_1024_qwen (version_id, model_id, plugin_type, embedding)
     VALUES ($1, $2, $3, $4)",
    version_id, model_id, file_version.special_type, embedding_vector
).execute(db).await?;
```

Poza tym `smartfs-ai` nie wie nic więcej o `smartfs-semantic` — nie odczytuje `consolidated`, nie importuje crate'a. To jest świadome: worker embeddingów pozostaje dokładnie tak prosty jak w v5.0, plus jedna kolumna, którą i tak trywialnie zna.

## Nowa odpowiedzialność: `search_text` (ADR-54)

Przy przebiegu generic-embedding (embeddings_384/768/1024_qwen — dotyczy WSZYSTKICH plików, v4.5 §10.3) `smartfs-ai` dodatkowo wypełnia `file_versions.search_text`, przez `smartfs-db::set_search_text` — nigdy bezpośrednim `sqlx`:

- Jeśli content da się zdekodować jako poprawny UTF-8 — dokładnie ten tekst (ten sam, który idzie do modelu embeddingowego, żeby nie było dwóch źródeł prawdy o tym, "co system w ogóle widzi" jako treść pliku).
- Jeśli nie (plik binarny) — konkatenacja wartości string ze schematu wtyczki tego pliku (np. `text_metadata`, `color_type` z `png.json`, v4.5 §10.1), rozdzielonych spacją. Jeśli wtyczka nie ma żadnych pól string w schemacie — `search_text = None` (partial index BM25 z migracji 006 to filtruje).

```rust
smartfs_db::set_search_text(db, version_id, search_text).await?;
```

Patrz [ADR-54](../adr/ADR-54-fulltext-search-backend.md) po pełne uzasadnienie.

## Reszta bez zmian

Supervisor loop, tree-sitter, `claim_pending_to_processing`, anti-starvation valve (ADR-42), retry z exponential backoff — wszystko z v5.0 bez zmian.
