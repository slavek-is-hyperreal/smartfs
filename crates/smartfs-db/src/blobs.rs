use crate::models::{BlobInsertResult, BlobRecord};
use smartfs_schema::error::{Result, SmartFsError};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// @id: 5ea721c4-648b-4bb8-86d1-447573eb8b22
/// Atomic dedup serialization point via `blobs` table (FIX-01, FIX-03, FIX-04, ADR-40).
/// Uses no-op UPDATE on conflict so RETURNING returns existing blob_id.
/// Returns `BlobInsertResult { blob_id, inserted }`.
pub async fn insert_blob(
    pool: &PgPool,
    content_hash: &str,
    blob_id: Uuid,
    backend_id: Option<Uuid>,
    size: i64,
) -> Result<BlobInsertResult> {
    let row = sqlx::query(
        r#"
        INSERT INTO blobs (content_hash, blob_id, backend_id, size, compressed_size)
        VALUES ($1, $2, $3, $4, NULL)
        ON CONFLICT (content_hash) DO UPDATE
            SET content_hash = blobs.content_hash
        RETURNING blob_id, (xmax = 0) AS inserted
        "#,
    )
    .bind(content_hash)
    .bind(blob_id)
    .bind(backend_id)
    .bind(size)
    .fetch_one(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("insert_blob error: {e}")))?;

    let returned_blob_id: Uuid = row
        .try_get("blob_id")
        .map_err(|e| SmartFsError::Db(format!("insert_blob extract blob_id: {e}")))?;
    let inserted: bool = row
        .try_get("inserted")
        .map_err(|e| SmartFsError::Db(format!("insert_blob extract inserted: {e}")))?;

    Ok(BlobInsertResult {
        blob_id: returned_blob_id,
        inserted,
    })
}

/// @id: fa17812e-131c-4b69-8086-a9ebc67d1a55
/// Update compressed size for a blob once store.put succeeds.
pub async fn update_blob_compressed_size(
    pool: &PgPool,
    content_hash: &str,
    compressed_size: i64,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE blobs
        SET compressed_size = $1
        WHERE content_hash = $2
        "#,
    )
    .bind(compressed_size)
    .bind(content_hash)
    .execute(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("update_blob_compressed_size error: {e}")))?;

    Ok(())
}

/// @id: 8816c271-e054-469b-9807-ca906e57df52
/// Compensation delete when store.put fails after blob insertion (FIX-04, ADR-46).
pub async fn compensate_blob_delete(pool: &PgPool, content_hash: &str) -> Result<()> {
    sqlx::query("DELETE FROM blobs WHERE content_hash = $1")
        .bind(content_hash)
        .execute(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("compensate_blob_delete error: {e}")))?;

    Ok(())
}

/// @id: c714e820-c711-4f93-b6d8-9dfcb9aa9882
/// Check whether a blob with the given content hash already exists.
pub async fn dedup_check(pool: &PgPool, content_hash: &str) -> Result<Option<Uuid>> {
    sqlx::query_scalar("SELECT blob_id FROM blobs WHERE content_hash = $1")
        .bind(content_hash)
        .fetch_optional(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("dedup_check error: {e}")))
}

/// @id: 3c597011-85b5-4b05-83e0-6e426e2e0618
/// Get a blob record by content hash.
pub async fn get_blob(pool: &PgPool, content_hash: &str) -> Result<Option<BlobRecord>> {
    sqlx::query_as::<_, BlobRecord>(
        r#"
        SELECT content_hash, blob_id, backend_id, size, compressed_size, created_at
        FROM blobs
        WHERE content_hash = $1
        "#,
    )
    .bind(content_hash)
    .fetch_optional(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("get_blob error: {e}")))
}

/// @id: d5db3387-d800-449b-a3b3-e8b7fe24dd82
/// Lists `(blob_id, content_hash)` pairs for the ADR-58 checksum scrub.
///
/// `sample` picks a random subset so a periodic pass covers the store over
/// time without ever reading all of it at once; `None` returns everything, for
/// a deliberate full run at low load. Randomization lives here, in the query,
/// so the scrub itself stays deterministic over whatever it is handed.
///
/// External-path versions are excluded: their bytes are not in the blob store,
/// so the scrub has nothing to verify them against.
pub async fn list_blob_digests(
    pool: &PgPool,
    sample: Option<i64>,
) -> Result<Vec<(Uuid, String)>> {
    let rows = match sample {
        Some(limit) => {
            sqlx::query_as::<_, (Uuid, String)>(
                "SELECT blob_id, content_hash FROM blobs ORDER BY random() LIMIT $1",
            )
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, (Uuid, String)>("SELECT blob_id, content_hash FROM blobs")
                .fetch_all(pool)
                .await
        }
    };
    rows.map_err(|e| SmartFsError::Db(format!("list_blob_digests error: {e}")))
}
