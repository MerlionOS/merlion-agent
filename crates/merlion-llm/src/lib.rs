//! LLM provider adapters.
//!
//! - [`OpenAiClient`] — `/v1/chat/completions` (covers 9 providers).
//! - [`AnthropicClient`] — Anthropic native `/v1/messages`.
//! - [`GeminiClient`] — Google AI Studio `streamGenerateContent`.
//! - [`BedrockClient`] — AWS Bedrock (Anthropic models, SigV4-signed, non-streaming).
//! - [`VertexClient`] — Google Vertex AI (Gemini wire, gcloud OAuth bearer).

pub mod anthropic;
pub mod bedrock;
pub mod fallback;
pub mod gemini;
pub mod openai;
pub mod retry;
pub mod vertex;

pub use anthropic::AnthropicClient;
pub use bedrock::BedrockClient;
pub use fallback::FallbackLlmClient;
pub use gemini::GeminiClient;
pub use openai::OpenAiClient;
pub use vertex::VertexClient;
