use smartfs_mcp::protocol::*;
use smartfs_mcp::tools::get_tool_definitions;
use smartfs_schema::error::SmartFsError;

#[test]
fn test_jsonrpc_request_deserialization() {
    let raw = r#"{"jsonrpc":"2.0","id":42,"method":"search_semantic","params":{"query_vector":[0.1,0.2]}}"#;
    let req: JsonRpcRequest = serde_json::from_str(raw).expect("valid request");
    assert_eq!(req.jsonrpc, "2.0");
    assert_eq!(req.id, Some(serde_json::json!(42)));
    assert_eq!(req.method, "search_semantic");
    assert!(req.params.is_some());
}

#[test]
fn test_jsonrpc_request_without_id_or_params() {
    let raw = r#"{"method":"find_broken_files"}"#;
    let req: JsonRpcRequest = serde_json::from_str(raw).expect("valid request");
    assert_eq!(req.jsonrpc, "2.0");
    assert_eq!(req.id, None);
    assert_eq!(req.method, "find_broken_files");
    assert_eq!(req.params, None);
}

#[test]
fn test_jsonrpc_success_response_serialization() {
    let resp = JsonRpcResponse::success(Some(serde_json::json!(1)), serde_json::json!({"count": 5}));
    let serialized = serde_json::to_string(&resp).expect("serialize");
    assert!(serialized.contains(r#""jsonrpc":"2.0""#));
    assert!(serialized.contains(r#""id":1"#));
    assert!(serialized.contains(r#""result":{"count":5}"#));
    assert!(!serialized.contains(r#""error""#));
}

#[test]
fn test_jsonrpc_error_response_serialization() {
    let err = JsonRpcError::method_not_found("unknown_tool");
    let resp = JsonRpcResponse::error(Some(serde_json::json!("abc")), err);
    let serialized = serde_json::to_string(&resp).expect("serialize");
    assert!(serialized.contains(r#""id":"abc""#));
    assert!(serialized.contains(r#""code":-32601"#));
    assert!(serialized.contains("Method 'unknown_tool' not found"));
    assert!(!serialized.contains(r#""result""#));
}

#[test]
fn test_standard_error_codes() {
    assert_eq!(PARSE_ERROR, -32700);
    assert_eq!(INVALID_REQUEST, -32600);
    assert_eq!(METHOD_NOT_FOUND, -32601);
    assert_eq!(INVALID_PARAMS, -32602);
    assert_eq!(INTERNAL_ERROR, -32603);
}

#[test]
fn test_smartfs_error_to_jsonrpc_error_mapping() {
    let err = SmartFsError::SyntaxError("missing param".to_string());
    let rpc_err = JsonRpcError::from(err);
    assert_eq!(rpc_err.code, INVALID_PARAMS);

    let err = SmartFsError::NotFound("inode not found".to_string());
    let rpc_err = JsonRpcError::from(err);
    assert_eq!(rpc_err.code, -32004);

    let err = SmartFsError::Conflict("token expired".to_string());
    let rpc_err = JsonRpcError::from(err);
    assert_eq!(rpc_err.code, -32009);

    let err = SmartFsError::Db("connection failed".to_string());
    let rpc_err = JsonRpcError::from(err);
    assert_eq!(rpc_err.code, INTERNAL_ERROR);
}

#[test]
fn test_tool_definitions_catalog() {
    let defs = get_tool_definitions();
    assert!(defs.len() >= 13);

    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"search_semantic"));
    assert!(names.contains(&"search_functions"));
    assert!(names.contains(&"get_file_history"));
    assert!(names.contains(&"query_by_metadata"));
    assert!(names.contains(&"find_by_hash"));
    assert!(names.contains(&"get_file_content"));
    assert!(names.contains(&"find_broken_files"));
    assert!(names.contains(&"diff_functions"));
    assert!(names.contains(&"search_by_concept"));
    assert!(names.contains(&"search_fulltext"));
    assert!(names.contains(&"get_actor_activity"));
    assert!(names.contains(&"describe_plugin_type"));
    assert!(names.contains(&"list_plugin_types"));
    assert!(names.contains(&"delete_file"));
    assert!(names.contains(&"overwrite_file"));
}
