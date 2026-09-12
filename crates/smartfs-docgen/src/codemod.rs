//! Idempotent codemod for backfilling persistent UUIDs into Rust source doc comments.

use std::fs;
use std::path::Path;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::error::{Error, Result};
use crate::parser::parse_items;

/// @id: 665badd5-a15d-4324-ad0f-09e9db55e1d1
/// Walks `.rs` files under `crates_path` and inserts a fresh `/// @id: <uuid>` doc comment
/// above any public item or impl block currently lacking an `@id:`.
///
/// Preserves existing indentation and doc comment blocks. This operation is strictly
/// idempotent: symbols that already possess an `@id:` are never modified.
///
/// # Errors
/// Returns `Error::Io` if reading or writing any source file fails.
pub fn backfill_missing_ids(crates_path: &Path) -> Result<usize> {
    let mut total_inserted = 0;

    for entry in WalkDir::new(crates_path).sort_by_file_name() {
        let entry = entry.map_err(|e| Error::IoGeneric(e.into()))?;
        let path = entry.path();

        if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            let content = fs::read_to_string(path).map_err(|e| Error::Io {
                path: path.to_path_buf(),
                source: e,
            })?;

            let items = parse_items(&content);
            let unannotated: Vec<_> = items.into_iter().filter(|it| it.id.is_none()).collect();

            if unannotated.is_empty() {
                continue;
            }

            let mut file_lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
            let has_trailing_newline = content.ends_with('\n');

            // Collect insertion points and sort in descending line order to preserve line offsets
            let mut insertions = Vec::new();
            for item in unannotated {
                let insert_idx = item.doc_start_line.unwrap_or(item.attr_start_line);
                let new_uuid = Uuid::new_v4();
                let line_text = format!("{}/// @id: {new_uuid}", item.indent);
                insertions.push((insert_idx, line_text));
            }

            insertions.sort_by_key(|a| std::cmp::Reverse(a.0));

            for (idx, line_text) in insertions {
                if idx <= file_lines.len() {
                    file_lines.insert(idx, line_text);
                    total_inserted += 1;
                }
            }

            let mut updated_content = file_lines.join("\n");
            if has_trailing_newline {
                updated_content.push('\n');
            }

            fs::write(path, updated_content).map_err(|e| Error::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
        }
    }

    Ok(total_inserted)
}
