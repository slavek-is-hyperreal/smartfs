//! Handler for the `recover` command — ADR-58's manual recovery pass.
//!
//! The daemon replays `pending/queue/` at startup and on a timer, but an
//! operator who has just dealt with an incident wants the pass to run now, not
//! at the next tick. This is that command.
//!
//! Safe to run while the daemon is up. Every marker is committed through
//! `cow_commit_with_id`, which is idempotent on `version_id`, so the loser of a
//! race with the daemon's drain commits nothing and the `unlink` tolerates
//! being second. It is also safe to run twice.

use std::path::Path;

use smartfs_db::{PgPool, ReplayReport};
use smartfs_schema::error::Result;
use smartfs_store::PendingQueue;

use crate::args::RecoverArgs;

/// @id: 7c3d5e91-4a68-4b02-8f3d-19e7c0a2b654
/// What a recovery pass did.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoverResult {
    /// Queue this pass ran against.
    pub queue_dir: String,
    /// `None` for a dry run, which inspects and commits nothing.
    pub report: Option<ReplayReport>,
    /// Markers present, counted before any commit.
    pub found: usize,
}

/// @id: 1f8b6d47-90ae-4c31-b57e-2a4d8f0e6c93
/// Handles execution of the `recover` command.
pub async fn handle_recover(
    pool: &PgPool,
    store_path: &Path,
    args: &RecoverArgs,
) -> Result<RecoverResult> {
    let queue = PendingQueue::new(store_path);
    let found = queue.list().await?.len();
    let queue_dir = queue.queue_dir().display().to_string();

    if args.dry_run {
        return Ok(RecoverResult {
            queue_dir,
            report: None,
            found,
        });
    }

    let report = smartfs_db::replay_pending_queue(pool, &queue).await?;

    // Staging files can only be left by a crash between create and rename, so
    // reclaiming the old ones here costs nothing and never touches a durable
    // marker. The age floor keeps it clear of writes in flight right now.
    match queue.sweep_tmp(args.sweep_tmp_older_than_secs).await {
        Ok(0) => {}
        Ok(n) => tracing::info!("recover: swept {n} stale staging file(s)"),
        Err(e) => tracing::warn!("recover: could not sweep staging files: {e}"),
    }

    Ok(RecoverResult {
        queue_dir,
        report: Some(report),
        found,
    })
}
