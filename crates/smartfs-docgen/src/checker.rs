//! Verification, link resolution, and consistency checking for SmartFS documentation.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use regex::Regex;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::error::{Error, Result};
use crate::parser::parse_items;
use crate::scanner::normalize_file_path;
use crate::types::{ConsistencyIssue, ResolvedLocation, SymbolRegistry};

/// @id: a1364823-1c3b-46f5-9142-a9c8c19ab040
/// Marks a symbol as tombstoned in the registry with a reason.
pub fn tombstone_symbol(registry: &mut SymbolRegistry, id: Uuid, reason: &str) {
    if let Some(record) = registry.symbols.iter_mut().find(|s| s.id == id) {
        record.tombstoned = true;
        record.tombstoned_reason = Some(reason.to_string());
    }
}

/// @id: 67b36dbb-ff19-44d7-a7b8-34d953925fd2
/// Resolves a `symbol://<uuid>` link, re-verifying the actual location on disk
/// rather than trusting cached lines unconditionally.
///
/// # Errors
/// Returns `Error::UnknownSymbol` if the UUID is not in the registry, or
/// `Error::SymbolNotFoundInCode` if an active symbol cannot be found on disk.
pub fn resolve_symbol_link(
    registry: &SymbolRegistry,
    id: Uuid,
    crates_root: &Path,
) -> Result<ResolvedLocation> {
    let record = registry.get(id).ok_or(Error::UnknownSymbol(id))?;

    if record.tombstoned {
        return Ok(ResolvedLocation::Tombstoned {
            removed_summary: record.doc_summary.clone(),
        });
    }

    // 1. Try to verify last_known_file first
    let mut candidate_paths = Vec::new();
    candidate_paths.push(crates_root.join(&record.last_known_file));

    let stripped = record
        .last_known_file
        .strip_prefix("crates/")
        .unwrap_or(&record.last_known_file);
    candidate_paths.push(crates_root.join(stripped));

    if let Some(parent) = crates_root.parent() {
        candidate_paths.push(parent.join(&record.last_known_file));
    }
    candidate_paths.push(PathBuf::from(&record.last_known_file));

    for cand in candidate_paths {
        if cand.is_file() {
            if let Ok(content) = fs::read_to_string(&cand) {
                let items = parse_items(&content);
                if let Some(item) = items.into_iter().find(|it| it.id == Some(id)) {
                    return Ok(ResolvedLocation::Found {
                        file: cand,
                        line: item.line,
                    });
                }
            }
        }
    }

    // 2. Scan entire crates tree if not found in last_known_file
    for entry in WalkDir::new(crates_root).sort_by_file_name() {
        let entry = entry.map_err(|e| Error::IoGeneric(e.into()))?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            if let Ok(content) = fs::read_to_string(path) {
                let items = parse_items(&content);
                if let Some(item) = items.into_iter().find(|it| it.id == Some(id)) {
                    return Ok(ResolvedLocation::Found {
                        file: path.to_path_buf(),
                        line: item.line,
                    });
                }
            }
        }
    }

    Err(Error::SymbolNotFoundInCode(id))
}

