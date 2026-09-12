//! JSON-RPC 2.0 protocol types, error codes, and serialization framing.

use serde::{Deserialize, Serialize};
use smartfs_schema::error::SmartFsError;

/// @id: 7ff948c0-d203-4e67-a5e1-1aaf6e494a27
/// Standard JSON-RPC 2.0 error codes.
pub const PARSE_ERROR: i32 = -32700;
/// @id: 7db56804-6d64-42b7-ab20-f7f611aaa130
pub const INVALID_REQUEST: i32 = -32600;
/// @id: 00913364-79cd-476c-8a2f-c61ea93394db
pub const METHOD_NOT_FOUND: i32 = -32601;
/// @id: 6d2fc3ba-5f0f-43d5-8833-407c4770cb07
pub const INVALID_PARAMS: i32 = -32602;
/// @id: 31b2dcb4-79e5-4ffd-aa4b-a3e6297fde7e
pub const INTERNAL_ERROR: i32 = -32603;

/// @id: ae95c3e3-42e2-4d45-9fdd-3d09956d8469
/// JSON-RPC 2.0 incoming request frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default = "default_jsonrpc")]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

fn default_jsonrpc() -> String {
    "2.0".to_string()
}

/// @id: aff33b65-4e12-4cd4-9205-678b8c5889b7
/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcError {
    /// @id: b53f9d43-07f3-4e91-85dc-0d5fcf0d60b9
    pub fn parse_error(detail: impl Into<String>) -> Self {
        Self {
            code: PARSE_ERROR,
            message: detail.into(),
            data: None,
        }
    }

    /// @id: 05002399-cadf-4750-8248-84a4476769da
    pub fn invalid_request(detail: impl Into<String>) -> Self {
        Self {
            code: INVALID_REQUEST,
            message: detail.into(),
            data: None,
        }
    }

    /// @id: 5a2c7966-c584-4782-9b51-a618a9681879
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: METHOD_NOT_FOUND,
            message: format!("Method '{method}' not found"),
            data: None,
        }
    }

    /// @id: 7a94ae6f-f04b-4e1b-8d6e-1017abee04aa
    pub fn invalid_params(detail: impl Into<String>) -> Self {
        Self {
            code: INVALID_PARAMS,
            message: detail.into(),
            data: None,
        }
    }

    /// @id: ea1c4543-bee0-47e5-9fb4-038f67cb55c3
    pub fn internal_error(detail: impl Into<String>) -> Self {
        Self {
            code: INTERNAL_ERROR,
            message: detail.into(),
            data: None,
        }
    }
}

impl From<SmartFsError> for JsonRpcError {
    fn from(err: SmartFsError) -> Self {
        match err {
            SmartFsError::NotFound(msg) => Self {
                code: -32004,
                message: format!("Not found: {msg}"),
                data: None,
            },
            SmartFsError::Conflict(msg) => Self {
                code: -32009,
                message: format!("Conflict: {msg}"),
                data: None,
            },
            SmartFsError::SyntaxError(msg) => Self {
                code: INVALID_PARAMS,
                message: format!("Syntax error: {msg}"),
                data: None,
            },
            SmartFsError::Db(msg) => Self::internal_error(format!("Database error: {msg}")),
            SmartFsError::Store(msg) => Self::internal_error(format!("Storage error: {msg}")),
            SmartFsError::Compression(msg) => Self::internal_error(format!("Compression error: {msg}")),
            SmartFsError::MissingCalibration { plugin_type, model_id } => Self {
                code: -32010,
                message: format!("Missing calibration for {plugin_type}/{model_id}"),
                data: None,
            },
            SmartFsError::AdvisoryLockUnavailable => Self {
                code: -32011,
                message: "Advisory lock unavailable".to_string(),
                data: None,
            },
            SmartFsError::Io(e) => Self::internal_error(format!("I/O error: {e}")),
            SmartFsError::Other(msg) => Self::internal_error(msg),
        }
    }
}

/// @id: a23dab65-4ab3-4c84-abb5-ca8d1d3c342e
/// JSON-RPC 2.0 outgoing response frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// @id: 3bfff210-a801-4f88-bfb4-a8d5f67b65fe
    pub fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: id.or(Some(serde_json::Value::Null)),
            result: Some(result),
            error: None,
        }
    }

    /// @id: 8e7c392a-5c12-4002-ba87-b61f20432d2f
    pub fn error(id: Option<serde_json::Value>, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: id.or(Some(serde_json::Value::Null)),
            result: None,
            error: Some(error),
        }
    }
}
