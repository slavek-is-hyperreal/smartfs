//! Directory tree scanning and registry generation for SmartFS crates.

use std::fs;
use std::path::Path;
use chrono::Utc;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::error::{Error, Result};
use crate::parser::parse_items;
use crate::types::{SymbolRecord, SymbolRegistry};

/// @id: ffb2c1c3-4dc8-4dd4-9e01-f2d43d441405
/// Scans the `src/` directory of a specific crate and extracts addressable Rust symbols.
///
/// When `include_unannotated` is `false`, only items possessing a valid `/// @id: <uuid>`
/// are returned. When `true`, unannotated public items are also included with `Uuid::nil()`.
///
/// # Errors
/// Returns `Error::Io` if reading files or walking directories fails.
pub fn scan_crate(crate_path: &Path, include_unannotated: bool) -> Result<Vec<SymbolRecord>> {
    let crate_name = extract_crate_name(crate_path);
    let src_dir = crate_path.join("src");
    let search_dir = if src_dir.is_dir() { &src_dir } else { crate_path };

    let mut records = Vec::new();

    for entry in WalkDir::new(search_dir).sort_by_file_name() {
        let entry = entry.map_err(|e| Error::IoGeneric(e.into()))?;
        let path = entry.path();

        if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            let content = fs::read_to_string(path).map_err(|e| Error::Io {
                path: path.to_path_buf(),
                source: e,
            })?;

            let parsed_items = parse_items(&content);
            let last_known_file = normalize_file_path(path);

            for item in parsed_items {
                match item.id {
                    Some(id) => {
                        records.push(SymbolRecord {
                            id,
                            crate_name: crate_name.clone(),
                            kind: item.kind,
                            name: item.name,
                            last_known_file: last_known_file.clone(),
                            last_known_line: item.line,
                            doc_summary: item.doc_summary,
                            tombstoned: false,
                            tombstoned_reason: None,
                        });
                    }
                    None if include_unannotated => {
                        records.push(SymbolRecord {
                            id: Uuid::nil(),
                            crate_name: crate_name.clone(),
                            kind: item.kind,
                            name: item.name,
                            last_known_file: last_known_file.clone(),
                            last_known_line: item.line,
                            doc_summary: item.doc_summary,
                            tombstoned: false,
                            tombstoned_reason: None,
                        });
                    }
                    None => {}
                }
            }
        }
    }

    Ok(records)
}

/// @id: a985e25a-dd02-4047-bed2-7b09e1ff512a
/// Scans all crates located under the specified crates directory (e.g. `./crates`).
///
/// # Errors
/// Returns `Error::Io` if traversing directories or reading source files fails.
pub fn scan_all_crates(crates_dir: &Path, include_unannotated: bool) -> Result<Vec<SymbolRecord>> {
    let mut all_symbols = Vec::new();

    // If the provided path itself has a Cargo.toml or src/, scan it as a single crate
    if crates_dir.join("Cargo.toml").is_file() || crates_dir.join("src").is_dir() {
        return scan_crate(crates_dir, include_unannotated);
    }

    if !crates_dir.is_dir() {
        return Err(Error::InvalidPath(format!(
            "Crates directory does not exist: {}",
            crates_dir.display()
        )));
    }

    let mut entries: Vec<_> = fs::read_dir(crates_dir)
        .map_err(|e| Error::Io {
            path: crates_dir.to_path_buf(),
            source: e,
        })?
        .filter_map(std::result::Result::ok)
        .collect();

    entries.sort_by_key(|e| e.path());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() && (path.join("Cargo.toml").is_file() || path.join("src").is_dir()) {
            let symbols = scan_crate(&path, include_unannotated)?;
            all_symbols.extend(symbols);
        }
    }

    Ok(all_symbols)
}

/// @id: 354592d6-b01a-4eef-96da-11b1847349c4
/// Scans all crates under `crates_dir`, stably sorts symbols, and produces `SymbolRegistry`.
///
/// Stable sorting order: `(crate_name, name, id)`.
///
/// # Errors
/// Returns `Error::Io` on filesystem traversal or file read failures.
pub fn generate_registry(crates_dir: &Path) -> Result<SymbolRegistry> {
    let mut symbols = scan_all_crates(crates_dir, false)?;

    symbols.sort_by(|a, b| {
        a.crate_name
            .cmp(&b.crate_name)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });

    Ok(SymbolRegistry {
        generated_at: Utc::now(),
        generator_version: env!("CARGO_PKG_VERSION").to_string(),
        symbols,
    })
}

/// @id: 225106f2-1b2c-4316-90a1-2bf0c8e5428d
/// Normalizes a path into a canonical relative string starting with `crates/` where applicable.
pub fn normalize_file_path(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix("./").unwrap_or(&s);
    if let Some(pos) = s.find("crates/") {
        s[pos..].to_string()
    } else {
        s.to_string()
    }
}

/// Discovers the crate name from its `Cargo.toml` if available, falling back to directory name.
fn extract_crate_name(crate_path: &Path) -> String {
    let cargo_toml = crate_path.join("Cargo.toml");
    if cargo_toml.is_file() {
        if let Ok(content) = fs::read_to_string(&cargo_toml) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("name =") {
                    let parts: Vec<&str> = trimmed.split('=').collect();
                    if parts.len() == 2 {
                        let name = parts[1].trim().trim_matches('"').trim();
                        if !name.is_empty() {
                            return name.to_string();
                        }
                    }
                }
            }
        }
    }

    crate_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}