/// @id: fa7f26b6-ff11-41da-9edf-0af3d1add7cf
/// CI consistency check: verifies complete ID coverage, detects duplicates, dead doc links, and cache drift.
///
/// # Errors
/// Returns `Error::Io` on filesystem or read errors.
pub fn check_registry_consistency(
    crates_root: &Path,
    docs_root: &Path,
) -> Result<Vec<ConsistencyIssue>> {
    let mut issues = Vec::new();
    let mut id_locations: HashMap<Uuid, Vec<(PathBuf, u32)>> = HashMap::new();

    // 1. Scan crates for MissingId and collect all code locations
    for entry in WalkDir::new(crates_root).sort_by_file_name() {
        let entry = entry.map_err(|e| Error::IoGeneric(e.into()))?;
        let path = entry.path();

        if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            let content = fs::read_to_string(path).map_err(|e| Error::Io {
                path: path.to_path_buf(),
                source: e,
            })?;

            let items = parse_items(&content);
            for item in items {
                match item.id {
                    Some(id) => {
                        id_locations
                            .entry(id)
                            .or_default()
                            .push((path.to_path_buf(), item.line));
                    }
                    None => {
                        issues.push(ConsistencyIssue::MissingId {
                            file: path.to_path_buf(),
                            line: item.line,
                            symbol_name: item.name,
                        });
                    }
                }
            }
        }
    }

    // 2. Check for duplicate IDs
    for (id, locations) in &id_locations {
        if locations.len() > 1 {
            issues.push(ConsistencyIssue::DuplicateId {
                id: *id,
                locations: locations.clone(),
            });
        }
    }

    // 3. Check for StaleCache if a symbol registry file exists
    let registry_opt = find_and_load_registry(docs_root, crates_root);

    if let Some(ref registry) = registry_opt {
        for symbol in &registry.symbols {
            if !symbol.tombstoned {
                let expected_file = PathBuf::from(&symbol.last_known_file);
                let expected_line = symbol.last_known_line;

                if let Some(locations) = id_locations.get(&symbol.id) {
                    if locations.len() == 1 {
                        let (actual_file, actual_line) = &locations[0];
                        let exp_norm = normalize_file_path(&expected_file);
                        let act_norm = normalize_file_path(actual_file);

                        if exp_norm != act_norm || expected_line != *actual_line {
                            issues.push(ConsistencyIssue::StaleCache {
                                id: symbol.id,
                                expected: (expected_file, expected_line),
                                actual: (actual_file.clone(), *actual_line),
                            });
                        }
                    }
                }
            }
        }
    }

    // 4. Check for DeadLink in markdown files under docs_root
    let re_link = Regex::new(r"symbol://([0-9a-fA-F-]{36})").expect("valid regex");

    if docs_root.is_dir() {
        for entry in WalkDir::new(docs_root).sort_by_file_name() {
            let entry = entry.map_err(|e| Error::IoGeneric(e.into()))?;
            let path = entry.path();

            if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
                let content = fs::read_to_string(path).map_err(|e| Error::Io {
                    path: path.to_path_buf(),
                    source: e,
                })?;

                for caps in re_link.captures_iter(&content) {
                    if let Ok(link_id) = Uuid::parse_str(&caps[1]) {
                        let is_tombstoned = registry_opt
                            .as_ref()
                            .and_then(|reg| reg.get(link_id))
                            .is_some_and(|rec| rec.tombstoned);

                        let exists_in_code = id_locations.contains_key(&link_id);

                        if is_tombstoned || !exists_in_code {
                            issues.push(ConsistencyIssue::DeadLink {
                                doc_file: path.to_path_buf(),
                                id: link_id,
                            });
                        }
                    }
                }
            }
        }
    }

    // Sort issues stably
    issues.sort_by(|a, b| {
        let key_a = issue_sort_key(a);
        let key_b = issue_sort_key(b);
        key_a.cmp(&key_b)
    });

    Ok(issues)
}

fn issue_sort_key(issue: &ConsistencyIssue) -> (u8, String, u32, String) {
    match issue {
        ConsistencyIssue::MissingId { file, line, symbol_name } => {
            (0, file.to_string_lossy().to_string(), *line, symbol_name.clone())
        }
        ConsistencyIssue::DuplicateId { id, locations } => {
            let loc_str = locations
                .first()
                .map(|(p, l)| format!("{}:{}", p.display(), l))
                .unwrap_or_default();
            (1, loc_str, 0, id.to_string())
        }
        ConsistencyIssue::DeadLink { doc_file, id } => {
            (2, doc_file.to_string_lossy().to_string(), 0, id.to_string())
        }
        ConsistencyIssue::StaleCache { id, expected, .. } => {
            (3, expected.0.to_string_lossy().to_string(), expected.1, id.to_string())
        }
    }
}

fn find_and_load_registry(docs_root: &Path, crates_root: &Path) -> Option<SymbolRegistry> {
    let candidates = [
        docs_root.join("symbol_registry.json"),
        crates_root.join("../docs/symbol_registry.json"),
        crates_root.join("docs/symbol_registry.json"),
        crates_root.join("symbol_registry.json"),
    ];

    for candidate in &candidates {
        if candidate.is_file() {
            if let Ok(content) = fs::read_to_string(candidate) {
                if let Ok(registry) = SymbolRegistry::from_json(&content) {
                    return Some(registry);
                }
            }
        }
    }

    None
}
