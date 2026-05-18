//! Slack gateway adapter — Socket Mode (WebSocket) over `tokio-tungstenite`.
//!
//! Receive: open a WSS URL via `apps.connections.open`, then parse each
//! `events_api` envelope. DMs are always forwarded; channel messages are
//! forwarded only when the bot is @-mentioned. Every envelope is ack'd back
//! on the same socket.
//!
//! Send: an independent loop drains `outgoing_rx` and posts via
//! `chat.postMessage`. `reply_to` (a Slack `ts`) becomes `thread_ts`.
//!
//! Reconnect: on socket drop or a `disconnect` frame, re-open the connection
//! with exponential backoff (5s / 30s / 60s, capped).

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tracing::{info, warn};

use crate::types::{Gateway, IncomingMessage, OutgoingMessage, User};
use crate::{Error, Result};

const SLACK_API: &str = "https://slack.com/api";

pub struct SlackGateway {
    app_token: String,
    bot_token: String,
    http: reqwest::Client,
}

impl SlackGateway {
    pub fn from_env() -> Result<Self> {
        let app_token = std::env::var("SLACK_APP_TOKEN")
            .map_err(|_| Error::Other("SLACK_APP_TOKEN env var not set".into()))?;
        let bot_token = std::env::var("SLACK_BOT_TOKEN")
            .map_err(|_| Error::Other("SLACK_BOT_TOKEN env var not set".into()))?;
        Ok(Self::new(app_token, bot_token))
    }

    pub fn new(app_token: impl Into<String>, bot_token: impl Into<String>) -> Self {
        Self {
            app_token: app_token.into(),
            bot_token: bot_token.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client build"),
        }
    }
}

#[async_trait]
impl Gateway for SlackGateway {
    fn name(&self) -> &'static str {
        "slack"
    }

    async fn run(
        self: Arc<Self>,
        incoming_tx: mpsc::Sender<IncomingMessage>,
        outgoing_rx: mpsc::Receiver<OutgoingMessage>,
    ) -> Result<()> {
        let bot_user_id = fetch_bot_user_id(&self.bot_token, &self.http).await?;
        info!(bot_user_id = %bot_user_id, "slack auth.test ok");

        let send_gw = self.clone();
        let send_task = tokio::spawn(async move { send_loop(send_gw, outgoing_rx).await });

        let recv_res = self.receive_loop(incoming_tx, bot_user_id).await;

        send_task.abort();
        recv_res
    }
}

impl SlackGateway {
    async fn receive_loop(
        &self,
        incoming_tx: mpsc::Sender<IncomingMessage>,
        bot_user_id: String,
    ) -> Result<()> {
        let backoffs = [5u64, 30, 60];
        let mut consecutive_failures: usize = 0;

        loop {
            let url = match open_socket_url(&self.app_token, &self.http).await {
                Ok(u) => u,
                Err(e) => {
                    let secs = backoffs[consecutive_failures.min(backoffs.len() - 1)];
                    warn!(error = %e, backoff_secs = secs, "slack apps.connections.open failed");
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                    consecutive_failures += 1;
                    continue;
                }
            };

            info!("slack opening socket");
            let (ws_stream, _resp) = match connect_async(&url).await {
                Ok(pair) => pair,
                Err(e) => {
                    let secs = backoffs[consecutive_failures.min(backoffs.len() - 1)];
                    warn!(error = %e, backoff_secs = secs, "slack ws connect failed");
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                    consecutive_failures += 1;
                    continue;
                }
            };

            consecutive_failures = 0;
            let (write, mut read) = ws_stream.split();
            let write = Arc::new(Mutex::new(write));

            let mut should_reconnect = false;
            while let Some(frame) = read.next().await {
                let msg = match frame {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(error = %e, "slack ws read error");
                        should_reconnect = true;
                        break;
                    }
                };

                let text = match msg {
                    WsMessage::Text(t) => t,
                    WsMessage::Binary(_) => continue,
                    WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
                    WsMessage::Close(_) => {
                        info!("slack ws close frame");
                        should_reconnect = true;
                        break;
                    }
                    WsMessage::Frame(_) => continue,
                };

                let parsed: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(error = %e, "slack ws non-json frame; skipping");
                        continue;
                    }
                };

