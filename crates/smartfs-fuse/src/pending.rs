//! The two-stage `cow_commit` pipeline (ADR-58).
//!
//! `release()` no longer waits on a Postgres transaction. It stores the blob,
//! writes a durable pending marker via [`smartfs_store::PendingQueue`], and
//! returns; a single background consumer drains the queue into Postgres.
//!
//! **Where durability actually lives.** In the `rename()` inside
//! `PendingQueue::enqueue`, and nowhere else. The in-memory queue here is only
//! an ordering optimization — losing the whole process loses nothing, because
//! everything it held is recoverable by rescanning `pending/queue/`
//! (decision point 2). Treat any code that makes the RAM queue load-bearing as
//! a bug.
//!
//! **Ordering of the acknowledgement.** Decision point 2 says the marker
//! reference joins the RAM queue after `release()` returns, and point 6 says a
//! full queue blocks the caller rather than dropping the write. Those two only
//! reconcile one way: reserve the queue slot *first* (blocking, with the
//! timeout from §Rozstrzygnięcia #1), then `rename()`, then acknowledge, then
//! hand the reserved slot the filename. Reserving first is what lets a full
//! queue answer `EAGAIN` while nothing has been made durable and nothing has
//! been acknowledged — reserving later would mean refusing a write we had
//! already promised to keep.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use smartfs_db::PgPool;
use smartfs_schema::error::{Result, SmartFsError};
use smartfs_schema::PendingMarker;
use smartfs_store::{BlobStore, PendingQueue};
use uuid::Uuid;
use tokio::sync::{mpsc, Notify};

/// Reads the periodic scan interval, once.
fn scan_interval_from_env() -> Duration {
    std::env::var("SMARTFS_PENDING_SCAN_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_SCAN_INTERVAL)
}

/// Fraction of total system RAM budgeted for the queue (decision point 5:
/// a fraction of *total* memory, fixed at startup, never of momentary free
/// memory — the same call `shared_buffers` makes).
const RAM_FRACTION_DENOMINATOR: u64 = 64;

/// Bytes charged per queued entry: a ~90-byte filename plus channel and
/// allocator overhead, rounded up hard. Only used to turn the RAM budget into
/// an entry count.
const BYTES_PER_ENTRY: u64 = 256;

/// The hard ceiling from decision point 5. On any ordinary machine this, not
/// the RAM fraction, is the operative limit; the fraction only bites on very
/// small systems.
const MAX_ENTRIES_CEILING: usize = 100_000;

/// Floor, so a misread `/proc/meminfo` can never produce a queue of zero.
const MIN_ENTRIES: usize = 1_024;

/// Default for §Rozstrzygnięcia #1.
const DEFAULT_BLOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the background scan of decision point 4 re-reads `pending/queue/`.
const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_secs(60);

/// @id: 751a6ca3-4434-40db-9b2f-0299d017bcb5
/// Startup-computed bounds for the pending pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingLimits {
    /// Maximum entries the RAM queue holds before writers are made to wait.
    pub max_entries: usize,
    /// How long a writer waits for a slot before the write is refused.
    pub block_timeout: Duration,
}

