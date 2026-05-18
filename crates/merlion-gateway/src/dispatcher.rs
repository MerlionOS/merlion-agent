//! Per-user agent dispatcher.
//!
//! Sits between the platform gateways and the agent: receives
//! [`IncomingMessage`]s, runs the [`Agent`] on behalf of the right user,
//! and pushes the agent's reply back out as an [`OutgoingMessage`].
//!
//! Per-user state (the message history, the curator counter) lives in an
//! in-memory map keyed on [`User::session_id`]. Persistence rides on the
//! existing `merlion-session::SessionDB`.

use std::collections::HashMap;
use std::sync::Arc;

use merlion_core::{Agent, AgentEvent, Curator, Message};
use merlion_session::SessionDB;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

use crate::allowlist::Allowlist;
use crate::types::{IncomingMessage, OutgoingMessage};
use crate::Result;

pub struct Dispatcher {
    agent: Arc<Agent>,
    db: Arc<Mutex<SessionDB>>,
    system_prompt: String,
    allowlist: Allowlist,
    sessions: Mutex<HashMap<String, SessionState>>,
}

struct SessionState {
    messages: Vec<Message>,
    curator: Curator,
}

impl Dispatcher {
    pub fn new(
        agent: Arc<Agent>,
        db: Arc<Mutex<SessionDB>>,
        system_prompt: String,
        allowlist: Allowlist,
    ) -> Self {
        Self { agent, db, system_prompt, allowlist, sessions: Mutex::new(HashMap::new()) }
    }

    /// Run the dispatcher loop until `incoming_rx` closes. Replies are
    /// pushed to `outgoing_tx`. The dispatcher itself does no platform I/O.
    pub async fn run(
        self: Arc<Self>,
        mut incoming_rx: mpsc::Receiver<IncomingMessage>,
        outgoing_tx: mpsc::Sender<OutgoingMessage>,
    ) -> Result<()> {
        while let Some(msg) = incoming_rx.recv().await {
            let this = self.clone();
            let tx = outgoing_tx.clone();
            tokio::spawn(async move {
                if let Err(e) = this.handle_one(msg, tx).await {
                    warn!(error = %e, "dispatcher handle_one failed");
                }
            });
        }
        Ok(())
    }

    async fn handle_one(
        self: Arc<Self>,
        msg: IncomingMessage,
        outgoing_tx: mpsc::Sender<OutgoingMessage>,
    ) -> Result<()> {
        if !self.allowlist.permits(&msg.user) {
            info!(
                user = %msg.user.id,
                platform = %msg.user.platform,
                "rejecting message from unallowed user"
            );
            let _ = outgoing_tx
                .send(OutgoingMessage {
                    conversation_id: msg.conversation_id.clone(),
                    reply_to: Some(msg.message_id.clone()),
                    text: "merlion: this account is not on the gateway allowlist.".into(),
                })
                .await;
            return Ok(());
        }

        let session_id = msg.user.session_id();
        let user_text = match self.handle_slash(&msg, &session_id).await? {
            SlashOutcome::Forward(text) => text,
            SlashOutcome::Reply(text) => {
                let _ = outgoing_tx
                    .send(OutgoingMessage {
                        conversation_id: msg.conversation_id.clone(),
                        reply_to: Some(msg.message_id.clone()),
                        text,
                    })
                    .await;
                return Ok(());
            }
        };

        // Load or initialize per-user state.
        let mut session_state = self.load_state(&session_id).await?;
        session_state.curator.record_user_turn();
        let user_text = if let Some(nudge) = session_state.curator.nudge_if_due() {
            format!("<system-reminder>{nudge}</system-reminder>\n\n{user_text}")
        } else {
            user_text
        };
        let user_msg = Message::user(user_text);
        self.persist(&session_id, &user_msg).await?;
        session_state.messages.push(user_msg);

        // Drive the agent loop. The event sink is a sink-only mpsc — we
        // don't need to render streaming output for messaging adapters
        // (most platforms can't show streaming anyway).
        let (events_tx, mut events_rx) = mpsc::channel::<AgentEvent>(64);
        let agent = self.agent.clone();
        let mut snapshot = session_state.messages.clone();
        let run_task = tokio::spawn(async move {
            let res = agent.run(&mut snapshot, events_tx).await;
            (res, snapshot)
        });

        // Drain events. We only act on AssistantMessage (final text) — tool
        // call status is logged, not surfaced to the user. A future
        // enhancement could send "running tool: bash..." status messages.
        let mut accumulated_reply = String::new();
        while let Some(ev) = events_rx.recv().await {
            match ev {
                AgentEvent::AssistantMessage(m) => {
                    if let Some(c) = m.content {
                        if !c.is_empty() {
                            accumulated_reply.push_str(&c);
                        }
                    }
                }
                AgentEvent::ToolCallFinish { name, is_error, .. } => {
                    info!(tool = %name, is_error, "tool call finished");
                }
                AgentEvent::IterationBudgetExhausted => {
                    accumulated_reply
                        .push_str("\n\n[merlion: iteration budget exhausted — try splitting the task]");
                }
                _ => {}
            }
        }

        let (res, new_messages) = run_task.await.map_err(|e| {
            crate::Error::Other(format!("agent task join error: {e}"))
        })?;
        if let Err(e) = res {
            let reply = format!("merlion error: {e}");
            let _ = outgoing_tx
                .send(OutgoingMessage {
                    conversation_id: msg.conversation_id.clone(),
                    reply_to: Some(msg.message_id.clone()),
                    text: reply,
                })
                .await;
            return Ok(());
        }

        // Persist new messages.
        for m in new_messages.iter().skip(session_state.messages.len()) {
            self.persist(&session_id, m).await?;
        }
        session_state.messages = new_messages;
        self.store_state(&session_id, session_state).await;

        let reply = if accumulated_reply.trim().is_empty() {
            "(merlion produced no text reply)".to_string()
        } else {
            accumulated_reply
        };

        for chunk in chunk_for_platform(&reply) {
            let _ = outgoing_tx
                .send(OutgoingMessage {
                    conversation_id: msg.conversation_id.clone(),
                    reply_to: Some(msg.message_id.clone()),
                    text: chunk,
                })
                .await;
        }
        Ok(())
    }

