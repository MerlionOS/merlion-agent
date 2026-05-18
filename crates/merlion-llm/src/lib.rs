//! OpenAI-compatible LLM client.
//!
//! Covers Nous Portal, OpenRouter, NovitaAI, NVIDIA NIM, Moonshot, MiniMax,
//! z.ai/GLM, OpenAI, and any other endpoint that speaks the chat-completions
//! protocol. Anthropic and Gemini get dedicated adapters in later phases.

pub mod openai;

pub use openai::OpenAiClient;
