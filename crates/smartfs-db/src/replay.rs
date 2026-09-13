//! Committing pending markers into Postgres (ADR-58 decision points 3 and 4).
//!
//! Lives here, rather than in `smartfs-fuse`, because two callers need it and
//! only one of them is a filesystem: the daemon's drain, and `smartfs-cli`'s
//! recovery command. Putting it in `smartfs-fuse` would make the CLI depend on
//! `fuser`, which is precisely the coupling ADR-58 §1.2 kept the daemon crate
//! separate to avoid.

use smartfs_schema::error::{Result, SmartFsError};
use smartfs_store::PendingQueue;

use crate::models::AstNodeInsert;
use crate::versions::cow_commit_with_id;
use crate::PgPool;

/// @id: be5276a0-ca16-4080-9ea6-c979b540dea9
/// Outcome of replaying a whole queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayReport {
    /// Markers found in the queue at the start of the pass.
    pub found: usize,
    /// Markers whose transaction was committed by this pass.
    pub committed: usize,
    /// Markers already committed before the crash; replaying them changed
    /// nothing, which is the point of the idempotency key.
    pub already_committed: usize,
    /// Markers that failed and were deliberately left in place for a retry.
    pub failed: usize,
}

/// @id: 428a690f-3abd-42ff-b76a-244d38466f18
/// Commits one pending marker and checkpoints it by unlinking.
///
/// Safe to call twice on the same marker, and safe to call while the daemon's
/// own drain is running: `cow_commit_with_id` is idempotent on `version_id`, so
/// the loser of a race commits nothing and the `unlink` tolerates being second.
///
/// `Ok(None)` means the marker was already gone, so this call checkpointed
/// nothing. `Ok(Some(inserted))` means this call removed it, with `inserted`
/// saying whether the transaction was new or a replay no-op. Callers use the
/// outer `Option` to decide whether to give a queue slot back.
pub async fn commit_pending_marker(
    pool: &PgPool,
    queue: &PendingQueue,
    file_name: &str,
) -> Result<Option<bool>> {
    let marker = match queue.read(file_name).await? {
        Some(m) => m,
        None => return Ok(None),
    };

    let ast_nodes: Vec<AstNodeInsert> =
        serde_json::from_value(marker.ast_nodes.clone()).map_err(|e| {
            SmartFsError::Store(format!(
                "pending marker {file_name} has unreadable ast_nodes: {e}"
            ))
        })?;

    let outcome = cow_commit_with_id(
        pool,
        marker.version_id,
        marker.inode_id,
        marker.blob_id,
        &marker.content_hash,
        marker.size,
        marker.compressed_size,
        marker.external_path.as_deref(),
        marker.special_type.as_deref(),
        marker.special_data.clone(),
        &ast_nodes,
    )
    .await?;

    // Checkpoint. Until this runs the write is replayable; after it, committed.
    queue.remove(file_name).await?;

    if outcome.inserted {
        tracing::debug!(
            version = outcome.version_number,
            inode = %marker.inode_id,
            "pending marker committed"
        );
    } else {
        tracing::info!(
            version_id = %marker.version_id,
            "pending marker was already committed before the crash; replay was a no-op"
        );
    }
    Ok(Some(outcome.inserted))
}

/// @id: 6b2f43e7-9514-4c15-93da-1dff5ce0d343
/// Replays every marker currently in the queue, in FIFO order.
///
/// A failing marker is left in place and the pass continues: one unreachable
/// inode must not strand every write behind it. Failures are counted so the
/// caller can report them rather than exit as if the queue were clean.
pub async fn replay_pending_queue(pool: &PgPool, queue: &PendingQueue) -> Result<ReplayReport> {
    let names = queue.list().await?;
    let mut report = ReplayReport {
        found: names.len(),
        ..Default::default()
    };

    for name in names {
        match commit_pending_marker(pool, queue, &name).await {
            Ok(Some(true)) => report.committed += 1,
            Ok(Some(false)) => report.already_committed += 1,
            Ok(None) => {}
            Err(e) => {
                report.failed += 1;
                tracing::error!("replay of {name} failed, marker left in place: {e}");
            }
        }
    }
    Ok(report)
}