/// @id: c9cfe0e8-59d1-4de3-aeaa-84d2659bc110
impl PendingLimits {
    /// @id: 46a78738-d96c-4c16-a62a-5e2f8fc6b8f1
    /// Computes the limits once, from total RAM and the environment.
    ///
    /// `SMARTFS_PENDING_QUEUE_MAX` overrides the entry count and
    /// `SMARTFS_PENDING_BLOCK_TIMEOUT_MS` the wait. Both are read once here,
    /// never re-read at runtime — decision point 5 is explicit that a limit
    /// recomputed under memory pressure would be stale before it could act.
    pub fn from_env() -> Self {
        let max_entries = std::env::var("SMARTFS_PENDING_QUEUE_MAX")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or_else(|| Self::entries_for_ram(read_total_ram_bytes()));

        let block_timeout = std::env::var("SMARTFS_PENDING_BLOCK_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_BLOCK_TIMEOUT);

        Self {
            max_entries,
            block_timeout,
        }
    }

    /// @id: 70b0020e-7e8d-4088-9474-dbd6fe198326
    /// Turns a total-RAM figure into an entry budget, clamped both ways.
    pub fn entries_for_ram(total_ram_bytes: u64) -> usize {
        let budget = total_ram_bytes / RAM_FRACTION_DENOMINATOR / BYTES_PER_ENTRY;
        (budget as usize).clamp(MIN_ENTRIES, MAX_ENTRIES_CEILING)
    }
}

/// @id: 1bc6cfbd-f76e-47ba-a415-bbf792d19196
/// Reads `MemTotal` from `/proc/meminfo`.
///
/// Falls back to 1 GiB when the file cannot be read or parsed, which lands on
/// [`MIN_ENTRIES`] — a small queue is a safe answer to "how much memory is
/// there", an unbounded one is not.
pub fn read_total_ram_bytes() -> u64 {
    const FALLBACK: u64 = 1024 * 1024 * 1024;
    let text = match std::fs::read_to_string("/proc/meminfo") {
        Ok(t) => t,
        Err(_) => return FALLBACK,
    };
    text.lines()
        .find_map(|line| {
            let rest = line.strip_prefix("MemTotal:")?;
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            Some(kb * 1024)
        })
        .unwrap_or(FALLBACK)
}

/// @id: a4f6eb30-db3a-4304-a3be-54dc0dff9ad8
/// What an uncommitted write already looks like to a reader.
///
/// Published the moment a marker becomes durable and withdrawn when its row
/// lands, so the window in which the database is behind is never observable
/// through the mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingView {
    /// Blob holding the new content. Readers fetch this instead of
    /// `inode_registry.current_blob_id`.
    pub blob_id: Option<Uuid>,
    /// Plaintext length, so `stat` reports the size just written.
    pub size: i64,
    /// SHA-256 of the plaintext, for callers verifying what they will get.
    pub content_hash: String,
    /// Marker this view came from, so the drain withdraws the right one.
    pub file_name: String,
}

/// @id: 45c003de-02c6-435d-ba71-a293c6b7d3e9
/// Read-through overlay of writes acknowledged but not yet committed.
///
/// Keyed by inode and holding only the newest uncommitted write per inode:
/// consecutive overwrites of one file each supersede the last, which is exactly
/// what a reader should see. Withdrawal is keyed by marker filename so a stale
/// drain result cannot retract a newer write that arrived meanwhile.
#[derive(Debug, Default)]
struct PendingOverlay {
    by_inode: Mutex<HashMap<Uuid, PendingView>>,
}

/// @id: 51a244ad-963c-4bc4-8fea-efee83c9faa5
impl PendingOverlay {
    fn publish(&self, inode_id: Uuid, view: PendingView) {
        if let Ok(mut map) = self.by_inode.lock() {
            map.insert(inode_id, view);
        }
    }

    fn get(&self, inode_id: Uuid) -> Option<PendingView> {
        self.by_inode.lock().ok()?.get(&inode_id).cloned()
    }

    /// Removes the entry only if it still refers to `file_name`. A later write
    /// to the same inode must survive an earlier marker finishing its commit.
    fn withdraw(&self, inode_id: Uuid, file_name: &str) {
        if let Ok(mut map) = self.by_inode.lock() {
            if map.get(&inode_id).is_some_and(|v| v.file_name == file_name) {
                map.remove(&inode_id);
            }
        }
    }

    fn len(&self) -> usize {
        self.by_inode.lock().map(|m| m.len()).unwrap_or(0)
    }
}

/// @id: 2f7c1d68-5b93-4a20-8e64-c1d0f3a75b28
/// Bounds writes accepted but not yet checkpointed (decision points 5 and 6).
///
/// A counter rather than a semaphore because the count is seeded from disk at
/// startup and must never drift above `max`: `Semaphore::add_permits` would
/// raise the ceiling permanently the first time a scan cleared a marker this
/// process never admitted.
#[derive(Debug)]
struct Gate {
    outstanding: AtomicUsize,
    max: usize,
    notify: Notify,
}

/// @id: da1f7fff-2663-4931-888b-9a3046fd0e22
impl Gate {
    fn new(max: usize, seeded: usize) -> Self {
        Self {
            outstanding: AtomicUsize::new(seeded),
            max,
            notify: Notify::new(),
        }
    }

