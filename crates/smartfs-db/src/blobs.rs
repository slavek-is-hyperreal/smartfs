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
    insert_blob_with_sharing(pool, content_hash, blob_id, backend_id, size, true).await
}

/// @id: 5f2a7c81-93e4-4b60-a7d8-1c05e3f9b247
/// `insert_blob`, with the blob's dedup participation chosen by the caller (ADR-62).
///
/// `shared = true` is the historical behaviour: the row joins the dedup index,
/// and identical content anywhere in the filesystem collapses onto one blob.
///
/// `shared = false` writes a private blob. It still gets an inventory row — that
/// is what keeps the Invariant #1 check and the checksum scrub, both of which
/// join this table, from silently ceasing to verify it — but it stays out of the
/// partial unique index, so it is never a dedup target and never a dedup source.
/// That exclusion is not an optimisation detail: without it, a dedup-enabled
/// file could be pointed at a private blob and the caller's guarantee that
/// deleting frees the space would evaporate silently.
///
/// A private insert cannot conflict: the blob id is freshly generated and the
/// content-hash uniqueness applies only `WHERE shared`. So it never reports
/// `inserted = false`, and FIX-04's compensating delete stays correct.
pub async fn insert_blob_with_sharing(
    pool: &PgPool,
    content_hash: &str,
    blob_id: Uuid,
    backend_id: Option<Uuid>,
    size: i64,
    shared: bool,
) -> Result<BlobInsertResult> {
    // Two statements rather than one with a conditional conflict target:
    // ON CONFLICT must name the partial index's predicate, which only makes
    // sense for the shared case.
    let sql = if shared {
        r#"
        INSERT INTO blobs (content_hash, blob_id, backend_id, size, compressed_size, shared)
        VALUES ($1, $2, $3, $4, NULL, TRUE)
        ON CONFLICT (content_hash) WHERE shared DO UPDATE
            SET content_hash = blobs.content_hash
        RETURNING blob_id, (xmax = 0) AS inserted
        "#
    } else {
        r#"
        INSERT INTO blobs (content_hash, blob_id, backend_id, size, compressed_size, shared)
        VALUES ($1, $2, $3, $4, NULL, FALSE)
        RETURNING blob_id, TRUE AS inserted
        "#
    };
    let row = sqlx::query(sql)
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
    // `WHERE shared` is load-bearing (ADR-62): handing back a private blob for
    // reuse would make a second file depend on it, and the private blob's whole
    // promise is that exactly one version owns it.
    sqlx::query_scalar("SELECT blob_id FROM blobs WHERE content_hash = $1 AND shared")
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

/// @id: 4c8e0b73-2a95-4f18-bd60-e7139af5c284
/// Blobs no `file_versions` row references any more (ADR-62 §Rozstrzygnięcia #4).
///
/// These leak today, and have from the beginning: deleting an inode cascades its
/// `file_versions` rows away and leaves the files on disk forever, and FIX-04's
/// compensating delete removes the row but not the bytes. FIX-01 removed
/// refcounts in favour of "GC by scan" and the scan was never written.
///
/// `older_than_secs` is a grace window, not politeness. Under ADR-58 there is a
/// window where a write has been acknowledged to its caller, the blob is on
/// disk, and the `file_versions` row does not exist yet; a scan without the
/// window would delete data the user was told was saved. The caller must ALSO
/// exclude blobs named by markers still sitting in `pending/queue/` — the two
/// protections cover the same race from different sides and neither is
/// sufficient alone.
pub async fn list_unreferenced_blobs(
    pool: &PgPool,
    older_than_secs: i64,
    limit: i64,
) -> Result<Vec<(Uuid, String)>> {
    sqlx::query_as::<_, (Uuid, String)>(
        r#"
        SELECT b.blob_id, b.content_hash
        FROM blobs b
        WHERE b.created_at < NOW() - make_interval(secs => $1)
          AND NOT EXISTS (
              SELECT 1 FROM file_versions fv WHERE fv.content_hash = b.content_hash
          )
        ORDER BY b.created_at
        LIMIT $2
        "#,
    )
    .bind(older_than_secs as f64)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("list_unreferenced_blobs error: {e}")))
}

/// @id: e91d4a26-7b58-40cf-93e2-6d0af8b71539
/// Drops one blob's inventory row, but only while it is still unreferenced.
///
/// The `NOT EXISTS` is repeated here rather than trusted from the scan: between
/// listing and deleting, a write can land a `file_versions` row pointing at this
/// content. Re-checking inside the delete makes the two steps one decision.
/// Returns whether the row was actually removed, so the caller knows whether it
/// may unlink the bytes.
pub async fn delete_blob_if_unreferenced(pool: &PgPool, blob_id: Uuid) -> Result<bool> {
    let done = sqlx::query(
        r#"
        DELETE FROM blobs b
        WHERE b.blob_id = $1
          AND NOT EXISTS (
              SELECT 1 FROM file_versions fv WHERE fv.content_hash = b.content_hash
          )
        "#,
    )
    .bind(blob_id)
    .execute(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("delete_blob_if_unreferenced error: {e}")))?;
    Ok(done.rows_affected() > 0)
}

/// @id: 7a3b91e5-08d6-4c47-b1f0-52ce8d604a3b
/// The private blob backing an inode's current version, if it has one.
///
/// Used by `unlink` to free space immediately. Returns `None` for a shared blob:
/// freeing that needs proof nobody else references it, which is the cleaner's
/// job, not the delete path's.
pub async fn private_blob_of_inode(pool: &PgPool, inode_id: Uuid) -> Result<Option<Uuid>> {
    sqlx::query_scalar(
        r#"
        SELECT b.blob_id
        FROM inode_registry i
        JOIN blobs b ON b.blob_id = i.current_blob_id
        WHERE i.id = $1 AND NOT b.shared
        "#,
    )
    .bind(inode_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("private_blob_of_inode error: {e}")))
}
