//! Error types for `smartfs-docgen`.

use std::path::PathBuf;
use uuid::Uuid;

/// @id: 748ee1fc-30a6-4261-9418-ca44dfb56c32
/// Errors that can occur during symbol scanning, link resolution, or registry generation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error with context path.
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Generic I/O error without path context.
    #[error("I/O error: {0}")]
    IoGeneric(#[from] std::io::Error),

    /// JSON serialization or deserialization failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// UUID parsing failure.
    #[error("UUID parse error: {0}")]
    Uuid(#[from] uuid::Error),

    /// Requested symbol UUID does not exist in registry.
    #[error("Unknown symbol ID: {0}")]
    UnknownSymbol(Uuid),

    /// Symbol exists in registry but could not be resolved in the source tree.
    #[error("Symbol {0} not found in source code")]
    SymbolNotFoundInCode(Uuid),

    /// Path is invalid or missing required structure.
    #[error("Invalid path: {0}")]
    InvalidPath(String),
}

/// Result alias for operations in `smartfs-docgen`.
pub type Result<T> = std::result::Result<T, Error>;
