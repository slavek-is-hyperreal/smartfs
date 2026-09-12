//! Inode path resolution and directory traversal utilities.

use smartfs_db::{inode_create, inode_lookup, inode_lookup_by_ino, InodeRecord, PgPool};
use smartfs_schema::error::{Result, SmartFsError};

/// @id: e6a1b2c3-2001-4000-8000-000000000001
/// Resolves an existing path inside SmartFS starting from the root inode (ino = 1).
pub async fn resolve_path(pool: &PgPool, path: &str) -> Result<InodeRecord> {
    let root = inode_lookup_by_ino(pool, 1)
        .await?
        .ok_or_else(|| SmartFsError::NotFound("Root inode not found".to_string()))?;

    let clean_path = path.trim().trim_matches('/');
    if clean_path.is_empty() {
        return Ok(root);
    }

    let mut current = root;
    for segment in clean_path.split('/') {
        if segment.is_empty() {
            continue;
        }
        let child = inode_lookup(pool, Some(current.id), segment)
            .await?
            .ok_or_else(|| {
                SmartFsError::NotFound(format!(
                    "Path '{path}' not found (missing segment '{segment}')"
                ))
            })?;
        current = child;
    }

    Ok(current)
}

/// @id: e6a1b2c3-2002-4000-8000-000000000002
/// Resolves a file path, creating parent directories and the file inode if they do not yet exist.
pub async fn resolve_or_create_file_path(pool: &PgPool, path: &str) -> Result<InodeRecord> {
    let root = inode_lookup_by_ino(pool, 1)
        .await?
        .ok_or_else(|| SmartFsError::NotFound("Root inode not found".to_string()))?;

    let clean_path = path.trim().trim_matches('/');
    if clean_path.is_empty() {
        return Err(SmartFsError::Conflict(
            "Cannot write to root filesystem as a regular file".to_string(),
        ));
    }

    let segments: Vec<&str> = clean_path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(SmartFsError::Conflict("Empty file path".to_string()));
    }

    let (dir_segments, file_name) = segments.split_at(segments.len() - 1);
    let leaf_name = file_name[0];

    let mut current = root;
    for seg in dir_segments {
        let child = inode_lookup(pool, Some(current.id), seg).await?;
        match child {
            Some(dir) => {
                if !dir.is_dir {
                    return Err(SmartFsError::Conflict(format!(
                        "Path component '{seg}' in '{path}' is not a directory"
                    )));
                }
                current = dir;
            }
            None => {
                let new_dir = inode_create(pool, Some(current.id), seg, true, 1000, 1000, 0o755).await?;
                current = new_dir;
            }
        }
    }

    let leaf = inode_lookup(pool, Some(current.id), leaf_name).await?;
    match leaf {
        Some(file) => {
            if file.is_dir {
                return Err(SmartFsError::Conflict(format!(
                    "Path '{path}' exists and is a directory"
                )));
            }
            Ok(file)
        }
        None => {
            let new_file = inode_create(pool, Some(current.id), leaf_name, false, 1000, 1000, 0o644).await?;
            Ok(new_file)
        }
    }
}

/// @id: e6a1b2c3-2003-4000-8000-000000000003
/// Resolves a directory path, creating intermediate and leaf directories if they do not yet exist.
pub async fn resolve_or_create_dir_path(pool: &PgPool, path: &str) -> Result<InodeRecord> {
    let root = inode_lookup_by_ino(pool, 1)
        .await?
        .ok_or_else(|| SmartFsError::NotFound("Root inode not found".to_string()))?;

    let clean_path = path.trim().trim_matches('/');
    if clean_path.is_empty() {
        return Ok(root);
    }

    let mut current = root;
    for seg in clean_path.split('/').filter(|s| !s.is_empty()) {
        let child = inode_lookup(pool, Some(current.id), seg).await?;
        match child {
            Some(dir) => {
                if !dir.is_dir {
                    return Err(SmartFsError::Conflict(format!(
                        "Path component '{seg}' in '{path}' is not a directory"
                    )));
                }
                current = dir;
            }
            None => {
                let new_dir = inode_create(pool, Some(current.id), seg, true, 1000, 1000, 0o755).await?;
                current = new_dir;
            }
        }
    }

    Ok(current)
}
