use crate::models::InodeRecord;
use smartfs_schema::error::{Result, SmartFsError};
use sqlx::PgPool;
use uuid::Uuid;

/// @id: 0e891cfa-723e-4d51-87ab-893d5be7ea21
/// Lookup an inode by parent directory UUID and entry name.
pub async fn inode_lookup(
    pool: &PgPool,
    parent_id: Option<Uuid>,
    name: &str,
) -> Result<Option<InodeRecord>> {
    sqlx::query_as::<_, InodeRecord>(
        r#"
        SELECT id, ino, parent_id, name, is_dir, uid, gid, mode, size, nlink,
               created_at, updated_at, current_blob_id, backend_id, on_prem,
               compression_level, versioning_enabled
        FROM inode_registry
        WHERE parent_id IS NOT DISTINCT FROM $1 AND name = $2
        "#,
    )
    .bind(parent_id)
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("inode_lookup error: {e}")))
}

/// @id: 59a117b8-6548-4e1b-9e4a-4d7620bc1230
/// Lookup an inode by kernel inode number (FUSE ino).
pub async fn inode_lookup_by_ino(pool: &PgPool, ino: i64) -> Result<Option<InodeRecord>> {
    sqlx::query_as::<_, InodeRecord>(
        r#"
        SELECT id, ino, parent_id, name, is_dir, uid, gid, mode, size, nlink,
               created_at, updated_at, current_blob_id, backend_id, on_prem,
               compression_level, versioning_enabled
        FROM inode_registry
        WHERE ino = $1
        "#,
    )
    .bind(ino)
    .fetch_optional(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("inode_lookup_by_ino error: {e}")))
}

/// @id: fa242ce0-c117-48f8-b397-bb701f5de1a2
/// Lookup an inode by its internal primary key UUID.
pub async fn inode_get(pool: &PgPool, id: Uuid) -> Result<Option<InodeRecord>> {
    sqlx::query_as::<_, InodeRecord>(
        r#"
        SELECT id, ino, parent_id, name, is_dir, uid, gid, mode, size, nlink,
               created_at, updated_at, current_blob_id, backend_id, on_prem,
               compression_level, versioning_enabled
        FROM inode_registry
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("inode_get error: {e}")))
}

/// @id: e15e58aa-2cbb-432d-88f5-9a3b6d772922
/// Create a new inode entry in `inode_registry`.
pub async fn inode_create(
    pool: &PgPool,
    parent_id: Option<Uuid>,
    name: &str,
    is_dir: bool,
    uid: i32,
    gid: i32,
    mode: i32,
) -> Result<InodeRecord> {
    sqlx::query_as::<_, InodeRecord>(
        r#"
        INSERT INTO inode_registry (parent_id, name, is_dir, uid, gid, mode)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id, ino, parent_id, name, is_dir, uid, gid, mode, size, nlink,
                  created_at, updated_at, current_blob_id, backend_id, on_prem,
                  compression_level, versioning_enabled
        "#,
    )
    .bind(parent_id)
    .bind(name)
    .bind(is_dir)
    .bind(uid)
    .bind(gid)
    .bind(mode)
    .fetch_one(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("inode_create error: {e}")))
}

/// @id: bb4552cf-a871-4a3d-a567-96a8e8e788bc
/// Delete an inode by UUID.
pub async fn inode_delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM inode_registry WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("inode_delete error: {e}")))?;
    Ok(())
}

/// @id: c7be9b25-0d7e-4ee2-b131-41712a230554
/// List all direct children of a directory inode.
pub async fn inode_list_children(
    pool: &PgPool,
    parent_id: Option<Uuid>,
) -> Result<Vec<InodeRecord>> {
    sqlx::query_as::<_, InodeRecord>(
        r#"
        SELECT id, ino, parent_id, name, is_dir, uid, gid, mode, size, nlink,
               created_at, updated_at, current_blob_id, backend_id, on_prem,
               compression_level, versioning_enabled
        FROM inode_registry
        WHERE parent_id IS NOT DISTINCT FROM $1
        ORDER BY ino ASC
        "#,
    )
    .bind(parent_id)
    .fetch_all(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("inode_list_children error: {e}")))
}