                let kind = parsed.get("type").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "hello" => {
                        info!("slack ws hello");
                        continue;
                    }
                    "disconnect" => {
                        info!("slack ws disconnect frame; reconnecting");
                        should_reconnect = true;
                        break;
                    }
                    _ => {}
                }

                let envelope_id = parsed
                    .get("envelope_id")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());

                if let Some(id) = envelope_id.as_ref() {
                    let ack = json!({ "envelope_id": id }).to_string();
                    let mut w = write.lock().await;
                    if let Err(e) = w.send(WsMessage::Text(ack)).await {
                        warn!(error = %e, envelope_id = %id, "slack ws ack send failed");
                        should_reconnect = true;
                        break;
                    }
                }

                match parse_envelope(&parsed, &bot_user_id) {
                    ParsedFrame::Forward(msg) => {
                        if incoming_tx.send(msg).await.is_err() {
                            return Ok(());
                        }
                    }
                    ParsedFrame::Skip => {}
                }
            }

            if !should_reconnect {
                info!("slack ws stream ended");
            }
        }
    }
}

async fn send_loop(gw: Arc<SlackGateway>, mut outgoing_rx: mpsc::Receiver<OutgoingMessage>) {
    while let Some(msg) = outgoing_rx.recv().await {
        let mut body = json!({
            "channel": msg.conversation_id,
            "text": msg.text,
        });
        if let Some(reply_to) = msg.reply_to.as_ref() {
            body["thread_ts"] = json!(reply_to);
        }

        let url = format!("{SLACK_API}/chat.postMessage");
        let res = gw
            .http
            .post(&url)
            .bearer_auth(&gw.bot_token)
            .json(&body)
            .send()
            .await;
        match res {
            Ok(resp) => {
                let status = resp.status();
                let parsed: std::result::Result<Value, _> = resp.json().await;
                match parsed {
                    Ok(v) if v.get("ok").and_then(Value::as_bool) == Some(true) => {}
                    Ok(v) => {
                        warn!(status = %status, body = %v, "slack chat.postMessage ok=false");
                    }
                    Err(e) => {
                        warn!(status = %status, error = %e, "slack chat.postMessage body parse");
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "slack chat.postMessage network error");
            }
        }
    }
}

async fn open_socket_url(app_token: &str, http: &reqwest::Client) -> Result<String> {
    let resp = http
        .post(format!("{SLACK_API}/apps.connections.open"))
        .bearer_auth(app_token)
        .send()
        .await
        .map_err(|e| Error::Other(format!("apps.connections.open: {e}")))?;
    let body: Value = resp
        .json()
        .await
        .map_err(|e| Error::Other(format!("apps.connections.open body: {e}")))?;
    if body.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(Error::Other(format!(
            "apps.connections.open ok=false: {body}"
        )));
    }
    body.get("url")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| Error::Other("apps.connections.open: no url in response".into()))
}

async fn fetch_bot_user_id(bot_token: &str, http: &reqwest::Client) -> Result<String> {
    let resp = http
        .post(format!("{SLACK_API}/auth.test"))
        .bearer_auth(bot_token)
        .send()
        .await
        .map_err(|e| Error::Other(format!("auth.test: {e}")))?;
    let body: Value = resp
        .json()
        .await
        .map_err(|e| Error::Other(format!("auth.test body: {e}")))?;
    if body.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(Error::Other(format!("auth.test ok=false: {body}")));
    }
    body.get("user_id")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| Error::Other(format!("auth.test missing user_id: {body}")))
}

enum ParsedFrame {
    Forward(IncomingMessage),
    Skip,
}

