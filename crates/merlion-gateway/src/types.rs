use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::Result;

/// Identifies a human (or service account) on a messaging platform.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct User {
    pub platform: String,
    pub id: String,
    pub display_name: String,
}

impl User {
    /// Build a session id that's stable for this user across runs. The CLI
    /// uses random UUIDs; messaging users get a derived id keyed on
    /// (platform, user_id) so their history persists.
    pub fn session_id(&self) -> String {
        format!("gateway_{}_{}", self.platform, self.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingMessage {
    pub user: User,
    /// Platform's conversation id (e.g. Telegram chat id). Sent back as the
    /// destination of the reply.
    pub conversation_id: String,
    /// Platform's message id (Telegram message_id, Discord message id, etc.)
    /// — used for threading/replies.
    pub message_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingMessage {
    pub conversation_id: String,
    /// If `Some`, the gateway should send this as a reply to the named
    /// message. If `None`, send as a fresh top-level message.
    pub reply_to: Option<String>,
    pub text: String,
}

/// A messaging-platform adapter. Implementors run two concurrent tasks:
///
/// - **Receive task** — poll/listen on the platform; for each message
///   construct an [`IncomingMessage`] and push it to `incoming_tx`.
/// - **Send task** — drain `outgoing_rx` and dispatch each
///   [`OutgoingMessage`] through the platform's send API.
///
/// `run` should return when either channel closes or the platform connection
/// is lost.
#[async_trait]
pub trait Gateway: Send + Sync {
    fn name(&self) -> &'static str;

    async fn run(
        self: std::sync::Arc<Self>,
        incoming_tx: mpsc::Sender<IncomingMessage>,
        outgoing_rx: mpsc::Receiver<OutgoingMessage>,
    ) -> Result<()>;
}
