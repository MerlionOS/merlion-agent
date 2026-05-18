use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("llm error: {0}")]
    Llm(String),

    #[error("tool `{name}` not found")]
    ToolNotFound { name: String },

    #[error("tool `{name}` failed: {message}")]
    ToolFailed { name: String, message: String },

    #[error("invalid tool arguments for `{name}`: {message}")]
    InvalidToolArgs { name: String, message: String },

    #[error("conversation interrupted")]
    Interrupted,

    #[error("iteration budget exhausted")]
    BudgetExhausted,

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}
