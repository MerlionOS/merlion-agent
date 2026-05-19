use async_trait::async_trait;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use tokio::fs;

#[derive(Default)]
pub struct Edit;

#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for Edit {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit".into(),
            description:
                "Exact-string find-and-replace in a file. Fails if `old_string` is not unique \
                 unless `replace_all` is true. Matches the Edit-tool semantics agents are trained on."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean", "default": false }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: serde_json::Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, "edit", format!("invalid arguments: {e}")),
        };
        if parsed.old_string == parsed.new_string {
            return err(
                call_id,
                "edit",
                "old_string and new_string are identical".into(),
            );
        }
        let text = match fs::read_to_string(&parsed.path).await {
            Ok(t) => t,
            Err(e) => return err(call_id, "edit", format!("read {}: {e}", parsed.path)),
        };
        let count = text.matches(&parsed.old_string).count();
        if count == 0 {
            return err(call_id, "edit", "old_string not found".into());
        }
        if count > 1 && !parsed.replace_all {
            return err(
                call_id,
                "edit",
                format!("old_string appears {count} times; pass replace_all=true or provide more context"),
            );
        }
        let updated = if parsed.replace_all {
            text.replace(&parsed.old_string, &parsed.new_string)
        } else {
            text.replacen(&parsed.old_string, &parsed.new_string, 1)
        };
        if let Err(e) = fs::write(&parsed.path, &updated).await {
            return err(call_id, "edit", format!("write {}: {e}", parsed.path));
        }
        ToolResult {
            tool_call_id: call_id.into(),
            name: "edit".into(),
            content: format!(
                "edited {} ({} replacement{})",
                parsed.path,
                count,
                if count == 1 { "" } else { "s" }
            ),
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