    fn depth(&self) -> usize {
        self.outstanding.load(Ordering::Acquire)
    }

    /// Takes a slot if one is free, without waiting.
    fn try_admit(&self) -> bool {
        self.outstanding
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.max).then_some(n + 1)
            })
            .is_ok()
    }

    /// Waits up to `timeout` for a slot.
    async fn admit(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Register interest before re-checking, so a release that lands
            // between the check and the wait cannot be missed.
            let waiter = self.notify.notified();
            tokio::pin!(waiter);
            waiter.as_mut().enable();

            if self.try_admit() {
                return true;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return false;
            }
            if tokio::time::timeout(deadline - now, waiter).await.is_err() {
                // One last look: a slot may have freed as the clock ran out.
                return self.try_admit();
            }
        }
    }

    /// Gives a slot back. Called once per marker actually checkpointed.
    fn release(&self) {
        let prev = self
            .outstanding
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_sub(1))
            });
        debug_assert!(prev.is_ok());
        self.notify.notify_waiters();
    }
}

/// @id: 3646cb49-4ecd-46bd-9e6e-3c38eef7b1d8
/// Producer side of the pipeline, held by the FUSE filesystem.
///
/// Cheap to clone; every clone addresses the same queue and the same drain.
///
/// No `Debug`: `dyn BlobStore` has none, and a derived one would print nothing
/// useful about a trait object anyway.
#[derive(Clone)]
pub struct PendingPipeline {
    queue: PendingQueue,
    /// Needed by the drain to reconcile the provisional blob a write left
    /// behind: on a dedup hit it is redundant and gets deleted, and if the
    /// canonical bytes are missing it is what heals them (ADR-62 phase C).
    store: Arc<dyn BlobStore + Send + Sync>,
    tx: mpsc::UnboundedSender<String>,
    seq: Arc<AtomicU64>,
    gate: Arc<Gate>,
    overlay: Arc<PendingOverlay>,
    /// Markers handed to the drain and not yet finished with.
    ///
    /// Without this the periodic scan would re-send every queued marker on
    /// every pass, and a stalled drain would grow the channel without bound —
    /// trading a bounded disk backlog for an unbounded memory one.
    inflight: Arc<Mutex<HashSet<String>>>,
    limits: PendingLimits,
}

