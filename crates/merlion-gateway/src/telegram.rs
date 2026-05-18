//! Telegram Bot adapter — long-polls `getUpdates` and POSTs `sendMessage`.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::warn;

use crate::types::{Gateway, IncomingMessage, OutgoingMessage, User};
use crate::{Error, Result};

const DEFAULT_BASE_URL: &str = "https://api.telegram.org";
const LONG_POLL_SECS: u64 = 30;

pub struct TelegramGateway {
    token: String,
    base_url: String,
    http: reqwest::Client,
}

impl TelegramGateway {
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("TELEGRAM_BOT_TOKEN")
            .map_err(|_| Error::Other("TELEGRAM_BOT_TOKEN env var not set".into()))?;
        Ok(Self::new(token))
    }

    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(LONG_POLL_SECS + 10))
                .build()
                .expect("reqwest client build"),
        }
    }

    /// Test-only: override the Telegram API base URL (e.g. a local fixture).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn endpoint(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base_url, self.token, method)
    }
}

#[async_trait]
impl Gateway for TelegramGateway {
    fn name(&self) -> &'static str {
        "telegram"
    }

    async fn run(
        self: Arc<Self>,
        incoming_tx: mpsc::Sender<IncomingMessage>,
        outgoing_rx: mpsc::Receiver<OutgoingMessage>,
    ) -> Result<()> {
        let recv_gw = self.clone();
        let send_gw = self.clone();

        let recv_task = tokio::spawn(async move { recv_gw.receive_loop(incoming_tx).await });
        let send_task = tokio::spawn(async move { send_gw.send_loop(outgoing_rx).await });

        let (recv_res, send_res) = tokio::try_join!(recv_task, send_task)
            .map_err(|e| Error::Other(format!("telegram task join error: {e}")))?;
        recv_res?;
        send_res?;
        Ok(())
    }
}

impl TelegramGateway {
    async fn receive_loop(&self, incoming_tx: mpsc::Sender<IncomingMessage>) -> Result<()> {
        let mut offset: i64 = 0;
        loop {
            let url = self.endpoint("getUpdates");
            let allowed = serde_json::to_string(&["message"]).unwrap();
            let req = self
                .http
                .get(&url)
                .query(&[
                    ("offset", offset.to_string()),
                    ("timeout", LONG_POLL_SECS.to_string()),
                    ("allowed_updates", allowed),
                ])
                .send()
                .await;

            let resp = match req {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "telegram getUpdates network error");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            if !resp.status().is_success() {
                warn!(status = %resp.status(), "telegram getUpdates non-200");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }

            let parsed: TgGetUpdatesResponse = match resp.json().await {
                Ok(p) => p,
                Err(e) => {
                    warn!(error = %e, "telegram getUpdates parse error");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            if !parsed.ok {
                warn!(
                    description = ?parsed.description,
                    "telegram getUpdates returned ok=false"
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }

            for update in parsed.result {
                let next_offset = update.update_id + 1;
                if next_offset > offset {
                    offset = next_offset;
                }
                match message_from_update(update) {
                    Some(msg) => {
                        if incoming_tx.send(msg).await.is_err() {
                            return Ok(());
                        }
                    }
                    None => {
                        // logged inside message_from_update
                    }
                }
            }
        }
    }

    async fn send_loop(&self, mut outgoing_rx: mpsc::Receiver<OutgoingMessage>) -> Result<()> {
        while let Some(msg) = outgoing_rx.recv().await {
            let chat_id: i64 = match msg.conversation_id.parse() {
                Ok(n) => n,
                Err(e) => {
                    warn!(
                        error = %e,
                        conversation_id = %msg.conversation_id,
                        "telegram conversation_id not parseable as i64; dropping"
                    );
                    continue;
                }
            };

            let mut body = json!({
                "chat_id": chat_id,
                "text": msg.text,
            });

            if let Some(reply_to) = msg.reply_to.as_ref() {
                match reply_to.parse::<i64>() {
                    Ok(n) => {
                        body["reply_to_message_id"] = json!(n);
                    }
                    Err(e) => {
                        warn!(error = %e, reply_to = %reply_to, "telegram reply_to not i64; ignoring");
                    }
                }
            }

            let url = self.endpoint("sendMessage");
            let res = self.http.post(&url).json(&body).send().await;
            match res {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => {
                    let status = resp.status();
                    let txt = resp.text().await.unwrap_or_default();
                    warn!(status = %status, body = %txt, "telegram sendMessage non-200");
                }
                Err(e) => {
                    warn!(error = %e, "telegram sendMessage network error");
                }
            }
        }
        Ok(())
    }
}

/// Convert a Telegram update into our platform-neutral `IncomingMessage`.
/// Returns `None` for updates we can't handle (no message, no sender, no text).
pub fn message_from_update(u: TgUpdate) -> Option<IncomingMessage> {
    let m = match u.message {
        Some(m) => m,
        None => {
            warn!(update_id = u.update_id, "telegram update has no message; skipping");
            return None;
        }
    };

    let text = match m.text {
        Some(t) => t,
        None => {
            warn!(
                message_id = m.message_id,
                "telegram message has no text (voice/photo/etc); skipping"
            );
            return None;
        }
    };

    let from = match m.from {
        Some(f) => f,
        None => {
            warn!(
                message_id = m.message_id,
                "telegram message has no `from`; skipping"
            );
            return None;
        }
    };

    let display_name = match (&from.first_name, &from.last_name) {
        (Some(first), Some(last)) => format!("{first} {last}"),
        (Some(first), None) => first.clone(),
        (None, Some(last)) => last.clone(),
        (None, None) => from.username.clone().unwrap_or_else(|| from.id.to_string()),
    };

    Some(IncomingMessage {
        user: User {
            platform: "telegram".into(),
            id: from.id.to_string(),
            display_name,
        },
        conversation_id: m.chat.id.to_string(),
        message_id: m.message_id.to_string(),
        text,
    })
}

#[derive(Debug, Deserialize)]
pub struct TgGetUpdatesResponse {
    pub ok: bool,
    #[serde(default)]
    pub result: Vec<TgUpdate>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TgUpdate {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
pub struct TgMessage {
    pub message_id: i64,
    #[serde(default)]
    pub from: Option<TgUser>,
    pub chat: TgChat,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TgUser {
    pub id: i64,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TgChat {
    pub id: i64,
}
