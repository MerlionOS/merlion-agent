//! `task` tool — spawn an isolated sub-conversation that shares the parent
//! [`Agent`]'s LLM client, tool registry, and approver but starts with a
//! fresh [`Vec<Message>`]. The parent's context is never leaked.
//!
//! The sub-agent inherits the parent's iteration budget (`max_iterations`)
//! through the shared `Agent` instance — there is no explicit recursion
//! check. A runaway recursive `task` chain is bounded by that budget on
//! every level.

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use merlion_core::{Agent, AgentEvent, Message, Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const MAX_REPLY_BYTES: usize = 16 * 1024;

const DEFAULT_SUB_SYSTEM_PROMPT: &str =
    "You are running as a subagent invoked via the `task` tool. You have no \
     memory of any prior conversation — your only context is the prompt the \
     parent agent sent. Focus narrowly on that task. When you have the \
     answer, reply with it directly as your final message.";

/// Sub-agent dispatch tool. The agent reference is injected after `Agent`
/// construction via [`TaskTool::install_agent`] (a `Weak` is stored so the
/// inevitable `Agent` ⇄ `ToolRegistry` ⇄ `TaskTool` cycle does not leak).
pub struct TaskTool {
    agent: OnceLock<Weak<Agent>>,
    /// Optional system-prompt override applied to every sub-task. When
    /// `None`, sub-tasks use [`DEFAULT_SUB_SYSTEM_PROMPT`]. A per-call
    /// override via the `system_prompt` argument takes priority over both.
    sub_system_prompt: Option<String>,
}

impl TaskTool {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            agent: OnceLock::new(),
            sub_system_prompt: None,
        })
    }

    pub fn with_system_prompt(prompt: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            agent: OnceLock::new(),
            sub_system_prompt: Some(prompt.into()),
        })
    }

    /// Install the parent [`Agent`] handle. Must be called once after the
    /// `Agent` is wrapped in an `Arc`. Storing a `Weak` breaks the cycle
    /// `Agent` → `ToolRegistry` → `TaskTool` → `Agent`.
    pub fn install_agent(&self, agent: &Arc<Agent>) {
        let _ = self.agent.set(Arc::downgrade(agent));
    }
}

#[derive(Debug, Deserialize)]
struct Args {
    prompt: String,
    #[serde(default)]
    system_prompt: Option<String>,
}

#[async_trait]
impl Tool for TaskTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "task".into(),
            description:
                "Delegate a focused subtask to a subagent. The subagent shares this agent's tools \
                 and model but starts with NO conversation history — its only context is the \
                 `prompt` argument. Use this when a task is large enough that running it inline \
                 would bloat your context, or when you want to parallelize multiple independent \
                 investigations. Returns the subagent's final text reply (truncated at 16 KiB)."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The task to give the subagent. Treat it like a fresh user message — include any context the subagent needs, since it cannot see the parent conversation."
                    },
                    "system_prompt": {
                        "type": "string",
                        "description": "Optional system-prompt override for this sub-task. If omitted, a generic subagent system message is used."
                    }
                },
                "required": ["prompt"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: Value) -> ToolResult {
        let agent = match self.agent.get().and_then(Weak::upgrade) {
            Some(a) => a,
            None => {
                return err(
                    call_id,
                    "task tool has no Agent installed — call TaskTool::install_agent \
                     after constructing the Agent (the parent Agent must outlive the tool)",
                );
            }
        };

        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, &format!("invalid arguments: {e}")),
        };

        let system_text = parsed
            .system_prompt
            .or_else(|| self.sub_system_prompt.clone())
            .unwrap_or_else(|| DEFAULT_SUB_SYSTEM_PROMPT.to_string());

        let mut messages = vec![Message::system(system_text), Message::user(parsed.prompt)];

        let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
        let agent_for_run = agent.clone();
        let run_handle = tokio::spawn(async move {
            let mut msgs = std::mem::take(&mut messages);
            let res = agent_for_run.run(&mut msgs, tx).await;
            (res, msgs)
        });

        let mut final_text = String::new();
        let mut budget_exhausted = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::AssistantMessage(msg) => {
                    if msg.tool_calls.is_empty() {
                        if let Some(text) = msg.content {
                            final_text = text;
                        }
                    }
                }
                AgentEvent::ToolCallStart { id, name, .. } => {
                    tracing::debug!(target: "merlion::task", subagent_call = %id, tool = %name, "subagent tool call");
                }
                AgentEvent::ToolCallFinish {
                    id, name, is_error, ..
                } => {
                    tracing::debug!(target: "merlion::task", subagent_call = %id, tool = %name, is_error, "subagent tool finish");
                }
                AgentEvent::IterationBudgetExhausted => {
                    budget_exhausted = true;
                }
                AgentEvent::AssistantDelta(_) | AgentEvent::Usage(_) | AgentEvent::Done => {}
            }
        }

        let (run_result, _msgs) = match run_handle.await {
            Ok(pair) => pair,
            Err(e) => return err(call_id, &format!("subagent task panicked: {e}")),
        };

        if let Err(e) = run_result {
            let msg = if budget_exhausted {
                format!("subagent exhausted iteration budget: {e}")
            } else {
                format!("subagent failed: {e}")
            };
            return err(call_id, &msg);
        }

        if final_text.is_empty() {
            final_text = if budget_exhausted {
                "subagent produced no final reply (iteration budget exhausted)".to_string()
            } else {
                "subagent produced no final reply".to_string()
            };
        }

        if final_text.len() > MAX_REPLY_BYTES {
            final_text.truncate(MAX_REPLY_BYTES);
            final_text.push_str("\n…[truncated subagent reply]");
        }

        ToolResult {
            tool_call_id: call_id.into(),
            name: "task".into(),
            content: final_text,
            is_error: false,
        }
    }
}

fn err(call_id: &str, msg: &str) -> ToolResult {
    ToolResult {
        tool_call_id: call_id.into(),
        name: "task".into(),
        content: msg.into(),
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_shape() {
        let tool = TaskTool::new();
        let s = tool.schema();
        assert_eq!(s.name, "task");
        let props = s
            .parameters
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("parameters.properties is an object");
        assert!(props.contains_key("prompt"), "schema must declare `prompt`");
        assert!(
            props.contains_key("system_prompt"),
            "schema must declare `system_prompt`"
        );
        let required = s
            .parameters
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required is an array");
        assert!(required.iter().any(|v| v == "prompt"));
    }

    #[tokio::test]
    async fn call_without_install_returns_error() {
        let tool = TaskTool::new();
        let res = tool.call("call_1", json!({ "prompt": "anything" })).await;
        assert!(res.is_error, "must be is_error when no agent installed");
        assert_eq!(res.tool_call_id, "call_1");
        assert_eq!(res.name, "task");
        assert!(
            res.content.contains("install_agent"),
            "error message should mention install_agent; got: {}",
            res.content
        );
    }
}
