//! Messaging gateway for Merlion Agent.
//!
//! A single dispatcher serves many users across many platforms. Each
//! platform adapter implements [`Gateway`] — it receives platform messages
//! and pushes them to the dispatcher, and drains outbound messages back to
//! the platform's send API. The dispatcher itself is platform-agnostic:
//! given an [`IncomingMessage`], it looks up the right session, runs the
//! agent, and emits [`OutgoingMessage`]s.

pub mod allowlist;
pub mod discord;
pub mod dispatcher;
pub mod joinkeys;
pub mod slack;
pub mod telegram;
pub mod types;

pub use allowlist::Allowlist;
pub use discord::DiscordGateway;
pub use dispatcher::Dispatcher;
pub use joinkeys::{JoinKey, JoinKeyStore};
pub use slack::SlackGateway;
pub use telegram::TelegramGateway;
pub use types::{Gateway, IncomingMessage, OutgoingMessage, User};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("gateway error: {0}")]
    Gateway(String),

    #[error("not allowed: {0}")]
    Forbidden(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
