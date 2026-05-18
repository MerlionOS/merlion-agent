//! Integration test for the Agent approval gate. Uses a fake LLM that
//! always emits a single bash tool call, plus a DenyAllApprover, to assert
//! that the tool dispatch path is bypassed and a tool_rejected message is
//! synthesized in its place.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use merlion_core::{
    Agent, AgentEvent, AgentOptions, AllowAllApprover, DenyAllApprover, LlmClient, LlmRequest,
    LlmStreamEvent, Message, Result, Tool, ToolCall, ToolRegistry, ToolResult, ToolSchema,
};
use serde_json::json;
use tokio::sync::mpsc;

/// LlmClient that emits one bash tool_call on the first call, then a final
/// "done" assistant message on the second. Lets us drive a complete
/// agent.run loop without a real backend.
struct ScriptedLlm {
    call_index: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl LlmClient for ScriptedLlm {
    async fn stream(
        &self,
        _req: LlmRequest,
    ) -> Result<BoxStream<'static, Result<LlmStreamEvent>>> {
        let idx = self.call_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let events: Vec<Result<LlmStreamEvent>> = if idx == 0 {
            vec![
                Ok(LlmStreamEvent::ToolCalls(vec![ToolCall {
                    id: "call_42".into(),
                    name: "bash".into(),
                    arguments: json!({ "command": "rm -rf /" }),
                }])),
                Ok(LlmStreamEvent::Done(Some("tool_calls".into()))),
            ]
        } else {
            vec![
                Ok(LlmStreamEvent::Delta("ok, I stopped.".into())),
                Ok(LlmStreamEvent::Done(Some("stop".into()))),
            ]
        };
        Ok(futures::stream::iter(events).boxed())
    }
}

/// Tool that fails the test if it's ever actually invoked.
struct ForbiddenBash;

#[async_trait]
impl Tool for ForbiddenBash {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash".into(),
            description: "should never run".into(),
            parameters: json!({"type": "object"}),
        }
    }
    async fn call(&self, _call_id: &str, _args: serde_json::Value) -> ToolResult {
        panic!("ForbiddenBash was invoked despite a denying approver");
    }
}

async fn collect_events(mut rx: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Some(ev) = rx.recv().await {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn denying_approver_blocks_dispatch_and_synthesizes_error_result() {
    let llm = Arc::new(ScriptedLlm { call_index: 0.into() });
    let mut tools = ToolRegistry::new();
    tools.register(ForbiddenBash);

    let agent = Agent::new(llm, tools, AgentOptions::default())
        .with_approver(Arc::new(DenyAllApprover { reason: "blocked in test".into() }));

    let (tx, rx) = mpsc::channel::<AgentEvent>(64);
    let mut messages = vec![Message::user("clean up")];
    let collector = tokio::spawn(collect_events(rx));

    agent.run(&mut messages, tx).await.unwrap();
    let events = collector.await.unwrap();

    let denied = events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallFinish { is_error: true, content, .. }
            if content.contains("tool rejected by user") && content.contains("blocked in test")
    ));
    assert!(denied, "expected a ToolCallFinish carrying the deny reason; got {events:#?}");

    let tool_msg = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, merlion_core::Role::Tool))
        .expect("expected a tool message in history");
    assert!(tool_msg.content.as_deref().unwrap().contains("blocked in test"));
}

#[tokio::test]
async fn default_approver_allows_dispatch() {
    let llm = Arc::new(ScriptedLlm { call_index: 0.into() });
    let mut tools = ToolRegistry::new();

    struct OkBash;
    #[async_trait]
    impl Tool for OkBash {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "bash".into(),
                description: "echoes".into(),
                parameters: json!({"type": "object"}),
            }
        }
        async fn call(&self, call_id: &str, _args: serde_json::Value) -> ToolResult {
            ToolResult {
                tool_call_id: call_id.into(),
                name: "bash".into(),
                content: "did the thing".into(),
                is_error: false,
            }
        }
    }
    tools.register(OkBash);

    let agent = Agent::new(llm, tools, AgentOptions::default())
        .with_approver(Arc::new(AllowAllApprover));

    let (tx, rx) = mpsc::channel::<AgentEvent>(64);
    let mut messages = vec![Message::user("do it")];
    let collector = tokio::spawn(collect_events(rx));

    agent.run(&mut messages, tx).await.unwrap();
    let events = collector.await.unwrap();

    let succeeded = events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallFinish { is_error: false, content, .. } if content == "did the thing"
    ));
    assert!(succeeded, "expected a successful ToolCallFinish; got {events:#?}");
}
