use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_MAX_RESULTS: usize = 200;

#[derive(Default)]
pub struct Grep;

#[derive(Debug, Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    #[serde(default)]
    files_only: bool,
    #[serde(default)]
    max_results: Option<usize>,
}

#[async_trait]
impl Tool for Grep {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "grep".into(),
            description:
                "Search files for a regex pattern using ripgrep (`rg`) when available, falling \
                 back to POSIX `grep -rn`. Returns matched lines (or filenames when `files_only`). \
                 Output is capped at `max_results` lines (default 200)."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search for." },
                    "path": { "type": "string", "description": "Directory or file to search. Defaults to '.'." },
                    "case_insensitive": { "type": "boolean", "description": "Case-insensitive match. Default false." },
                    "files_only": { "type": "boolean", "description": "List matching filenames only. Default false." },
                    "max_results": { "type": "integer", "description": "Cap output to this many lines. Default 200." }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: serde_json::Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, format!("invalid arguments: {e}")),
        };
        let path = parsed.path.as_deref().unwrap_or(".");
        let max_results = parsed.max_results.unwrap_or(DEFAULT_MAX_RESULTS);

        let use_rg = rg_available().await;

        let mut cmd = if use_rg {
            let mut c = Command::new("rg");
            if parsed.files_only {
                c.arg("-l");
            } else {
                c.arg("--no-heading").arg("--line-number");
            }
            if parsed.case_insensitive {
                c.arg("--ignore-case");
            }
            c.arg("--").arg(&parsed.pattern).arg(path);
            c
        } else {
            let mut c = Command::new("grep");
            let flags = if parsed.files_only { "-rln" } else { "-rn" };
            c.arg(flags);
            if parsed.case_insensitive {
                c.arg("-i");
            }
            c.arg("--").arg(&parsed.pattern).arg(path);
            c
        };

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return err(call_id, format!("spawn failed: {e}")),
        };

        let result = tokio::time::timeout(TIMEOUT, collect_output(child)).await;
        match result {
            Ok(Ok((status, combined))) => {
                // Both `rg` and `grep` exit with code 1 when there are no matches.
                // That is not a tool failure.
                let code = status.code().unwrap_or(-1);
                let trimmed = combined.trim_end_matches('\n');
                if trimmed.is_empty() && (code == 0 || code == 1) {
                    return ToolResult {
                        tool_call_id: call_id.into(),
                        name: "grep".into(),
                        content: "no matches".into(),
                        is_error: false,
                    };
                }

                let (content, was_truncated) = truncate_lines(trimmed, max_results);
                let is_error = !status.success() && code != 1;

                let mut final_content = content;
                if is_error {
                    final_content.push_str(&format!("\n[exit: {code}]"));
                }
                if let Some(extra) = was_truncated {
                    final_content.push_str(&format!("\n…[{extra} more matches, truncated]"));
                }
                if final_content.is_empty() {
                    final_content = "no matches".into();
                }

                ToolResult {
                    tool_call_id: call_id.into(),
                    name: "grep".into(),
                    content: final_content,
                    is_error,
                }
            }
            Ok(Err(e)) => err(call_id, format!("io: {e}")),
            Err(_) => err(call_id, format!("timed out after {TIMEOUT:?}")),
        }
    }
}

async fn rg_available() -> bool {
    let mut cmd = Command::new("rg");
    cmd.arg("--version");
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    match cmd.status().await {
        Ok(s) => s.success(),
        Err(_) => false,
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

/// Truncate to at most `max` lines. Returns the (possibly truncated) string
/// and `Some(extra_count)` when lines were dropped, `None` otherwise.
fn truncate_lines(s: &str, max: usize) -> (String, Option<usize>) {
    if s.is_empty() {
        return (String::new(), None);
    }
    let lines: Vec<&str> = s.split('\n').collect();
    if lines.len() <= max {
        return (s.to_string(), None);
    }
    let kept = lines[..max].join("\n");
    let extra = lines.len() - max;
    (kept, Some(extra))
}

fn err(call_id: &str, msg: String) -> ToolResult {
    ToolResult {
        tool_call_id: call_id.into(),
        name: "grep".into(),
        content: msg,
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn finds_match_in_cargo_toml() {
        let tool = Grep;
        let res = tool
            .call(
                "call-1",
                json!({
                    "pattern": "package",
                    "path": "Cargo.toml"
                }),
            )
            .await;
        assert!(!res.is_error, "expected success, got: {}", res.content);
        assert_ne!(
            res.content, "no matches",
            "should have found 'package' in Cargo.toml"
        );
        assert!(
            res.content.contains("package"),
            "expected output to contain 'package', got: {}",
            res.content
        );
    }

    #[tokio::test]
    async fn returns_no_matches_for_absent_pattern() {
        let tool = Grep;
        let res = tool
            .call(
                "call-2",
                json!({
                    "pattern": "zzzzz_definitely_not_present_xxqqww_marker_string",
                    "path": "Cargo.toml"
                }),
            )
            .await;
        assert!(
            !res.is_error,
            "no matches should not be an error: {}",
            res.content
        );
        assert_eq!(res.content, "no matches");
    }

    #[tokio::test]
    async fn files_only_lists_filenames() {
        let tool = Grep;
        let res = tool
            .call(
                "call-3",
                json!({
                    "pattern": "package",
                    "path": "Cargo.toml",
                    "files_only": true
                }),
            )
            .await;
        assert!(!res.is_error, "expected success, got: {}", res.content);
        assert_ne!(res.content, "no matches");
        // The output should reference Cargo.toml and NOT include a line-number-style
        // prefix like "Cargo.toml:1:" — files_only emits bare filenames.
        assert!(
            res.content.contains("Cargo.toml"),
            "expected filename in output, got: {}",
            res.content
        );
        // No `:<digit>` line-number column for the first line.
        let first = res.content.lines().next().unwrap_or("");
        assert!(
            !first.contains(":1:") && !first.contains(":2:"),
            "files_only output should not contain line numbers, got: {first}"
        );
    }
}
