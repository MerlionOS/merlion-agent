use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Default)]
pub struct BashDocker;

#[derive(Debug, Deserialize)]
struct Args {
    command: String,
    container: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    cwd: Option<String>,
}

#[async_trait]
impl Tool for BashDocker {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash_docker".into(),
            description:
                "Run a shell command inside a running Docker container. Use this when the \
                 user has provisioned a sandbox container they want commands isolated to. \
                 `container` is the container name or id (must already be running — use \
                 `docker ps` to verify). `bash` runs commands on the host; this tool runs \
                 them in the container."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The shell command to run inside the container." },
                    "container": { "type": "string", "description": "Name or id of a running Docker container." },
                    "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds. Default 120000." },
                    "cwd": { "type": "string", "description": "Working directory inside the container (passed via `docker exec -w`)." }
                },
                "required": ["command", "container"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: serde_json::Value) -> ToolResult {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, "bash_docker", format!("invalid arguments: {e}")),
        };
        let timeout = Duration::from_millis(parsed.timeout_ms.unwrap_or(120_000));

        let mut cmd = Command::new(program_name());
        cmd.arg("exec");
        if let Some(cwd) = parsed.cwd.as_deref() {
            cmd.arg("-w").arg(cwd);
        }
        cmd.arg(&parsed.container);
        cmd.arg("bash").arg("-lc").arg(&parsed.command);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    return err(
                        call_id,
                        "bash_docker",
                        "docker binary not found on PATH".into(),
                    );
                }
                return err(call_id, "bash_docker", format!("spawn failed: {e}"));
            }
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
                    name: "bash_docker".into(),
                    content: truncate(content, 32 * 1024),
                    is_error: !status.success(),
                }
            }
            Ok(Err(e)) => err(call_id, "bash_docker", format!("io: {e}")),
            Err(_) => err(
                call_id,
                "bash_docker",
                format!("timed out after {timeout:?}"),
            ),
        }
    }
}

fn program_name() -> String {
    std::env::var("MERLION_DOCKER_BIN").unwrap_or_else(|_| "docker".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn missing_container_arg_returns_error_result() {
        let tool = BashDocker;
        let result = tool.call("call-1", json!({ "command": "echo hi" })).await;
        assert!(result.is_error, "expected is_error=true, got {:?}", result);
        assert_eq!(result.name, "bash_docker");
        assert!(
            result.content.contains("invalid arguments"),
            "unexpected content: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn missing_command_arg_returns_error_result() {
        let tool = BashDocker;
        let result = tool.call("call-2", json!({ "container": "demo" })).await;
        assert!(result.is_error);
        assert!(result.content.contains("invalid arguments"));
    }

    /// Process-global mutex for tests that mutate MERLION_DOCKER_BIN.
    /// Cargo runs tests within a binary in parallel by default, so two
    /// tests both mutating the same env var collide. Serialize them.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Holding ENV_GUARD across await is intentional: the lock is what keeps
    // MERLION_DOCKER_BIN stable while `.call` reads it. The lock is taken
    // only by tests in this module; no risk of cross-task deadlock.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn docker_binary_missing_returns_clean_error() {
        let _g = ENV_GUARD.lock().unwrap();
        let tool = BashDocker;
        // SAFETY: tests in this module set env to override the resolved docker
        // binary. Cargo runs `#[tokio::test]` on a multi-thread runtime per
        // test by default, so we set+unset within a single test body.
        let prev = std::env::var("MERLION_DOCKER_BIN").ok();
        std::env::set_var(
            "MERLION_DOCKER_BIN",
            "merlion-nonexistent-docker-binary-xyz",
        );
        let result = tool
            .call(
                "call-3",
                json!({ "command": "echo hi", "container": "demo" }),
            )
            .await;
        match prev {
            Some(v) => std::env::set_var("MERLION_DOCKER_BIN", v),
            None => std::env::remove_var("MERLION_DOCKER_BIN"),
        }
        assert!(result.is_error, "expected error result, got {:?}", result);
        assert_eq!(
            result.content, "docker binary not found on PATH",
            "expected NotFound mapped to friendly message, got: {}",
            result.content
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn fake_docker_script_runs_and_passes_args() {
        let _g = ENV_GUARD.lock().unwrap();
        // Build a fake "docker" shim that just echoes its args, to verify
        // the call() path end-to-end without needing a real docker engine.
        let dir =
            std::env::temp_dir().join(format!("merlion-bash-docker-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-docker.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nfor a in \"$@\"; do echo \"arg:$a\"; done\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }

        let prev = std::env::var("MERLION_DOCKER_BIN").ok();
        std::env::set_var("MERLION_DOCKER_BIN", &script);
        let tool = BashDocker;
        let result = tool
            .call(
                "call-4",
                json!({
                    "command": "echo hi",
                    "container": "demo",
                    "cwd": "/work"
                }),
            )
            .await;
        match prev {
            Some(v) => std::env::set_var("MERLION_DOCKER_BIN", v),
            None => std::env::remove_var("MERLION_DOCKER_BIN"),
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert!(!result.is_error, "unexpected error: {:?}", result);
        // Verify ordered args: exec, -w, /work, demo, bash, -lc, echo hi
        let expected_order = ["exec", "-w", "/work", "demo", "bash", "-lc", "echo hi"];
        let mut cursor = 0usize;
        for needle in expected_order {
            let line = format!("arg:{needle}");
            match result.content[cursor..].find(&line) {
                Some(p) => cursor += p + line.len(),
                None => panic!(
                    "missing or out-of-order arg `{needle}` in output:\n{}",
                    result.content
                ),
            }
        }
    }

    #[tokio::test]
    #[ignore]
    async fn real_docker_exec() {
        // Requires a running container named `merlion-test`. Run manually:
        //   docker run -d --name merlion-test --rm alpine sleep 3600
        //   cargo test -p merlion-tools bash_docker::tests::real_docker_exec -- --ignored
        let tool = BashDocker;
        let result = tool
            .call(
                "call-real",
                json!({ "command": "echo hello-from-container", "container": "merlion-test" }),
            )
            .await;
        assert!(!result.is_error, "{:?}", result);
        assert!(result.content.contains("hello-from-container"));
    }
}
