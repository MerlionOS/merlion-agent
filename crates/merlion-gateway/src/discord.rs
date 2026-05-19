//! Discord gateway adapter — built on `serenity`.
//!
//! Receive: a serenity `EventHandler` forwards each non-bot message that is
//! either a DM or an explicit @mention to `incoming_tx`. The bot's own
//! mention prefix is stripped from the text.
//!
//! Send: an independent loop drains `outgoing_rx` and posts via a separate
//! `Http` instance built from the same token, so we don't need to share
//! state with the running gateway client.

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

use serenity::all::{
    ChannelId, Context, CreateMessage, EventHandler, GatewayIntents, Http, Message, MessageId,
    MessageReference, MessageReferenceKind, Ready,
};
use serenity::Client;

use crate::types::{Gateway, IncomingMessage, OutgoingMessage, User};
use crate::{Error, Result};

pub struct DiscordGateway {
    token: String,
}

impl DiscordGateway {
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("DISCORD_BOT_TOKEN")
            .map_err(|_| Error::Other("DISCORD_BOT_TOKEN env var not set".into()))?;
        Ok(Self::new(token))
    }

    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

struct DiscordHandler {
    incoming_tx: mpsc::Sender<IncomingMessage>,
}

#[async_trait]
impl EventHandler for DiscordHandler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        info!(user = %ready.user.name, "discord gateway ready");
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        let is_dm = msg.guild_id.is_none();
        let mentioned = msg.mentions_me(&ctx).await.unwrap_or(false);
        if !is_dm && !mentioned {
            return;
        }

        let bot_user_id = ctx.cache.current_user().id;
        let cleaned = strip_bot_mention(&msg.content, bot_user_id.get());

        let display_name = msg
            .author
            .global_name
            .clone()
            .unwrap_or_else(|| msg.author.name.clone());

        let incoming = IncomingMessage {
            user: User {
                platform: "discord".into(),
                id: msg.author.id.to_string(),
                display_name,
            },
            conversation_id: msg.channel_id.to_string(),
            message_id: msg.id.to_string(),
            text: cleaned,
        };

        if let Err(e) = self.incoming_tx.send(incoming).await {
            warn!(error = %e, "discord incoming_tx closed; dropping message");
        }
    }
}

#[async_trait]
impl Gateway for DiscordGateway {
    fn name(&self) -> &'static str {
        "discord"
    }

    async fn run(
        self: Arc<Self>,
        incoming_tx: mpsc::Sender<IncomingMessage>,
        outgoing_rx: mpsc::Receiver<OutgoingMessage>,
    ) -> Result<()> {
        let handler = DiscordHandler { incoming_tx };
        let intents = GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;

        let mut client = Client::builder(&self.token, intents)
            .event_handler(handler)
            .await
            .map_err(|e| Error::Other(format!("discord client build: {e}")))?;

        let http = Arc::new(Http::new(&self.token));
        let send_task = tokio::spawn(send_loop(http, outgoing_rx));

        let client_res = client
            .start()
            .await
            .map_err(|e| Error::Other(format!("discord client start: {e}")));

        send_task.abort();
        client_res
    }
}

async fn send_loop(http: Arc<Http>, mut outgoing_rx: mpsc::Receiver<OutgoingMessage>) {
    while let Some(msg) = outgoing_rx.recv().await {
        let channel_id: u64 = match msg.conversation_id.parse() {
            Ok(n) => n,
            Err(e) => {
                warn!(
                    error = %e,
                    conversation_id = %msg.conversation_id,
                    "discord conversation_id not parseable as u64; dropping"
                );
                continue;
            }
        };
        let channel = ChannelId::new(channel_id);

        let mut builder = CreateMessage::new().content(&msg.text);

        if let Some(reply_to) = msg.reply_to.as_ref() {
            match reply_to.parse::<u64>() {
                Ok(n) => {
                    let mut reference =
                        MessageReference::new(MessageReferenceKind::Default, channel);
                    reference.message_id = Some(MessageId::new(n));
                    builder = builder.reference_message(reference);
                }
                Err(e) => {
                    warn!(error = %e, reply_to = %reply_to, "discord reply_to not u64; ignoring");
                }
            }
        }

        if let Err(e) = channel.send_message(http.as_ref(), builder).await {
            warn!(error = %e, channel = channel_id, "discord send_message error");
        }
    }
}

/// Strip a leading `<@BOT_ID>` or `<@!BOT_ID>` mention from `content`, then
/// trim. If no leading mention matches, just trim. Empty results are valid —
/// the dispatcher decides what to do with a bare mention.
fn strip_bot_mention(content: &str, bot_id: u64) -> String {
    let plain = format!("<@{bot_id}>");
    let nick = format!("<@!{bot_id}>");
    let trimmed = content.trim_start();
    let after = if let Some(rest) = trimmed.strip_prefix(&plain) {
        rest
    } else if let Some(rest) = trimmed.strip_prefix(&nick) {
        rest
    } else {
        trimmed
    };
    after.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_without_panic() {
        let gw = DiscordGateway::new("fake-token");
        assert_eq!(gw.token, "fake-token");
    }

    #[test]
    fn strips_plain_mention() {
        assert_eq!(
            strip_bot_mention("<@12345> hello there", 12345),
            "hello there"
        );
    }

    #[test]
    fn strips_nick_mention() {
        assert_eq!(strip_bot_mention("<@!12345> hi", 12345), "hi");
    }

    #[test]
    fn leaves_other_mentions_alone() {
        assert_eq!(
            strip_bot_mention("<@99999> hello <@12345>", 12345),
            "<@99999> hello <@12345>"
        );
    }

    #[test]
    fn empty_after_mention() {
        assert_eq!(strip_bot_mention("<@12345>", 12345), "");
        assert_eq!(strip_bot_mention("<@12345>   ", 12345), "");
    }

    #[test]
    fn leading_whitespace_then_mention() {
        assert_eq!(strip_bot_mention("   <@12345> hey", 12345), "hey");
    }

    #[test]
    fn no_mention_just_trims() {
        assert_eq!(strip_bot_mention("  hello world  ", 12345), "hello world");
    }
}
