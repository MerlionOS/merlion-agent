//! `CodexClient` — shells out to the OpenAI `codex` CLI so the LLM call is
//! billed against the user's ChatGPT subscription quota instead of a
//! per-token API key.
//!
//! # How it works
//!
//! Each `stream()` call spawns `codex exec --json --color never
//! --skip-git-repo-check -s read-only ...` with one of two prompt shapes:
//!
//! 1. **First turn of a conversation:** the entire merlion message history
//!    is serialized into a single prompt string and passed as a positional
//!    argument. Codex returns a fresh `session_meta.payload.id` (a UUID)
//!    in its JSONL output; we cache that under a fingerprint of the
//!    `(system_prompt, first_user_message)` pair.
//!
//! 2. **Continuation turn:** we look up the cached session-id by the same
//!    fingerprint and spawn `codex exec resume <id> "<latest user msg>"`
//!    so codex picks up where it left off.
//!
//! # Tradeoffs
//!
//! - **No streaming of tokens.** Codex emits structured events; we collect
//!   the final `agent_message` text and forward it as a single
//!   `LlmStreamEvent::Delta`. Per-token streaming would require parsing
//!   codex's incremental events, which aren't a stable contract.
//! - **Codex's own tools run.** Codex is an agent CLI — it does its own
//!   `bash` / `read` / `edit` calls internally. Merlion's tool registry
//!   doesn't participate. For pure-prompt use cases (`merlion -z "what
//!   is 2+2"`) this is fine; for complex flows that depend on merlion's
//!   tools or MCP servers, prefer the API-key path.
//! - **`-s read-only`** sandbox by default. The user can bypass with
//!   `MERLION_CODEX_DANGEROUS=1` but should know what they're agreeing to.
//! - **Spawn-per-turn overhead** (~2–5s per `codex exec`).
//!
//! # Auth
//!
//! Auth comes from `~/.codex/auth.json` populated by `codex login`. There's
//! no API key to set and no env var to read — if the user isn't logged in,
//! the first `complete()` call will surface codex's own error.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use merlion_core::error::{Error, Result};
use merlion_core::llm::{LlmClient, LlmRequest, LlmResponse, LlmStreamEvent, Usage};
use merlion_core::message::Role;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

/// Default codex binary; overridden by `MERLION_CODEX_BIN` if set.
const DEFAULT_CODEX_BIN: &str = "codex";

pub struct CodexClient {
    bin: String,
    /// `codex` defaults to `-s read-only`; flipped on if
    /// `MERLION_CODEX_DANGEROUS=1` is set. The user is opting into the
    /// `--dangerously-bypass-approvals-and-sandbox` flag when they do this.
    dangerous: bool,
    /// Map from conversation fingerprint -> codex session UUID. Lets us
    /// `codex exec resume <id>` for multi-turn flows instead of replaying
    /// the entire history every turn.
    sessions: Mutex<HashMap<String, String>>,
}

impl CodexClient {
    pub fn from_env() -> Result<Self> {
        let bin = std::env::var("MERLION_CODEX_BIN").unwrap_or_else(|_| DEFAULT_CODEX_BIN.into());
        let dangerous = std::env::var("MERLION_CODEX_DANGEROUS")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false);
        Ok(Self {
            bin,
            dangerous,
            sessions: Mutex::new(HashMap::new()),
        })
    }
}

/// SHA-256 of `(system_prompt ?? "") || "\n----\n" || first_user_message`.
/// Stable across turns of the same conversation. Two different merlion
/// sessions opening with the exact same system+user pair would collide;
/// that's a rare/benign case for an MVP.
fn fingerprint(req: &LlmRequest) -> String {
    let mut hasher = Sha256::new();
    for m in &req.messages {
        if m.role == Role::System {
            if let Some(c) = m.content.as_deref() {
                hasher.update(c.as_bytes());
            }
            hasher.update(b"\n----\n");
            break;
        }
    }
    for m in &req.messages {
        if m.role == Role::User {
            if let Some(c) = m.content.as_deref() {
                hasher.update(c.as_bytes());
            }
            break;
        }
    }
    format!("{:x}", hasher.finalize())
}

