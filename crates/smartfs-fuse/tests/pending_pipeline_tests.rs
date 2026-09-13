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
    let inserted = smartfs_fuse::commit_one(&pool, pipeline.queue(), &m.file_name())
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
    // The drain here cannot make progress — the inode does not exist, so every
    // transaction fails the foreign key. This is the case a channel-based bound
    // gets wrong: it would free a slot on each error and let writes be accepted
    // forever while nothing commits. The gate holds the slot instead.
    let pool = pool().await;
    let store = TempDir::new().unwrap();

    let pipeline = PendingPipeline::start(
        pool.clone(),
        store.path(),
        &tokio::runtime::Handle::current(),
        PendingLimits {
            max_entries: 1,
            block_timeout: Duration::from_millis(300),
        },
    )
    .await
    .expect("pipeline failed to start");

    let orphan = Uuid::new_v4();
    let mut refused = false;
    for i in 0..8 {
        let m = marker(pipeline.next_seq(), orphan, &format!("body {i}"));
        match pipeline.submit(&m).await {
            Ok(()) => {}
            Err(smartfs_schema::SmartFsError::PendingQueueFull) => {
                refused = true;
                break;
            }
            Err(e) => panic!("expected PendingQueueFull, got {e}"),
        }
    }

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
    let pool = pool().await;
    let store = TempDir::new().unwrap();

    let pipeline = PendingPipeline::start(
        pool.clone(),
        store.path(),
        &tokio::runtime::Handle::current(),
        PendingLimits {
            max_entries: 64,
            block_timeout: Duration::from_millis(200),
        },
    )
    .await
    .expect("pipeline failed to start");

    // An orphan inode means every commit fails, so markers stay queued.
    let orphan = Uuid::new_v4();
    for i in 0..4 {
        let m = marker(pipeline.next_seq(), orphan, &format!("stuck {i}"));
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
}
