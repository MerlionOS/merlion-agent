//! Model Context Protocol client for Merlion Agent.
//!
//! Implements the JSON-RPC 2.0 client side of the [MCP spec][spec] over
//! whichever transport you give it. The shipped transport is `stdio` —
//! spawn a server process and exchange newline-framed JSON-RPC over its
//! stdin/stdout.
//!
//! [spec]: https://spec.modelcontextprotocol.io/specification/2024-11-05/

pub mod client;
pub mod http;
pub mod oauth;
pub mod proto;
pub mod proxy;
pub mod registry;
pub mod stdio;

pub use client::{McpClient, Transport};
pub use http::HttpTransport;
pub use oauth::{OauthFlow, TokenStore, Tokens};
pub use proto::{CallToolResult, ContentItem, McpTool, RpcError, PROTOCOL_VERSION};
pub use proxy::{make_exposed_name, McpProxyTool};
pub use registry::{McpRegistry, ServerEntry, TransportSpec};
pub use stdio::StdioTransport;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("rpc error: {0}")]
    Rpc(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
