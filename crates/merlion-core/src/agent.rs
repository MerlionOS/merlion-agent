use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::mpsc;

use crate::approval::{AllowAllApprover, ApprovalDecision, ToolApprover};
use crate::error::{Error, Result};
use crate::llm::{LlmClient, LlmRequest, LlmStreamEvent, Usage};
use crate::message::{Message, Role, ToolCall, ToolResult};
use crate::tool::ToolRegistry;

#[derive(Debug, Clone)]
pub struct AgentOptions {
    pub model: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Hard cap on assistant ↔ tool round-trips inside a single
    /// [`Agent::run`] call. Hermes calls this the iteration budget; matching
    /// that vocabulary makes the port read like the original.
    pub max_iterations: u32,
    /// Cap on tool-result `content` length in characters. Anything longer is
    /// truncated with a `…[truncated tool output]` suffix before being fed
    /// back to the model. Individual tools may further restrict themselves;
    /// this is a backstop. `0` disables truncation.
    pub max_tool_result_chars: usize,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            model: "gpt-4o-mini".into(),
            temperature: None,
            max_tokens: None,
            max_iterations: 32,
            max_tool_result_chars: 16 * 1024,
        }
    }
}

/// Events emitted while the agent runs — consumed by the CLI to render
/// streaming output and tool activity.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    AssistantDelta(String),
    AssistantMessage(Message),
    ToolCallStart { id: String, name: String, arguments: serde_json::Value },
    ToolCallFinish { id: String, name: String, content: String, is_error: bool },
    Usage(Usage),
    IterationBudgetExhausted,
    Done,
}

/// Drives the assistant ↔ tool loop. One [`Agent`] is reusable across many
/// `run` calls; the conversation lives in the caller-owned `messages` Vec.
pub struct Agent {
    llm: Arc<dyn LlmClient>,
    tools: ToolRegistry,
    options: AgentOptions,
    approver: Arc<dyn ToolApprover>,
}

impl Agent {
    pub fn new(llm: Arc<dyn LlmClient>, tools: ToolRegistry, options: AgentOptions) -> Self {
        Self { llm, tools, options, approver: Arc::new(AllowAllApprover) }
    }

    /// Install a tool approver. Defaults to [`AllowAllApprover`] — replace
    /// with a real implementation (e.g. the CLI's console prompter) to gate
    /// sensitive tools.
    pub fn with_approver(mut self, approver: Arc<dyn ToolApprover>) -> Self {
        self.approver = approver;
        self
    }

    pub fn options(&self) -> &AgentOptions {
        &self.options
    }

    pub fn options_mut(&mut self) -> &mut AgentOptions {
        &mut self.options
    }

    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Run the agent until the model returns a turn with no tool calls or the
    /// iteration budget is exhausted. `messages` is mutated in place to
    /// reflect the new turns. Streaming events are pushed to `events`.
    pub async fn run(
        &self,
        messages: &mut Vec<Message>,
        events: mpsc::Sender<AgentEvent>,
    ) -> Result<()> {
        for _ in 0..self.options.max_iterations {
            let req = LlmRequest {
                model: self.options.model.clone(),
                messages: messages.clone(),
                tools: self.tools.schemas(),
                temperature: self.options.temperature,
                max_tokens: self.options.max_tokens,
            };

            let mut stream = self.llm.stream(req).await?;
            let mut text_buf = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();

            while let Some(ev) = stream.next().await {
                match ev? {
                    LlmStreamEvent::Delta(s) => {
                        text_buf.push_str(&s);
                        let _ = events.send(AgentEvent::AssistantDelta(s)).await;
                    }
                    LlmStreamEvent::ToolCalls(calls) => {
                        tool_calls = calls;
                    }
                    LlmStreamEvent::Usage(u) => {
                        let _ = events.send(AgentEvent::Usage(u)).await;
                    }
                    LlmStreamEvent::Done(_) => break,
                }
            }

            let assistant_msg = if tool_calls.is_empty() {
                Message::assistant_text(text_buf.clone())
            } else if text_buf.is_empty() {
                Message::assistant_tool_calls(tool_calls.clone())
            } else {
                // Both content and tool calls — OpenAI allows this; keep both.
                Message {
                    role: Role::Assistant,
                    content: Some(text_buf.clone()),
                    tool_calls: tool_calls.clone(),
                    tool_call_id: None,
                    name: None,
                }
            };
            messages.push(assistant_msg.clone());
            let _ = events.send(AgentEvent::AssistantMessage(assistant_msg)).await;

            if tool_calls.is_empty() {
                let _ = events.send(AgentEvent::Done).await;
                return Ok(());
            }

            for call in tool_calls {
                let decision = self.approver.approve(&call.name, &call.arguments).await;

                // Emit Start *after* approval so the CLI's render output and
                // the approver's stdin prompt don't fight for the terminal.
                let _ = events
                    .send(AgentEvent::ToolCallStart {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    })
                    .await;

                let mut result = match decision {
                    ApprovalDecision::Deny { reason } => ToolResult {
                        tool_call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: format!("tool rejected by user: {reason}"),
                        is_error: true,
                    },
                    ApprovalDecision::Allow => match self.tools.get(&call.name) {
                        Ok(tool) => tool.call(&call.id, call.arguments.clone()).await,
                        Err(e) => ToolResult {
                            tool_call_id: call.id.clone(),
                            name: call.name.clone(),
                            content: format!("error: {e}"),
                            is_error: true,
                        },
                    },
                };
                if self.options.max_tool_result_chars > 0
                    && result.content.len() > self.options.max_tool_result_chars
                {
                    result.content.truncate(self.options.max_tool_result_chars);
                    result.content.push_str("\n…[truncated tool output]");
                }

                let _ = events
                    .send(AgentEvent::ToolCallFinish {
                        id: result.tool_call_id.clone(),
                        name: result.name.clone(),
                        content: result.content.clone(),
                        is_error: result.is_error,
                    })
                    .await;

                messages.push(Message::tool_response(result));
            }
        }

        let _ = events.send(AgentEvent::IterationBudgetExhausted).await;
        Err(Error::BudgetExhausted)
    }
}
