//! smartfs-cli — Command-line interface and client library for SmartFS.
//!
//! Owns exclusively:
//! - CLI argument parsing via `clap` (v4 derive).
//! - Thin delegation layer over `smartfs-db`, `smartfs-semantic`, `smartfs-store`, and `smartfs-compress`.
//!
//! Strictly enforces:
//! - Root Invariant #1: content_hash calculated before compression.
//! - Invariant: Zero raw SQL inline — all persistence operations delegate through `smartfs-db` and `smartfs-semantic`.

pub mod args;
pub mod commands;
pub mod dispatch;
pub mod path;

pub use args::{
    CalibrateArgs, CatArgs, Cli, Commands, ConceptsArgs, DiffArgs, HistoryArgs, ImportArgs,
    ImportMode, RecoverArgs, SearchArgs, StatusArgs, WriteArgs,
};
pub use commands::{
    handle_calibrate, handle_cat, handle_concepts, handle_diff, handle_history, handle_import,
    handle_recover, handle_search, handle_status, handle_write, read_pending_queue_state,
    CalibrateResult, CatResult, ConceptSummary, ConceptsResult, DiffResult, HistoryResult,
    ImportResult, RecoverResult, SearchResult, SystemStatus, WriteResult,
};
pub use dispatch::{dispatch_command, run_cli};
pub use path::{resolve_or_create_dir_path, resolve_or_create_file_path, resolve_path};
