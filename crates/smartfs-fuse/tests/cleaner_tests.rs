//! Integration tests for the ADR-62 blob cleaner.
//!
//! Needs a live Postgres, so `#[ignore]` by default:
//!   SMARTFS_TEST_DATABASE_URL=postgres://…/smartfs_cleaner_test \
//!     cargo test -p smartfs-fuse --test cleaner_tests -- --ignored
//!
//! Written to panic on an unreachable database rather than return early. A GC
//! test that quietly executes nothing is worse than no GC test: it would report
//! green for code that deletes data.

use std::time::Duration;

use smartfs_db::PgPool;
use smartfs_fuse::clean_once;
use smartfs_schema::PendingMarker;
use smartfs_store::{BlobStore, LocalDiskStore, PendingQueue};
use tempfile::TempDir;
use uuid::Uuid;

async fn pool() -> PgPool {
    let url = std::env::var("SMARTFS_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@172.17.0.2:5432/smartfs".to_string());
    smartfs_db::connect_pool(&url)
        .await
        .unwrap_or_else(|e| panic!("cannot reach Postgres: {e}. This is a FAILURE, not a skip."))
}

/// Inserts a blob row backdated past any grace window, plus its file.
async fn seed_blob(pool: &PgPool, store: &LocalDiskStore, shared: bool) -> (Uuid, String) {
    let blob_id = Uuid::new_v4();
    let hash = format!("{:0>64}", Uuid::new_v4().simple().to_string());
    smartfs_db::insert_blob_with_sharing(pool, &hash, blob_id, None, 4, shared)
        .await
        .expect("insert_blob");
    sqlx::query("UPDATE blobs SET created_at = NOW() - INTERVAL '2 days' WHERE blob_id = $1")
        .bind(blob_id)
        .execute(pool)
        .await
        .expect("backdate");
    store.put(blob_id, b"data").await.expect("put");
    (blob_id, hash)
}

