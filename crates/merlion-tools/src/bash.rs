use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Default)]
pub struct Bash;

#[derive(Debug, Deserialize)]
struct Args {
    command: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    cwd: Option<String>,
}

#[async_trait]
impl Tool for Bash {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash".into(),
            description:
                "Run a shell command via `bash -lc`. Returns combined stdout+stderr (truncated at 32 KiB). \
                 Use `timeout_ms` to cap runtime (default 120000)."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The shell command to run." },
                    "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds. Default 120000." },
                    "cwd": { "type": "string", "description": "Working directory. Defaults to the agent's cwd." }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: serde_json::Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, "bash", format!("invalid arguments: {e}")),
        };
        let timeout = Duration::from_millis(parsed.timeout_ms.unwrap_or(120_000));

        let mut cmd = Command::new("bash");
        cmd.arg("-lc").arg(&parsed.command);
        if let Some(cwd) = parsed.cwd.as_deref() {
            cmd.current_dir(cwd);
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return err(call_id, "bash", format!("spawn failed: {e}")),
        };

        let output = tokio::time::timeout(timeout, collect_output(child)).await;
        match output {
            Ok(Ok((status, combined))) => {
                let mut content = combined;
                if !status.success() {
                    content.push_str(&format!("\n[exit: {}]", status.code().unwrap_or(-1)));
                }
                ToolResult {
                    tool_call_id: call_id.into(),
                    name: "bash".into(),
                    content: truncate(content, 32 * 1024),
                    is_error: !status.success(),
                }
            }
            Ok(Err(e)) => err(call_id, "bash", format!("io: {e}")),
            Err(_) => err(call_id, "bash", format!("timed out after {timeout:?}")),
        }
    }
}

async fn collect_output(
    mut child: tokio::process::Child,
) -> std::io::Result<(std::process::ExitStatus, String)> {
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();
    let read_out = stdout.read_to_end(&mut out_buf);
    let read_err = stderr.read_to_end(&mut err_buf);
    let (_, _, status) = tokio::try_join!(read_out, read_err, child.wait())?;
    let mut combined = String::from_utf8_lossy(&out_buf).into_owned();
    if !err_buf.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&err_buf));
    }
    Ok((status, combined))
}

fn truncate(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    s.truncate(max);
    s.push_str("\n…[truncated]");
    s
}

fn err(call_id: &str, name: &str, msg: String) -> ToolResult {
    ToolResult {
        tool_call_id: call_id.into(),
        name: name.into(),
        content: msg,
        is_error: true,
    }
}
