//! The `pending/` queue directory — durability for the first stage of the
//! two-stage `cow_commit` (ADR-58 decision points 1, 3 and 4).
//!
//! Layout, under the same `store_path` as the blobs so `rename()` never crosses
//! a filesystem boundary:
//!
//! ```text
//! <store_path>/pending/tmp/     partially written markers, never read by the drain
//! <store_path>/pending/queue/   durable markers, <seq>_<content_hash>.json
//! ```
//!
//! This is the Maildir pattern: write the whole marker under `tmp/`, `fdatasync`
//! it, then `rename()` into `queue/`. A same-filesystem `rename()` is atomic at
//! the kernel level, so the drain never observes a half-written marker and a
//! crash mid-write leaves nothing but a stray file under `tmp/`. That atomicity
//! *is* the durability guarantee — there is deliberately no WAL format here.
//!
//! The RAM queue that orders the drain lives in `smartfs-fuse`; it is an
//! optimization only, and everything it holds is recoverable from `list()`.
//!
//! This module knows nothing about SQL. The marker body is
//! [`smartfs_schema::PendingMarker`], and what the drain does with it is
//! `smartfs-fuse`'s and `smartfs-db`'s business.

use std::path::{Path, PathBuf};

use smartfs_schema::{PendingMarker, Result, SmartFsError};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;

/// @id: 18a2fd93-de07-41eb-8732-4310365b2c70
/// Handle on `<store_path>/pending/{tmp,queue}/`.
///
/// Cheap to clone; holds paths only.
#[derive(Debug, Clone)]
pub struct PendingQueue {
    tmp: PathBuf,
    queue: PathBuf,
}

/// @id: 76ccab36-32b6-4f6b-b66f-9ca1da1ccb73
impl PendingQueue {
    /// @id: 64566646-b03a-424a-9fe3-7922d0fd13c3
    /// Binds to the pending directories under `store_root` without touching disk.
    ///
    /// Call [`PendingQueue::ensure_dirs`] before the first write.
    pub fn new(store_root: impl AsRef<Path>) -> Self {
        let base = store_root.as_ref().join("pending");
        Self {
            tmp: base.join("tmp"),
            queue: base.join("queue"),
        }
    }

    /// @id: 2db95c17-0155-491b-8a53-cbceb1813b67
    /// Returns the `queue/` directory. Startup and periodic scans read this.
    pub fn queue_dir(&self) -> &Path {
        &self.queue
    }

    /// @id: 42b84f67-e0e9-4be1-aef8-451d77f9b32f
    /// Returns the `tmp/` staging directory.
    pub fn tmp_dir(&self) -> &Path {
        &self.tmp
    }

