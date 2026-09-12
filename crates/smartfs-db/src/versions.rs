use crate::models::{AstNodeInsert, AstNodeRecord, FileVersionRecord};
use smartfs_schema::error::{Result, SmartFsError};
use sqlx::PgPool;
use uuid::Uuid;

/// @id: 28f69e6b-a25e-4bb5-9831-297eb0b9a671
/// Commit metadata for a new file version inside a short SQL transaction (FIX-02, FIX-04, ADR-31).
///
/// Invariant: AST parse and blob storage happen BEFORE this call.
/// Invariant (FIX-02): `is_current` is NEVER touched here; it is exclusively recomputed
/// by the async embedding worker in `smartfs-ai` via `refresh_is_current`.
#[allow(clippy::too_many_arguments)]
pub async fn cow_commit(
    pool: &PgPool,
    inode_id: Uuid,
    blob_id: Option<Uuid>,
    content_hash: &str,
    size: i64,
    compressed_size: Option<i64>,
    external_path: Option<&str>,
    special_type: Option<&str>,
    special_data: Option<serde_json::Value>,
    ast_nodes: &[AstNodeInsert],
) -> Result<Uuid> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| SmartFsError::Db(format!("cow_commit begin tx: {e}")))?;

    // 1. Lock inode to serialize version increment
    let _: Uuid = sqlx::query_scalar(
        "SELECT id FROM inode_registry WHERE id = $1 FOR UPDATE",
    )
    .bind(inode_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| SmartFsError::Db(format!("cow_commit lock inode: {e}")))?;

    // 2. Next version number
    let next_ver: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version_number), 0) + 1 FROM file_versions WHERE inode_id = $1",
    )
    .bind(inode_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| SmartFsError::Db(format!("cow_commit version calc: {e}")))?;

    // 3. Parent version for DAG link
    let prev_ver: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM file_versions WHERE inode_id = $1 ORDER BY version_number DESC LIMIT 1",
    )
    .bind(inode_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| SmartFsError::Db(format!("cow_commit prev version lookup: {e}")))?;

    let version_id = Uuid::new_v4();
    let stype = special_type.unwrap_or("generic");
    let sdata = special_data.unwrap_or_else(|| serde_json::json!({}));

    // 4. Insert file_version
    sqlx::query(
        r#"
        INSERT INTO file_versions (
            id, inode_id, version_number, blob_id, size, compressed_size,
            external_path, content_hash, special_type, special_data,
            status, parent_version_id
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'pending'::processing_status, $11)
        "#,
    )
    .bind(version_id)
    .bind(inode_id)
    .bind(next_ver)
    .bind(blob_id)
    .bind(size)
    .bind(compressed_size)
    .bind(external_path)
    .bind(content_hash)
    .bind(stype)
    .bind(sdata)
    .bind(prev_ver)
    .execute(&mut *tx)
    .await
    .map_err(|e| SmartFsError::Db(format!("cow_commit insert file_version: {e}")))?;

    // 5. Insert AST nodes (if any)
    for node in ast_nodes {
        let node_id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO ast_nodes (
                id, version_id, kind, name, start_line, end_line, source, content_hash
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (version_id, content_hash) DO NOTHING
            "#,
        )
        .bind(node_id)
        .bind(version_id)
        .bind(&node.kind)
        .bind(&node.name)
        .bind(node.start_line)
        .bind(node.end_line)
        .bind(&node.source)
        .bind(&node.content_hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| SmartFsError::Db(format!("cow_commit insert ast_node: {e}")))?;
    }

    // 6. Update current inode pointer and size
    sqlx::query(
        r#"
        UPDATE inode_registry
        SET current_blob_id = $1, size = $2, updated_at = NOW()
        WHERE id = $3
        "#,
    )
    .bind(blob_id)
    .bind(size)
    .bind(inode_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| SmartFsError::Db(format!("cow_commit update inode: {e}")))?;

    // NOTE: is_current is NEVER touched in cow_commit (FIX-02).

    tx.commit()
        .await
        .map_err(|e| SmartFsError::Db(format!("cow_commit commit tx: {e}")))?;

    Ok(version_id)
}