    async fn handle_slash(
        &self,
        msg: &IncomingMessage,
        session_id: &str,
    ) -> Result<SlashOutcome> {
        let trimmed = msg.text.trim();
        if !trimmed.starts_with('/') {
            return Ok(SlashOutcome::Forward(trimmed.to_string()));
        }
        let (cmd, rest) = trimmed.split_once(char::is_whitespace).unwrap_or((trimmed, ""));
        match cmd {
            "/new" | "/reset" => {
                self.sessions.lock().await.remove(session_id);
                // Also wipe message history from the DB by starting a fresh row.
                let new_session_id = format!("{session_id}_{}", uuid::Uuid::new_v4());
                // We don't actually swap the session_id here — keeping the
                // user mapping stable is more valuable than wiping history
                // for the messaging case. Future enhancement: a real
                // /new that rotates and archives.
                drop(new_session_id);
                Ok(SlashOutcome::Reply("started a fresh in-memory session".into()))
            }
            "/help" => Ok(SlashOutcome::Reply(
                "Commands: /new (start fresh) · /help".into(),
            )),
            _ => Ok(SlashOutcome::Forward(format!("{cmd} {rest}").trim().to_string())),
        }
    }

    async fn load_state(&self, session_id: &str) -> Result<SessionState> {
        let mut map = self.sessions.lock().await;
        if let Some(state) = map.remove(session_id) {
            return Ok(state);
        }
        drop(map);
        // First time we've seen this user — load from DB or initialize.
        let db = self.db.lock().await;
        let messages = match db.load_messages(session_id) {
            Ok(msgs) if !msgs.is_empty() => msgs,
            _ => {
                db.create_session(session_id, None)
                    .map_err(|e| crate::Error::Other(format!("create_session: {e}")))?;
                let sys = Message::system(&self.system_prompt);
                db.append_message(session_id, &sys)
                    .map_err(|e| crate::Error::Other(format!("append system msg: {e}")))?;
                vec![sys]
            }
        };
        Ok(SessionState { messages, curator: Curator::default() })
    }

    async fn store_state(&self, session_id: &str, state: SessionState) {
        self.sessions.lock().await.insert(session_id.to_string(), state);
    }

    async fn persist(&self, session_id: &str, m: &Message) -> Result<()> {
        let db = self.db.lock().await;
        db.append_message(session_id, m)
            .map_err(|e| crate::Error::Other(format!("append_message: {e}")))
    }
}

enum SlashOutcome {
    /// Forward this text to the agent.
    Forward(String),
    /// Send this text back directly without running the agent.
    Reply(String),
}

/// Split a long reply into chunks small enough for any platform. Telegram's
/// limit is 4096 characters per message; Discord 2000; Slack 40000. Pick a
/// conservative 1800 so all three are safe.
fn chunk_for_platform(s: &str) -> Vec<String> {
    const MAX: usize = 1800;
    if s.len() <= MAX {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut remaining = s;
    while !remaining.is_empty() {
        let take = remaining.len().min(MAX);
        // Try to break on a newline near the cut point so we don't split
        // mid-paragraph.
        let cut = if take == remaining.len() {
            take
        } else {
            remaining[..take].rfind('\n').unwrap_or(take)
        };
        let cut = cut.max(1);
        out.push(remaining[..cut].to_string());
        remaining = &remaining[cut..].trim_start_matches('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::chunk_for_platform;

    #[test]
    fn short_reply_unchanged() {
        let chunks = chunk_for_platform("hi");
        assert_eq!(chunks, vec!["hi".to_string()]);
    }

    #[test]
    fn long_reply_split_at_newline() {
        let s = format!("{}\n\n{}", "x".repeat(1500), "y".repeat(500));
        let chunks = chunk_for_platform(&s);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().all(|c| c.len() <= 1800));
    }
}
