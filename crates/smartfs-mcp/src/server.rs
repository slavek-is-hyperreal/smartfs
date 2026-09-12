//! MCP server runner providing JSON-RPC 2.0 framing over stdio.

use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{error, info};

use crate::handler::ToolHandler;
use crate::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use crate::tools::get_tool_definitions;
use smartfs_schema::error::Result;

/// @id: 6ad16b38-b1d6-4a00-ba4c-3ccb7d82e556
/// Stdio MCP server runner.
pub struct McpServer {
    handler: Arc<ToolHandler>,
}

/// @id: 83be4353-d6c9-4274-b134-c7e0f2100985
impl McpServer {
    /// @id: 0f967d95-af26-48c1-a568-8fcf812ce8fd
    /// Creates a new `McpServer` wrapping a `ToolHandler`.
    pub fn new(handler: ToolHandler) -> Self {
        Self {
            handler: Arc::new(handler),
        }
    }

    /// @id: 67e6dcee-389c-4143-8b45-5ca9d177ae93
    /// Access the underlying `ToolHandler`.
    pub fn handler(&self) -> &ToolHandler {
        &self.handler
    }

    /// @id: 261abf6b-9cc7-494d-b4d2-f4a001f558a6
    /// Runs the stdio JSON-RPC loop (one request per line, one response per line).
    pub async fn run_stdio(&self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin).lines();

        info!("Starting smartfs-mcp JSON-RPC 2.0 stdio server loop");

        while let Some(line) = reader.next_line().await? {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some(resp) = self.handle_line(line).await {
                let serialized = serde_json::to_string(&resp)
                    .unwrap_or_else(|e| format!(r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"Internal error: {e}"}}}}"#));

                stdout.write_all(serialized.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }

        info!("smartfs-mcp stdio loop terminated (EOF)");
        Ok(())
    }

    /// @id: 437b6edc-60e5-4e5e-945b-d69ff46c8a22
    /// Processes a single raw line of input and returns an optional JSON-RPC response.
    pub async fn handle_line(&self, line: &str) -> Option<JsonRpcResponse> {
        let req: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                error!("JSON parse error: {e}");
                return Some(JsonRpcResponse::error(
                    None,
                    JsonRpcError::parse_error(format!("Parse error: {e}")),
                ));
            }
        };

        self.handle_request(req).await
    }

    /// @id: 50bedb66-50fe-4ca2-a62f-71245872e6c0
    /// Dispatches a structured `JsonRpcRequest` and returns an optional `JsonRpcResponse`.
    pub async fn handle_request(&self, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
        if req.jsonrpc != "2.0" {
            return Some(JsonRpcResponse::error(
                req.id,
                JsonRpcError::invalid_request("Field 'jsonrpc' must be '2.0'"),
            ));
        }

        let id = req.id.clone();

        match req.method.as_str() {
            // MCP protocol handshake
            "initialize" => {
                let init_res = serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "smartfs-mcp",
                        "version": "0.1.0"
                    }
                });
                Some(JsonRpcResponse::success(id, init_res))
            }

            // Notification: initialized
            "notifications/initialized" => None,

            // MCP tools discovery
            "tools/list" => {
                let tools = get_tool_definitions();
                let res = serde_json::json!({ "tools": tools });
                Some(JsonRpcResponse::success(id, res))
            }

            // Standard MCP tool invocation
            "tools/call" => {
                let params = req.params.unwrap_or(serde_json::Value::Null);
                let tool_name = match params.get("name").and_then(|n| n.as_str()) {
                    Some(n) => n,
                    None => {
                        return Some(JsonRpcResponse::error(
                            id,
                            JsonRpcError::invalid_params("Missing tool name in tools/call"),
                        ));
                    }
                };

                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                match self.handler.dispatch_tool_call(tool_name, arguments).await {
                    Ok(val) => {
                        let text_content = match serde_json::to_string(&val) {
                            Ok(s) => s,
                            Err(e) => format!("Serialization error: {e}"),
                        };
                        let mcp_res = serde_json::json!({
                            "content": [
                                {
                                    "type": "text",
                                    "text": text_content
                                }
                            ],
                            "isError": false
                        });
                        Some(JsonRpcResponse::success(id, mcp_res))
                    }
                    Err(e) => {
                        let err_resp = serde_json::json!({
                            "content": [
                                {
                                    "type": "text",
                                    "text": format!("Error: {e}")
                                }
                            ],
                            "isError": true
                        });
                        Some(JsonRpcResponse::success(id, err_resp))
                    }
                }
            }

            // Direct tool calls by method name (e.g. method: "get_file_history", params: {...})
            direct_method => {
                let params = req.params.unwrap_or(serde_json::Value::Null);
                match self.handler.dispatch_tool_call(direct_method, params).await {
                    Ok(val) => Some(JsonRpcResponse::success(id, val)),
                    Err(e) => Some(JsonRpcResponse::error(id, JsonRpcError::from(e))),
                }
            }
        }
    }
}
