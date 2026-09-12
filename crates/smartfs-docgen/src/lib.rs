//! # smartfs-docgen
//!
//! Synchronous dev-tool for persistent UUID symbol identification, documentation registry
//! maintenance, and documentation link consistency checking.
//!
//! Conforms to SmartFS specification v6.0 and `docs/symbol_registry.schema.json`.

pub mod checker;
pub mod codemod;
pub mod error;
pub mod parser;
pub mod scanner;
pub mod types;

pub use checker::{check_registry_consistency, resolve_symbol_link, tombstone_symbol};
pub use codemod::backfill_missing_ids;
pub use error::{Error, Result};
pub use scanner::{generate_registry, scan_all_crates, scan_crate};
pub use types::{
    ConsistencyIssue, ResolvedLocation, SymbolKind, SymbolRecord, SymbolRegistry,
};

#[cfg(test)]
mod tests {
    use std::fs;
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use crate::parser::parse_items;

    #[test]
    fn test_parse_items_and_doc_comments() {
        let code = r#"
/// @id: 11111111-2222-3333-4444-555555555555
/// Summary of function foo.
/// Detailed description line.
pub async fn foo_bar() -> u32 {
    42
}

/// @id: 22222222-3333-4444-5555-666666666666
/// Summary of struct Bar.
#[derive(Debug, Clone)]
pub struct Bar {
    pub x: u32,
}

/// Summary of enum without id.
pub enum Status {
    Active,
    Inactive,
}

/// @id: 33333333-4444-5555-6666-777777777777
/// Trait definition.
pub trait Runner {
    fn run(&self);
}

/// @id: 44444444-5555-6666-7777-888888888888
/// Impl block.
impl Runner for Bar {
    fn run(&self) {}
}

impl Bar {
    /// @id: 55555555-6666-7777-8888-999999999999
    /// Method on Bar.
    pub fn get_x(&self) -> u32 {
        self.x
    }
}
"#;

        let items = parse_items(code);
        assert_eq!(items.len(), 7);

        // 1. foo_bar
        assert_eq!(items[0].kind, SymbolKind::Function);
        assert_eq!(items[0].name, "foo_bar");
        assert_eq!(
            items[0].id,
            Some(Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap())
        );
        assert_eq!(items[0].doc_summary.as_deref(), Some("Summary of function foo."));
        assert_eq!(items[0].line, 5);

        // 2. Bar struct
        assert_eq!(items[1].kind, SymbolKind::Struct);
        assert_eq!(items[1].name, "Bar");
        assert_eq!(
            items[1].id,
            Some(Uuid::parse_str("22222222-3333-4444-5555-666666666666").unwrap())
        );
        assert_eq!(items[1].doc_summary.as_deref(), Some("Summary of struct Bar."));
        assert_eq!(items[1].line, 12);

        // 3. Status enum (unannotated)
        assert_eq!(items[2].kind, SymbolKind::Enum);
        assert_eq!(items[2].name, "Status");
        assert_eq!(items[2].id, None);
        assert_eq!(items[2].doc_summary.as_deref(), Some("Summary of enum without id."));
        assert_eq!(items[2].line, 17);

        // 4. Runner trait
        assert_eq!(items[3].kind, SymbolKind::Trait);
        assert_eq!(items[3].name, "Runner");
        assert_eq!(
            items[3].id,
            Some(Uuid::parse_str("33333333-4444-5555-6666-777777777777").unwrap())
        );
        assert_eq!(items[3].line, 24);

        // 5. Impl Runner for Bar
        assert_eq!(items[4].kind, SymbolKind::ImplBlock);
        assert_eq!(items[4].name, "Runner for Bar");
        assert_eq!(
            items[4].id,
            Some(Uuid::parse_str("44444444-5555-6666-7777-888888888888").unwrap())
        );
        assert_eq!(items[4].line, 30);

        // 6. Bar impl block
        assert_eq!(items[5].kind, SymbolKind::ImplBlock);
        assert_eq!(items[5].name, "Bar");
        assert_eq!(items[5].id, None);
        assert_eq!(items[5].line, 34);

        // 7. get_x method
        assert_eq!(items[6].kind, SymbolKind::Function);
        assert_eq!(items[6].name, "get_x");
        assert_eq!(
            items[6].id,
            Some(Uuid::parse_str("55555555-6666-7777-8888-999999999999").unwrap())
        );
        assert_eq!(items[6].doc_summary.as_deref(), Some("Method on Bar."));
        assert_eq!(items[6].line, 37);
    }

