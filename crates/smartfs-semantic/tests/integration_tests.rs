use smartfs_db::{connect_pool, get_default_model_id, inode_create, insert_embedding_1024_qwen};
use smartfs_schema::error::SmartFsError;
use smartfs_semantic::*;
use std::io::Write;
use uuid::Uuid;

fn get_db_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@172.17.0.2:5432/smartfs".to_string())
}

#[tokio::test]
async fn test_advisory_lock_lifecycle() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => {
            eprintln!("Database not available, skipping test");
            return;
        }
    };

    let model_id = Uuid::new_v4();
    let lock_key = advisory_lock_key("test_plugin", model_id);

    // Acquire lock
    let acquired = try_advisory_lock(&pool, lock_key).await.expect("acquire");
    assert!(acquired);

    // Release lock
    release_advisory_lock(&pool, lock_key).await.expect("release");
}

#[tokio::test]
async fn test_config_calibration() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let plugin_type = format!("test_plug_{}", Uuid::new_v4().simple());
    let model_id = get_default_model_id(&pool).await.expect("default model");

    // Missing configuration returns MissingCalibration
    let err = load_consolidation_config(&pool, &plugin_type, model_id)
        .await
        .expect_err("should fail with missing calibration");
    match err {
        SmartFsError::MissingCalibration {
            plugin_type: p,
            model_id: m,
        } => {
            assert_eq!(p, plugin_type);
            assert_eq!(m, model_id);
        }
        other => panic!("Expected MissingCalibration error, got: {other:?}"),
    }

    // Calibrate threshold
    let calibrated = calibrate_join_threshold(&pool, &plugin_type, model_id, 0.42)
        .await
        .expect("calibrate");
    assert!((calibrated - 0.42).abs() < 1e-6);

    // Configuration now loads successfully
    let cfg = load_consolidation_config(&pool, &plugin_type, model_id)
        .await
        .expect("load config");
    assert!((cfg.join_threshold - 0.42).abs() < 1e-6);
}

#[tokio::test]
async fn test_consolidation_and_search_lifecycle() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let plugin_type = format!("test_consol_{}", Uuid::new_v4().simple());
    let model_id = get_default_model_id(&pool).await.expect("default model");

    // Calibrate
    calibrate_join_threshold(&pool, &plugin_type, model_id, 0.5)
        .await
        .expect("calibrate");

    let cfg = load_consolidation_config(&pool, &plugin_type, model_id)
        .await
        .expect("load config");

    // Create an inode and version in smartfs-db
    let inode = inode_create(&pool, None, "test_file.txt", false, 1000, 1000, 0o644)
        .await
        .expect("create inode");

    let version_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO file_versions (id, inode_id, version_number, size, content_hash, created_at, special_type)
         VALUES ($1, $2, 1, 100, 'hash1', NOW(), $3)",
    )
    .bind(version_id)
    .bind(inode.id)
    .bind(&plugin_type)
    .execute(&pool)
    .await
    .expect("insert version");

    // Insert unconsolidated embedding
    let mut vec_a = vec![0.0f32; 1024];
    vec_a[0] = 1.0;
    insert_embedding_1024_qwen(&pool, version_id, model_id, &plugin_type, &vec_a)
        .await
        .expect("insert embedding");

    // Count unconsolidated
    let count = count_unconsolidated(&pool, &plugin_type, model_id)
        .await
        .expect("count");
    assert_eq!(count, 1);

    // Consolidate batch
    let processed = consolidate_batch(&pool, &plugin_type, model_id, &cfg)
        .await
        .expect("consolidate batch");
    assert_eq!(processed, 1);

    // Count unconsolidated should now be 0
    let count_after = count_unconsolidated(&pool, &plugin_type, model_id)
        .await
        .expect("count after");
    assert_eq!(count_after, 0);

    // Search by concept
    let hits = search_by_concept(&pool, &vec_a, &plugin_type, model_id, 10)
        .await
        .expect("search");
    assert!(!hits.is_empty());
    assert_eq!(hits[0].id, version_id);
    assert_eq!(hits[0].source, HitSource::Crystallized);
    assert!(hits[0].distance < 1e-4);

    // Test label centroid
    let centroid_id: Uuid = sqlx::query_scalar(
        "SELECT centroid_id FROM centroid_members_1024_qwen WHERE version_id = $1"
    )
    .bind(version_id)
    .fetch_one(&pool)
    .await
    .expect("find centroid");

    let label = label_centroid_from_members(&pool, centroid_id)
        .await
        .expect("label centroid");
    assert!(label.is_some());
}

