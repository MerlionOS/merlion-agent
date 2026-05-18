//! `memory` tool — the model's interface to the persistent memory store.
//!
//! One tool with an `action` switch keeps the model's schema burden low:
//! `{"action": "write", "name": "...", "description": "...", "kind": "...",
//! "body": "..."}` instead of four separate tools to learn.

use async_trait::async_trait;
use chrono::Utc;
use merlion_core::{Tool, ToolResult, ToolSchema};
use merlion_memory::{Memory, MemoryStore, MemoryType};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct MemoryTool {
    store: Arc<MemoryStore>,
}

impl MemoryTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Args {
    /// List index entries. Returns `name — hook` per line.
    List,
    /// Read a memory's body verbatim. Errors if missing.
    Read { name: String },
    /// Create or overwrite a memory. All fields required on first write;
    /// `kind` defaults to "project" on overwrite if omitted.
    Write {
        name: String,
        description: String,
        body: String,
        #[serde(default)]
        kind: Option<String>,
    },
    /// Idempotent — no error if the memory doesn't exist.
    Delete { name: String },
}

#[async_trait]
impl Tool for MemoryTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory".into(),
            description:
                "Manage the agent's persistent memory store. Use this to remember things \
                 about the user, their projects, and their preferences across sessions. \
                 `action`: list | read | write | delete."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "read", "write", "delete"] },
                    "name": { "type": "string", "description": "kebab-case slug (required for read/write/delete)" },
                    "description": { "type": "string", "description": "one-line summary (required for write)" },
                    "body": { "type": "string", "description": "markdown body (required for write)" },
                    "kind": {
                        "type": "string",
                        "enum": ["user", "feedback", "project", "reference"],
                        "description": "type tag; required on first write"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, format!("invalid arguments: {e}")),
        };
        let store = self.store.clone();
        match tokio::task::spawn_blocking(move || dispatch(&store, parsed)).await {
            Ok(Ok(content)) => ok(call_id, content),
            Ok(Err(e)) => err(call_id, e),
            Err(e) => err(call_id, format!("memory tool panicked: {e}")),
        }
    }
}

fn dispatch(store: &MemoryStore, args: Args) -> Result<String, String> {
    match args {
        Args::List => {
            let rows = store.list().map_err(|e| e.to_string())?;
            if rows.is_empty() {
                return Ok("(no memories saved)".into());
            }
            let mut out = String::new();
            for r in rows {
                out.push_str(&format!("{} — {}\n", r.name, r.hook));
            }
            Ok(out)
        }
        Args::Read { name } => {
            let m = store.read(&name).map_err(|e| e.to_string())?;
            Ok(format!(
                "name: {}\ndescription: {}\nkind: {:?}\nupdated_at: {}\n\n{}",
                m.name, m.description, m.kind, m.updated_at, m.body
            ))
        }
        Args::Write { name, description, body, kind } => {
            let kind = parse_kind(kind.as_deref())?;
            let now = Utc::now();
            let m = Memory {
                name: name.clone(),
                description,
                kind,
                body,
                created_at: now,
                updated_at: now,
            };
            store.write(&m).map_err(|e| e.to_string())?;
            Ok(format!("saved memory `{name}`"))
        }
        Args::Delete { name } => {
            store.delete(&name).map_err(|e| e.to_string())?;
            Ok(format!("deleted memory `{name}` (idempotent)"))
        }
    }
}

fn parse_kind(s: Option<&str>) -> Result<MemoryType, String> {
    match s {
        Some("user") => Ok(MemoryType::User),
        Some("feedback") => Ok(MemoryType::Feedback),
        Some("project") | None => Ok(MemoryType::Project),
        Some("reference") => Ok(MemoryType::Reference),
        Some(other) => Err(format!(
            "unknown kind `{other}` — expected one of: user, feedback, project, reference"
        )),
    }
}

fn ok(call_id: &str, content: String) -> ToolResult {
    ToolResult { tool_call_id: call_id.into(), name: "memory".into(), content, is_error: false }
}

fn err(call_id: &str, msg: String) -> ToolResult {
    ToolResult { tool_call_id: call_id.into(), name: "memory".into(), content: msg, is_error: true }
}
