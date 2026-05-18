//! LLM provider adapters.
//!
//! - [`OpenAiClient`] speaks `/v1/chat/completions` and covers Nous Portal,
//!   OpenRouter, NovitaAI, NVIDIA NIM, Moonshot, MiniMax, z.ai/GLM, Groq,
//!   DeepSeek, and any other endpoint that implements the chat-completions
//!   protocol.
//! - [`AnthropicClient`] speaks Anthropic's native `/v1/messages` API with
//!   `x-api-key` auth, top-level `system`, and tool_use/tool_result content
//!   blocks.
//! - [`GeminiClient`] speaks Google's `models/<m>:streamGenerateContent`
//!   with `x-goog-api-key` auth, `system_instruction`, and
//!   `functionCall`/`functionResponse` parts.

pub mod anthropic;
pub mod gemini;
pub mod openai;

pub use anthropic::AnthropicClient;
pub use gemini::GeminiClient;
pub use openai::OpenAiClient;
