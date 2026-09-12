//! Handler for `write` command.

use std::io::Read;
use smartfs_db::{compensate_blob_delete, cow_commit, insert_blob, update_blob_compressed_size, PgPool};
use smartfs_schema::error::Result;
use smartfs_store::BlobStore;
use uuid::Uuid;

use crate::args::WriteArgs;
use crate::path::resolve_or_create_file_path;

/// @id: e6a1b2c3-3001-4000-8000-000000000001
/// Result of the `write` command.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteResult {
    pub path: String,
    pub inode_id: Uuid,
    pub blob_id: Uuid,
    pub version_id: Uuid,
    pub content_hash: String,
    pub size: i64,
    pub compressed_size: Option<i64>,
}

/// @id: e6a1b2c3-3001-4000-8000-000000000002
/// Handles execution of the `write` command.
pub async fn handle_write(
    pool: &PgPool,
    store: &dyn BlobStore,
    args: &WriteArgs,
) -> Result<WriteResult> {
    let data = match &args.file_to_read {
        Some(path) => tokio::fs::read(path).await?,
        None => {
            let mut buffer = Vec::new();
            std::io::stdin().read_to_end(&mut buffer)?;
            buffer
        }
    };

    let size = data.len() as i64;
    let content_hash = smartfs_compress::hash_bytes(&data);

    let inode = resolve_or_create_file_path(pool, &args.path).await?;
    let new_blob_uuid = Uuid::new_v4();

    let insert_res = insert_blob(
        pool,
        &content_hash.0,
        new_blob_uuid,
        inode.backend_id,
        size,
    )
    .await?;

    let blob_id = insert_res.blob_id;
    let compression_level = if inode.compression_level > 0 {
        inode.compression_level as i32
    } else {
        3
    };

    let mut compressed_size = None;
    if insert_res.inserted {
        let compressed = match smartfs_compress::compress(&data, compression_level) {
            Ok(c) => c,
            Err(e) => {
                let _ = compensate_blob_delete(pool, &content_hash.0).await;
                return Err(e);
            }
        };

        if let Err(e) = store.put(blob_id, &compressed).await {
            let _ = compensate_blob_delete(pool, &content_hash.0).await;
            return Err(e);
        }

        let c_len = compressed.len() as i64;
        compressed_size = Some(c_len);
        update_blob_compressed_size(pool, &content_hash.0, c_len).await?;
    } else {
        // Dedup hit: verify physical store existence (FIX-03)
        if !store.exists(blob_id).await.unwrap_or(false) {
            if let Ok(compressed) = smartfs_compress::compress(&data, compression_level) {
                let _ = store.put(blob_id, &compressed).await;
                compressed_size = Some(compressed.len() as i64);
            }
        }
    }

    let special_type = if args.force {
        Some("syntax_error")
    } else {
        Some("generic")
    };

    let special_data = if args.force {
        Some(serde_json::json!({
            "override_syntax_check": true,
            "status": "syntax_error"
        }))
    } else {
        None
    };

    let version_id = cow_commit(
        pool,
        inode.id,
        Some(blob_id),
        &content_hash.0,
        size,
        compressed_size,
        None,
        special_type,
        special_data,
        &[],
    )
    .await?;

    Ok(WriteResult {
        path: args.path.clone(),
        inode_id: inode.id,
        blob_id,
        version_id,
        content_hash: content_hash.0,
        size,
        compressed_size,
    })
}
