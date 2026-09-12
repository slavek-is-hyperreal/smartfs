use smartfs_ai::activity::ActivityMonitor;
use smartfs_ai::ast::{extract_ast_nodes, parse_ast_nodes_blocking};
use smartfs_ai::engine::{CpuEmbeddingEngine, EmbeddingEngine};
use smartfs_ai::worker::{embed_version, finish_embed, run_worker_supervisor, should_embed_despite_activity};
use smartfs_compress::write_blob_streaming;
use smartfs_db::*;
use smartfs_store::LocalDiskStore;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::watch;
use uuid::Uuid;

#[test]
fn test_ast_extraction_rust() {
    let code = r#"
// Some comment
pub fn calculate_hash(data: &[u8]) -> String {
    let hash = "abc";
    hash.to_string()
}

pub struct UserRecord {
    pub id: u64,
    pub name: String,
}

pub enum Status {
    Active,
    Inactive,
}

pub trait Processor {
    fn process(&self);
}

impl Processor for UserRecord {
    fn process(&self) {
        println!("{}", self.name);
    }
}
"#;

    let nodes = extract_ast_nodes(code, "rust");
    assert_eq!(nodes.len(), 5);

    assert_eq!(nodes[0].kind, "function");
    assert_eq!(nodes[0].name, "calculate_hash");
    assert!(nodes[0].source.contains("fn calculate_hash"));

    assert_eq!(nodes[1].kind, "struct");
    assert_eq!(nodes[1].name, "UserRecord");

    assert_eq!(nodes[2].kind, "enum");
    assert_eq!(nodes[2].name, "Status");

    assert_eq!(nodes[3].kind, "trait");
    assert_eq!(nodes[3].name, "Processor");

    assert_eq!(nodes[4].kind, "impl");
    assert_eq!(nodes[4].name, "Processor");
}

#[tokio::test]
async fn test_ast_extraction_async_blocking() {
    let py_code = r#"
def process_item(item_id, count=1):
    total = item_id * count
    return total

class ItemManager:
    def __init__(self):
        self.items = []
"#;

    let nodes = parse_ast_nodes_blocking(py_code.to_string(), "python".to_string()).await;
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0].kind, "function");
    assert_eq!(nodes[0].name, "process_item");
    assert_eq!(nodes[1].kind, "class");
    assert_eq!(nodes[1].name, "ItemManager");
}

#[tokio::test]
async fn test_activity_monitor() {
    let monitor = ActivityMonitor::new();
    assert!(monitor.time_since_last_write() >= Duration::from_secs(1));

    monitor.notify_write();
    assert!(monitor.time_since_last_write() < Duration::from_millis(50));

    // wait_for_idle with 100ms
    let start = std::time::Instant::now();
    monitor.wait_for_idle(Duration::from_millis(100)).await;
    assert!(start.elapsed() >= Duration::from_millis(90));
}

#[tokio::test]
async fn test_cpu_embedding_engine_normalization() {
    let engine = CpuEmbeddingEngine::default_qwen();

    let text1 = "SmartFS intelligent semantic filesystem";
    let vec1 = engine.embed(text1, 1024).await.expect("embed 1024");
    assert_eq!(vec1.len(), 1024);

    // L2 norm must be approximately 1.0
    let norm_sq: f32 = vec1.iter().map(|v| v * v).sum();
    assert!((norm_sq - 1.0).abs() < 1e-4);

    // Deterministic: same text yields same vector
    let vec1_repeat = engine.embed(text1, 1024).await.expect("embed 1024 repeat");
    assert_eq!(vec1, vec1_repeat);

    // Different text yields different vector
    let text2 = "Unrelated completely different string content";
    let vec2 = engine.embed(text2, 1024).await.expect("embed 1024 diff");
    assert_ne!(vec1, vec2);
}

fn get_db_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@172.17.0.2:5432/smartfs".to_string())
}

