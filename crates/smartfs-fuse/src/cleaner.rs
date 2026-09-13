//! Blob garbage collection — the "sprzątacz" of ADR-62 §Rozstrzygnięcia #4.
//!
//! Reclaims blobs no `file_versions` row references any more. These have leaked
//! since the beginning and independently of anything in ADR-62: deleting an
//! inode cascades its versions away and leaves the bytes on disk forever, and
//! FIX-04's compensating delete drops the row but not the file. FIX-01 removed
//! refcounts in favour of "GC by scan" and the scan was never written.
//!
//! **This is not the scrub and not the pending scan.** Three loops, three jobs:
//!
//! | | looks for | finds |
//! |---|---|---|
//! | pending scan | a row missing for a file that exists | an uncommitted write |
//! | scrub | a file gone bad under a row that exists | bit rot |
//! | cleaner (here) | a file no row references | wasted space |
//!
//! **Two independent protections, both required.** Under ADR-58 there is a
//! window where a write is acknowledged, the blob is on disk, and the
//! `file_versions` row does not exist yet. Deleting then destroys data the
//! caller was told was saved. So the cleaner (a) skips anything younger than a
//! grace window and (b) skips any blob named by a marker still in
//! `pending/queue/`. They cover the same race from different sides: the window
//! catches writes in flight, the queue check catches a backlog older than the
//! window. Neither alone is enough.

use std::collections::HashSet;
use std::time::Duration;

use smartfs_db::PgPool;
use smartfs_schema::error::Result;
use smartfs_schema::PendingMarker;
use smartfs_store::{BlobStore, PendingQueue};

/// Default grace window. An hour is what migration 001's own comment on the
/// GC-by-scan sketch proposed, and it is generous on purpose: the cost of
/// waiting is disk space, the cost of being early is losing an acknowledged
/// write.
pub const DEFAULT_GRACE_SECS: i64 = 3600;

/// Blobs examined per pass. Bounded so one pass cannot monopolise the database
/// however much has accumulated; successive passes work through the rest.
pub const DEFAULT_BATCH: i64 = 500;

/// @id: 1d7c5f04-8e93-4a26-b0d7-42f96cae3b18
/// Outcome of one cleaning pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanReport {
    /// Unreferenced blobs the scan proposed.
    pub candidates: usize,
    /// Rows deleted and bytes reclaimed.
    pub reclaimed: usize,
    /// Skipped because a pending marker still names them.
    pub skipped_pending: usize,
    /// Skipped because a `file_versions` row appeared between listing and
    /// deleting — the re-check inside the delete caught it.
    pub skipped_raced: usize,
    /// Row went, file did not. Reported rather than retried: the row is the
    /// authority, and an orphan file is the scrub's and the next pass's problem.
    pub file_delete_failed: usize,
}

/// @id: 8b0e62a7-3d41-49fc-85b2-e0c7d1f4a539
/// Runs one pass. Returns what it did rather than logging and forgetting.
pub async fn clean_once(
    pool: &PgPool,
    store: &dyn BlobStore,
    queue: &PendingQueue,
    grace_secs: i64,
    batch: i64,
) -> Result<CleanReport> {
    let mut report = CleanReport::default();

    // Read the queue FIRST. Doing it after the scan would leave a window in
    // which a marker is enqueued and then its blob judged unreferenced.
    let mut protected: HashSet<String> = HashSet::new();
    for name in queue.list().await? {
        if let Some((_, hash)) = PendingMarker::parse_file_name(&name) {
            protected.insert(hash.to_string());
        }
    }

    let candidates = smartfs_db::list_unreferenced_blobs(pool, grace_secs, batch).await?;
    report.candidates = candidates.len();

    for (blob_id, content_hash) in candidates {
        if protected.contains(&content_hash) {
            report.skipped_pending += 1;
            continue;
        }

        // Row first, bytes second. A crash between them leaves an orphan file —
        // invisible through the mount and caught by the next pass — whereas the
        // other order leaves a row pointing at bytes that are gone, which fails
        // every read of it forever.
        if !smartfs_db::delete_blob_if_unreferenced(pool, blob_id).await? {
            report.skipped_raced += 1;
            continue;
        }

        match store.delete(blob_id).await {
            Ok(()) => report.reclaimed += 1,
            Err(e) => {
                report.file_delete_failed += 1;
                tracing::warn!("blob row {blob_id} removed but its file remains: {e}");
            }
        }
    }

    Ok(report)
}

/// @id: 3f6a80d2-95be-4c17-a3e8-71b40df9c2a6
/// The cleaning loop, run during the filesystem's idle phase.
///
/// `interval` should come from `consolidation_thresholds.idle_before_sleep_secs`
/// — the idle signal migration 005 already defines and the consolidation
/// supervisors already use. A second, competing notion of "the system is quiet"
/// would be one too many.
pub async fn cleaner_loop(
    pool: PgPool,
    store: std::sync::Arc<dyn BlobStore + Send + Sync>,
    queue: PendingQueue,
    interval: Duration,
    grace_secs: i64,
) {
    loop {
        tokio::time::sleep(interval).await;
        match clean_once(&pool, store.as_ref(), &queue, grace_secs, DEFAULT_BATCH).await {
            Ok(r) if r.reclaimed == 0 && r.candidates == 0 => {}
            Ok(r) => tracing::info!(
                candidates = r.candidates,
                reclaimed = r.reclaimed,
                skipped_pending = r.skipped_pending,
                skipped_raced = r.skipped_raced,
                "cleaner pass complete"
            ),
            Err(e) => tracing::error!("cleaner pass failed: {e}"),
        }
    }
}
