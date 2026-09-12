//! smartfs-mcp — Model Context Protocol (MCP) server for SmartFS.
//!
//! Exclusively owns:
//! - MCP protocol handling and tool catalog
//! - JSON-RPC 2.0 framing over stdin/stdout
//! - Confirmation token mechanism for destructive actions (ADR-48)
//! - Tool dispatching through `smartfs-db`, `smartfs-semantic`, and `smartfs-store` public APIs
//! - Agent continuity queries (ADR-56)
//! - Pick-style plugin schema reflection (ADR-57)
//!
//! Enforces:
//! - NEVER use raw SQL inline — all data access through public APIs
//! - Line-delimited JSON-RPC 2.0 framing (one request per line, one response per line)
//! - Destructive operations requires token generation on first request, execution only on confirmation

pub mod handler;
pub mod protocol;
pub mod server;
pub mod tokens;
pub mod tools;
pub mod types;

pub use handler::ToolHandler;
pub use protocol::{
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, INTERNAL_ERROR, INVALID_PARAMS,
    INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
};
pub use server::McpServer;
pub use tokens::{DestructiveAction, PendingConfirmation, TokenManager};
pub use tools::{get_tool_definitions, ToolDefinition};
pub use types::{
    AstDiff, CentroidSummary, ConfirmationRequired, DestructiveResult, FileContentResult,
    PluginSchema, PluginSummary,
};
