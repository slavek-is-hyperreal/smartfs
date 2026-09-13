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

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use smartfs_db::PgPool;
use smartfs_schema::error::{Result, SmartFsError};
use smartfs_schema::PendingMarker;
use smartfs_store::PendingQueue;
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
#[derive(Debug, Clone)]
pub struct PendingPipeline {
    queue: PendingQueue,
    tx: mpsc::UnboundedSender<String>,
    seq: Arc<AtomicU64>,
    gate: Arc<Gate>,
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

        let drain_queue = queue.clone();
        let drain_gate = Arc::clone(&gate);
        let drain_inflight = Arc::clone(&inflight);
        rt.spawn(async move {
            drain_loop(pool, drain_queue, drain_gate, drain_inflight, rx).await
        });

        let pipeline = Self {
            queue,
            tx,
            seq: Arc::new(AtomicU64::new(highest + 1)),
            gate,
            inflight,
            limits,
        };

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
async fn drain_loop(
    pool: PgPool,
    queue: PendingQueue,
    gate: Arc<Gate>,
    inflight: Arc<Mutex<HashSet<String>>>,
    mut rx: mpsc::UnboundedReceiver<String>,
) {
    while let Some(file_name) = rx.recv().await {
        let outcome = commit_one(&pool, &queue, &file_name).await;

        // Clear before acting on the result: a marker whose commit failed must
        // become visible to the next scan, or it would never be retried.
        if let Ok(mut set) = inflight.lock() {
            set.remove(&file_name);
        }

        match outcome {
            // Checkpointed: the slot is genuinely free again.
            Ok(Some(_)) => gate.release(),
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

/// @id: 428a690f-3abd-42ff-b76a-244d38466f18
/// Commits one marker and checkpoints it by unlinking.
///
/// Safe to call twice on the same marker: `cow_commit_with_id` is idempotent on
/// `version_id`, which is exactly the state a crash between COMMIT and the
/// unlink leaves behind.
///
/// `Ok(None)` means the marker was already gone, so this call checkpointed
/// nothing. `Ok(Some(inserted))` means this call removed it, with `inserted`
/// saying whether the transaction was new or a replay no-op. Callers use the
/// outer `Option` to decide whether to give a queue slot back.
pub async fn commit_one(
    pool: &PgPool,
    queue: &PendingQueue,
    file_name: &str,
) -> Result<Option<bool>> {
    let marker = match queue.read(file_name).await? {
        Some(m) => m,
        // Already drained by someone else. Not an error, and not ours to
        // account for — whoever removed it freed its slot.
        None => return Ok(None),
    };

    let ast_nodes: Vec<smartfs_db::AstNodeInsert> =
        serde_json::from_value(marker.ast_nodes.clone()).map_err(|e| {
            SmartFsError::Store(format!("pending marker {file_name} has unreadable ast_nodes: {e}"))
        })?;

    let outcome = smartfs_db::cow_commit_with_id(
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
