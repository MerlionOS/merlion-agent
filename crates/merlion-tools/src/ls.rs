use async_trait::async_trait;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use tokio::fs;

#[derive(Default)]
pub struct Ls;

#[derive(Debug, Deserialize)]
struct Args {
    path: String,
}

#[async_trait]
impl Tool for Ls {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "ls".into(),
            description: "List directory contents (one entry per line, dirs suffixed `/`).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to a directory." }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: serde_json::Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, "ls", format!("invalid arguments: {e}")),
        };
        let mut entries = match fs::read_dir(&parsed.path).await {
            Ok(e) => e,
            Err(e) => return err(call_id, "ls", format!("read_dir {}: {e}", parsed.path)),
        };
        let mut names: Vec<String> = Vec::new();
        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                    names.push(if is_dir { format!("{name}/") } else { name });
                }
                Ok(None) => break,
                Err(e) => return err(call_id, "ls", format!("iter: {e}")),
            }
        }
        names.sort();
        ToolResult {
            tool_call_id: call_id.into(),
            name: "ls".into(),
            content: if names.is_empty() {
                "(empty)".into()
            } else {
                names.join("\n")
            },
            is_error: false,
        }
    }
}

fn err(call_id: &str, name: &str, msg: String) -> ToolResult {
    ToolResult {
        tool_call_id: call_id.into(),
        name: name.into(),
        content: msg,
        is_error: true,
    }
}
