//! Committing pending markers into Postgres (ADR-58 decision points 3 and 4).
//!
//! Lives here, rather than in `smartfs-fuse`, because two callers need it and
//! only one of them is a filesystem: the daemon's drain, and `smartfs-cli`'s
//! recovery command. Putting it in `smartfs-fuse` would make the CLI depend on
//! `fuser`, which is precisely the coupling ADR-58 §1.2 kept the daemon crate
//! separate to avoid.

use smartfs_schema::error::{Result, SmartFsError};
use smartfs_store::{BlobStore, PendingQueue};
use uuid::Uuid;

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
    store: Option<&(dyn BlobStore + Send + Sync)>,
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

    // ADR-62 phase C: dedup happens HERE, not on the write path, and shares one
    // transaction with the version commit. Two autocommits were two WAL flushes
    // at 27ms each on this deployment; the write path now pays neither.
    let outcome = match marker.blob_id {
        Some(provisional) => {
            let mut tx = pool
                .begin()
                .await
                .map_err(|e| SmartFsError::Db(format!("drain begin tx: {e}")))?;

            let dedup = crate::blobs::insert_blob_tx(
                &mut tx,
                &marker.content_hash,
                provisional,
                None,
                marker.size,
                marker.compressed_size,
                marker.shared,
            )
            .await?;

            // On a dedup hit the canonical blob is someone else's, and the
            // version must point at that one — not at the copy this write made.
            let canonical = dedup.blob_id;

            let outcome = crate::versions::cow_commit_tx(
                &mut tx,
                marker.version_id,
                marker.inode_id,
                Some(canonical),
                &marker.content_hash,
                marker.size,
                marker.compressed_size,
                marker.external_path.as_deref(),
                marker.special_type.as_deref(),
                marker.special_data.clone(),
                &ast_nodes,
            )
            .await?;

            if outcome.inserted {
                tx.commit()
                    .await
                    .map_err(|e| SmartFsError::Db(format!("drain commit tx: {e}")))?;
            } else {
                let _ = tx.rollback().await;
            }

            if let Some(store) = store {
                reconcile_provisional_blob(store, provisional, canonical, &dedup).await;
            }
            outcome
        }
        // External-path version: no blob of ours to reconcile.
        None => {
            cow_commit_with_id(
                pool,
                marker.version_id,
                marker.inode_id,
                None,
                &marker.content_hash,
                marker.size,
                marker.compressed_size,
                marker.external_path.as_deref(),
                marker.special_type.as_deref(),
                marker.special_data.clone(),
                &ast_nodes,
            )
            .await?
        }
    };

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
pub async fn replay_pending_queue(
    pool: &PgPool,
    queue: &PendingQueue,
    store: Option<&(dyn BlobStore + Send + Sync)>,
) -> Result<ReplayReport> {
    let names = queue.list().await?;
    let mut report = ReplayReport {
        found: names.len(),
        ..Default::default()
    };

    for name in names {
        match commit_pending_marker(pool, queue, store, &name).await {
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

/// @id: 9c45e0a7-1d63-4b82-a0f5-7e21b93cd648
/// Decides what becomes of the provisional blob a write left behind (ADR-62 C).
///
/// Under phase C the writer stores its bytes under a fresh id before knowing
/// whether identical content already exists. The drain then learns the answer
/// and one of three things is true:
///
/// - **New content.** The provisional id IS the canonical one. Nothing to do.
/// - **Dedup hit, canonical bytes present.** The provisional copy is redundant;
///   delete it. This is the cost of deferring dedup, paid in wasted I/O on
///   duplicates rather than in latency on every write.
/// - **Dedup hit, canonical bytes MISSING.** FIX-03's case, and it can now
///   actually heal: the provisional file holds exactly the right bytes, so copy
///   them to the canonical id. Before phase C the heal had to recompress from
///   the caller's buffer; here the bytes are already on disk.
///
/// Every failure is logged and swallowed. The version is committed by this
/// point; turning a storage hiccup into a failed write would lose data that is
/// already durable, and a stray provisional blob is exactly what the ADR-62
/// cleaner reclaims.
async fn reconcile_provisional_blob(
    store: &(dyn BlobStore + Send + Sync),
    provisional: Uuid,
    canonical: Uuid,
    dedup: &crate::models::BlobInsertResult,
) {
    if dedup.inserted || provisional == canonical {
        return;
    }

    match store.exists(canonical).await {
        Ok(true) => {
            if let Err(e) = store.delete(provisional).await {
                tracing::warn!(
                    "duplicate blob {provisional} left on disk after dedup onto {canonical}: {e}"
                );
            }
        }
        Ok(false) => {
            tracing::warn!(
                "dedup pointed at {canonical} but its bytes are missing; \
                 healing from the provisional copy (FIX-03)"
            );
            match store.get(provisional, None).await {
                Ok(bytes) => {
                    if let Err(e) = store.put(canonical, &bytes).await {
                        tracing::error!("could not heal blob {canonical}: {e}");
                    } else if let Err(e) = store.delete(provisional).await {
                        tracing::warn!("healed {canonical} but {provisional} remains: {e}");
                    }
                }
                Err(e) => tracing::error!(
                    "blob {canonical} is missing and the provisional copy is unreadable: {e}"
                ),
            }
        }
        Err(e) => tracing::warn!("could not check whether blob {canonical} exists: {e}"),
    }
}
