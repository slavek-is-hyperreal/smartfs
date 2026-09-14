//! Handler for `status` command.

use std::path::Path;

use smartfs_db::{count_all_unconsolidated, oldest_pending_age_secs, pending_backlog_count, PgPool};
use smartfs_schema::error::Result;
use smartfs_schema::PendingMarker;
use smartfs_store::PendingQueue;

use crate::args::StatusArgs;

/// @id: e6a1b2c3-3009-4000-8000-000000000001
/// System status information.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemStatus {
    pub pending_backlog_count: i64,
    pub oldest_pending_age_secs: Option<f64>,
    pub unconsolidated_embedding_count: i64,
    pub calibrated_models_count: usize,
    /// ADR-58: markers sitting in `<store>/pending/queue/`, i.e. writes
    /// acknowledged to a caller that Postgres has not seen yet.
    pub pending_queue_depth: usize,
    /// Age of the oldest such marker. A number that only grows is the signal
    /// that the drain is stuck.
    pub pending_queue_oldest_age_secs: Option<f64>,
    /// Blobs outside the dedup index (ADR-62). Deleting the file that owns one
    /// frees its space at once; a shared blob's space returns only when the
    /// cleaner can prove nothing references it.
    pub private_blob_count: i64,
    /// Blobs participating in dedup.
    pub shared_blob_count: i64,
}

/// @id: 4e0a9b31-27cd-4c85-a9f2-8d61b3e0c47a
/// Reads the ADR-58 pending queue: how many writes are durable but not yet
/// committed, and how long the oldest has been waiting.
///
/// Reported from disk rather than from the daemon's in-memory queue: the CLI is
/// a separate process and cannot observe that counter. The on-disk figure is
/// the one that matters anyway — it is what survives a crash, and what a
/// restart has to replay.
pub async fn read_pending_queue_state(
    store_path: &Path,
) -> Result<(usize, Option<f64>)> {
    let queue = PendingQueue::new(store_path);
    let names = queue.list().await?;
    let depth = names.len();

    // list() is FIFO-ordered, so the oldest is first. Its own timestamp is
    // used rather than the file mtime, which a backup or a copy would reset.
    let oldest = match names.first() {
        Some(name) => match queue.read(name).await? {
            Some(marker) => age_secs(&marker),
            None => None,
        },
        None => None,
    };
    Ok((depth, oldest))
}

/// Seconds since a marker's `created_at`, or `None` if it cannot be parsed.
fn age_secs(marker: &PendingMarker) -> Option<f64> {
    let created = chrono::DateTime::parse_from_rfc3339(&marker.created_at).ok()?;
    let secs = (chrono::Utc::now() - created.with_timezone(&chrono::Utc)).num_milliseconds();
    Some(secs as f64 / 1000.0)
}

/// @id: e6a1b2c3-3009-4000-8000-000000000002
/// Handles execution of the `status` command.
pub async fn handle_status(
    pool: &PgPool,
    store_path: &Path,
    _args: &StatusArgs,
) -> Result<SystemStatus> {
    let backlog = pending_backlog_count(pool).await?;
    let oldest_age = oldest_pending_age_secs(pool).await?;
    let unconsolidated = count_all_unconsolidated(pool).await?;
    let combos = smartfs_semantic::fetch_calibrated_combinations(pool)
        .await
        .unwrap_or_default();

    let (queue_depth, queue_oldest) = read_pending_queue_state(store_path).await?;

    // ADR-62 §Rozstrzygnięcia #2: whether a delete frees space is a property of
    // the blob, not of the inode's dedup flag, so the split has to be visible.
    let (private_blobs, shared_blobs) = sqlx::query_as::<_, (i64, i64)>(
        "SELECT count(*) FILTER (WHERE NOT shared), count(*) FILTER (WHERE shared) FROM blobs",
    )
    .fetch_one(pool)
    .await
    .unwrap_or((0, 0));

    Ok(SystemStatus {
        pending_backlog_count: backlog,
        oldest_pending_age_secs: oldest_age,
        unconsolidated_embedding_count: unconsolidated,
        calibrated_models_count: combos.len(),
        pending_queue_depth: queue_depth,
        pending_queue_oldest_age_secs: queue_oldest,
        private_blob_count: private_blobs,
        shared_blob_count: shared_blobs,
    })
}
