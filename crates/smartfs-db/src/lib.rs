//! smartfs-db — PostgreSQL database logic, Inode CRUD, CoW commit, and dedup.
//!
//! Exclusively owns all SQL logic across the SmartFS workspace.
//! Strictly enforces Root Invariant #1 (pre-compression hash), Invariant #4 (no runtime DDL),
//! Invariant #5 (isolated vector dimension tables), FIX-01 (refcount-free blobs),
//! FIX-02 (is_current owned by worker, not cow_commit), FIX-03 (physical blob existence check),
//! and FIX-04 (store.put failure compensation).

pub mod blobs;
pub mod embeddings;
pub mod inodes;
pub mod models;
pub mod preflight;
pub mod search;
pub mod versions;
pub mod worker;

pub use blobs::{
    compensate_blob_delete, dedup_check, get_blob, insert_blob, list_blob_digests,
    update_blob_compressed_size,
};
pub use embeddings::{
    get_default_model_id, get_model_dimensions, get_model_id_by_name, insert_ast_embedding_1536,
    insert_embedding_1024_qwen, insert_embedding_384, insert_embedding_768,
};
pub use inodes::{
    inode_create, inode_delete, inode_get, inode_list_children, inode_lookup,
    inode_lookup_by_ino, inode_rename, inode_set_index_mode, inode_update_attrs,
};
pub use models::{
    AstNodeInsert, AstNodeRecord, BlobInsertResult, BlobRecord, FileVersionRecord, FulltextHit,
    FulltextHitKind, InodeRecord,
};
pub use preflight::{column_exists, ping, table_exists};
pub use search::{count_all_unconsolidated, search_fulltext_bm25, set_search_text};
pub use versions::{
    cow_commit, cow_commit_with_id, get_ast_nodes, version_find_by_hash, version_get,
    version_get_by_id, version_history, CowCommitOutcome,
};
pub use worker::{
    claim_pending_to_processing, mark_clean, mark_failed, oldest_pending_age_secs,
    pending_backlog_count, reaper, refresh_is_current, revert_to_pending,
};

pub use sqlx::postgres::PgPoolOptions;
pub use sqlx::{PgPool, Pool, Postgres};

use smartfs_schema::error::{Result, SmartFsError};

/// @id: 1a9f3b2c-8d7e-4601-b532-cf840192a781
/// Establish a connection pool to the PostgreSQL database.
pub async fn connect_pool(database_url: &str) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(20)
        .connect(database_url)
        .await
        .map_err(|e| SmartFsError::Db(format!("Failed to connect to database: {e}")))
}
