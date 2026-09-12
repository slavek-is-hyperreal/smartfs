//! Core data types and registry structures for `smartfs-docgen`.

use std::path::PathBuf;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Result;

/// @id: 0bc096a9-0db8-4212-bdec-f74ec4b9cecb
/// Classification of addressable Rust language items in SmartFS codebases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    /// Function or method declaration (`pub fn`, `pub async fn`, etc.).
    Function,
    /// Struct declaration (`pub struct`).
    Struct,
    /// Enum declaration (`pub enum`).
    Enum,
    /// Trait declaration (`pub trait`).
    Trait,
    /// Implementation block (`impl ... for ...` or `impl Type`).
    ImplBlock,
}

/// @id: 5374253a-9668-46bd-b105-1600e26f0cea
/// Persistent record of an addressable symbol matching `symbol_registry.schema.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRecord {
    /// Persistent UUID identifying this symbol across code refactorings and file moves.
    pub id: Uuid,
    /// Name of the containing crate (e.g. `smartfs-semantic`).
    pub crate_name: String,
    /// Syntax kind of the item.
    pub kind: SymbolKind,
    /// Symbol name or identifier.
    pub name: String,
    /// Relative path to the last known source file (e.g. `crates/smartfs-semantic/src/worker.rs`).
    pub last_known_file: String,
    /// 1-indexed line number in the source file.
    pub last_known_line: u32,
    /// First non-empty documentation summary line without `@id:`.
    pub doc_summary: Option<String>,
    /// Whether this symbol has been deleted or deprecated from the codebase.
    pub tombstoned: bool,
    /// Human-readable explanation when symbol is tombstoned.
    pub tombstoned_reason: Option<String>,
}

/// @id: b28d7766-a097-4609-9e72-04d97da07886
/// Root container of the generated symbol registry matching `symbol_registry.schema.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRegistry {
    /// UTC timestamp of registry generation.
    pub generated_at: DateTime<Utc>,
    /// Semver version of the generator tool.
    pub generator_version: String,
    /// Ordered collection of symbols.
    pub symbols: Vec<SymbolRecord>,
}

impl SymbolRegistry {
    /// @id: deccec49-9c9d-46a5-9323-5623d21b3f8d
    /// Looks up a symbol record by its persistent UUID.
    pub fn get(&self, id: Uuid) -> Option<&SymbolRecord> {
        self.symbols.iter().find(|s| s.id == id)
    }

    /// @id: c05e166a-d38c-4726-bc64-c9d35669767a
    /// Parses a `SymbolRegistry` from a JSON string.
    ///
    /// # Errors
    /// Returns `Error::Json` if the JSON structure is malformed.
    pub fn from_json(json_str: &str) -> Result<Self> {
        serde_json::from_str(json_str).map_err(Into::into)
    }

    /// @id: 3646551c-0b1f-47cc-8ffb-234122821e23
    /// Serializes this registry into pretty-printed JSON conforming to schema.
    ///
    /// # Errors
    /// Returns `Error::Json` on serialization error.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(Into::into)
    }
}

/// @id: 12915cc7-8924-4df5-b5d9-f8b777f5f7d3
/// Result of resolving a `symbol://<uuid>` link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedLocation {
    /// Symbol found and verified at specific file and line.
    Found {
        /// Path to the verified source file.
        file: PathBuf,
        /// 1-indexed line number where the symbol definition starts.
        line: u32,
    },
    /// Symbol was removed from the codebase and marked tombstoned.
    Tombstoned {
        /// Saved doc summary at time of removal.
        removed_summary: Option<String>,
    },
}

/// @id: 6e9c40ea-0732-44ca-8211-1172a6a5a73a
/// Discrepancy discovered between source code annotations, registry, or docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsistencyIssue {
    /// Public symbol is declared without a `/// @id: <uuid>` doc comment.
    MissingId {
        /// Source file path.
        file: PathBuf,
        /// Line number of the unannotated symbol.
        line: u32,
        /// Identifier of the unannotated symbol.
        symbol_name: String,
    },
    /// The same UUID is used by multiple items across the codebase.
    DuplicateId {
        /// Colliding UUID.
        id: Uuid,
        /// All locations where this UUID was discovered.
        locations: Vec<(PathBuf, u32)>,
    },
    /// Markdown documentation contains a `symbol://` link that does not resolve or is tombstoned.
    DeadLink {
        /// Markdown file containing the dead link.
        doc_file: PathBuf,
        /// Target UUID of the dead link.
        id: Uuid,
    },
    /// The cached location in `symbol_registry.json` differs from current code layout.
    StaleCache {
        /// Symbol UUID whose cache entry is outdated.
        id: Uuid,
        /// Expected `(file, line)` as recorded in `symbol_registry.json`.
        expected: (PathBuf, u32),
        /// Actual `(file, line)` discovered in current source code.
        actual: (PathBuf, u32),
    },
}
