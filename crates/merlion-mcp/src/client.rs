//! Transport-agnostic MCP client.
//!
//! The [`Transport`] trait abstracts request/notify; [`McpClient`] sequences
//! the spec's initialize handshake and exposes typed `list_tools` and
//! `call_tool` wrappers on top.

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::proto::{
    CallToolParams, CallToolResult, ClientInfo, InitializeParams, InitializeResult, ListToolsResult,
    McpTool, PROTOCOL_VERSION,
};
use crate::{Error, Result};

#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a JSON-RPC request and await its matching response. The
    /// `Transport` implementation owns id generation and the pending-request
    /// table — callers just pass `method` + `params` and get back the raw
    /// `result` JSON.
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value>;

    /// Send a JSON-RPC notification (no id, no response).
    async fn notify(&self, method: &str, params: Option<Value>) -> Result<()>;

    /// Cleanly shut the transport down. Idempotent.
    async fn close(&self) -> Result<()>;
}

pub struct McpClient {
    transport: Box<dyn Transport>,
    server_info: Mutex<Option<InitializeResult>>,
    client_name: String,
    client_version: String,
}

impl McpClient {
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Self {
            transport,
            server_info: Mutex::new(None),
            client_name: "merlion".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    pub fn with_client_info(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.client_name = name.into();
        self.client_version = version.into();
        self
    }

    /// Run the MCP initialize handshake: send `initialize`, then the
    /// `notifications/initialized` notification. Returns the server's
    /// `InitializeResult` (also stored internally for later inspection).
    pub async fn initialize(&self) -> Result<InitializeResult> {
        let params = InitializeParams {
            protocol_version: PROTOCOL_VERSION.into(),
            capabilities: json!({}),
            client_info: ClientInfo {
                name: self.client_name.clone(),
                version: self.client_version.clone(),
            },
        };
        let raw = self
            .transport
            .request("initialize", Some(serde_json::to_value(&params)?))
            .await?;
        let init: InitializeResult = serde_json::from_value(raw)
            .map_err(|e| Error::Protocol(format!("initialize result: {e}")))?;
        *self.server_info.lock().await = Some(init.clone());
        self.transport.notify("notifications/initialized", None).await?;
        Ok(init)
    }

    /// `tools/list`. Returns the deserialized tool list.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let raw = self.transport.request("tools/list", None).await?;
        let parsed: ListToolsResult = serde_json::from_value(raw)
            .map_err(|e| Error::Protocol(format!("tools/list result: {e}")))?;
        Ok(parsed.tools)
    }

    /// `tools/call`. Returns the `CallToolResult` (content + isError) —
    /// this is *not* an error result; transport- or protocol-level failures
    /// bubble through `Result::Err` instead.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult> {
        let params = CallToolParams { name: name.to_string(), arguments: Some(arguments) };
        let raw = self
            .transport
            .request("tools/call", Some(serde_json::to_value(&params)?))
            .await?;
        let parsed: CallToolResult = serde_json::from_value(raw)
            .map_err(|e| Error::Protocol(format!("tools/call result: {e}")))?;
        Ok(parsed)
    }

    /// Read the cached server info from the most recent `initialize` call.
    pub async fn server_info(&self) -> Option<InitializeResult> {
        self.server_info.lock().await.clone()
    }

    pub async fn close(&self) -> Result<()> {
        self.transport.close().await
    }
}
