use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::message::{Message, ToolCall};
use crate::tool::ToolSchema;

/// Token-usage summary reported by the LLM at the end of a stream.
/// All three shipped adapters populate this when the provider reports it;
/// fields are `None` if the provider doesn't include them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

impl Usage {
    pub fn merge(&mut self, other: &Usage) {
        if let Some(p) = other.prompt_tokens {
            *self.prompt_tokens.get_or_insert(0) += p;
        }
        if let Some(c) = other.completion_tokens {
            *self.completion_tokens.get_or_insert(0) += c;
        }
        if let Some(t) = other.total_tokens {
            *self.total_tokens.get_or_insert(0) += t;
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

/// A complete (non-streaming) LLM response. Either content or tool_calls (or
/// both) may be present.
#[derive(Debug, Clone, Default)]
pub struct LlmResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

/// Incremental events while streaming a response. The agent loop consumes
/// these to render output token-by-token and to know when tool calls are
/// finalized.
#[derive(Debug, Clone)]
pub enum LlmStreamEvent {
    /// A chunk of assistant text content.
    Delta(String),
    /// One or more tool calls have been emitted (final, ready to dispatch).
    ToolCalls(Vec<ToolCall>),
    /// Token-usage summary. Emitted at most once per stream — usually just
    /// before `Done` — when the provider reports usage. Adapters that don't
    /// report usage simply skip this event.
    Usage(Usage),
    /// Stream finished. The string is the model-reported finish_reason
    /// ("stop", "tool_calls", "length", ...).
    Done(Option<String>),
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Non-streaming call. Default implementation collects from the stream.
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse> {
        use futures::StreamExt;
        let mut stream = self.stream(req).await?;
        let mut acc = LlmResponse::default();
        let mut buf = String::new();
        while let Some(ev) = stream.next().await {
            match ev? {
                LlmStreamEvent::Delta(s) => buf.push_str(&s),
                LlmStreamEvent::ToolCalls(calls) => acc.tool_calls = calls,
                LlmStreamEvent::Usage(u) => acc.usage = Some(u),
                LlmStreamEvent::Done(reason) => acc.finish_reason = reason,
            }
        }
        if !buf.is_empty() {
            acc.content = Some(buf);
        }
        Ok(acc)
    }

    async fn stream(
        &self,
        req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamEvent>>>;
}