/// @id: 5698b671-55ad-4560-b636-69f88c83aa25
/// Update POSIX attributes of an inode.
pub async fn inode_update_attrs(
    pool: &PgPool,
    id: Uuid,
    uid: Option<i32>,
    gid: Option<i32>,
    mode: Option<i32>,
    size: Option<i64>,
) -> Result<InodeRecord> {
    sqlx::query_as::<_, InodeRecord>(
        r#"
        UPDATE inode_registry
        SET uid = COALESCE($2, uid),
            gid = COALESCE($3, gid),
            mode = COALESCE($4, mode),
            size = COALESCE($5, size),
            updated_at = NOW()
        WHERE id = $1
        RETURNING id, ino, parent_id, name, is_dir, uid, gid, mode, size, nlink,
                  created_at, updated_at, current_blob_id, backend_id, on_prem,
                  compression_level, versioning_enabled
        "#,
    )
    .bind(id)
    .bind(uid)
    .bind(gid)
    .bind(mode)
    .bind(size)
    .fetch_one(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("inode_update_attrs error: {e}")))
}

/// @id: 61e9da74-06eb-460d-a0c5-5a1e2fbc44f1
/// Atomic rename of an inode, removing colliding target if present (ADR-31, §3.6).
pub async fn inode_rename(
    pool: &PgPool,
    old_parent_id: Option<Uuid>,
    old_name: &str,
    new_parent_id: Option<Uuid>,
    new_name: &str,
) -> Result<()> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| SmartFsError::Db(format!("inode_rename begin tx error: {e}")))?;

    // 1. Locate source inode
    let source_id: Uuid = sqlx::query_scalar(
        r#"
        SELECT id FROM inode_registry
        WHERE parent_id IS NOT DISTINCT FROM $1 AND name = $2
        FOR UPDATE
        "#,
    )
    .bind(old_parent_id)
    .bind(old_name)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| SmartFsError::Db(format!("inode_rename source lookup: {e}")))?
    .ok_or_else(|| SmartFsError::NotFound(format!("Source inode '{old_name}' not found")))?;

    // 2. Remove colliding target if exists and not same as source
    let target_id: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id FROM inode_registry
        WHERE parent_id IS NOT DISTINCT FROM $1 AND name = $2
        FOR UPDATE
        "#,
    )
    .bind(new_parent_id)
    .bind(new_name)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| SmartFsError::Db(format!("inode_rename target lookup: {e}")))?;

    if let Some(target) = target_id {
        if target != source_id {
            sqlx::query("DELETE FROM inode_registry WHERE id = $1")
                .bind(target)
                .execute(&mut *tx)
                .await
                .map_err(|e| SmartFsError::Db(format!("inode_rename delete target: {e}")))?;
        }
    }

    // 3. Move and rename source inode
    sqlx::query(
        r#"
        UPDATE inode_registry
        SET parent_id = $1, name = $2, updated_at = NOW()
        WHERE id = $3
        "#,
    )
    .bind(new_parent_id)
    .bind(new_name)
    .bind(source_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| SmartFsError::Db(format!("inode_rename update: {e}")))?;

    tx.commit()
        .await
        .map_err(|e| SmartFsError::Db(format!("inode_rename commit error: {e}")))?;

    Ok(())
}

/// @id: d8e19f2a-7b34-4a56-89bc-0123456789ab
/// Set an inode to index mode (versioning_enabled = FALSE, on_prem = FALSE)
/// for external/in-place file tracking satisfying `blob_or_empty_or_virtual` check constraint.
pub async fn inode_set_index_mode(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE inode_registry
        SET versioning_enabled = FALSE, on_prem = FALSE, updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("inode_set_index_mode error: {e}")))?;

    Ok(())
}
