//! Interactive console approver for sensitive tools.
//!
//! Mirrors hermes's command-approval pattern (`tools/approval.py`): the user
//! is asked before bash/write/edit/web_fetch fire. "Always allow" decisions
//! persist to `~/.merlion/approvals.yaml` so the user only has to answer
//! once per tool across sessions.
//!
//! `MERLION_AUTO_APPROVE=1` bypasses the prompt entirely — handy for tests
//! and headless runs.

use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use merlion_core::{ApprovalDecision, ToolApprover};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SENSITIVE_TOOLS: &[&str] =
    &["bash", "bash_docker", "bash_ssh", "write", "edit", "web_fetch"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ApprovalsFile {
    /// Tools the user has globally "always allow"-ed.
    #[serde(default)]
    always_allow: Vec<String>,
}

pub struct ConsoleApprover {
    /// Union of (loaded from disk) + (added this session).
    always: Mutex<HashSet<String>>,
    auto_approve: bool,
    persistence_path: Option<PathBuf>,
}

impl ConsoleApprover {
    pub fn new() -> Self {
        let auto_approve = std::env::var("MERLION_AUTO_APPROVE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false);
        let path = approvals_path();
        let loaded: HashSet<String> = match load_approvals(&path) {
            Ok(f) => f.always_allow.into_iter().collect(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "could not load approvals");
                HashSet::new()
            }
        };
        Self {
            always: Mutex::new(loaded),
            auto_approve,
            persistence_path: Some(path),
        }
    }

    /// In-memory variant for tests (no disk side effects).
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self {
            always: Mutex::new(HashSet::new()),
            auto_approve: false,
            persistence_path: None,
        }
    }

    fn remember_always(&self, tool_name: &str) {
        let mut guard = self.always.lock().unwrap();
        let inserted = guard.insert(tool_name.to_string());
        let snapshot: Vec<String> = guard.iter().cloned().collect();
        drop(guard);
        if !inserted {
            return;
        }
        let Some(path) = self.persistence_path.as_deref() else { return };
        let file = ApprovalsFile { always_allow: snapshot };
        if let Err(e) = save_approvals(path, &file) {
            tracing::warn!(error = %e, path = %path.display(), "could not persist approval");
        }
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
            return ApprovalDecision::Deny {
                reason: format!(
                    "tool `{tool_name}` requires approval but stdin is not a TTY \
                     (set MERLION_AUTO_APPROVE=1 to bypass)"
                ),
            };
        }
        let preview = preview_args(args);
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        let _ = writeln!(stderr, "\x1b[33m[merlion]\x1b[0m Allow tool `{tool_name}`?");
        let _ = writeln!(stderr, "  args: {preview}");
        let _ = write!(
            stderr,
            "  [y]es / [N]o / [a]lways (persisted to ~/.merlion/approvals.yaml): "
        );
        let _ = stderr.flush();

        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return ApprovalDecision::Deny { reason: "could not read stdin".into() };
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => ApprovalDecision::Allow,
            "a" | "always" => {
                self.remember_always(tool_name);
                ApprovalDecision::Allow
            }
            _ => ApprovalDecision::Deny { reason: "user declined".into() },
        }
    }
}

fn approvals_path() -> PathBuf {
    if let Ok(home) = std::env::var("MERLION_HOME") {
        return PathBuf::from(home).join("approvals.yaml");
    }
    dirs::home_dir()
        .map(|h| h.join(".merlion").join("approvals.yaml"))
        .unwrap_or_else(|| PathBuf::from(".merlion/approvals.yaml"))
}

fn load_approvals(path: &Path) -> anyhow::Result<ApprovalsFile> {
    if !path.exists() {
        return Ok(ApprovalsFile::default());
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(ApprovalsFile::default());
    }
    Ok(serde_yaml::from_str(&text)?)
}

fn save_approvals(path: &Path, file: &ApprovalsFile) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let yaml = serde_yaml::to_string(file)?;
    std::fs::write(path, yaml)?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_save_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("approvals.yaml");
        let f = ApprovalsFile { always_allow: vec!["bash".into(), "write".into()] };
        save_approvals(&path, &f).unwrap();
        let back = load_approvals(&path).unwrap();
        assert_eq!(back.always_allow, vec!["bash".to_string(), "write".to_string()]);
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let path = std::path::Path::new("/nonexistent/approvals.yaml");
        let f = load_approvals(path).unwrap();
        assert!(f.always_allow.is_empty());
    }

    #[test]
    fn in_memory_approver_does_not_touch_disk() {
        let a = ConsoleApprover::in_memory();
        a.remember_always("bash"); // would crash if it tried to write
        assert!(a.always.lock().unwrap().contains("bash"));
    }
}
