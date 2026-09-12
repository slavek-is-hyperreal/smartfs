//! Domain types, data transfer objects, and tool response structures for SmartFS MCP.

use serde::{Deserialize, Serialize};
use smartfs_db::AstNodeRecord;
use uuid::Uuid;

/// @id: 499ea5a7-7977-4774-9f16-96d62142c8d0
/// Difference in AST nodes between two versions of a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AstDiff {
    pub added: Vec<AstNodeRecord>,
    pub changed: Vec<AstNodeRecord>,
    pub removed: Vec<AstNodeRecord>,
}

/// @id: 98acd6fb-4e73-44b8-8dc8-960c773c3477
/// Detailed plugin schema as described in ADR-57 and Architecture §10.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSchema {
    #[serde(rename = "type")]
    pub plugin_type: String,
    pub description: String,
    pub match_extensions: Vec<String>,
    pub schema: serde_json::Value,
    pub ast: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<serde_json::Value>,
}

/// @id: c365be44-7e34-4939-a789-3bef1b465fd8
/// Brief summary of a registered plugin type (ADR-57).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSummary {
    #[serde(rename = "type")]
    pub plugin_type: String,
    pub description: String,
    pub match_extensions: Vec<String>,
}

/// @id: 73da8ab2-c824-468c-858c-3f044453382e
/// Active concept centroid summary for topic discovery (ADR-50/53).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CentroidSummary {
    pub id: Uuid,
    pub label: Option<String>,
    pub member_count: i64,
    pub sample_names: Vec<String>,
}

/// @id: dcac812e-3acd-490a-a90b-58205607b365
/// Result of retrieving file content from storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileContentResult {
    pub path: String,
    pub version: i32,
    pub size: i64,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_base64: Option<String>,
}

/// @id: 0b347358-a708-4f07-bb75-96e6b5d31e43
/// Response when a destructive operation requires confirmation (ADR-48).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmationRequired {
    pub status: String,
    pub confirmation_token: String,
    pub action: String,
    pub path: String,
    pub message: String,
}

/// @id: 758033a5-a4c9-463a-ba6c-7b8e3e47daa5
/// Result of a completed destructive action upon valid confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestructiveResult {
    pub status: String,
    pub action: String,
    pub path: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<Uuid>,
}
