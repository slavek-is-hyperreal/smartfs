//! smartfs-fuse — FUSE Daemon implementation for SmartFS.
//!
//! Owns exclusively FUSE operation handlers (§3.6).
//! Translates VFS calls into `smartfs-db` and `smartfs-store` calls.
//! Strictly enforces Root Invariant #1 (pre-compression hash), Invariant #4 (no runtime DDL),
//! FIX-01 (refcount-free blobs), FIX-02 (is_current owned by worker, not cow_commit),
//! FIX-03 (physical blob existence check), and FIX-04 (store.put failure compensation).

pub mod error;
pub mod fs;
pub mod mount;
pub mod pending;
pub mod state;
pub mod syntax;

pub use error::error_to_errno;
pub use fs::{SmartFsFuse, TTL};
pub use mount::{default_mount_options, mount_smartfs, spawn_mount_smartfs};
pub use pending::{commit_one, PendingLimits, PendingPipeline, PendingView};
pub use state::{
    inode_to_file_attr, system_time_from_datetime, FuseStateManager, InodeState, OpenHandle,
};
pub use syntax::{
    detect_syntax_language, extract_ast_nodes, validate_and_extract_ast_blocking,
    validate_balanced_delimiters, validate_syntax,
};
