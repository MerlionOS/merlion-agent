use async_trait::async_trait;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use tokio::fs;

#[derive(Default)]
pub struct Write;

#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for Write {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "write".into(),
            description:
                "Write a file, creating parent directories if needed. Overwrites existing files."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the file." },
                    "content": { "type": "string", "description": "Full file contents." }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: serde_json::Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, "write", format!("invalid arguments: {e}")),
        };
        if let Some(parent) = std::path::Path::new(&parsed.path).parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = fs::create_dir_all(parent).await {
                    return err(call_id, "write", format!("mkdir {}: {e}", parent.display()));
                }
            }
        }
        match fs::write(&parsed.path, &parsed.content).await {
            Ok(()) => ToolResult {
                tool_call_id: call_id.into(),
                name: "write".into(),
                content: format!("wrote {} bytes to {}", parsed.content.len(), parsed.path),
                is_error: false,
            },
            Err(e) => err(call_id, "write", format!("write {}: {e}", parsed.path)),
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