/// @id: a246a0bb-0e35-4473-a21e-e74fc53b798e
impl PendingPipeline {
    /// @id: 8a0cdf63-390a-4371-83f2-63352e4c4e58
    /// Starts the pipeline: binds the queue directory, seeds the sequence
    /// counter above anything already on disk, and spawns the single drain
    /// consumer on `rt`.
    ///
    /// Seeding above the highest sequence found on disk matters: markers left
    /// by a previous run must keep sorting before anything this run enqueues,
    /// or FIFO order breaks across a restart.
    pub async fn start(
        pool: PgPool,
        store_root: impl AsRef<Path>,
        store: Arc<dyn BlobStore + Send + Sync>,
        rt: &tokio::runtime::Handle,
        limits: PendingLimits,
    ) -> Result<Self> {
        let queue = PendingQueue::new(store_root);
        queue.ensure_dirs().await?;

        let queued = queue.list().await?;
        let highest = queued
            .last()
            .and_then(|name| PendingMarker::parse_file_name(name).map(|(seq, _)| seq))
            .unwrap_or(0);

        // Seed the gate from what is already queued, so restarting into a
        // backlog does not hand writers a fresh full allowance. Seeding above
        // the maximum is intentional and correct: writes stay refused until the
        // backlog drains back under the limit.
        let backlog = queued.len();
        let gate = Arc::new(Gate::new(limits.max_entries, backlog));

        // Unbounded: capacity is governed by the gate, which only frees a slot
        // once a marker is actually checkpointed.
        let (tx, rx) = mpsc::unbounded_channel::<String>();

        let inflight = Arc::new(Mutex::new(HashSet::new()));
        let overlay = Arc::new(PendingOverlay::default());

        let drain_queue = queue.clone();
        let drain_gate = Arc::clone(&gate);
        let drain_inflight = Arc::clone(&inflight);
        let drain_overlay = Arc::clone(&overlay);
        let drain_store = Arc::clone(&store);
        rt.spawn(async move {
            drain_loop(
                pool,
                drain_queue,
                drain_store,
                drain_gate,
                drain_inflight,
                drain_overlay,
                rx,
            )
            .await
        });

        let pipeline = Self {
            queue,
            store,
            tx,
            seq: Arc::new(AtomicU64::new(highest + 1)),
            gate,
            overlay,
            inflight,
            limits,
        };

        // Rebuild the overlay from disk before anything can read, so a restart
        // with a backlog does not serve the pre-crash content of files whose
        // writes were already acknowledged.
        pipeline.rebuild_overlay(&queued).await?;

        // Decision point 4, first half: the startup scan is mandatory, and it
        // runs before this function returns so the daemon cannot declare
        // readiness while a backlog sits unqueued.
        let replayed = pipeline.rescan().await?;

        // Second half: keep rescanning, so a marker whose commit failed is
        // retried rather than waiting for the next restart.
        let scan_pipeline = pipeline.clone();
        let interval = scan_interval_from_env();
        rt.spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                match scan_pipeline.rescan().await {
                    Ok(n) if n > 0 => {
                        tracing::warn!(requeued = n, "periodic scan found uncommitted markers")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::error!("periodic pending scan failed: {e}"),
                }
            }
        });

        tracing::info!(
            max_entries = limits.max_entries,
            block_timeout_ms = limits.block_timeout.as_millis() as u64,
            scan_interval_s = interval.as_secs(),
            resume_seq_above = highest,
            backlog,
            replayed,
            "pending pipeline started"
        );

        Ok(pipeline)
    }

    /// @id: 6607534f-7f9e-4438-b4c7-5875ce4b7797
    /// The queue directory, for the scan and for status reporting.
    pub fn queue(&self) -> &PendingQueue {
        &self.queue
    }

    /// @id: 113913e7-200e-42fd-b191-9e09d61a3e4c
    /// Bounds this pipeline was started with.
    pub fn limits(&self) -> PendingLimits {
        self.limits
    }

    /// @id: 0500ab41-3d14-4956-80d4-457c05899f8a
    /// Takes the next FIFO sequence number.
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// @id: 36cf00be-6f40-4e4b-877f-25f622225e5a
    /// Writes accepted but not yet checkpointed. This is what the limit bounds
    /// and what `smartfs-cli status` reports as queue occupancy.
    pub fn ram_depth(&self) -> usize {
        self.gate.depth()
    }

    /// @id: bfb4ecb7-c77e-4756-a2d9-e94fcf54e3a1
    /// Stage 1 of the commit: make the write durable, then hand it to the drain.
    ///
    /// Returns only once the marker is on disk and fsynced, so a caller may
    /// treat `Ok(())` as "this write survives a crash". On a full queue it
    /// waits up to [`PendingLimits::block_timeout`] and then returns
    /// [`SmartFsError::PendingQueueFull`], which `error_to_errno` maps to
    /// `EAGAIN` — nothing was made durable and nothing was acknowledged, so
    /// the refusal loses no data (decision point 6).
    pub async fn submit(&self, marker: &PendingMarker) -> Result<()> {
        // Take the slot before making anything durable — see the module docs.
        if !self.gate.admit(self.limits.block_timeout).await {
            tracing::warn!(
                max_entries = self.limits.max_entries,
                outstanding = self.gate.depth(),
                waited_ms = self.limits.block_timeout.as_millis() as u64,
                "pending queue full; refusing the write with EAGAIN"
            );
            return Err(SmartFsError::PendingQueueFull);
        }

        let file_name = marker.file_name();
        if let Err(e) = self.queue.enqueue(marker).await {
            // Nothing became durable, so the slot must go back or it leaks.
            self.gate.release();
            return Err(e);
        }

        // Durable now, so readers must see it. Published before the drain is
        // told about it: the other order leaves a window where the row is
        // already committed and withdrawn while the overlay still lacks it.
        self.overlay.publish(
            marker.inode_id,
            PendingView {
                blob_id: marker.blob_id,
                size: marker.size,
                content_hash: marker.content_hash.clone(),
                file_name: file_name.clone(),
            },
        );

        self.mark_inflight(file_name.clone());
        if self.tx.send(file_name.clone()).is_err() {
            self.clear_inflight(&file_name);
            // The drain is gone. The marker is durable and the periodic scan
            // will still find it, so this is not data loss — but the caller
            // must not be told the pipeline is healthy.
            tracing::error!("pending drain is gone; marker left for the scan to replay");
            return Err(SmartFsError::Store(
                "pending drain is not running; write is durable but uncommitted".to_string(),
            ));
        }
        Ok(())
    }

    /// @id: d38f0a52-6c71-4e94-b5a3-0197fe28cd46
    /// The blob store this pipeline drains against.
    pub fn store(&self) -> &Arc<dyn BlobStore + Send + Sync> {
        &self.store
    }

    /// @id: fe5886be-9f14-4b15-b473-495b13742fbe
    /// The newest uncommitted write for `inode_id`, if there is one.
    ///
    /// The read and getattr paths call this before falling back to
    /// `inode_registry`, which is what keeps read-after-write intact across the
    /// window between acknowledgement and commit.
    pub fn view_of(&self, inode_id: Uuid) -> Option<PendingView> {
        self.overlay.get(inode_id)
    }

    /// @id: c5bbe7db-1633-48e5-8e2d-41b13791e7be
    /// Inodes currently shadowed by an uncommitted write.
    pub fn overlay_depth(&self) -> usize {
        self.overlay.len()
    }

    /// @id: de72cbdc-9f27-4c16-9db5-5c8c6f6b1b46
    /// Repopulates the overlay from markers already on disk.
    ///
    /// Runs during `start`, before the daemon can serve anything: markers left
    /// by a previous run describe writes their callers were already told had
    /// succeeded, so serving the pre-crash content for them would be the same
    /// violation the overlay exists to prevent.
    ///
    /// `names` is FIFO-ordered, so later markers overwrite earlier ones per
    /// inode and the newest write wins.
    async fn rebuild_overlay(&self, names: &[String]) -> Result<()> {
        let mut restored = 0usize;
        for name in names {
            if let Some(marker) = self.queue.read(name).await? {
                self.overlay.publish(
                    marker.inode_id,
                    PendingView {
                        blob_id: marker.blob_id,
                        size: marker.size,
                        content_hash: marker.content_hash,
                        file_name: name.clone(),
                    },
                );
                restored += 1;
            }
        }
        if restored > 0 {
            tracing::info!(restored, "rebuilt the pending read overlay from disk");
        }
        Ok(())
    }

    /// @id: 20b4f9c7-8e51-4a36-bd02-7f1c6a8e35d9
    /// Decision point 4: rescans `pending/queue/` and hands the drain anything
    /// it is not already working on. Returns how many markers were requeued.
    ///
    /// Called once at startup and on a timer afterwards, and exposed so an
    /// operator can force a recovery pass straight after an incident instead of
    /// waiting for the next tick.
    ///
    /// The scan never commits anything itself. All commits stay in the single
    /// drain task, which is what keeps decision point 3's "one sequential
    /// consumer" true — and with it the property that `version_number`,
    /// computed per transaction, follows write order.
    pub async fn rescan(&self) -> Result<usize> {
        let mut requeued = 0usize;
        for name in self.queue.list().await? {
            if !self.mark_inflight(name.clone()) {
                continue; // already handed to the drain
            }
            if self.tx.send(name.clone()).is_err() {
                self.clear_inflight(&name);
                return Err(SmartFsError::Store(
                    "pending drain is not running; cannot replay the queue".to_string(),
                ));
            }
            requeued += 1;
        }
        Ok(requeued)
    }

    /// Records a marker as handed to the drain. Returns false if it already was.
    fn mark_inflight(&self, name: String) -> bool {
        match self.inflight.lock() {
            Ok(mut set) => set.insert(name),
            // A poisoned lock would mean the drain panicked mid-update. Send
            // anyway: a duplicate costs one no-op commit, a dropped marker
            // costs an uncommitted write.
            Err(_) => true,
        }
    }

    fn clear_inflight(&self, name: &str) {
        if let Ok(mut set) = self.inflight.lock() {
            set.remove(name);
        }
    }

    /// @id: 4477e438-ea1a-437b-9d3a-607abd142c72
    /// Waits until the RAM queue is empty, or `timeout` elapses.
    ///
    /// Used on clean shutdown so queued writes reach Postgres before the
    /// process leaves. Returning `false` is not data loss — the markers are
    /// still on disk and the next start replays them — only a slower recovery.
    pub async fn quiesce(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.ram_depth() > 0 {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        true
    }
}

