use async_trait::async_trait;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use tokio::fs;

#[derive(Default)]
pub struct Read;

#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for Read {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read".into(),
            description:
                "Read a file from disk. Returns content with 1-indexed line-number prefixes \
                 (`<lineno>\\t<line>`). Use `offset` and `limit` for large files."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the file." },
                    "offset": { "type": "integer", "description": "1-indexed line to start at." },
                    "limit": { "type": "integer", "description": "Max lines to return. Default 2000." }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: serde_json::Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, "read", format!("invalid arguments: {e}")),
        };
        match fs::read_to_string(&parsed.path).await {
            Ok(text) => {
                let start = parsed.offset.unwrap_or(1).saturating_sub(1);
                let limit = parsed.limit.unwrap_or(2000);
                let mut out = String::new();
                for (i, line) in text.lines().enumerate().skip(start).take(limit) {
                    out.push_str(&format!("{}\t{}\n", i + 1, line));
                }
                if out.is_empty() {
                    out.push_str("(empty file or out-of-range)");
                }
                ToolResult {
                    tool_call_id: call_id.into(),
                    name: "read".into(),
                    content: out,
                    is_error: false,
                }
            }
            Err(e) => err(call_id, "read", format!("read {}: {e}", parsed.path)),
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
