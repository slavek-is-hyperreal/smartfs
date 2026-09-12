//! Handler for `cat` command.

use smartfs_db::{version_get, PgPool};
use smartfs_schema::error::{Result, SmartFsError};
use smartfs_store::BlobStore;

use crate::args::CatArgs;
use crate::path::resolve_path;

/// @id: e6a1b2c3-3002-4000-8000-000000000001
/// Result of the `cat` command, returning raw decompressed file bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct CatResult {
    pub path: String,
    pub version_number: i32,
    pub content_hash: String,
    pub data: Vec<u8>,
}

/// @id: e6a1b2c3-3002-4000-8000-000000000002
/// Handles execution of the `cat` command.
pub async fn handle_cat(
    pool: &PgPool,
    store: &dyn BlobStore,
    args: &CatArgs,
) -> Result<CatResult> {
    let inode = resolve_path(pool, &args.path).await?;
    let version_rec = version_get(pool, inode.id, args.version)
        .await?
        .ok_or_else(|| {
            SmartFsError::NotFound(format!("Version {:?} of '{}' not found", args.version, args.path))
        })?;

    let data = if let Some(ext_path) = version_rec.external_path.as_deref() {
        store
            .get(version_rec.blob_id.unwrap_or_default(), Some(ext_path))
            .await?
    } else {
        let blob_id = version_rec.blob_id.ok_or_else(|| {
            SmartFsError::NotFound(format!(
                "Version {} of '{}' has neither blob_id nor external_path",
                version_rec.version_number, args.path
            ))
        })?;
        let compressed = store.get(blob_id, None).await?;
        smartfs_compress::decompress(&compressed)?
    };

    Ok(CatResult {
        path: args.path.clone(),
        version_number: version_rec.version_number,
        content_hash: version_rec.content_hash,
        data,
    })
}
