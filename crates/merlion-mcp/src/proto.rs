//! Wire types for MCP / JSON-RPC 2.0.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const JSONRPC_VERSION: &str = "2.0";

/// A JSON-RPC request or notification (a notification omits `id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn call(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id: Some(Value::from(id)),
            method: method.into(),
            params,
        }
    }

    pub fn notify(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id: None,
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC response — either success with `result` or failure with `error`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Description of a tool exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema describing the tool's input. The MCP spec calls this
    /// `inputSchema`; we expose it as `input_schema` in Rust.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<McpTool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolResult {
    #[serde(default)]
    pub content: Vec<ContentItem>,
    /// MCP marks tool-execution failures here rather than via the JSON-RPC
    /// `error` field — the model is supposed to see and react to the failure.
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    Text {
        text: String,
    },
    /// Image / resource / etc — captured opaquely so unknown variants don't
    /// break the parse. The MVP only renders `text`; richer types come later.
    #[serde(other)]
    Other,
}

impl ContentItem {
    /// Flatten the content list into a single string for the merlion-core
    /// `ToolResult.content` field. Non-text blocks are noted but skipped.
    pub fn join(items: &[ContentItem]) -> String {
        let mut buf = String::new();
        let mut skipped = 0;
        for it in items {
            match it {
                ContentItem::Text { text } => {
                    if !buf.is_empty() && !buf.ends_with('\n') {
                        buf.push('\n');
                    }
                    buf.push_str(text);
                }
                ContentItem::Other => skipped += 1,
            }
        }
        if skipped > 0 {
            if !buf.is_empty() && !buf.ends_with('\n') {
                buf.push('\n');
            }
            buf.push_str(&format!("[{skipped} non-text content block(s) elided]"));
        }
        buf
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default, rename = "serverInfo")]
    pub server_info: Option<ServerInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_with_jsonrpc_field() {
        let r = Request::call(7, "tools/list", None);
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""jsonrpc":"2.0""#));
        assert!(s.contains(r#""id":7"#));
        assert!(s.contains(r#""method":"tools/list""#));
    }

    #[test]
    fn notification_omits_id() {
        let r = Request::notify("notifications/initialized", None);
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("id").is_none(), "notifications must not carry an id");
    }

    #[test]
    fn content_join_concatenates_text_and_notes_skipped_blocks() {
        let items = vec![
            ContentItem::Text {
                text: "hello".into(),
            },
            ContentItem::Other,
            ContentItem::Text {
                text: "world".into(),
            },
        ];
        let s = ContentItem::join(&items);
        assert!(s.contains("hello") && s.contains("world"));
        assert!(s.contains("elided"));
    }

    #[test]
    fn unknown_content_type_round_trips_as_other() {
        let json = r#"{"type":"image","data":"base64...","mimeType":"image/png"}"#;
        let item: ContentItem = serde_json::from_str(json).unwrap();
        assert!(matches!(item, ContentItem::Other));
    }
}