async fn blob_row_exists(pool: &PgPool, blob_id: Uuid) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM blobs WHERE blob_id = $1")
        .bind(blob_id)
        .fetch_one(pool)
        .await
        .unwrap()
        > 0
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn an_unreferenced_blob_is_reclaimed() {
    let pool = pool().await;
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStore::new(dir.path());
    let queue = PendingQueue::new(dir.path());
    queue.ensure_dirs().await.unwrap();

    let (blob_id, _) = seed_blob(&pool, &store, true).await;
    assert!(store.exists(blob_id).await.unwrap());

    let r = clean_once(&pool, &store, &queue, 60, 500).await.unwrap();

    assert!(r.reclaimed >= 1, "expected at least this blob reclaimed: {r:?}");
    assert!(!store.exists(blob_id).await.unwrap(), "bytes must be gone");
    assert!(!blob_row_exists(&pool, blob_id).await, "row must be gone");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn a_blob_named_by_a_pending_marker_is_never_touched() {
    // The window ADR-58 creates: the write is acknowledged, the blob is on disk,
    // the file_versions row does not exist yet. Deleting here destroys data the
    // caller was told was saved — the single worst thing this component could do.
    let pool = pool().await;
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStore::new(dir.path());
    let queue = PendingQueue::new(dir.path());
    queue.ensure_dirs().await.unwrap();

    let (blob_id, hash) = seed_blob(&pool, &store, true).await;

    let marker = PendingMarker {
        seq: 1,
        inode_id: Uuid::new_v4(),
        parent_inode: None,
        name: "queued.txt".to_string(),
        version_id: Uuid::new_v4(),
        content_hash: hash.clone(),
        blob_id: Some(blob_id),
        size: 4,
        compressed_size: None,
        external_path: None,
        mode: 0o100644,
        uid: 0,
        gid: 0,
        special_type: None,
        special_data: None,
        ast_nodes: serde_json::json!([]),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    queue.enqueue(&marker).await.unwrap();

    let r = clean_once(&pool, &store, &queue, 60, 500).await.unwrap();

    assert!(r.skipped_pending >= 1, "the marker must have protected it: {r:?}");
    assert!(
        store.exists(blob_id).await.unwrap(),
        "an acknowledged write must survive the cleaner"
    );
    assert!(blob_row_exists(&pool, blob_id).await);

    sqlx::query("DELETE FROM blobs WHERE blob_id = $1").bind(blob_id).execute(&pool).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn the_grace_window_protects_a_fresh_blob() {
    // Second, independent protection: a marker not yet written still leaves a
    // young blob, and youth alone must be enough to spare it.
    let pool = pool().await;
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStore::new(dir.path());
    let queue = PendingQueue::new(dir.path());
    queue.ensure_dirs().await.unwrap();

    let blob_id = Uuid::new_v4();
    let hash = format!("{:0>64}", Uuid::new_v4().simple().to_string());
    smartfs_db::insert_blob_with_sharing(&pool, &hash, blob_id, None, 4, true)
        .await
        .unwrap();
    store.put(blob_id, b"data").await.unwrap();

    let r = clean_once(&pool, &store, &queue, 3600, 500).await.unwrap();

    assert!(store.exists(blob_id).await.unwrap(), "grace window must spare it: {r:?}");
    assert!(blob_row_exists(&pool, blob_id).await);

    sqlx::query("DELETE FROM blobs WHERE blob_id = $1").bind(blob_id).execute(&pool).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn a_referenced_blob_is_never_reclaimed() {
    let pool = pool().await;
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStore::new(dir.path());
    let queue = PendingQueue::new(dir.path());
    queue.ensure_dirs().await.unwrap();

    let root: Uuid = sqlx::query_scalar("SELECT id FROM inode_registry WHERE ino = 1")
        .fetch_one(&pool)
        .await
        .expect("root inode");
    let inode = smartfs_db::inode_create(
        &pool,
        Some(root),
        &format!("cleaner-{}.txt", Uuid::new_v4()),
        false,
        0,
        0,
        0o100644,
    )
    .await
    .unwrap();

    let (blob_id, hash) = seed_blob(&pool, &store, true).await;
    smartfs_db::cow_commit(&pool, inode.id, Some(blob_id), &hash, 4, None, None, None, None, &[])
        .await
        .expect("cow_commit");

    let _ = clean_once(&pool, &store, &queue, 60, 500).await.unwrap();

    assert!(store.exists(blob_id).await.unwrap(), "referenced blob must survive");
    assert!(blob_row_exists(&pool, blob_id).await);

    sqlx::query("DELETE FROM file_versions WHERE inode_id = $1").bind(inode.id).execute(&pool).await.ok();
    let _ = smartfs_db::inode_delete(&pool, inode.id).await;
    sqlx::query("DELETE FROM blobs WHERE blob_id = $1").bind(blob_id).execute(&pool).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live Postgres; run with --ignored"]
async fn private_and_shared_blobs_of_identical_content_coexist() {
    // The schema property the whole per-inode switch rests on, and the reason
    // the primary key moved off content_hash.
    let pool = pool().await;
    let dir = TempDir::new().unwrap();
    let store = LocalDiskStore::new(dir.path());

    let hash = format!("{:0>64}", Uuid::new_v4().simple().to_string());
    let shared_id = Uuid::new_v4();
    let private_id = Uuid::new_v4();

    smartfs_db::insert_blob_with_sharing(&pool, &hash, shared_id, None, 4, true).await.unwrap();
    smartfs_db::insert_blob_with_sharing(&pool, &hash, private_id, None, 4, false).await.unwrap();
    store.put(shared_id, b"data").await.unwrap();
    store.put(private_id, b"data").await.unwrap();

    // dedup must offer the shared one and never the private one.
    let found = smartfs_db::dedup_check(&pool, &hash).await.unwrap();
    assert_eq!(
        found,
        Some(shared_id),
        "dedup must reuse the shared blob and never hand back a private one"
    );

    // A second *shared* blob of the same content must still be impossible.
    let second_shared = smartfs_db::insert_blob_with_sharing(
        &pool, &hash, Uuid::new_v4(), None, 4, true,
    )
    .await
    .unwrap();
    assert!(!second_shared.inserted, "dedup must collapse it onto the existing row");
    assert_eq!(second_shared.blob_id, shared_id);

    sqlx::query("DELETE FROM blobs WHERE content_hash = $1").bind(&hash).execute(&pool).await.ok();
    let _ = Duration::from_secs(0);
}
