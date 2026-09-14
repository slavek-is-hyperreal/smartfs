//! Integration tests for the ADR-58 two-stage commit pipeline.
//!
//! These need a live Postgres, so they are `#[ignore]` by default and opted in
//! with `cargo test -p smartfs-fuse -- --ignored`. They are deliberately NOT
//! written as
//!
//! ```ignore
//! let pool = match connect_pool(&url).await { Ok(p) => p, Err(_) => return };
//! ```
//!
//! which is the anti-pattern that let B-04 ship green: with no database that
//! test executes zero assertions and `cargo test` prints it as a pass. Here an
//! unreachable database panics. `ignored` is an honest outcome; a pass that
//! asserted nothing is not.
//!
//! Point them at a scratch database — they insert rows and delete them again,
//! but they are not written to be safe against a database you care about:
//!
//! ```sh
//! SMARTFS_TEST_DATABASE_URL=postgres://postgres:postgres@172.17.0.2:5432/smartfs_pipeline_test \
//!   cargo test -p smartfs-fuse -- --ignored
//! ```

use std::time::Duration;

use smartfs_db::PgPool;
use smartfs_fuse::{PendingLimits, PendingPipeline};
use smartfs_schema::PendingMarker;
use smartfs_store::{BlobStore, LocalDiskStore};
use tempfile::TempDir;
use uuid::Uuid;

