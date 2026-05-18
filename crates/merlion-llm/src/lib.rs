//! LLM provider adapters.
//!
//! - [`OpenAiClient`] speaks `/v1/chat/completions` and covers Nous Portal,
//!   OpenRouter, NovitaAI, NVIDIA NIM, Moonshot, MiniMax, z.ai/GLM, Groq,
//!   DeepSeek, and any other endpoint that implements the chat-completions
//!   protocol.
//! - [`AnthropicClient`] speaks Anthropic's native `/v1/messages` API with
//!   `x-api-key` auth, top-level `system`, and tool_use/tool_result content
//!   blocks.

pub mod anthropic;
pub mod openai;

pub use anthropic::AnthropicClient;
pub use openai::OpenAiClient;
