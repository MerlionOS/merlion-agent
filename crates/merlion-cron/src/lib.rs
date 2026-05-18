//! Scheduled-job manager for Merlion Agent.
//!
//! Jobs are stored in `~/.merlion/cron.yaml` as a list of `(name, schedule,
//! prompt)` triples where `schedule` is a 5-field POSIX cron expression
//! (with an optional 6th seconds field — the `cron` crate's default). When
//! `merlion cron daemon` runs, each job fires at its scheduled time and
//! the resulting agent response is logged (or delivered through a
//! gateway, if configured).

pub mod registry;
pub mod scheduler;

pub use registry::{CronRegistry, Job};
pub use scheduler::{run_once, Scheduler};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid cron expression `{0}`: {1}")]
    BadSchedule(String, String),
    #[error("no job named `{0}`")]
    NotFound(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
