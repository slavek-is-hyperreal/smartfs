use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// @id: c7e1892f-b258-46cb-8451-b852ea9d6710
/// Inode record from `inode_registry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct InodeRecord {
    pub id: Uuid,
    pub ino: i64,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub is_dir: bool,
    pub uid: i32,
    pub gid: i32,
    pub mode: i32,
    pub size: i64,
    pub nlink: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub current_blob_id: Option<Uuid>,
    pub backend_id: Option<Uuid>,
    pub on_prem: bool,
    pub compression_level: i16,
    pub versioning_enabled: bool,
    /// Last access time, maintained under `relatime` rules (ADR-61 point 3).
    pub atime: DateTime<Utc>,
    /// Last time the file's CONTENT changed. Metadata-only changes move
    /// `updated_at`, which serves as POSIX `ctime`, and never this (ADR-61).
    pub mtime: DateTime<Utc>,
    /// Device number for `S_IFCHR`/`S_IFBLK`, 0 for every other type (ADR-59).
    ///
    /// `i64` because Linux `dev_t` is 64-bit: `makedev()` with a large minor
    /// number does not fit in 32 bits.
    pub rdev: i64,
}

/// @id: 54cfcbe4-8461-460d-85fa-7f897368d1ab
/// File version record from `file_versions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileVersionRecord {
    pub id: Uuid,
    pub inode_id: Uuid,
    pub version_number: i32,
    pub created_at: DateTime<Utc>,
    pub blob_id: Option<Uuid>,
    pub size: i64,
    pub compressed_size: Option<i64>,
    pub external_path: Option<String>,
    pub content_hash: String,
    pub cid: Option<String>,
    pub ipfs_pinned: Option<bool>,
    pub special_type: String,
    pub special_data: serde_json::Value,
    pub status: String,
    pub retry_count: i32,
    pub parent_version_id: Option<Uuid>,
    pub search_text: Option<String>,
}

/// @id: a7f8d689-0be3-48ef-b4b1-7a70ec86c478
/// Content-addressed blob record from `blobs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct BlobRecord {
    pub content_hash: String,
    pub blob_id: Uuid,
    pub backend_id: Option<Uuid>,
    pub size: i64,
    pub compressed_size: Option<i64>,
    pub created_at: DateTime<Utc>,
}

/// @id: 8db2ef68-98e3-4613-b541-efc255d60ca5
/// Result of atomic blob insertion (FIX-01, FIX-03, FIX-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobInsertResult {
    pub blob_id: Uuid,
    pub inserted: bool,
}

/// @id: bbc054d1-c357-4148-bd06-d242273189d5
/// Stored AST node record from `ast_nodes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct AstNodeRecord {
    pub id: Uuid,
    pub version_id: Uuid,
    pub kind: String,
    pub name: String,
    pub start_line: i32,
    pub end_line: i32,
    pub source: String,
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
}

/// @id: 412ec947-f40a-40a2-9b2f-7be81f9a2e8c
/// Parameters for inserting a new AST node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AstNodeInsert {
    pub kind: String,
    pub name: String,
    pub start_line: i32,
    pub end_line: i32,
    pub source: String,
    pub content_hash: String,
}

/// @id: 97ae1f7c-74a4-4f24-8b63-c7e6c1e550c1
/// Target kind for fulltext hits (ADR-54).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FulltextHitKind {
    File,
    AstNode,
}

/// @id: ce9d4bcb-cb19-402a-b35a-566d36b4cf23
impl std::fmt::Display for FulltextHitKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File => write!(f, "file"),
            Self::AstNode => write!(f, "ast_node"),
        }
    }
}

/// @id: d4b39174-8bce-4ee2-bb72-10f769caef89
/// Result of fulltext BM25 search (ADR-54).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FulltextHit {
    pub id: Uuid,
    pub kind: FulltextHitKind,
    pub version_id: Uuid,
    pub name: Option<String>,
    pub snippet: String,
    pub score: f32,
    pub plugin_type: String,
}
