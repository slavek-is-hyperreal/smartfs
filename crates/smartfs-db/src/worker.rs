use smartfs_schema::error::{Result, SmartFsError};
use sqlx::PgPool;
use uuid::Uuid;

/// @id: 5a2c4e8b-1234-4567-89ab-cdef01234567
/// Claim pending file versions for processing using `SKIP LOCKED` concurrency control (§3.5).
pub async fn claim_pending_to_processing(pool: &PgPool, limit: i64) -> Result<Vec<Uuid>> {
    let rows: Vec<Uuid> = sqlx::query_scalar(
        r#"
        WITH pending AS (
            SELECT id FROM file_versions
            WHERE status = 'pending'
            ORDER BY created_at ASC
            LIMIT $1
            FOR UPDATE SKIP LOCKED
        )
        UPDATE file_versions
        SET status = 'processing'
        FROM pending
        WHERE file_versions.id = pending.id
        RETURNING file_versions.id
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("claim_pending_to_processing error: {e}")))?;

    Ok(rows)
}

/// @id: 7b3d5f9c-2345-6789-abcd-ef0123456789
/// Mark a file version as cleanly processed after embeddings are created.
pub async fn mark_clean(pool: &PgPool, version_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE file_versions SET status = 'clean' WHERE id = $1")
        .bind(version_id)
        .execute(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("mark_clean error: {e}")))?;

    Ok(())
}

/// @id: 9c4e6a0d-3456-789a-bcde-f0123456789a
/// Mark a file version as failed and increment retry count.
pub async fn mark_failed(pool: &PgPool, version_id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE file_versions SET status = 'failed', retry_count = retry_count + 1 WHERE id = $1",
    )
    .bind(version_id)
    .execute(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("mark_failed error: {e}")))?;

    Ok(())
}

/// @id: 1d5f7b1e-4567-89ab-cdef-0123456789ab
/// Revert a processing file version back to pending (e.g. on preemption).
pub async fn revert_to_pending(pool: &PgPool, version_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE file_versions SET status = 'pending' WHERE id = $1 AND status = 'processing'")
        .bind(version_id)
        .execute(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("revert_to_pending error: {e}")))?;

    Ok(())
}

/// @id: 3e6a8c2f-5678-9abc-def0-123456789abc
/// Reaper executed at daemon startup to reset stale `processing` rows back to `pending`.
pub async fn reaper(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query("UPDATE file_versions SET status = 'pending' WHERE status = 'processing'")
        .execute(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("reaper error: {e}")))?;

    Ok(result.rows_affected())
}

/// @id: 5f7b9d3a-6789-abcd-ef01-23456789abcd
/// Age of oldest pending version in seconds (used by ADR-42 anti-starvation valve).
pub async fn oldest_pending_age_secs(pool: &PgPool) -> Result<Option<f64>> {
    let age: Option<f64> = sqlx::query_scalar(
        r#"
        SELECT EXTRACT(EPOCH FROM (NOW() - MIN(created_at)))::double precision
        FROM file_versions
        WHERE status = 'pending'
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("oldest_pending_age_secs error: {e}")))?;

    Ok(age)
}

/// @id: 7a8c0e4b-789a-bcde-f012-3456789abcde
/// Count of pending file versions awaiting processing (used by ADR-42 anti-starvation valve).
pub async fn pending_backlog_count(pool: &PgPool) -> Result<i64> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM file_versions WHERE status = 'pending'")
        .fetch_one(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("pending_backlog_count error: {e}")))?;

    Ok(count)
}

/// @id: 9b0d1f5c-89ab-cdef-0123-456789abcdef
/// Recompute `is_current` across all AST embeddings for the given inode (FIX-02, ADR-45).
/// Order-independent: ensures `is_current = TRUE` strictly for vectors belonging to the latest version.
pub async fn refresh_is_current(pool: &PgPool, version_id: Uuid) -> Result<()> {
    // 1. Inode id of this version
    let inode_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT inode_id FROM file_versions WHERE id = $1",
    )
    .bind(version_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("refresh_is_current inode lookup: {e}")))?;

    let inode_id = match inode_id {
        Some(id) => id,
        None => return Ok(()),
    };

    // 2. Latest version id of this inode
    let latest_vid: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM file_versions WHERE inode_id = $1 ORDER BY version_number DESC LIMIT 1",
    )
    .bind(inode_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("refresh_is_current latest version lookup: {e}")))?;

    let latest_vid = match latest_vid {
        Some(id) => id,
        None => return Ok(()),
    };

    // 3. Recompute is_current for all AST embeddings of this inode
    sqlx::query(
        r#"
        UPDATE ast_embeddings_1536 ae
        SET is_current = (fv.id = $1)
        FROM ast_nodes an
        JOIN file_versions fv ON an.version_id = fv.id
        WHERE ae.ast_node_id = an.id
          AND fv.inode_id = $2
        "#,
    )
    .bind(latest_vid)
    .bind(inode_id)
    .execute(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("refresh_is_current update: {e}")))?;

    Ok(())
}