/// @id: 6a201847-b522-4b19-a8af-0227a0a59973
/// The single sequential consumer of decision point 3.
///
/// One task, one marker at a time, in FIFO order. Sequential is not an
/// oversight: it is what keeps `version_number`, computed as `MAX+1` inside
/// each transaction, following write order.
#[allow(clippy::too_many_arguments)]
async fn drain_loop(
    pool: PgPool,
    queue: PendingQueue,
    store: Arc<dyn BlobStore + Send + Sync>,
    gate: Arc<Gate>,
    inflight: Arc<Mutex<HashSet<String>>>,
    overlay: Arc<PendingOverlay>,
    mut rx: mpsc::UnboundedReceiver<String>,
) {
    while let Some(file_name) = rx.recv().await {
        // Read the marker's inode before committing: after a successful commit
        // the marker is unlinked and there is nothing left to look it up from.
        let inode_id = queue
            .read(&file_name)
            .await
            .ok()
            .flatten()
            .map(|m| m.inode_id);

        let outcome = commit_one(&pool, &queue, Some(store.as_ref()), &file_name).await;

        // Clear before acting on the result: a marker whose commit failed must
        // become visible to the next scan, or it would never be retried.
        if let Ok(mut set) = inflight.lock() {
            set.remove(&file_name);
        }

        match outcome {
            // Checkpointed: the row is real, so the overlay must step aside and
            // the slot is genuinely free again.
            Ok(Some(_)) => {
                if let Some(inode_id) = inode_id {
                    overlay.withdraw(inode_id, &file_name);
                }
                gate.release()
            }
            // Someone else already checkpointed it and freed its slot.
            Ok(None) => {}
            Err(e) => {
                // Leave the marker in place and, deliberately, keep holding its
                // slot. It is still durable and the periodic scan will retry
                // it; freeing the slot here would let writes keep being
                // accepted while nothing commits, which is exactly the runaway
                // decision point 6 exists to prevent.
                tracing::error!(
                    "pending commit failed for {file_name}, left queued for retry: {e}"
                );
            }
        }
    }
    tracing::info!("pending drain stopped");
}

