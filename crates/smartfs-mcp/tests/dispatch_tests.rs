use std::sync::Arc;
use smartfs_db::connect_pool;
use smartfs_mcp::{McpServer, ToolHandler};
use smartfs_store::LocalDiskStore;

fn get_db_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@172.17.0.2:5432/smartfs".to_string())
}

#[tokio::test]
async fn test_mcp_server_initialize() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };
    let temp_dir = std::env::temp_dir().join(format!("smartfs_test_{}", uuid::Uuid::new_v4()));
    let _ = tokio::fs::create_dir_all(&temp_dir).await;
    let store = Arc::new(LocalDiskStore::new(&temp_dir));
    let handler = ToolHandler::new(pool, store, None);
    let server = McpServer::new(handler);

    let init_req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let resp = server.handle_line(init_req).await.expect("response expected");
    assert_eq!(resp.jsonrpc, "2.0");
    assert_eq!(resp.id, Some(serde_json::json!(1)));

    let res = resp.result.expect("result expected");
    assert_eq!(res["protocolVersion"], "2024-11-05");
    assert_eq!(res["serverInfo"]["name"], "smartfs-mcp");

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
}

#[tokio::test]
async fn test_mcp_server_tools_list() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };
    let temp_dir = std::env::temp_dir().join(format!("smartfs_test_{}", uuid::Uuid::new_v4()));
    let _ = tokio::fs::create_dir_all(&temp_dir).await;
    let store = Arc::new(LocalDiskStore::new(&temp_dir));
    let handler = ToolHandler::new(pool, store, None);
    let server = McpServer::new(handler);

    let list_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    let resp = server.handle_line(list_req).await.expect("response expected");
    let res = resp.result.expect("result expected");
    let tools = res["tools"].as_array().expect("array of tools");
    assert!(tools.len() >= 13);

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
}

#[tokio::test]
async fn test_mcp_server_invalid_json() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };
    let temp_dir = std::env::temp_dir().join(format!("smartfs_test_{}", uuid::Uuid::new_v4()));
    let _ = tokio::fs::create_dir_all(&temp_dir).await;
    let store = Arc::new(LocalDiskStore::new(&temp_dir));
    let handler = ToolHandler::new(pool, store, None);
    let server = McpServer::new(handler);

    let malformed = "this is not json";
    let resp = server.handle_line(malformed).await.expect("response expected");
    assert!(resp.error.is_some());
    assert_eq!(resp.error.unwrap().code, -32700);

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
}

#[tokio::test]
async fn test_plugin_introspection_dispatch() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };
    let temp_dir = std::env::temp_dir().join(format!("smartfs_test_{}", uuid::Uuid::new_v4()));
    let _ = tokio::fs::create_dir_all(&temp_dir).await;
    let store = Arc::new(LocalDiskStore::new(&temp_dir));
    let handler = ToolHandler::new(pool, store, None);

    // list_plugin_types
    let list_res = handler
        .dispatch_tool_call("list_plugin_types", serde_json::json!({}))
        .await
        .expect("list_plugin_types success");
    let list_arr = list_res.as_array().expect("array");
    assert!(list_arr.iter().any(|p| p["type"] == "rust"));
    assert!(list_arr.iter().any(|p| p["type"] == "png"));

    // describe_plugin_type rust
    let rust_schema = handler
        .dispatch_tool_call(
            "describe_plugin_type",
            serde_json::json!({ "plugin_type": "rust" }),
        )
        .await
        .expect("describe_plugin_type rust");
    assert_eq!(rust_schema["type"], "rust");
    assert_eq!(rust_schema["ast"], true);

    // describe_plugin_type non_existent
    let err = handler
        .dispatch_tool_call(
            "describe_plugin_type",
            serde_json::json!({ "plugin_type": "unknown_type_xyz" }),
        )
        .await;
    assert!(err.is_err());

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
}

#[tokio::test]
async fn test_destructive_operations_confirmation_flow() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };
    let temp_dir = std::env::temp_dir().join(format!("smartfs_test_{}", uuid::Uuid::new_v4()));
    let _ = tokio::fs::create_dir_all(&temp_dir).await;
    let store = Arc::new(LocalDiskStore::new(&temp_dir));
    let handler = ToolHandler::new(pool, store, None);

    // 1. delete_file without token -> confirmation required
    let res = handler
        .dispatch_tool_call(
            "delete_file",
            serde_json::json!({ "path": "/test/file.txt" }),
        )
        .await
        .expect("delete_file initial request");
    assert_eq!(res["status"], "confirmation_required");
    let token = res["confirmation_token"].as_str().expect("token present");
    assert!(!token.is_empty());

    // 2. delete_file with wrong token -> conflict
    let err = handler
        .dispatch_tool_call(
            "delete_file",
            serde_json::json!({ "path": "/test/file.txt", "confirmation_token": "bogus-token" }),
        )
        .await;
    assert!(err.is_err());

    // 3. overwrite_file without token -> confirmation required
    let res_ow = handler
        .dispatch_tool_call(
            "overwrite_file",
            serde_json::json!({ "path": "/test/file.txt", "content": "hello world" }),
        )
        .await
        .expect("overwrite_file initial request");
    assert_eq!(res_ow["status"], "confirmation_required");
    assert!(res_ow["confirmation_token"].as_str().is_some());

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
}

#[tokio::test]
async fn test_search_by_concept_without_vector() {
    let pool = match connect_pool(&get_db_url()).await {
        Ok(p) => p,
        Err(_) => return,
    };
    let temp_dir = std::env::temp_dir().join(format!("smartfs_test_{}", uuid::Uuid::new_v4()));
    let _ = tokio::fs::create_dir_all(&temp_dir).await;
    let store = Arc::new(LocalDiskStore::new(&temp_dir));
    let handler = ToolHandler::new(pool, store, None);

    let res = handler
        .dispatch_tool_call("search_by_concept", serde_json::json!({ "plugin_type": "rust" }))
        .await
        .expect("search_by_concept without vector");
    assert!(res.is_array());

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
}
