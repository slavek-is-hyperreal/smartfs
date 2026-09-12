//! Handler for `import` command.

use std::path::PathBuf;
use smartfs_db::{
    compensate_blob_delete, cow_commit, insert_blob, update_blob_compressed_size, PgPool,
};
use smartfs_schema::error::{Result, SmartFsError};
use smartfs_store::BlobStore;
use uuid::Uuid;

use crate::args::{ImportArgs, ImportMode};
use crate::path::resolve_or_create_dir_path;

/// @id: e6a1b2c3-3006-4000-8000-000000000001
/// Result of the `import` command.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportResult {
    pub root_dir: PathBuf,
    pub mode: ImportMode,
    pub files_imported: usize,
    pub directories_imported: usize,
    pub total_bytes: i64,
}

/// @id: e6a1b2c3-3006-4000-8000-000000000002
/// Handles execution of the `import` command.
pub async fn handle_import(
    pool: &PgPool,
    store: &dyn BlobStore,
    args: &ImportArgs,
) -> Result<ImportResult> {
    let root = args.dir.canonicalize().map_err(SmartFsError::Io)?;
    if !root.is_dir() {
        return Err(SmartFsError::NotFound(format!(
            "Import target '{}' is not a directory",
            root.display()
        )));
    }

    let mut files_imported = 0;
    let mut directories_imported = 0;
    let mut total_bytes = 0i64;

    // Collect all entries recursively
    let mut dir_queue = vec![root.clone()];

    while let Some(current_dir) = dir_queue.pop() {
        let mut entries = tokio::fs::read_dir(&current_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;

            let rel_path = match path.strip_prefix(&root) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let rel_path_str = rel_path.to_string_lossy().to_string();

            if file_type.is_dir() {
                // Ensure directory inode exists
                resolve_or_create_dir_path(pool, &rel_path_str).await?;
                directories_imported += 1;
                dir_queue.push(path);
            } else if file_type.is_file() {
                // Ensure parent dir and file inode exist
                let file_inode = crate::path::resolve_or_create_file_path(pool, &rel_path_str).await?;
                let bytes = tokio::fs::read(&path).await?;
                let size = bytes.len() as i64;
                let content_hash = smartfs_compress::hash_bytes(&bytes);

                match args.mode {
                    ImportMode::Index => {
                        smartfs_db::inode_set_index_mode(pool, file_inode.id).await?;
                        let abs_path = path.to_string_lossy().to_string();
                        cow_commit(
                            pool,
                            file_inode.id,
                            None,
                            &content_hash.0,
                            size,
                            None,
                            Some(&abs_path),
                            Some("generic"),
                            None,
                            &[],
                        )
                        .await?;
                    }
                    ImportMode::Cow => {
                        let new_blob_uuid = Uuid::new_v4();
                        let insert_res = insert_blob(
                            pool,
                            &content_hash.0,
                            new_blob_uuid,
                            file_inode.backend_id,
                            size,
                        )
                        .await?;

                        let blob_id = insert_res.blob_id;
                        let compression_level = if file_inode.compression_level > 0 {
                            file_inode.compression_level as i32
                        } else {
                            3
                        };

                        let mut compressed_size = None;
                        if insert_res.inserted {
                            let compressed = match smartfs_compress::compress(&bytes, compression_level) {
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
                        } else if !store.exists(blob_id).await.unwrap_or(false) {
                            if let Ok(compressed) = smartfs_compress::compress(&bytes, compression_level) {
                                let _ = store.put(blob_id, &compressed).await;
                                compressed_size = Some(compressed.len() as i64);
                            }
                        }

                        cow_commit(
                            pool,
                            file_inode.id,
                            Some(blob_id),
                            &content_hash.0,
                            size,
                            compressed_size,
                            None,
                            Some("generic"),
                            None,
                            &[],
                        )
                        .await?;
                    }
                }

                files_imported += 1;
                total_bytes += size;
            }
        }
    }

    Ok(ImportResult {
        root_dir: root,
        mode: args.mode,
        files_imported,
        directories_imported,
        total_bytes,
    })
}