/// Commits one marker and checkpoints it by unlinking.
///
/// Re-exported from `smartfs-db`, where it lives so `smartfs-cli` can replay a
/// queue without depending on `fuser`.
pub use smartfs_db::commit_pending_marker as commit_one;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_budget_is_clamped_at_both_ends() {
        // Tiny machine: the fraction bites, but never below the floor.
        assert_eq!(PendingLimits::entries_for_ram(16 * 1024 * 1024), MIN_ENTRIES);
        // Ordinary machine: the hard ceiling is what actually applies.
        assert_eq!(
            PendingLimits::entries_for_ram(24 * 1024 * 1024 * 1024),
            MAX_ENTRIES_CEILING
        );
        // Zero must not yield a queue of zero, which would deadlock every write.
        assert_eq!(PendingLimits::entries_for_ram(0), MIN_ENTRIES);
    }

    #[test]
    fn ram_budget_scales_between_the_clamps() {
        // 512 MiB / 64 / 256 = 32768 entries, inside both bounds.
        assert_eq!(PendingLimits::entries_for_ram(512 * 1024 * 1024), 32_768);
    }

    #[test]
    fn total_ram_is_readable_and_sane() {
        let ram = read_total_ram_bytes();
        assert!(
            ram >= 64 * 1024 * 1024,
            "MemTotal parsed as {ram} bytes, which cannot be right"
        );
    }

    #[test]
    fn limits_come_from_the_environment_when_set() {
        // Uses the same parsing path as from_env without mutating the shared
        // process environment, which would race other tests.
        assert_eq!(PendingLimits::entries_for_ram(u64::MAX), MAX_ENTRIES_CEILING);
        assert_eq!(DEFAULT_BLOCK_TIMEOUT, Duration::from_secs(30));
    }
}