/// @id: a48b59e3-23a7-47b6-9bb2-1594ec1b01c3
/// Get a specific version of a file or its latest version if version_number is None.
pub async fn version_get(
    pool: &PgPool,
    inode_id: Uuid,
    version_number: Option<i32>,
) -> Result<Option<FileVersionRecord>> {
    match version_number {
        Some(v) => sqlx::query_as::<_, FileVersionRecord>(
            r#"
            SELECT id, inode_id, version_number, created_at, blob_id, size,
                   compressed_size, external_path, content_hash, cid, ipfs_pinned,
                   special_type, special_data, status::text AS status, retry_count,
                   parent_version_id, search_text
            FROM file_versions
            WHERE inode_id = $1 AND version_number = $2
            "#,
        )
        .bind(inode_id)
        .bind(v)
        .fetch_optional(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("version_get error: {e}"))),
        None => sqlx::query_as::<_, FileVersionRecord>(
            r#"
            SELECT id, inode_id, version_number, created_at, blob_id, size,
                   compressed_size, external_path, content_hash, cid, ipfs_pinned,
                   special_type, special_data, status::text AS status, retry_count,
                   parent_version_id, search_text
            FROM file_versions
            WHERE inode_id = $1
            ORDER BY version_number DESC
            LIMIT 1
            "#,
        )
        .bind(inode_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("version_get latest error: {e}"))),
    }
}

/// @id: ee509d3b-65bc-4876-88a2-fa4b1b3699c2
/// Get a file version by its primary key UUID.
pub async fn version_get_by_id(
    pool: &PgPool,
    version_id: Uuid,
) -> Result<Option<FileVersionRecord>> {
    sqlx::query_as::<_, FileVersionRecord>(
        r#"
        SELECT id, inode_id, version_number, created_at, blob_id, size,
               compressed_size, external_path, content_hash, cid, ipfs_pinned,
               special_type, special_data, status::text AS status, retry_count,
               parent_version_id, search_text
        FROM file_versions
        WHERE id = $1
        "#,
    )
    .bind(version_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("version_get_by_id error: {e}")))
}

/// @id: 5df9e0f1-a189-4a96-a9ea-a42cb0985289
/// List all historical versions for an inode ordered by version number ascending.
pub async fn version_history(
    pool: &PgPool,
    inode_id: Uuid,
) -> Result<Vec<FileVersionRecord>> {
    sqlx::query_as::<_, FileVersionRecord>(
        r#"
        SELECT id, inode_id, version_number, created_at, blob_id, size,
               compressed_size, external_path, content_hash, cid, ipfs_pinned,
               special_type, special_data, status::text AS status, retry_count,
               parent_version_id, search_text
        FROM file_versions
        WHERE inode_id = $1
        ORDER BY version_number ASC
        "#,
    )
    .bind(inode_id)
    .fetch_all(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("version_history error: {e}")))
}

/// @id: 6a2c2b3e-3245-4298-8ce9-0d19b33a59df
/// Find an existing file version by its content hash.
pub async fn version_find_by_hash(
    pool: &PgPool,
    content_hash: &str,
) -> Result<Option<FileVersionRecord>> {
    sqlx::query_as::<_, FileVersionRecord>(
        r#"
        SELECT id, inode_id, version_number, created_at, blob_id, size,
               compressed_size, external_path, content_hash, cid, ipfs_pinned,
               special_type, special_data, status::text AS status, retry_count,
               parent_version_id, search_text
        FROM file_versions
        WHERE content_hash = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(content_hash)
    .fetch_optional(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("version_find_by_hash error: {e}")))
}

/// @id: bd237b67-4e05-4f40-8b1e-9134b2ea67fa
/// Get all AST nodes associated with a file version.
pub async fn get_ast_nodes(
    pool: &PgPool,
    version_id: Uuid,
) -> Result<Vec<AstNodeRecord>> {
    sqlx::query_as::<_, AstNodeRecord>(
        r#"
        SELECT id, version_id, kind, name, start_line, end_line, source,
               content_hash, created_at
        FROM ast_nodes
        WHERE version_id = $1
        ORDER BY start_line ASC
        "#,
    )
    .bind(version_id)
    .fetch_all(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("get_ast_nodes error: {e}")))
}