/// Serialize the full conversation history as a single prompt string for
/// the first-turn `codex exec`. Codex doesn't accept a structured message
/// array on the CLI; flatten with role markers.
fn serialize_history(req: &LlmRequest) -> String {
    let mut out = String::new();
    for m in &req.messages {
        let role = match m.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let body = m.content.as_deref().unwrap_or("");
        out.push_str(&format!("[{role}]\n{body}\n\n"));
    }
    out
}

fn latest_user_message(req: &LlmRequest) -> Option<&str> {
    req.messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .and_then(|m| m.content.as_deref())
}

#[async_trait]
impl LlmClient for CodexClient {
    async fn stream(&self, req: LlmRequest) -> Result<BoxStream<'static, Result<LlmStreamEvent>>> {
        let fp = fingerprint(&req);
        let cached_session = self
            .sessions
            .lock()
            .map_err(|e| Error::Llm(format!("codex session map poisoned: {e}")))?
            .get(&fp)
            .cloned();

        let mut cmd = Command::new(&self.bin);
        cmd.arg("exec")
            .arg("--json")
            .arg("--color")
            .arg("never")
            .arg("--skip-git-repo-check")
            .arg("-m")
            .arg(&req.model);
        if self.dangerous {
            cmd.arg("--dangerously-bypass-approvals-and-sandbox");
        } else {
            cmd.arg("-s").arg("read-only");
        }
        if let Some(id) = &cached_session {
            cmd.arg("resume").arg(id);
            let prompt = latest_user_message(&req).unwrap_or_default().to_string();
            cmd.arg(prompt);
        } else {
            let prompt = serialize_history(&req);
            cmd.arg(prompt);
        }