    /// @id: ab4d0953-86a6-43aa-b007-366f4ebe1552
    /// Creates both directories if absent. Idempotent.
    pub async fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.tmp).await?;
        fs::create_dir_all(&self.queue).await?;
        Ok(())
    }

    /// @id: 6178e20b-48b1-4b12-8bbf-4efd53280b89
    /// Durably enqueues a marker, returning the path it now lives at.
    ///
    /// Write to `tmp/`, `sync_all`, `rename()` into `queue/`. The `sync_all`
    /// is what makes the rename meaningful: without it the rename could be
    /// durable while the bytes it points at are not.
    ///
    /// The caller may treat a successful return as "the write survives a crash"
    /// — that is the contract the FUSE `release()` acknowledgement rests on.
    pub async fn enqueue(&self, marker: &PendingMarker) -> Result<PathBuf> {
        self.ensure_dirs().await?;

        let body = serde_json::to_vec_pretty(marker).map_err(|e| {
            SmartFsError::Store(format!("cannot serialize pending marker: {e}"))
        })?;

        let name = marker.file_name();
        // The staging name is unique per process and sequence, so two daemons
        // sharing a store never collide in tmp/.
        let staged = self.tmp.join(format!("{}.{}.tmp", name, std::process::id()));
        let final_path = self.queue.join(&name);

        {
            let mut file = File::create(&staged).await?;
            file.write_all(&body).await?;
            file.sync_all().await?;
        }

        match fs::rename(&staged, &final_path).await {
            Ok(()) => Ok(final_path),
            Err(e) => {
                // Leave nothing half-staged behind on failure.
                let _ = fs::remove_file(&staged).await;
                Err(SmartFsError::Store(format!(
                    "cannot publish pending marker {name}: {e}"
                )))
            }
        }
    }

    /// @id: 6c156359-b6a2-46ce-bdeb-57018ba8bcf4
    /// Lists queued marker filenames in FIFO order.
    ///
    /// Names that do not parse as `<seq>_<content_hash>.json` are skipped, not
    /// guessed at — a stray file in the queue directory must never be replayed
    /// as if it were a write. A missing `queue/` directory yields an empty list
    /// (nothing has been enqueued yet), which is a different thing from an
    /// unreadable one, and that still surfaces as an error.
    pub async fn list(&self) -> Result<Vec<String>> {
        let mut dir = match fs::read_dir(&self.queue).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };

        let mut names = Vec::new();
        while let Some(entry) = dir.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if PendingMarker::parse_file_name(&name).is_some() {
                names.push(name);
            }
        }
        // Zero-padded seq makes lexicographic order FIFO order.
        names.sort_unstable();
        Ok(names)
    }

    /// @id: 97140abd-af4c-4f36-9ab7-2898c6b4802e
    /// Reads one queued marker by filename.
    ///
    /// `Ok(None)` means the marker is gone — a concurrent drain already
    /// committed and unlinked it, which is normal and not an error.
    pub async fn read(&self, file_name: &str) -> Result<Option<PendingMarker>> {
        let path = self.queue.join(file_name);
        let bytes = match fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let marker = serde_json::from_slice(&bytes).map_err(|e| {
            SmartFsError::Store(format!("pending marker {file_name} is not valid JSON: {e}"))
        })?;
        Ok(Some(marker))
    }

    /// @id: 09c53b6f-e2ef-4055-857e-47dc67ab06e9
    /// Removes a marker after its transaction has COMMITted. This is the
    /// checkpoint of decision point 3; until it runs, the write is replayable.
    ///
    /// Removing an already-removed marker succeeds: replay must be safe to run
    /// twice over the same file.
    pub async fn remove(&self, file_name: &str) -> Result<()> {
        match fs::remove_file(self.queue.join(file_name)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// @id: fc3447a1-acff-4478-8bbe-d1a5db3d1fc8
    /// Number of markers currently awaiting drain. Feeds `smartfs-cli status`.
    pub async fn depth(&self) -> Result<usize> {
        Ok(self.list().await?.len())
    }

    /// @id: c19b7e88-6292-469e-bf91-e67b85a952a3
    /// Deletes staging files in `tmp/` older than `older_than_secs`.
    ///
    /// Only a crash between `create` and `rename` leaves anything here, so this
    /// reclaims space without ever touching a durable marker. The age floor
    /// keeps it from racing a write that is in flight right now. Returns how
    /// many files were removed.
    pub async fn sweep_tmp(&self, older_than_secs: u64) -> Result<usize> {
        let mut dir = match fs::read_dir(&self.tmp).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };

        let mut removed = 0usize;
        while let Some(entry) = dir.next_entry().await? {
            let meta = match entry.metadata().await {
                Ok(m) => m,
                Err(_) => continue,
            };
            let stale = meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .map(|age| age.as_secs() >= older_than_secs)
                .unwrap_or(false);
            if stale && fs::remove_file(entry.path()).await.is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartfs_schema::Uuid;
    use tempfile::tempdir;

    const H1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const H2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn marker(seq: u64, hash: &str) -> PendingMarker {
        PendingMarker {
            seq,
            inode_id: Uuid::new_v4(),
            parent_inode: Some(Uuid::new_v4()),
            name: format!("file-{seq}.txt"),
            version_id: Uuid::new_v4(),
            content_hash: hash.to_string(),
            blob_id: Some(Uuid::new_v4()),
            size: 11,
            compressed_size: Some(9),
            external_path: None,
            mode: 0o100644,
            uid: 1000,
            gid: 1000,
            special_type: None,
            special_data: None,
            ast_nodes: serde_json::json!([]),
            created_at: "2026-09-13T00:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn enqueue_then_read_round_trips() {
        let dir = tempdir().unwrap();
        let q = PendingQueue::new(dir.path());
        let m = marker(1, H1);

        let path = q.enqueue(&m).await.unwrap();
        assert!(path.exists());
        assert_eq!(path.parent().unwrap(), q.queue_dir());

        let back = q.read(&m.file_name()).await.unwrap().unwrap();
        assert_eq!(back, m);
    }

    #[tokio::test]
    async fn enqueue_leaves_nothing_in_tmp() {
        let dir = tempdir().unwrap();
        let q = PendingQueue::new(dir.path());
        q.enqueue(&marker(1, H1)).await.unwrap();

        let leftovers = std::fs::read_dir(q.tmp_dir()).unwrap().count();
        assert_eq!(leftovers, 0, "the rename must move the staged file, not copy it");
    }

    #[tokio::test]
    async fn list_is_fifo_regardless_of_write_order() {
        let dir = tempdir().unwrap();
        let q = PendingQueue::new(dir.path());
        for seq in [10u64, 1, 3, 2] {
            q.enqueue(&marker(seq, H1)).await.unwrap();
        }
        let seqs: Vec<u64> = q
            .list()
            .await
            .unwrap()
            .iter()
            .map(|n| PendingMarker::parse_file_name(n).unwrap().0)
            .collect();
        assert_eq!(seqs, vec![1, 2, 3, 10]);
    }

    #[tokio::test]
    async fn stray_files_in_the_queue_are_skipped_not_replayed() {
        let dir = tempdir().unwrap();
        let q = PendingQueue::new(dir.path());
        q.enqueue(&marker(1, H1)).await.unwrap();
        std::fs::write(q.queue_dir().join("README"), b"not a marker").unwrap();
        std::fs::write(q.queue_dir().join("7_short.json"), b"{}").unwrap();

        assert_eq!(q.list().await.unwrap().len(), 1);
        assert_eq!(q.depth().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn remove_is_idempotent() {
        let dir = tempdir().unwrap();
        let q = PendingQueue::new(dir.path());
        let m = marker(1, H1);
        q.enqueue(&m).await.unwrap();

        q.remove(&m.file_name()).await.unwrap();
        // Replay must tolerate running twice over the same marker.
        q.remove(&m.file_name()).await.unwrap();
        assert_eq!(q.depth().await.unwrap(), 0);
        assert!(q.read(&m.file_name()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn missing_queue_dir_lists_empty_rather_than_erroring() {
        let dir = tempdir().unwrap();
        let q = PendingQueue::new(dir.path().join("never-created"));
        assert!(q.list().await.unwrap().is_empty());
        assert_eq!(q.depth().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn same_hash_under_different_seq_are_distinct_entries() {
        // Two files with identical content: dedup collapses the blob, but each
        // write is still its own queued version.
        let dir = tempdir().unwrap();
        let q = PendingQueue::new(dir.path());
        q.enqueue(&marker(1, H1)).await.unwrap();
        q.enqueue(&marker(2, H1)).await.unwrap();
        q.enqueue(&marker(3, H2)).await.unwrap();
        assert_eq!(q.depth().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn sweep_tmp_spares_fresh_files_and_reaps_old_ones() {
        let dir = tempdir().unwrap();
        let q = PendingQueue::new(dir.path());
        q.ensure_dirs().await.unwrap();
        std::fs::write(q.tmp_dir().join("crashed.tmp"), b"partial").unwrap();

        // A zero-second floor treats everything as stale.
        assert_eq!(q.sweep_tmp(0).await.unwrap(), 1);

        std::fs::write(q.tmp_dir().join("in-flight.tmp"), b"partial").unwrap();
        assert_eq!(q.sweep_tmp(3600).await.unwrap(), 0);
        assert_eq!(std::fs::read_dir(q.tmp_dir()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn corrupt_marker_body_is_an_error_not_a_silent_skip() {
        let dir = tempdir().unwrap();
        let q = PendingQueue::new(dir.path());
        q.ensure_dirs().await.unwrap();
        let name = PendingMarker::file_name_for(1, H1);
        std::fs::write(q.queue_dir().join(&name), b"{ truncated").unwrap();

        assert!(q.read(&name).await.is_err());
    }
}