fn db_url() -> String {
    std::env::var("SMARTFS_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| "postgres://postgres:postgres@172.17.0.2:5432/smartfs".to_string())
}

/// Connects, or fails the test loudly. Never returns early on error.
async fn pool() -> PgPool {
    let url = db_url();
    smartfs_db::connect_pool(&url).await.unwrap_or_else(|e| {
        panic!(
            "cannot reach Postgres at {}: {e}\n\
             This is a FAILURE of this test, not a reason to skip it. Point \
             SMARTFS_TEST_DATABASE_URL at a scratch database and re-run.",
            url.split('@').next_back().unwrap_or("<url>")
        )
    })
}

/// Creates a throwaway inode to hang versions off, returning its id.
async fn make_inode(pool: &PgPool) -> Uuid {
    let name = format!("pipeline-test-{}.txt", Uuid::new_v4());
    let root: Uuid = sqlx::query_scalar("SELECT id FROM inode_registry WHERE ino = 1")
        .fetch_one(pool)
        .await
        .expect("no root inode (ino = 1); migrations 001-006 are not applied");

    smartfs_db::inode_create(pool, Some(root), &name, false, 0, 0, 0o100644)
        .await
        .expect("inode_create failed")
        .id
}

async fn cleanup(pool: &PgPool, inode_id: Uuid) {
    let _ = sqlx::query("DELETE FROM file_versions WHERE inode_id = $1")
        .bind(inode_id)
        .execute(pool)
        .await;
    let _ = smartfs_db::inode_delete(pool, inode_id).await;
}

fn marker(seq: u64, inode_id: Uuid, content: &str) -> PendingMarker {
    let hash = smartfs_compress::hash_bytes(content.as_bytes()).0;
    PendingMarker {
        seq,
        inode_id,
        parent_inode: None,
        name: "pipeline.txt".to_string(),
        version_id: Uuid::new_v4(),
        content_hash: hash,
        blob_id: Some(Uuid::new_v4()),
        size: content.len() as i64,
        compressed_size: None,
        shared: true,
        external_path: None,
        mode: 0o100644,
        uid: 0,
        gid: 0,
        special_type: Some("generic".to_string()),
        special_data: None,
        ast_nodes: serde_json::json!([]),
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

async fn versions_of(pool: &PgPool, inode_id: Uuid) -> Vec<(i32, String)> {
    sqlx::query_as::<_, (i32, String)>(
        "SELECT version_number, content_hash FROM file_versions
         WHERE inode_id = $1 ORDER BY version_number",
    )
    .bind(inode_id)
    .fetch_all(pool)
    .await
    .expect("version query failed")
}

/// Waits for the drain to reach `want` rows, then returns them. Panics on
/// timeout rather than asserting on a half-drained queue.
async fn await_versions(pool: &PgPool, inode_id: Uuid, want: usize) -> Vec<(i32, String)> {
    for _ in 0..200 {
        let rows = versions_of(pool, inode_id).await;
        if rows.len() >= want {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "drain did not produce {want} file_versions rows within 5s (got {})",
        versions_of(pool, inode_id).await.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn submitted_markers_reach_postgres_in_write_order() {
    let pool = pool().await;
    let store = TempDir::new().unwrap();
    let inode_id = make_inode(&pool).await;

    let pipeline = PendingPipeline::start(
        pool.clone(),
        store.path(),
        std::sync::Arc::new(LocalDiskStore::new(store.path())),
        &tokio::runtime::Handle::current(),
        PendingLimits::from_env(),
    )
    .await
    .expect("pipeline failed to start");

    let bodies = ["first revision", "second revision", "third revision"];
    for body in bodies {
        let m = marker(pipeline.next_seq(), inode_id, body);
        pipeline.submit(&m).await.expect("submit failed");
    }

    let rows = await_versions(&pool, inode_id, 3).await;

    // Root Invariant #2: contiguous 1..n, one row per write.
    assert_eq!(
        rows.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "version numbers must be contiguous 1..n even though they are assigned \
         in the drain, not at submit time"
    );

    // The single FIFO consumer must preserve write order.
    let expected: Vec<String> = bodies
        .iter()
        .map(|b| smartfs_compress::hash_bytes(b.as_bytes()).0)
        .collect();
    assert_eq!(
        rows.iter().map(|(_, h)| h.clone()).collect::<Vec<_>>(),
        expected,
        "drain order must be write order"
    );

    // Checkpointed: nothing left queued once committed.
    assert_eq!(pipeline.queue().depth().await.unwrap(), 0);

    cleanup(&pool, inode_id).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn submit_is_durable_before_the_drain_runs() {
    // The contract release() relies on: once submit() returns, the marker is on
    // disk, whether or not Postgres has seen it.
    let pool = pool().await;
    let store = TempDir::new().unwrap();
    let inode_id = make_inode(&pool).await;

    let pipeline = PendingPipeline::start(
        pool.clone(),
        store.path(),
        std::sync::Arc::new(LocalDiskStore::new(store.path())),
        &tokio::runtime::Handle::current(),
        PendingLimits::from_env(),
    )
    .await
    .expect("pipeline failed to start");

    let m = marker(pipeline.next_seq(), inode_id, "durable before commit");
    pipeline.submit(&m).await.expect("submit failed");

    let on_disk = std::fs::read_dir(pipeline.queue().queue_dir()).unwrap().count();
    let committed = versions_of(&pool, inode_id).await.len();
    assert!(
        on_disk == 1 || committed == 1,
        "after submit the write must exist either as a durable marker or as a \
         committed row — never as neither"
    );

    await_versions(&pool, inode_id, 1).await;
    cleanup(&pool, inode_id).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn replaying_a_committed_marker_is_a_no_op() {
    // The exact state a crash between COMMIT and unlink leaves behind.
    let pool = pool().await;
    let store = TempDir::new().unwrap();
    let inode_id = make_inode(&pool).await;

    let pipeline = PendingPipeline::start(
        pool.clone(),
        store.path(),
        std::sync::Arc::new(LocalDiskStore::new(store.path())),
        &tokio::runtime::Handle::current(),
        PendingLimits::from_env(),
    )
    .await
    .expect("pipeline failed to start");

    let m = marker(pipeline.next_seq(), inode_id, "replay me");
    pipeline.submit(&m).await.expect("submit failed");
    await_versions(&pool, inode_id, 1).await;

    // Put the marker back, as a crash would have left it, and replay.
    pipeline.queue().enqueue(&m).await.expect("re-enqueue failed");
    let inserted = smartfs_fuse::commit_one(&pool, pipeline.queue(), None, &m.file_name())
        .await
        .expect("replay failed");

    assert_eq!(
        inserted,
        Some(false),
        "replay must checkpoint the marker (Some) while inserting nothing (false)"
    );
    assert_eq!(
        versions_of(&pool, inode_id).await.len(),
        1,
        "replaying a committed marker must not create a second version"
    );
    assert_eq!(
        pipeline.queue().depth().await.unwrap(),
        0,
        "replay must still checkpoint the marker away"
    );

    cleanup(&pool, inode_id).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn a_full_queue_refuses_with_pending_queue_full() {
    // Decision point 6: back-pressure, never a silent drop.
    //
    // The drain is stalled by closing its pool, so every commit attempt errors.
    // This used to be simulated with an inode that does not exist, which stopped
    // working at 0c36a07: a write to a deleted inode is now correctly discarded
    // rather than retried forever, so the drain made progress and the gate
    // emptied. The assertion is unchanged — only the way the stall is produced.
    //
    // This is the case a channel-based bound gets wrong: it would free a slot on
    // each error and let writes be accepted forever while nothing commits. The
    // gate holds the slot instead.
    let live = pool().await;
    let stalled = pool().await;
    let store = TempDir::new().unwrap();

    let pipeline = PendingPipeline::start(
        stalled.clone(),
        store.path(),
        std::sync::Arc::new(LocalDiskStore::new(store.path())),
        &tokio::runtime::Handle::current(),
        PendingLimits {
            max_entries: 1,
            block_timeout: Duration::from_millis(300),
        },
    )
    .await
    .expect("pipeline failed to start");

    // From here the drain can do nothing at all.
    stalled.close().await;

    let inode = make_inode(&live).await;
    let mut refused = false;
    for i in 0..8 {
        let m = marker(pipeline.next_seq(), inode, &format!("body {i}"));
        match pipeline.submit(&m).await {
            Ok(()) => {}
            Err(smartfs_schema::SmartFsError::PendingQueueFull) => {
                refused = true;
                break;
            }
            Err(e) => panic!("expected PendingQueueFull, got {e}"),
        }
    }

    cleanup(&live, inode).await;
    assert!(
        refused,
        "a queue of one behind a stalled drain must eventually refuse a write \
         with PendingQueueFull rather than growing without bound"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn a_restart_replays_markers_left_by_a_dead_daemon() {
    // Decision point 4: the startup scan is what makes the pending stage a real
    // durability boundary. Simulated by writing markers straight into the queue
    // directory — exactly what a daemon killed between rename() and drain
    // leaves behind — and then starting a pipeline over it.
    let pool = pool().await;
    let store = TempDir::new().unwrap();
    let inode_id = make_inode(&pool).await;

    let queue = smartfs_store::PendingQueue::new(store.path());
    let bodies = ["orphan one", "orphan two"];
    for (i, body) in bodies.iter().enumerate() {
        let m = marker(i as u64 + 1, inode_id, body);
        queue.enqueue(&m).await.expect("enqueue failed");
    }
    assert_eq!(queue.depth().await.unwrap(), 2);
    assert!(
        versions_of(&pool, inode_id).await.is_empty(),
        "precondition: nothing committed yet"
    );

    // A fresh daemon comes up over the same store.
    let pipeline = PendingPipeline::start(
        pool.clone(),
        store.path(),
        std::sync::Arc::new(LocalDiskStore::new(store.path())),
        &tokio::runtime::Handle::current(),
        PendingLimits::from_env(),
    )
    .await
    .expect("pipeline failed to start");

    let rows = await_versions(&pool, inode_id, 2).await;
    assert_eq!(
        rows.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        vec![1, 2],
        "replayed markers must land in seq order, contiguously"
    );
    assert_eq!(
        queue.depth().await.unwrap(),
        0,
        "replay must checkpoint every marker it commits"
    );

    // The sequence counter must resume above what was on disk, or a new write
    // would sort before the recovered ones and break FIFO across the restart.
    assert!(
        pipeline.next_seq() > 2,
        "seq must resume above the highest marker found on disk"
    );

    cleanup(&pool, inode_id).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn rescan_does_not_requeue_what_is_already_in_flight() {
    // Guards the failure mode that makes a periodic scan dangerous: with a
    // stalled drain, re-sending every marker on every pass turns a bounded disk
    // backlog into an unbounded memory one.
    //
    // Stall produced by closing the drain's pool — see the note in
    // a_full_queue_refuses_with_pending_queue_full for why an orphan inode no
    // longer stalls anything.
    let live = pool().await;
    let pool = pool().await;
    let store = TempDir::new().unwrap();

    let pipeline = PendingPipeline::start(
        pool.clone(),
        store.path(),
        std::sync::Arc::new(LocalDiskStore::new(store.path())),
        &tokio::runtime::Handle::current(),
        PendingLimits {
            max_entries: 64,
            block_timeout: Duration::from_millis(200),
        },
    )
    .await
    .expect("pipeline failed to start");

    pool.close().await;

    let inode = make_inode(&live).await;
    for i in 0..4 {
        let m = marker(pipeline.next_seq(), inode, &format!("stuck {i}"));
        let _ = pipeline.submit(&m).await;
    }
    // Let the drain fail each one and drop it from the in-flight set.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let first = pipeline.rescan().await.expect("rescan failed");
    assert!(first > 0, "a failed commit must be visible to the next scan");

    // Immediately again: everything is in flight, so nothing may be re-sent.
    let second = pipeline.rescan().await.expect("rescan failed");
    assert_eq!(
        second, 0,
        "a marker already handed to the drain must not be requeued by the very \
         next scan"
    );

    cleanup(&live, inode).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn an_uncommitted_write_is_visible_to_readers() {
    // Read-after-write. Acknowledging before the drain commits leaves
    // inode_registry describing the PREVIOUS version, so without the overlay a
    // fresh open() would serve stale bytes and stat() a stale size to the very
    // process that just wrote. Both are outright POSIX violations.
    let pool = pool().await;
    let store = TempDir::new().unwrap();
    let inode_id = make_inode(&pool).await;

    // Drain cannot progress: markers stay uncommitted for the whole test, which
    // is exactly the window being checked.
    let pipeline = PendingPipeline::start(
        pool.clone(),
        store.path(),
        std::sync::Arc::new(LocalDiskStore::new(store.path())),
        &tokio::runtime::Handle::current(),
        PendingLimits {
            max_entries: 8,
            block_timeout: Duration::from_millis(200),
        },
    )
    .await
    .expect("pipeline failed to start");

    assert!(
        pipeline.view_of(inode_id).is_none(),
        "nothing written yet, so nothing to shadow"
    );

    let body = "content a reader must see immediately";
    let m = marker(pipeline.next_seq(), inode_id, body);
    pipeline.submit(&m).await.expect("submit failed");

    let view = pipeline
        .view_of(inode_id)
        .expect("an acknowledged write must be visible before it is committed");
    assert_eq!(view.blob_id, m.blob_id, "reads must reach the new blob");
    assert_eq!(view.size, body.len() as i64, "stat must report the new size");
    assert_eq!(view.content_hash, m.content_hash);

    // Overwrite: the newer write supersedes the older for the same inode.
    let body2 = "and then a second, newer revision";
    let m2 = marker(pipeline.next_seq(), inode_id, body2);
    pipeline.submit(&m2).await.expect("submit failed");
    let view2 = pipeline.view_of(inode_id).expect("still shadowed");
    assert_eq!(
        view2.content_hash, m2.content_hash,
        "the newest uncommitted write is the one a reader should see"
    );

    cleanup(&pool, inode_id).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn the_overlay_steps_aside_once_the_row_is_real() {
    let pool = pool().await;
    let store = TempDir::new().unwrap();
    let inode_id = make_inode(&pool).await;

    let pipeline = PendingPipeline::start(
        pool.clone(),
        store.path(),
        std::sync::Arc::new(LocalDiskStore::new(store.path())),
        &tokio::runtime::Handle::current(),
        PendingLimits::from_env(),
    )
    .await
    .expect("pipeline failed to start");

    let m = marker(pipeline.next_seq(), inode_id, "committed shortly");
    pipeline.submit(&m).await.expect("submit failed");
    await_versions(&pool, inode_id, 1).await;

    // The database is authoritative again, so the overlay must not keep
    // shadowing it — a stale entry would pin readers to an old blob forever.
    for _ in 0..100 {
        if pipeline.view_of(inode_id).is_none() {
            cleanup(&pool, inode_id).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("overlay still shadows the inode after its version was committed");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn a_restart_rebuilds_the_overlay_before_serving() {
    // Markers left by a dead daemon describe writes their callers were already
    // told had succeeded. Serving pre-crash content for them is the same
    // violation, just after a restart.
    let pool = pool().await;
    let store = TempDir::new().unwrap();
    let inode_id = make_inode(&pool).await;

    let queue = smartfs_store::PendingQueue::new(store.path());
    let m = marker(1, inode_id, "survived the crash");
    queue.enqueue(&m).await.expect("enqueue failed");

    // Point the drain at a dead database so the marker cannot be committed
    // away before the assertion — the overlay must carry it regardless.
    let stalled = smartfs_db::connect_pool("postgres://postgres:postgres@127.0.0.1:1/nope")
        .await
        .err();
    assert!(stalled.is_some(), "expected the bogus URL to be unreachable");

    let pipeline = PendingPipeline::start(
        pool.clone(),
        store.path(),
        std::sync::Arc::new(LocalDiskStore::new(store.path())),
        &tokio::runtime::Handle::current(),
        PendingLimits::from_env(),
    )
    .await
    .expect("pipeline failed to start");

    // start() rebuilds the overlay before returning, so this holds even if the
    // drain has already raced ahead and committed.
    let seen_shadow = pipeline.view_of(inode_id).is_some();
    let committed = !versions_of(&pool, inode_id).await.is_empty();
    assert!(
        seen_shadow || committed,
        "after a restart the write must be readable either from the overlay or \
         from a committed row — never from neither"
    );

    await_versions(&pool, inode_id, 1).await;
    cleanup(&pool, inode_id).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn deferred_dedup_collapses_a_duplicate_and_removes_its_copy() {
    // ADR-62 phase C: the writer no longer knows whether identical content
    // exists, so it always writes its own blob and the drain decides. The cost
    // of that deferral is a wasted write on duplicates — this proves the waste
    // is actually reclaimed rather than left lying around.
    let pool = pool().await;
    let dir = TempDir::new().unwrap();
    let disk = std::sync::Arc::new(LocalDiskStore::new(dir.path()));
    let inode_a = make_inode(&pool).await;
    let inode_b = make_inode(&pool).await;

    let pipeline = PendingPipeline::start(
        pool.clone(),
        dir.path(),
        disk.clone(),
        &tokio::runtime::Handle::current(),
        PendingLimits::from_env(),
    )
    .await
    .expect("pipeline failed to start");

    // Unique per run. With a fixed string the first write would be deduped onto
    // a blob left by an earlier run of this very test, and "the first write's
    // blob becomes canonical" would fail for a reason that has nothing to do
    // with the code — the shared-database coupling this suite criticises
    // elsewhere.
    let body = &format!("identical content written twice, run {}", Uuid::new_v4());

    let mut first = marker(pipeline.next_seq(), inode_a, body);
    first.blob_id = Some(Uuid::new_v4());
    disk.put(first.blob_id.unwrap(), b"payload-a").await.unwrap();
    pipeline.submit(&first).await.unwrap();
    await_versions(&pool, inode_a, 1).await;

    let mut second = marker(pipeline.next_seq(), inode_b, body);
    second.blob_id = Some(Uuid::new_v4());
    disk.put(second.blob_id.unwrap(), b"payload-b").await.unwrap();
    pipeline.submit(&second).await.unwrap();
    await_versions(&pool, inode_b, 1).await;

    // Both versions must point at ONE blob — the first one written.
    let blob_of = |inode| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT blob_id FROM file_versions WHERE inode_id = $1 AND blob_id IS NOT NULL",
            )
            .bind(inode)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(
        blob_of(inode_a).await,
        blob_of(inode_b).await,
        "identical content must collapse onto one blob even though dedup is deferred"
    );
    assert_eq!(blob_of(inode_a).await, first.blob_id.unwrap());

    // The second write's provisional copy must be gone.
    for _ in 0..100 {
        if !disk.exists(second.blob_id.unwrap()).await.unwrap() {
            cleanup(&pool, inode_a).await;
            cleanup(&pool, inode_b).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the duplicate's provisional blob was never reclaimed");
}