#[tokio::test]
async fn test_ai_worker_end_to_end() {
    let db_url = get_db_url();
    let pool = match connect_pool(&db_url).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Skipping DB test (could not connect to {db_url}): {e}");
            return;
        }
    };

    let temp_dir = tempdir().expect("tempdir");
    let store = Arc::new(LocalDiskStore::new(temp_dir.path()));
    let engine = CpuEmbeddingEngine::default_qwen();
    let monitor = ActivityMonitor::new();

    // 1. Setup inode and blob
    let root = inode_lookup_by_ino(&pool, 1)
        .await
        .expect("root lookup")
        .expect("root exists");

    let dir_name = format!("ai_test_dir_{}", Uuid::new_v4());
    let dir = inode_create(&pool, Some(root.id), &dir_name, true, 1000, 1000, 0o755)
        .await
        .expect("create dir");

    let file_name = "processor.rs";
    let file = inode_create(&pool, Some(dir.id), file_name, false, 1000, 1000, 0o644)
        .await
        .expect("create file");

    let file_code = format!(
        "// test run {}\npub fn run_pipeline() -> bool {{\n    true\n}}\n",
        Uuid::new_v4()
    );
    let (blob_uuid, content_hash_obj, orig_size, comp_size) = write_blob_streaming(
        file_code.as_bytes(),
        3,
        store.as_ref(),
    )
    .await
    .expect("write blob");
    let content_hash = content_hash_obj.0;

    // Insert blob record
    let insert_res = insert_blob(
        &pool,
        &content_hash,
        blob_uuid,
        None,
        orig_size as i64,
    )
    .await
    .expect("insert blob");
    update_blob_compressed_size(
        &pool,
        &content_hash,
        comp_size as i64,
    )
    .await
    .expect("update blob compressed");

    // Extract AST nodes
    let ast_nodes = extract_ast_nodes(&file_code, "rust");

    // Commit file version 1
    let v1_id = cow_commit(
        &pool,
        file.id,
        Some(insert_res.blob_id),
        &content_hash,
        orig_size as i64,
        Some(comp_size as i64),
        None,
        Some("rust"),
        Some(serde_json::json!({"ast": true})),
        &ast_nodes,
    )
    .await
    .expect("cow_commit v1");

    let default_model_id = get_default_model_id(&pool)
        .await
        .expect("get default model");

    // 2. Direct embed_version call
    embed_version(
        &pool,
        store.as_ref(),
        engine.as_ref(),
        v1_id,
        default_model_id,
    )
    .await
    .expect("embed_version should succeed");
    finish_embed(&pool, v1_id, Ok(())).await;

    // Verify version is now clean
    let v1_after = version_get_by_id(&pool, v1_id)
        .await
        .expect("get v1")
        .expect("v1 exists");
    assert_eq!(v1_after.status, "clean");
    assert!(v1_after.search_text.is_some());

    // 3. Test should_embed_despite_activity
    let force = should_embed_despite_activity(&pool).await;
    let _ = force;

    // 4. Test supervisor loop with second version
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let pool_clone = pool.clone();
    let store_clone = store.clone();
    let engine_clone = engine.clone();
    let monitor_clone = monitor.clone();

    let supervisor_handle = tokio::spawn(async move {
        run_worker_supervisor(
            pool_clone,
            store_clone,
            engine_clone,
            monitor_clone,
            5,
            shutdown_rx,
        )
        .await;
    });

    // Create version 2
    let code_v2 = format!(
        "// test run {}\npub fn new_feature() -> i32 {{ 42 }}",
        Uuid::new_v4()
    );
    let (blob_uuid_v2, content_hash_obj_v2, orig_size_v2, comp_size_v2) = write_blob_streaming(
        code_v2.as_bytes(),
        3,
        store.as_ref(),
    )
    .await
    .expect("write blob v2");
    let content_hash_v2 = content_hash_obj_v2.0;

    let insert_res_v2 = insert_blob(
        &pool,
        &content_hash_v2,
        blob_uuid_v2,
        None,
        orig_size_v2 as i64,
    )
    .await
    .expect("insert blob v2");

    let v2_id = cow_commit(
        &pool,
        file.id,
        Some(insert_res_v2.blob_id),
        &content_hash_v2,
        orig_size_v2 as i64,
        Some(comp_size_v2 as i64),
        None,
        Some("rust"),
        Some(serde_json::json!({"ast": true})),
        &[],
    )
    .await
    .expect("cow_commit v2");

    // Wait for supervisor to automatically claim and process v2
    let mut clean = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let v2 = version_get_by_id(&pool, v2_id).await.unwrap().unwrap();
        if v2.status == "clean" {
            clean = true;
            break;
        }
    }
    assert!(clean, "supervisor must automatically process v2 and mark it clean");

    // Signal shutdown and wait for supervisor task to exit
    let _ = shutdown_tx.send(true);
    let _ = supervisor_handle.await;

    // Cleanup
    let _ = inode_delete(&pool, dir.id).await;
    let _ = compensate_blob_delete(&pool, &content_hash).await;
    let _ = compensate_blob_delete(&pool, &content_hash_v2).await;
}
