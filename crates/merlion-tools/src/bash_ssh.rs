//! `bash_ssh` tool — runs shell commands on a remote host over SSH.
//!
//! Shells out to the local `ssh` binary. Assumes the user has SSH keys or
//! a configured agent set up; we don't manage credentials ourselves.
//!
//! The remote command is fed through `bash -lc` on the far side so the
//! model can use familiar shell idioms (pipes, expansions, `&&`).

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Default)]
pub struct BashSsh;

#[derive(Debug, Deserialize)]
struct Args {
    command: String,
    /// `user@host` or just `host` (SSH config picks up the user).
    target: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    cwd: Option<String>,
    /// Extra ssh args (e.g. `-p 2222`, `-i ~/.ssh/special_key`). Split on
    /// whitespace; quoting not supported — users who need spaces should
    /// configure them in `~/.ssh/config` instead.
    #[serde(default)]
    ssh_options: Option<String>,
}

#[async_trait]
impl Tool for BashSsh {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash_ssh".into(),
            description:
                "Run a shell command on a remote host over SSH. Combined stdout+stderr is \
                 returned (truncated at 32 KiB). `target` is `user@host` or just `host`; \
                 SSH keys/agent must already be set up. `bash` runs commands on the local \
                 host, `bash_docker` runs them in a container — this runs them on a remote \
                 machine."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "target": { "type": "string", "description": "user@host or host" },
                    "timeout_ms": { "type": "integer", "description": "default 120000" },
                    "cwd": { "type": "string", "description": "working directory on the remote host" },
                    "ssh_options": { "type": "string", "description": "extra ssh args, e.g. \"-p 2222\"" }
                },
                "required": ["command", "target"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: serde_json::Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, format!("invalid arguments: {e}")),
        };
        let timeout = Duration::from_millis(parsed.timeout_ms.unwrap_or(120_000));

        // Compose the remote payload. Quote single quotes inside the command
        // using the standard `'\''` POSIX shell trick.
        let escaped = parsed.command.replace('\'', "'\\''");
        let remote = match parsed.cwd {
            Some(cwd) => {
                let cwd_esc = cwd.replace('\'', "'\\''");
                format!("cd '{cwd_esc}' && bash -lc '{escaped}'")
            }
            None => format!("bash -lc '{escaped}'"),
        };

        let mut cmd = Command::new("ssh");
        // Sensible defaults: no host-key prompt, no TTY allocation.
        cmd.arg("-o").arg("BatchMode=yes");
        cmd.arg("-o").arg("StrictHostKeyChecking=accept-new");
        cmd.arg("-T");
        if let Some(opts) = parsed.ssh_options.as_deref() {
            for piece in opts.split_whitespace() {
                cmd.arg(piece);
            }
        }
        cmd.arg(&parsed.target);
        cmd.arg(remote);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return err(call_id, "ssh binary not found on PATH".into());
            }
            Err(e) => return err(call_id, format!("spawn ssh: {e}")),
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
                    name: "bash_ssh".into(),
                    content: truncate(content, 32 * 1024),
                    is_error: !status.success(),
                }
            }
            Ok(Err(e)) => err(call_id, format!("io: {e}")),
            Err(_) => err(call_id, format!("timed out after {timeout:?}")),
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

fn err(call_id: &str, msg: String) -> ToolResult {
    ToolResult { tool_call_id: call_id.into(), name: "bash_ssh".into(), content: msg, is_error: true }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_target_arg_returns_error_result() {
        let r = BashSsh.call("c1", json!({ "command": "ls" })).await;
        assert!(r.is_error);
        assert!(r.content.contains("invalid arguments"), "got: {}", r.content);
    }

    #[tokio::test]
    async fn invalid_target_fails_quickly_with_batch_mode() {
        // BatchMode=yes makes ssh fail-fast instead of prompting for a password.
        // Use a guaranteed-unresolvable host. We don't assert on the exact
        // error since it depends on the local resolver, but the call should
        // return a result (error) within the timeout.
        let r = BashSsh
            .call(
                "c2",
                json!({
                    "command": "echo hi",
                    "target": "nonexistent-host.merlion.invalid",
                    "timeout_ms": 5000
                }),
            )
            .await;
        assert!(r.is_error, "expected error result, got ok: {}", r.content);
    }
}
