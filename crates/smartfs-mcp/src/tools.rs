//! Tool catalog and MCP schema definitions for `tools/list`.

use serde::{Deserialize, Serialize};

/// @id: c7007471-9175-48a4-9454-be815051e2b3
/// MCP Tool descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// @id: 742a1595-fa7e-4a4a-be13-def4175c648d
/// Returns the full list of tool definitions provided by SmartFS MCP server.
pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "search_semantic".to_string(),
            description: "Cosine similarity search across embeddings (embeddings_384, embeddings_768, embeddings_1024_qwen).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language query text (embedded on server if query_vector omitted)" },
                    "query_vector": { "type": "array", "items": { "type": "number" }, "description": "Dense embedding vector" },
                    "model_id": { "type": "string", "description": "Embedding model UUID" },
                    "limit": { "type": "integer", "description": "Maximum number of results to return" },
                    "type_filter": { "type": "string", "description": "Optional plugin_type filter" }
                }
            }),
        },
        ToolDefinition {
            name: "search_functions".to_string(),
            description: "Cosine search on function-level AST embeddings (ast_embeddings_1536).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Function description or signature text (embedded on server if query_vector omitted)" },
                    "query_vector": { "type": "array", "items": { "type": "number" }, "description": "Dense embedding vector" },
                    "language": { "type": "string", "description": "Target programming language" },
                    "kind": { "type": "string", "description": "AST node kind filter (e.g. function, method)" },
                    "limit": { "type": "integer", "description": "Maximum number of results" }
                }
            }),
        },
        ToolDefinition {
            name: "get_file_history".to_string(),
            description: "Resolves path to inode and returns versions with parent_version_id chain.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "Filesystem path" }
                }
            }),
        },
        ToolDefinition {
            name: "query_by_metadata".to_string(),
            description: "Structured query on special_data JSONB attributes.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["filter"],
                "properties": {
                    "filter": { "type": "object", "description": "JSON filter to match in special_data" }
                }
            }),
        },
        ToolDefinition {
            name: "find_by_hash".to_string(),
            description: "Find an existing file version record by its content SHA-256 hash.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["content_hash"],
                "properties": {
                    "content_hash": { "type": "string", "description": "Hex-encoded SHA-256 content hash" }
                }
            }),
        },
        ToolDefinition {
            name: "get_file_content".to_string(),
            description: "Fetches raw file content or text from storage backend for a given path and version.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "Filesystem path" },
                    "version": { "type": "integer", "description": "Optional specific version number (default: latest)" }
                }
            }),
        },
        ToolDefinition {
            name: "find_broken_files".to_string(),
            description: "Finds file versions where processing status is 'syntax_error'.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "diff_functions".to_string(),
            description: "Function-level AST diff between two versions of a file (added, changed, removed).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["path", "v1", "v2"],
                "properties": {
                    "path": { "type": "string", "description": "Filesystem path" },
                    "v1": { "type": "integer", "description": "First version number" },
                    "v2": { "type": "integer", "description": "Second version number" }
                }
            }),
        },
        ToolDefinition {
            name: "search_by_concept".to_string(),
            description: "Searches centroid graph and working memory. If query_vector and query absent, returns active centroids sorted by member_count (ADR-50/53).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Concept query text (embedded on server if query_vector omitted; if both omitted, browses active centroids)" },
                    "query_vector": { "type": "array", "items": { "type": "number" }, "description": "Optional dense query vector" },
                    "plugin_type": { "type": "string", "description": "Optional plugin type filter" },
                    "model_id": { "type": "string", "description": "Optional model UUID" },
                    "limit": { "type": "integer", "description": "Maximum number of results" }
                }
            }),
        },
        ToolDefinition {
            name: "search_fulltext".to_string(),
            description: "Literal BM25 fulltext search querying both file versions and code AST nodes (ADR-54).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "description": "Fulltext search query" },
                    "plugin_type": { "type": "string", "description": "Optional plugin type filter" },
                    "limit": { "type": "integer", "description": "Maximum hits to return (default: 20)" }
                }
            }),
        },
        ToolDefinition {
            name: "get_actor_activity".to_string(),
            description: "Queries file versions authored by a specific agent actor_id (ADR-56).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["actor_id"],
                "properties": {
                    "actor_id": { "type": "string", "description": "Agent/actor identifier" },
                    "since": { "type": "string", "description": "ISO 8601 timestamp cutoff" },
                    "limit": { "type": "integer", "description": "Maximum results to return" }
                }
            }),
        },
        ToolDefinition {
            name: "describe_plugin_type".to_string(),
            description: "Describes the schema, description, and properties of a registered plugin type (ADR-57).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["plugin_type"],
                "properties": {
                    "plugin_type": { "type": "string", "description": "Plugin type identifier (e.g. rust, png)" }
                }
            }),
        },
        ToolDefinition {
            name: "list_plugin_types".to_string(),
            description: "Lists all registered plugin types with descriptions and match extensions (ADR-57).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "delete_file".to_string(),
            description: "Destructive file deletion. Generates confirmation token on first request; requires token to execute (ADR-48).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "Filesystem path to delete" },
                    "confirmation_token": { "type": "string", "description": "Token obtained from prior call" }
                }
            }),
        },
        ToolDefinition {
            name: "overwrite_file".to_string(),
            description: "Destructive file overwrite. Generates confirmation token on first request; requires token to execute (ADR-48).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": { "type": "string", "description": "Filesystem path to overwrite" },
                    "content": { "type": "string", "description": "New file content" },
                    "confirmation_token": { "type": "string", "description": "Token obtained from prior call" }
                }
            }),
        },
    ]
}
