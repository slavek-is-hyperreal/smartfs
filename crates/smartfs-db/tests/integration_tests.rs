use smartfs_db::*;
use uuid::Uuid;

fn get_db_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@172.17.0.2:5432/smartfs".to_string())
}

#[tokio::test]
async fn test_db_full_lifecycle() {
    let db_url = get_db_url();
    let pool = match connect_pool(&db_url).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Skipping DB test (could not connect to {db_url}): {e}");
            return;
        }
    };

    // 1. Inode lookup root (ino=1)
    let root = inode_lookup_by_ino(&pool, 1)
        .await
        .expect("root inode should exist")
        .expect("root inode is Some");
    assert_eq!(root.ino, 1);
    assert!(root.is_dir);

    // 2. Inode create child directory
    let dir_name = format!("test_dir_{}", Uuid::new_v4());
    let dir = inode_create(&pool, Some(root.id), &dir_name, true, 1000, 1000, 0o755)
        .await
        .expect("create test dir");
    assert_eq!(dir.name, dir_name);
    assert!(dir.is_dir);

    // 3. Inode create child file
    let file_name = "test_file.rs";
    let file = inode_create(&pool, Some(dir.id), file_name, false, 1000, 1000, 0o644)
        .await
        .expect("create test file");
    assert_eq!(file.name, file_name);
    assert!(!file.is_dir);

    // 4. Inode lookup by parent and name
    let looked_up = inode_lookup(&pool, Some(dir.id), file_name)
        .await
        .expect("lookup file")
        .expect("file exists");
    assert_eq!(looked_up.id, file.id);

    // 5. Inode list children
    let children = inode_list_children(&pool, Some(dir.id))
        .await
        .expect("list children");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, file.id);

    // 6. Inode update attrs
    let updated = inode_update_attrs(&pool, file.id, Some(1001), None, Some(0o600), None)
        .await
        .expect("update attrs");
    assert_eq!(updated.uid, 1001);
    assert_eq!(updated.mode, 0o600);
    assert_eq!(updated.size, 0);

    // 7. Inode rename
    let new_file_name = "renamed_file.rs";
    inode_rename(&pool, Some(dir.id), file_name, Some(dir.id), new_file_name)
        .await
        .expect("rename file");
    let after_rename = inode_lookup(&pool, Some(dir.id), new_file_name)
        .await
        .expect("lookup renamed")
        .expect("renamed exists");
    assert_eq!(after_rename.id, file.id);

    // 8. Blobs: insert_blob and dedup
    let content_hash = format!("hash_{}", Uuid::new_v4());
    let blob_id = Uuid::new_v4();
    let res1 = insert_blob(&pool, &content_hash, blob_id, None, 1024)
        .await
        .expect("insert blob 1");
    assert_eq!(res1.blob_id, blob_id);
    assert!(res1.inserted, "first insert should have inserted=true");

    // Second insert with same content_hash must return existing blob_id and inserted=false
    let res2 = insert_blob(&pool, &content_hash, Uuid::new_v4(), None, 1024)
        .await
        .expect("insert blob 2");
    assert_eq!(res2.blob_id, blob_id, "dedup must reuse existing blob_id");
    assert!(!res2.inserted, "duplicate insert must have inserted=false");

    // 9. Update compressed size
    update_blob_compressed_size(&pool, &content_hash, 512)
        .await
        .expect("update compressed size");

    // 10. Dedup check
    let checked_blob = dedup_check(&pool, &content_hash)
        .await
        .expect("dedup check")
        .expect("blob exists");
    assert_eq!(checked_blob, blob_id);

    // 11. Get blob
    let blob_rec = get_blob(&pool, &content_hash)
        .await
        .expect("get blob")
        .expect("blob record exists");
    assert_eq!(blob_rec.compressed_size, Some(512));

    // 12. CoW commit version 1
    let ast_nodes = vec![
        AstNodeInsert {
            kind: "function".to_string(),
            name: "hello_world".to_string(),
            start_line: 1,
            end_line: 5,
            source: "fn hello_world() -> bool { true }".to_string(),
            content_hash: format!("ast_hash_1_{}", Uuid::new_v4()),
        },
        AstNodeInsert {
            kind: "struct".to_string(),
            name: "TestStruct".to_string(),
            start_line: 7,
            end_line: 10,
            source: "struct TestStruct { a: i32 }".to_string(),
            content_hash: format!("ast_hash_2_{}", Uuid::new_v4()),
        },
    ];

    let v1_id = cow_commit(
        &pool,
        file.id,
        Some(blob_id),
        &content_hash,
        1024,
        Some(512),
        None,
        Some("rust"),
        Some(serde_json::json!({"ast": true})),
        &ast_nodes,
    )
    .await
    .expect("cow_commit v1");

    let v1 = version_get_by_id(&pool, v1_id)
        .await
        .expect("get v1 by id")
        .expect("v1 exists");
    assert_eq!(v1.version_number, 1);
    assert_eq!(v1.status, "pending");
    assert!(v1.parent_version_id.is_none());

    // Verify AST nodes inserted
    let stored_ast = get_ast_nodes(&pool, v1_id).await.expect("get ast nodes");
    assert_eq!(stored_ast.len(), 2);
    assert_eq!(stored_ast[0].name, "hello_world");
    assert_eq!(stored_ast[1].name, "TestStruct");

    // 13. CoW commit version 2
    let content_hash_v2 = format!("hash_v2_{}", Uuid::new_v4());
    let v2_id = cow_commit(
        &pool,
        file.id,
        Some(blob_id),
        &content_hash_v2,
        2048,
        Some(1024),
        None,
        Some("rust"),
        Some(serde_json::json!({"ast": true})),
        &[],
    )
    .await
    .expect("cow_commit v2");

    let v2 = version_get(&pool, file.id, None)
        .await
        .expect("get latest")
        .expect("latest exists");
    assert_eq!(v2.id, v2_id);
    assert_eq!(v2.version_number, 2);
    assert_eq!(v2.parent_version_id, Some(v1_id));

    // 14. Version history
    let history = version_history(&pool, file.id)
        .await
        .expect("version history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].version_number, 1);
    assert_eq!(history[1].version_number, 2);

    // 15. Worker queries
    let claimed = claim_pending_to_processing(&pool, 10)
        .await
        .expect("claim pending");
    assert!(claimed.contains(&v1_id));
    assert!(claimed.contains(&v2_id));

    // Revert v2 to pending
    revert_to_pending(&pool, v2_id).await.expect("revert v2");
    let v2_reverted = version_get_by_id(&pool, v2_id)
        .await
        .expect("get v2")
        .expect("v2 exists");
    assert_eq!(v2_reverted.status, "pending");

    // Mark v1 clean
    mark_clean(&pool, v1_id).await.expect("mark clean");
    let v1_clean = version_get_by_id(&pool, v1_id)
        .await
        .expect("get v1")
        .expect("v1 exists");
    assert_eq!(v1_clean.status, "clean");

    // Refresh is_current (FIX-02 order-independent recomputation)
    refresh_is_current(&pool, v1_id)
        .await
        .expect("refresh_is_current v1");

    // Reaper
    let reaped = reaper(&pool).await.expect("reaper");
    let _ = reaped;

    // Valve metrics
    let backlog = pending_backlog_count(&pool)
        .await
        .expect("pending backlog");
    assert!(backlog >= 1); // at least v2 is pending

    let age = oldest_pending_age_secs(&pool)
        .await
        .expect("oldest pending age");
    assert!(age.is_some());

    // 16. Search functions (v6.0 delta)
    let unconsolidated = count_all_unconsolidated(&pool)
        .await
        .expect("count_all_unconsolidated");
    assert!(unconsolidated >= 0);

    // set_search_text (ADR-54)
    set_search_text(
        &pool,
        v1_id,
        Some("SmartFS intelligent semantic filesystem core rust database"),
    )
    .await
    .expect("set_search_text");

    let v1_with_text = version_get_by_id(&pool, v1_id)
        .await
        .expect("get v1")
        .expect("v1 exists");
    assert_eq!(
        v1_with_text.search_text.as_deref(),
        Some("SmartFS intelligent semantic filesystem core rust database")
    );

    // search_fulltext_bm25 (ADR-54)
    let hits = search_fulltext_bm25(&pool, "intelligent", None, 10)
        .await
        .expect("search_fulltext_bm25 file");
    assert!(!hits.is_empty(), "should find hit for 'intelligent'");
    assert_eq!(hits[0].kind, FulltextHitKind::File);
    assert_eq!(hits[0].version_id, v1_id);

    // search AST code
    let ast_hits = search_fulltext_bm25(&pool, "hello_world", None, 10)
        .await
        .expect("search_fulltext_bm25 ast");
    assert!(!ast_hits.is_empty(), "should find hit for 'hello_world'");
    assert_eq!(ast_hits[0].kind, FulltextHitKind::AstNode);

    // Clean up
    inode_delete(&pool, dir.id).await.expect("cleanup dir cascade");
    compensate_blob_delete(&pool, &content_hash)
        .await
        .expect("cleanup blob 1");
    compensate_blob_delete(&pool, &content_hash_v2)
        .await
        .expect("cleanup blob 2");
}
