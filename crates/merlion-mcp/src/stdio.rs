//! Stdio transport for MCP.
//!
//! Spawns the configured server as a child process and exchanges
//! newline-delimited JSON-RPC over its stdin/stdout. Stderr is logged.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::client::Transport;
use crate::proto::{Request, Response};
use crate::{Error, Result};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>;

pub struct StdioTransport {
    child: Mutex<Option<Child>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    pending: PendingMap,
    next_id: AtomicU64,
    reader_task: Mutex<Option<JoinHandle<()>>>,
    stderr_task: Mutex<Option<JoinHandle<()>>>,
    request_timeout: Duration,
}

impl StdioTransport {
    /// Spawn `program` with `args` and `env`, returning a connected
    /// transport. The reader/stderr tasks are detached and will exit when
    /// `close()` is called or the child process dies.
    pub async fn spawn(program: &str, args: &[String], env: &[(String, String)]) -> Result<Self> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Transport(format!("spawn {program}: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Transport("child stdin missing".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Transport("child stdout missing".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Transport("child stderr missing".into()))?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        let reader_pending = pending.clone();
        let reader_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Response>(&line) {
                            Ok(resp) => {
                                let id = match resp.id.as_u64() {
                                    Some(id) => id,
                                    None => {
                                        tracing::warn!(
                                            response = %line,
                                            "mcp response id was not a u64"
                                        );
                                        continue;
                                    }
                                };
                                let sender = reader_pending.lock().await.remove(&id);
                                if let Some(tx) = sender {
                                    let _ = tx.send(resp);
                                } else {
                                    tracing::warn!(id = id, "mcp response had no pending request");
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    line = %line,
                                    "failed to parse mcp response line"
                                );
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "error reading mcp stdout");
                        break;
                    }
                }
            }
        });

        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        tracing::warn!(server_stderr = %line);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "error reading mcp stderr");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child: Mutex::new(Some(child)),
            stdin: Arc::new(Mutex::new(Some(stdin))),
            pending,
            next_id: AtomicU64::new(1),
            reader_task: Mutex::new(Some(reader_task)),
            stderr_task: Mutex::new(Some(stderr_task)),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        })
    }

    /// Convenience for callers who don't want to construct the env slice.
    pub async fn spawn_simple(program: &str, args: &[String]) -> Result<Self> {
        Self::spawn(program, args, &[]).await
    }

    /// Override the per-request timeout (default 60s). Used by tests.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    async fn write_line(&self, line: String) -> Result<()> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| Error::Transport("transport closed".into()))?;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| Error::Transport(format!("write stdin: {e}")))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| Error::Transport(format!("write stdin newline: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| Error::Transport(format!("flush stdin: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = Request::call(id, method, params);
        let body = serde_json::to_string(&req)?;

        let (tx, rx) = oneshot::channel::<Response>();
        self.pending.lock().await.insert(id, tx);

        if let Err(e) = self.write_line(body).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

        let outcome = tokio::time::timeout(self.request_timeout, rx).await;
        match outcome {
            Ok(Ok(resp)) => {
                if let Some(err) = resp.error {
                    Err(Error::Rpc(err.message))
                } else {
                    Ok(resp.result.unwrap_or(Value::Null))
                }
            }
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&id);
                Err(Error::Transport("response channel closed".into()))
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(Error::Transport(format!(
                    "request '{method}' timed out after {:?}",
                    self.request_timeout
                )))
            }
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let req = Request::notify(method, params);
        let body = serde_json::to_string(&req)?;
        self.write_line(body).await
    }

    async fn close(&self) -> Result<()> {
        if let Some(mut stdin) = self.stdin.lock().await.take() {
            let _ = stdin.shutdown().await;
        }
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        if let Some(handle) = self.reader_task.lock().await.take() {
            handle.abort();
        }
        if let Some(handle) = self.stderr_task.lock().await.take() {
            handle.abort();
        }
        Ok(())
    }
}
