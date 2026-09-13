//! Command execution and dispatch integration tests for `smartfs-cli`.

use std::sync::Arc;
use smartfs_cli::args::*;
use smartfs_cli::commands::*;
use smartfs_cli::dispatch::dispatch_command;
use smartfs_db::connect_pool;
use smartfs_store::LocalDiskStore;
use uuid::Uuid;

fn get_db_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@172.17.0.2:5432/smartfs".to_string())
}

#[tokio::test]
async fn test_dispatch_write_cat_history_lifecycle() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let temp_dir = std::env::temp_dir().join(format!("smartfs_cli_test_{}", Uuid::new_v4()));
    let _ = tokio::fs::create_dir_all(&temp_dir).await;
    let store = Arc::new(LocalDiskStore::new(&temp_dir));

    let unique_file_name = format!("cli_test_{}.txt", Uuid::new_v4());
    let target_path = format!("/tests/{unique_file_name}");

    // Create a local file to write from
    let local_file = temp_dir.join("source.txt");
    let content = b"Hello from SmartFS CLI automated test!";
    tokio::fs::write(&local_file, content).await.unwrap();

    // 1. Test handle_write
    let write_args = WriteArgs {
        path: target_path.clone(),
        file_to_read: Some(local_file.clone()),
        force: false,
    };
    let write_res = handle_write(&pool, store.as_ref(), &write_args)
        .await
        .expect("write should succeed");

    assert_eq!(write_res.path, target_path);
    assert_eq!(write_res.size, content.len() as i64);
    assert!(!write_res.content_hash.is_empty());

    // 2. Test handle_cat
    let cat_args = CatArgs {
        path: target_path.clone(),
        version: None,
    };
    let cat_res = handle_cat(&pool, store.as_ref(), &cat_args)
        .await
        .expect("cat should succeed");

    assert_eq!(cat_res.path, target_path);
    assert_eq!(cat_res.version_number, 1);
    assert_eq!(cat_res.data, content);

    // 3. Test handle_history
    let hist_args = HistoryArgs {
        path: target_path.clone(),
    };
    let hist_res = handle_history(&pool, &hist_args)
        .await
        .expect("history should succeed");

    assert_eq!(hist_res.versions.len(), 1);
    assert_eq!(hist_res.versions[0].version_number, 1);
    assert_eq!(hist_res.versions[0].size, content.len() as i64);

    // 4. Test handle_write with force = true (creates version 2)
    let local_file_v2 = temp_dir.join("source_v2.txt");
    let content_v2 = b"Version 2 updated content!";
    tokio::fs::write(&local_file_v2, content_v2).await.unwrap();

    let write_v2_args = WriteArgs {
        path: target_path.clone(),
        file_to_read: Some(local_file_v2),
        force: true,
    };
    let write_v2_res = handle_write(&pool, store.as_ref(), &write_v2_args)
        .await
        .expect("write v2 should succeed");
    assert_eq!(write_v2_res.size, content_v2.len() as i64);

    // Verify cat latest gets version 2
    let cat_v2 = handle_cat(&pool, store.as_ref(), &cat_args)
        .await
        .expect("cat latest should be v2");
    assert_eq!(cat_v2.version_number, 2);
    assert_eq!(cat_v2.data, content_v2);

    // Verify cat version 1 explicitly
    let cat_v1_args = CatArgs {
        path: target_path.clone(),
        version: Some(1),
    };
    let cat_v1 = handle_cat(&pool, store.as_ref(), &cat_v1_args)
        .await
        .expect("cat v1 should succeed");
    assert_eq!(cat_v1.version_number, 1);
    assert_eq!(cat_v1.data, content);

    // 5. Test handle_diff
    let diff_args = DiffArgs {
        path: target_path.clone(),
        v1: 1,
        v2: 2,
    };
    let diff_res = handle_diff(&pool, &diff_args)
        .await
        .expect("diff should succeed");
    assert_eq!(diff_res.v1, 1);
    assert_eq!(diff_res.v2, 2);

    // 6. Test dispatch_command output string formatting
    let dispatch_out = dispatch_command(&pool, store.as_ref(), &temp_dir, &Commands::Cat(cat_v1_args))
        .await
        .expect("dispatch cat should succeed");
    assert_eq!(dispatch_out, String::from_utf8_lossy(content));

    let hist_out = dispatch_command(&pool, store.as_ref(), &temp_dir, &Commands::History(hist_args))
        .await
        .expect("dispatch history should succeed");
    assert!(hist_out.contains("Version history for"));

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
}

