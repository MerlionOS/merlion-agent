//! Tool approval — a gate the agent runs through before dispatching a tool
//! call. Mirrors hermes's `approval.py`/`set_approval_callback` pattern but
//! shaped around an async trait so non-CLI surfaces (a future messaging
//! gateway, an HTTP API) can plug in their own approver.

use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    Allow,
    Deny { reason: String },
}

#[async_trait]
pub trait ToolApprover: Send + Sync {
    /// Decide whether `tool_name(args)` may run. Implementations should treat
    /// this as the trust boundary — any side effect on user data (filesystem
    /// writes, shell execution, network) belongs behind it.
    async fn approve(&self, tool_name: &str, args: &Value) -> ApprovalDecision;
}

/// Default approver — allows everything. Suitable for tests and headless
/// runs; the interactive CLI replaces it with a console prompter.
pub struct AllowAllApprover;

#[async_trait]
impl ToolApprover for AllowAllApprover {
    async fn approve(&self, _tool_name: &str, _args: &Value) -> ApprovalDecision {
        ApprovalDecision::Allow
    }
}

/// Convenience approver that denies everything — useful in tests that want
/// to verify the deny path without setting up a real approver.
pub struct DenyAllApprover {
    pub reason: String,
}

impl Default for DenyAllApprover {
    fn default() -> Self {
        Self { reason: "denied by policy".into() }
    }
}

#[async_trait]
impl ToolApprover for DenyAllApprover {
    async fn approve(&self, _tool_name: &str, _args: &Value) -> ApprovalDecision {
        ApprovalDecision::Deny { reason: self.reason.clone() }
    }
}
