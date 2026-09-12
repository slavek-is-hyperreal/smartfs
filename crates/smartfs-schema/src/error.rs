use thiserror::Error;
use uuid::Uuid;

/// Global unified error type across all SmartFS crates.
/// All crates must use this error enum to preserve error transparency and interoperability.
#[derive(Error, Debug)]
pub enum SmartFsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database error: {0}")]
    Db(String),

    #[error("Storage backend error: {0}")]
    Store(String),

    #[error("Compression error: {0}")]
    Compression(String),

    #[error("Syntax validation error: {0}")]
    SyntaxError(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    /// Missing row in `consolidation_thresholds` for `(plugin_type, model_id)` (ADR-53).
    /// The supervisor for this combination will deliberately not spawn until calibrated.
    #[error("Missing calibration threshold for plugin '{plugin_type}' and model '{model_id}'")]
    MissingCalibration { plugin_type: String, model_id: Uuid },

    /// `pg_try_advisory_lock` failed to acquire lock (ADR-53).
    /// Another instance is already consolidating this combination; not a fatal error.
    #[error("Advisory lock unavailable; batch skipped for concurrent worker")]
    AdvisoryLockUnavailable,

    #[error("Internal error: {0}")]
    Other(String),
}

/// Universal Result alias using `SmartFsError`.
pub type Result<T> = std::result::Result<T, SmartFsError>;