#[tokio::test]
async fn test_dispatch_status_and_calibrate() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let temp_dir = std::env::temp_dir().join(format!("smartfs_cli_status_{}", Uuid::new_v4()));
    let _ = tokio::fs::create_dir_all(&temp_dir).await;
    let store = Arc::new(LocalDiskStore::new(&temp_dir));

    // 1. Status command
    let status_res = handle_status(&pool, &temp_dir, &StatusArgs {})
        .await
        .expect("status should succeed");
    assert!(status_res.pending_backlog_count >= 0);
    assert!(status_res.unconsolidated_embedding_count >= 0);

    let status_out = dispatch_command(&pool, store.as_ref(), &temp_dir, &Commands::Status(StatusArgs {}))
        .await
        .expect("dispatch status");
    assert!(status_out.contains("SmartFS System Status:"));

    // 2. Calibrate command
    let model_id = smartfs_db::get_default_model_id(&pool)
        .await
        .unwrap_or_else(|_| Uuid::new_v4());

    let cal_args = CalibrateArgs {
        plugin_type: "rust".to_string(),
        model_id,
        target_percentile: Some(0.18),
    };
    let cal_res = handle_calibrate(&pool, &cal_args)
        .await
        .expect("calibrate should succeed");
    assert!((cal_res.join_threshold - 0.18).abs() < 1e-6);

    let cal_out = dispatch_command(&pool, store.as_ref(), &temp_dir, &Commands::Calibrate(cal_args))
        .await
        .expect("dispatch calibrate");
    assert!(cal_out.contains("Successfully calibrated"));

    // 3. Concepts command
    let concepts_args = ConceptsArgs {
        plugin_type: Some("rust".to_string()),
        limit: Some(10),
    };
    let concepts_res = handle_concepts(&pool, &concepts_args)
        .await
        .expect("concepts should succeed");
    assert!(concepts_res.concepts.iter().all(|c| c.plugin_type == "rust"));

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
}

#[tokio::test]
async fn test_dispatch_import() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let temp_dir = std::env::temp_dir().join(format!("smartfs_cli_import_{}", Uuid::new_v4()));
    let import_dir = temp_dir.join("to_import");
    let sub_dir = import_dir.join("sub");
    tokio::fs::create_dir_all(&sub_dir).await.unwrap();

    tokio::fs::write(import_dir.join("file1.txt"), b"File 1 content").await.unwrap();
    tokio::fs::write(sub_dir.join("file2.txt"), b"File 2 content").await.unwrap();

    let store = Arc::new(LocalDiskStore::new(&temp_dir));

    // Import with mode Cow
    let import_cow_args = ImportArgs {
        dir: import_dir.clone(),
        mode: ImportMode::Cow,
    };
    let res_cow = handle_import(&pool, store.as_ref(), &import_cow_args)
        .await
        .expect("import cow should succeed");
    assert_eq!(res_cow.files_imported, 2);
    assert_eq!(res_cow.directories_imported, 1);
    assert!(res_cow.total_bytes > 0);

    // Import with mode Index
    let import_index_args = ImportArgs {
        dir: import_dir.clone(),
        mode: ImportMode::Index,
    };
    let res_index = handle_import(&pool, store.as_ref(), &import_index_args)
        .await
        .expect("import index should succeed");
    assert_eq!(res_index.files_imported, 2);

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
}

#[tokio::test]
async fn test_dispatch_search() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let search_args = SearchArgs {
        query: "smartfs".to_string(),
        plugin_type: None,
        limit: 10,
    };
    let res = handle_search(&pool, &search_args).await;
    // search should succeed (either via pg_search or fallback)
    assert!(res.is_ok());
}