        // Close stdin to suppress codex's "Reading additional input from
        // stdin..." informational line and prevent any blocking read.
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Llm(format!("spawn `{}`: {e}", self.bin)))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Llm("codex stdout missing".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Llm("codex stderr missing".into()))?;

        let mut reader = BufReader::new(stdout).lines();
        let mut content = String::new();
        let mut usage: Option<Usage> = None;
        let mut new_session_id: Option<String> = None;

        while let Some(line) = reader
            .next_line()
            .await
            .map_err(|e| Error::Llm(format!("read codex stdout: {e}")))?
        {
            let v: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // codex exec --json event schema (codex-cli 0.130.0):
            //   {"type":"thread.started","thread_id":"<uuid>"}
            //   {"type":"turn.started"}
            //   {"type":"item.completed","item":{"type":"agent_message","text":"..."}}
            //   {"type":"turn.completed","usage":{"input_tokens":N,"output_tokens":N,...}}
            //   {"type":"turn.failed","error":{"message":"..."}}
            //   {"type":"error","message":"..."}
            let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match kind {
                "thread.started" => {
                    if let Some(id) = v.get("thread_id").and_then(|i| i.as_str()) {
                        new_session_id = Some(id.to_string());
                    }
                }
                "item.completed" => {
                    let item = v.get("item");
                    let itype = item
                        .and_then(|i| i.get("type"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    if itype == "agent_message" {
                        if let Some(text) =
                            item.and_then(|i| i.get("text")).and_then(|t| t.as_str())
                        {
                            if !content.is_empty() {
                                content.push_str("\n\n");
                            }
                            content.push_str(text);
                        }
                    }
                }
                "turn.completed" => {
                    let u = v.get("usage");
                    let pt = u
                        .and_then(|u| u.get("input_tokens"))
                        .and_then(|n| n.as_u64())
                        .map(|n| n as u32);
                    let ct = u
                        .and_then(|u| u.get("output_tokens"))
                        .and_then(|n| n.as_u64())
                        .map(|n| n as u32);
                    if pt.is_some() || ct.is_some() {
                        usage = Some(Usage {
                            prompt_tokens: pt,
                            completion_tokens: ct,
                            total_tokens: pt.and_then(|p| ct.map(|c| p + c)).or(pt).or(ct),
                        });
                    }
                }
                "turn.failed" | "error" => {
                    let msg = v
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .or_else(|| v.get("message").and_then(|m| m.as_str()))
                        .unwrap_or("(no error message)");
                    return Err(Error::Llm(format!("codex returned error: {msg}")));
                }
                _ => {}
            }
        }

        // Drain stderr (don't block on it but surface on failure).
        let status = child
            .wait()
            .await
            .map_err(|e| Error::Llm(format!("waiting on codex: {e}")))?;
        if !status.success() {
            let mut err_buf = String::new();
            let mut err_reader = BufReader::new(stderr);
            let _ = err_reader.read_to_string(&mut err_buf).await;
            return Err(Error::Llm(format!(
                "codex exec exited {} (stderr: {})",
                status,
                err_buf.trim()
            )));
        }

        if let Some(id) = new_session_id {
            // Only store on first-turn paths — resume turns reuse the
            // existing session-id and codex doesn't emit a new
            // `session_meta` for them.
            if cached_session.is_none() {
                if let Ok(mut map) = self.sessions.lock() {
                    map.insert(fp, id);
                }
            }
        }

        if content.is_empty() {
            content = "(codex returned no agent_message)".into();
        }

        let events: Vec<Result<LlmStreamEvent>> = {
            let mut v: Vec<Result<LlmStreamEvent>> = vec![Ok(LlmStreamEvent::Delta(content))];
            if let Some(u) = usage {
                v.push(Ok(LlmStreamEvent::Usage(u)));
            }
            v.push(Ok(LlmStreamEvent::Done(Some("stop".into()))));
            v
        };
        Ok(Box::pin(stream::iter(events)) as BoxStream<'static, Result<LlmStreamEvent>>)
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse> {
        let mut stream = self.stream(req).await?;
        let mut resp = LlmResponse::default();
        let mut buf = String::new();
        while let Some(ev) = stream.next().await {
            match ev? {
                LlmStreamEvent::Delta(s) => buf.push_str(&s),
                LlmStreamEvent::ToolCalls(c) => resp.tool_calls = c,
                LlmStreamEvent::Usage(u) => resp.usage = Some(u),
                LlmStreamEvent::Done(r) => resp.finish_reason = r,
            }
        }
        if !buf.is_empty() {
            resp.content = Some(buf);
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use merlion_core::message::Message;

    fn req(model: &str, msgs: Vec<Message>) -> LlmRequest {
        LlmRequest {
            model: model.into(),
            messages: msgs,
            tools: vec![],
            temperature: None,
            max_tokens: None,
        }
    }

    #[test]
    fn fingerprint_stable_across_calls() {
        let msgs = vec![
            Message::system("you are helpful"),
            Message::user("hello"),
            Message::assistant_text("hi"),
            Message::user("follow-up"),
        ];
        let a = fingerprint(&req("gpt-5-codex", msgs.clone()));
        let b = fingerprint(&req("gpt-5-codex", msgs));
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_differs_on_different_first_user() {
        let m1 = vec![Message::system("sys"), Message::user("question A")];
        let m2 = vec![Message::system("sys"), Message::user("question B")];
        assert_ne!(fingerprint(&req("x", m1)), fingerprint(&req("x", m2)));
    }

    #[test]
    fn fingerprint_differs_on_different_system() {
        let m1 = vec![Message::system("system A"), Message::user("hi")];
        let m2 = vec![Message::system("system B"), Message::user("hi")];
        assert_ne!(fingerprint(&req("x", m1)), fingerprint(&req("x", m2)));
    }

    #[test]
    fn latest_user_picks_last_user_turn() {
        let msgs = vec![
            Message::user("first"),
            Message::assistant_text("reply"),
            Message::user("second"),
            Message::assistant_text("reply2"),
            Message::user("third"),
        ];
        assert_eq!(latest_user_message(&req("x", msgs)), Some("third"));
    }

    #[test]
    fn serialize_history_includes_all_roles() {
        let msgs = vec![
            Message::system("sys"),
            Message::user("u1"),
            Message::assistant_text("a1"),
            Message::user("u2"),
        ];
        let s = serialize_history(&req("x", msgs));
        assert!(s.contains("[system]\nsys"));
        assert!(s.contains("[user]\nu1"));
        assert!(s.contains("[assistant]\na1"));
        assert!(s.contains("[user]\nu2"));
    }
}