    #[test]
    fn test_backfill_missing_ids_idempotent() {
        let dir = tempdir().unwrap();
        let crate_dir = dir.path().join("my-crate");
        let src_dir = crate_dir.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let initial_code = r#"/// @id: 11111111-2222-3333-4444-555555555555
/// Already annotated.
pub fn existing() {}

/// Unannotated function with doc.
pub fn unannotated_with_doc() {}

#[derive(Debug)]
pub struct UnannotatedStruct;

pub enum UnannotatedEnum {
    A,
}

pub trait UnannotatedTrait {}

impl UnannotatedStruct {}
"#;

        let file_path = src_dir.join("lib.rs");
        fs::write(&file_path, initial_code).unwrap();

        // First run: should backfill 5 items
        let count1 = backfill_missing_ids(&crate_dir).unwrap();
        assert_eq!(count1, 5);

        let content_after_run1 = fs::read_to_string(&file_path).unwrap();

        // Existing symbol should remain untouched
        assert!(content_after_run1.contains("11111111-2222-3333-4444-555555555555"));

        // All items should now have @id:
        let parsed_after_run1 = parse_items(&content_after_run1);
        assert_eq!(parsed_after_run1.len(), 6);
        for item in &parsed_after_run1 {
            assert!(item.id.is_some(), "Item {} had no ID after backfill", item.name);
        }

        // Second run: strictly idempotent, 0 modifications
        let count2 = backfill_missing_ids(&crate_dir).unwrap();
        assert_eq!(count2, 0);

        let content_after_run2 = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content_after_run1, content_after_run2);
    }

    #[test]
    fn test_scan_crate_and_generate_registry() {
        let dir = tempdir().unwrap();
        let crate_dir = dir.path().join("smartfs-example");
        let src_dir = crate_dir.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        fs::write(
            crate_dir.join("Cargo.toml"),
            r#"[package]
name = "smartfs-example"
version = "0.1.0"
"#,
        )
        .unwrap();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        let src = format!(
            r#"/// @id: {id1}
/// Example struct.
pub struct Example;

/// Unannotated item.
pub fn unannotated_fn() {{}}

/// @id: {id2}
/// Example method.
impl Example {{
    pub fn execute(&self) {{}}
}}
"#
        );
        fs::write(src_dir.join("lib.rs"), src).unwrap();

        // Scan without unannotated
        let annotated_only = scan_crate(&crate_dir, false).unwrap();
        assert_eq!(annotated_only.len(), 2);
        assert_eq!(annotated_only[0].crate_name, "smartfs-example");

        // Scan with unannotated
        let with_unannotated = scan_crate(&crate_dir, true).unwrap();
        assert_eq!(with_unannotated.len(), 4);

        // Generate registry
        let registry = generate_registry(&crate_dir).unwrap();
        assert_eq!(registry.symbols.len(), 2);
        assert_eq!(registry.generator_version, env!("CARGO_PKG_VERSION"));
        assert!(registry.get(id1).is_some());
        assert!(registry.get(id2).is_some());

        // Test JSON roundtrip
        let json = registry.to_json().unwrap();
        let deserialized = SymbolRegistry::from_json(&json).unwrap();
        assert_eq!(deserialized.symbols.len(), 2);
    }

    #[test]
    fn test_resolve_symbol_link_and_reverification() {
        let dir = tempdir().unwrap();
        let crates_dir = dir.path().join("crates");
        let crate_dir = crates_dir.join("test-crate");
        let src_dir = crate_dir.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let id = Uuid::new_v4();
        let file_path = src_dir.join("worker.rs");

        // Initial location at line 4
        let initial_code = format!(
            r#"
/// @id: {id}
/// Worker function.
pub fn worker_loop() {{}}
"#
        );
        fs::write(&file_path, initial_code).unwrap();

        let record = SymbolRecord {
            id,
            crate_name: "test-crate".to_string(),
            kind: SymbolKind::Function,
            name: "worker_loop".to_string(),
            last_known_file: "crates/test-crate/src/worker.rs".to_string(),
            last_known_line: 4,
            doc_summary: Some("Worker function.".to_string()),
            tombstoned: false,
            tombstoned_reason: None,
        };

        let mut registry = SymbolRegistry {
            generated_at: chrono::Utc::now(),
            generator_version: "0.1.0".to_string(),
            symbols: vec![record],
        };

        // 1. Initial resolution matches
        let resolved = resolve_symbol_link(&registry, id, &crates_dir).unwrap();
        match resolved {
            ResolvedLocation::Found { line, .. } => assert_eq!(line, 4),
            ResolvedLocation::Tombstoned { .. } => panic!("Expected found"),
        }

        // 2. Shift the line down by inserting comments above
        let shifted_code = format!(
            r#"// Extra line 1
// Extra line 2
// Extra line 3
/// @id: {id}
/// Worker function.
pub fn worker_loop() {{}}
"#
        );
        fs::write(&file_path, shifted_code).unwrap();

        // Re-verify finds new line 6
        let resolved_shifted = resolve_symbol_link(&registry, id, &crates_dir).unwrap();
        match resolved_shifted {
            ResolvedLocation::Found { line, .. } => assert_eq!(line, 6),
            ResolvedLocation::Tombstoned { .. } => panic!("Expected found"),
        }

        // 3. Tombstoned symbol
        tombstone_symbol(&mut registry, id, "Function deprecated in v6");
        let tombstoned_res = resolve_symbol_link(&registry, id, &crates_dir).unwrap();
        match tombstoned_res {
            ResolvedLocation::Tombstoned { removed_summary } => {
                assert_eq!(removed_summary.as_deref(), Some("Worker function."));
            }
            ResolvedLocation::Found { .. } => panic!("Expected tombstoned"),
        }
    }

    #[test]
    fn test_check_registry_consistency_all_issues() {
        let dir = tempdir().unwrap();
        let crates_dir = dir.path().join("crates");
        let docs_dir = dir.path().join("docs");
        let crate_dir = crates_dir.join("sample-crate");
        let src_dir = crate_dir.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&docs_dir).unwrap();

        let valid_id = Uuid::new_v4();
        let duplicate_id = Uuid::new_v4();
        let tombstoned_id = Uuid::new_v4();
        let unknown_doc_id = Uuid::new_v4();

        // File 1 has a valid item and an unannotated item (MissingId)
        let file1_code = format!(
            r#"/// @id: {valid_id}
/// Valid item.
pub fn valid_item() {{}}

pub fn missing_item() {{}}
"#
        );
        fs::write(src_dir.join("file1.rs"), file1_code).unwrap();

        // File 2 has duplicate IDs (DuplicateId)
        let file2_code = format!(
            r#"/// @id: {duplicate_id}
/// First dup.
pub fn dup1() {{}}

/// @id: {duplicate_id}
/// Second dup.
pub fn dup2() {{}}
"#
        );
        fs::write(src_dir.join("file2.rs"), file2_code).unwrap();

        // Docs with dead link to tombstoned and dead link to unknown
        let doc_content = format!(
            r#"# Documentation
Link to valid: [Valid](symbol://{valid_id})
Link to dead unknown: [Unknown](symbol://{unknown_doc_id})
Link to tombstoned: [Tombstoned](symbol://{tombstoned_id})
"#
        );
        fs::write(docs_dir.join("guide.md"), doc_content).unwrap();

        // Create symbol_registry.json with stale line for valid_id
        let registry = SymbolRegistry {
            generated_at: chrono::Utc::now(),
            generator_version: "0.1.0".to_string(),
            symbols: vec![
                SymbolRecord {
                    id: valid_id,
                    crate_name: "sample-crate".to_string(),
                    kind: SymbolKind::Function,
                    name: "valid_item".to_string(),
                    last_known_file: "crates/sample-crate/src/file1.rs".to_string(),
                    last_known_line: 999, // Stale line! Actual is 3
                    doc_summary: Some("Valid item.".to_string()),
                    tombstoned: false,
                    tombstoned_reason: None,
                },
                SymbolRecord {
                    id: tombstoned_id,
                    crate_name: "sample-crate".to_string(),
                    kind: SymbolKind::Function,
                    name: "old_item".to_string(),
                    last_known_file: "crates/sample-crate/src/file1.rs".to_string(),
                    last_known_line: 1,
                    doc_summary: Some("Old item.".to_string()),
                    tombstoned: true,
                    tombstoned_reason: Some("Removed".to_string()),
                },
            ],
        };
        fs::write(
            docs_dir.join("symbol_registry.json"),
            registry.to_json().unwrap(),
        )
        .unwrap();

        let issues = check_registry_consistency(&crates_dir, &docs_dir).unwrap();

        let mut has_missing = false;
        let mut has_duplicate = false;
        let mut dead_link_count = 0;
        let mut has_stale = false;

        for issue in issues {
            match issue {
                ConsistencyIssue::MissingId { symbol_name, .. } => {
                    if symbol_name == "missing_item" {
                        has_missing = true;
                    }
                }
                ConsistencyIssue::DuplicateId { id, .. } => {
                    if id == duplicate_id {
                        has_duplicate = true;
                    }
                }
                ConsistencyIssue::DeadLink { id, .. } => {
                    if id == unknown_doc_id || id == tombstoned_id {
                        dead_link_count += 1;
                    }
                }
                ConsistencyIssue::StaleCache { id, expected, actual } => {
                    if id == valid_id {
                        assert_eq!(expected.1, 999);
                        assert_eq!(actual.1, 3);
                        has_stale = true;
                    }
                }
            }
        }

        assert!(has_missing, "Expected MissingId for missing_item");
        assert!(has_duplicate, "Expected DuplicateId for duplicate_id");
        assert_eq!(dead_link_count, 2, "Expected 2 DeadLink issues");
        assert!(has_stale, "Expected StaleCache for valid_id");
    }
}