/// Inspect a Slack Socket Mode envelope. Returns:
/// - `Forward(msg)` if it's an `events_api` `message` event from a real user
///   in a DM or that @-mentions us.
/// - `Skip` for everything else (other envelope types, bot messages, messages
///   in a channel without a mention, malformed events).
fn parse_envelope(envelope: &Value, bot_user_id: &str) -> ParsedFrame {
    if envelope.get("type").and_then(Value::as_str) != Some("events_api") {
        return ParsedFrame::Skip;
    }

    let event = match envelope.pointer("/payload/event") {
        Some(e) => e,
        None => return ParsedFrame::Skip,
    };

    if event.get("type").and_then(Value::as_str) != Some("message") {
        return ParsedFrame::Skip;
    }

    if event.get("bot_id").is_some() {
        return ParsedFrame::Skip;
    }

    if event.get("subtype").is_some() {
        return ParsedFrame::Skip;
    }

    let user = match event.get("user").and_then(Value::as_str) {
        Some(u) => u.to_string(),
        None => return ParsedFrame::Skip,
    };
    let channel = match event.get("channel").and_then(Value::as_str) {
        Some(c) => c.to_string(),
        None => return ParsedFrame::Skip,
    };
    let ts = match event.get("ts").and_then(Value::as_str) {
        Some(t) => t.to_string(),
        None => return ParsedFrame::Skip,
    };
    let text = event
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let channel_type = event
        .get("channel_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let is_dm = channel_type == "im";
    let mention = format!("<@{bot_user_id}>");
    let is_mention = text.contains(&mention);

    if !is_dm && !is_mention {
        return ParsedFrame::Skip;
    }

    let cleaned = strip_bot_mention(&text, bot_user_id);

    ParsedFrame::Forward(IncomingMessage {
        user: User {
            platform: "slack".into(),
            id: user.clone(),
            display_name: user,
        },
        conversation_id: channel,
        message_id: ts,
        text: cleaned,
    })
}

/// Strip any `<@BOT_ID>` mentions of our bot from `content`, then collapse
/// surrounding whitespace and trim. Slack has no `<@!ID>` nick variant —
/// just the plain form. Mentions can appear anywhere in a message; we
/// remove all of them so the agent sees the user's actual text.
fn strip_bot_mention(content: &str, bot_id: &str) -> String {
    let token = format!("<@{bot_id}>");
    let without = content.replace(&token, " ");
    let mut out = String::with_capacity(without.len());
    let mut last_was_ws = true;
    for ch in without.chars() {
        if ch.is_whitespace() {
            if !last_was_ws {
                out.push(' ');
            }
            last_was_ws = true;
        } else {
            out.push(ch);
            last_was_ws = false;
        }
    }
    out.trim().to_string()
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AuthTestResponse {
    ok: bool,
    user_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn constructs_without_panic() {
        let gw = SlackGateway::new("xapp-fake", "xoxb-fake");
        assert_eq!(gw.app_token, "xapp-fake");
        assert_eq!(gw.bot_token, "xoxb-fake");
    }

    #[test]
    fn strips_leading_mention() {
        assert_eq!(strip_bot_mention("<@U12345> hello there", "U12345"), "hello there");
    }

    #[test]
    fn strips_trailing_mention() {
        assert_eq!(strip_bot_mention("hey <@U12345>", "U12345"), "hey");
    }

    #[test]
    fn strips_middle_mention() {
        assert_eq!(
            strip_bot_mention("hey <@U12345> how are you", "U12345"),
            "hey how are you"
        );
    }

    #[test]
    fn leaves_other_mentions_alone() {
        assert_eq!(
            strip_bot_mention("<@U99999> hello <@U12345>", "U12345"),
            "<@U99999> hello"
        );
    }

    #[test]
    fn empty_after_mention() {
        assert_eq!(strip_bot_mention("<@U12345>", "U12345"), "");
        assert_eq!(strip_bot_mention("   <@U12345>   ", "U12345"), "");
    }

    #[test]
    fn bare_mention_handling() {
        assert_eq!(strip_bot_mention("<@U12345>", "U12345"), "");
    }

    #[test]
    fn no_mention_just_trims() {
        assert_eq!(strip_bot_mention("  hello world  ", "U12345"), "hello world");
    }

    #[test]
    fn parses_dm_envelope() {
        let envelope = json!({
            "envelope_id": "env-abc",
            "type": "events_api",
            "payload": {
                "event": {
                    "type": "message",
                    "user": "U999",
                    "channel": "D456",
                    "channel_type": "im",
                    "text": "hello bot",
                    "ts": "1234.5678",
                    "thread_ts": null
                }
            }
        });

        match parse_envelope(&envelope, "U12345") {
            ParsedFrame::Forward(msg) => {
                assert_eq!(msg.user.platform, "slack");
                assert_eq!(msg.user.id, "U999");
                assert_eq!(msg.user.display_name, "U999");
                assert_eq!(msg.conversation_id, "D456");
                assert_eq!(msg.message_id, "1234.5678");
                assert_eq!(msg.text, "hello bot");
            }
            ParsedFrame::Skip => panic!("expected DM to forward"),
        }
    }

    #[test]
    fn parses_channel_mention_envelope() {
        let envelope = json!({
            "envelope_id": "env-xyz",
            "type": "events_api",
            "payload": {
                "event": {
                    "type": "message",
                    "user": "U999",
                    "channel": "C111",
                    "channel_type": "channel",
                    "text": "<@U12345> what's up",
                    "ts": "9999.0001"
                }
            }
        });

        match parse_envelope(&envelope, "U12345") {
            ParsedFrame::Forward(msg) => {
                assert_eq!(msg.conversation_id, "C111");
                assert_eq!(msg.message_id, "9999.0001");
                assert_eq!(msg.text, "what's up");
            }
            ParsedFrame::Skip => panic!("expected channel mention to forward"),
        }
    }

    #[test]
    fn skips_channel_without_mention() {
        let envelope = json!({
            "envelope_id": "env-1",
            "type": "events_api",
            "payload": {
                "event": {
                    "type": "message",
                    "user": "U999",
                    "channel": "C111",
                    "channel_type": "channel",
                    "text": "hello channel",
                    "ts": "1.0"
                }
            }
        });
        assert!(matches!(
            parse_envelope(&envelope, "U12345"),
            ParsedFrame::Skip
        ));
    }

    #[test]
    fn skips_bot_message() {
        let envelope = json!({
            "envelope_id": "env-bot",
            "type": "events_api",
            "payload": {
                "event": {
                    "type": "message",
                    "user": "U999",
                    "bot_id": "B123",
                    "channel": "D456",
                    "channel_type": "im",
                    "text": "I am a bot",
                    "ts": "1.0"
                }
            }
        });
        assert!(matches!(
            parse_envelope(&envelope, "U12345"),
            ParsedFrame::Skip
        ));
    }

    #[test]
    fn skips_non_events_api_envelope() {
        let envelope = json!({
            "envelope_id": "env-q",
            "type": "slash_commands",
            "payload": {}
        });
        assert!(matches!(
            parse_envelope(&envelope, "U12345"),
            ParsedFrame::Skip
        ));
    }

    #[test]
    fn skips_message_with_subtype() {
        // edits, joins, etc. carry a subtype — skip them.
        let envelope = json!({
            "envelope_id": "env-edit",
            "type": "events_api",
            "payload": {
                "event": {
                    "type": "message",
                    "subtype": "message_changed",
                    "user": "U999",
                    "channel": "D456",
                    "channel_type": "im",
                    "text": "edited",
                    "ts": "1.0"
                }
            }
        });
        assert!(matches!(
            parse_envelope(&envelope, "U12345"),
            ParsedFrame::Skip
        ));
    }

    // Note: the WebSocket fixture server test for the full Socket Mode loop is
    // deferred — too much scaffolding for a unit suite. The receive_loop is
    // manually tested against real Slack workspaces.
}
