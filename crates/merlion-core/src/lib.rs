//! merlion-core
//!
//! Shared types for the Merlion Agent: chat messages, tool calls, the tool
//! trait, and the agent loop that drives an LLM ↔ tool conversation.
//!
//! This crate is intentionally provider-agnostic. `merlion-llm` plugs a
//! concrete LLM behind [`LlmClient`], and `merlion-tools` plugs concrete
//! tools behind [`Tool`].

pub mod agent;
pub mod approval;
pub mod curator;
pub mod error;
pub mod llm;
pub mod message;
pub mod tool;

pub use agent::{Agent, AgentEvent, AgentOptions};
pub use approval::{AllowAllApprover, ApprovalDecision, DenyAllApprover, ToolApprover};
pub use curator::Curator;
pub use error::{Error, Result};
pub use llm::{LlmClient, LlmRequest, LlmResponse, LlmStreamEvent, Usage};
pub use message::{Message, Role, ToolCall, ToolResult};
pub use tool::{Tool, ToolRegistry, ToolSchema};