#[tokio::test]
async fn test_wordnet_import_and_link() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let temp_dir = std::env::temp_dir();
    let file_path = temp_dir.join(format!("wordnet_test_{}.json", Uuid::new_v4()));

    let json_content = r#"{
        "language": "pl",
        "nodes": [
            { "lemma": "funkcja", "pos": "noun", "external_ref": "syn-01" },
            { "lemma": "metoda", "pos": "noun", "external_ref": "syn-02" }
        ],
        "edges": [
            { "from": "funkcja", "to": "metoda", "relation": "hypernym" }
        ]
    }"#;

    {
        let mut f = std::fs::File::create(&file_path).expect("create file");
        f.write_all(json_content.as_bytes()).expect("write");
    }

    let count = import_wordnet(&pool, &file_path, "pl")
        .await
        .expect("import wordnet");
    assert!(count >= 2);

    let _ = std::fs::remove_file(file_path);
}

#[tokio::test]
async fn test_split_and_merge_centroids_no_delete() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let plugin_type = format!("test_merge_{}", Uuid::new_v4().simple());
    let model_id = get_default_model_id(&pool).await.expect("default model");

    // Calibrate with small split thresholds
    calibrate_join_threshold(&pool, &plugin_type, model_id, 0.5)
        .await
        .expect("calibrate");

    let mut cfg = load_consolidation_config(&pool, &plugin_type, model_id)
        .await
        .expect("load config");
    cfg.split_variance_threshold = 0.01; // Low variance threshold to force split
    cfg.max_members_per_centroid = 2;

    // Create 3 versions with distinct vectors
    let mut version_ids = Vec::new();
    for i in 0..3 {
        let inode = inode_create(&pool, None, &format!("file_{i}.txt"), false, 1000, 1000, 0o644)
            .await
            .expect("create inode");
        let v_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO file_versions (id, inode_id, version_number, size, content_hash, created_at, special_type)
             VALUES ($1, $2, 1, 100, $3, NOW(), $4)",
        )
        .bind(v_id)
        .bind(inode.id)
        .bind(format!("hash_{i}"))
        .bind(&plugin_type)
        .execute(&pool)
        .await
        .expect("insert version");

        let mut vec = vec![0.0f32; 1024];
        vec[i] = 1.0;
        insert_embedding_1024_qwen(&pool, v_id, model_id, &plugin_type, &vec)
            .await
            .expect("insert embedding");

        version_ids.push(v_id);
    }

    // Consolidate batch
    let processed = consolidate_batch(&pool, &plugin_type, model_id, &cfg)
        .await
        .expect("consolidate batch");
    assert_eq!(processed, 3);

    // Verify centroids exist and some may have split
    let total_centroids: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM concept_centroids_1024_qwen WHERE plugin_type = $1"
    )
    .bind(&plugin_type)
    .fetch_one(&pool)
    .await
    .expect("count centroids");
    assert!(total_centroids >= 1);

    // Test merge_centroids: run with high threshold to merge them
    let mut tx = pool.begin().await.expect("begin");
    let merged_count = merge_centroids(&mut tx, &plugin_type, model_id, 2.0)
        .await
        .expect("merge centroids");
    tx.commit().await.expect("commit");

    if merged_count > 0 {
        // Verify invariant: old merged centroids are is_active = FALSE and have merged_into set, NEVER deleted!
        let inactive_with_merged_into: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM concept_centroids_1024_qwen
             WHERE plugin_type = $1 AND is_active = FALSE AND merged_into IS NOT NULL"
        )
        .bind(&plugin_type)
        .fetch_one(&pool)
        .await
        .expect("count inactive centroids");
        assert!(inactive_with_merged_into >= 2);
    }
}
