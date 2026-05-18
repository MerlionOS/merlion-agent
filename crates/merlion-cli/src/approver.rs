//! Interactive console approver for sensitive tools.
//!
//! Mirrors hermes's command-approval pattern (`tools/approval.py`): the user
//! is asked before bash/write/edit/web_fetch fire. The session can "always
//! approve" a tool, which is remembered in-memory for the rest of the
//! process; persistent allowlists are a Phase 2.7 follow-up.
//!
//! `MERLION_AUTO_APPROVE=1` bypasses the prompt entirely — handy for tests
//! and headless runs.

use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::sync::Mutex;

use merlion_core::{ApprovalDecision, ToolApprover};
use serde_json::Value;

const SENSITIVE_TOOLS: &[&str] = &["bash", "write", "edit", "web_fetch"];

pub struct ConsoleApprover {
    /// Tools the user has marked "always allow" for this session.
    always: Mutex<HashSet<String>>,
    auto_approve: bool,
}

impl ConsoleApprover {
    pub fn new() -> Self {
        let auto_approve = std::env::var("MERLION_AUTO_APPROVE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false);
        Self { always: Mutex::new(HashSet::new()), auto_approve }
    }
}

impl Default for ConsoleApprover {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ToolApprover for ConsoleApprover {
    async fn approve(&self, tool_name: &str, args: &Value) -> ApprovalDecision {
        if self.auto_approve || !SENSITIVE_TOOLS.contains(&tool_name) {
            return ApprovalDecision::Allow;
        }
        if self.always.lock().unwrap().contains(tool_name) {
            return ApprovalDecision::Allow;
        }
        if !io::stdin().is_terminal() {
            // No way to ask — default deny is safer than default allow when
            // we're not interactive and the user hasn't set auto-approve.
            return ApprovalDecision::Deny {
                reason: format!(
                    "tool `{tool_name}` requires approval but stdin is not a TTY \
                     (set MERLION_AUTO_APPROVE=1 to bypass)"
                ),
            };
        }
        let preview = preview_args(args);
        // Use stderr so the prompt doesn't interleave with model output.
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        let _ = writeln!(stderr, "\x1b[33m[merlion]\x1b[0m Allow tool `{tool_name}`?");
        let _ = writeln!(stderr, "  args: {preview}");
        let _ = write!(stderr, "  [y]es / [N]o / [a]lways: ");
        let _ = stderr.flush();

        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return ApprovalDecision::Deny { reason: "could not read stdin".into() };
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => ApprovalDecision::Allow,
            "a" | "always" => {
                self.always.lock().unwrap().insert(tool_name.to_string());
                ApprovalDecision::Allow
            }
            _ => ApprovalDecision::Deny { reason: "user declined".into() },
        }
    }
}

fn preview_args(v: &Value) -> String {
    let s = v.to_string();
    let max = 240;
    if s.len() <= max {
        s
    } else {
        format!("{}…", &s[..max])
    }
}
