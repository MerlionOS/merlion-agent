//! Cron scheduler — polls each job's next fire time and dispatches to a
//! caller-supplied [`JobRunner`] when due.
//!
//! The "what to do with the prompt" decision is intentionally not baked in.
//! `merlion cron daemon` provides a default runner that constructs an
//! [`Agent`] from the user's current config and runs the prompt non-
//! streamingly, then prints the response. A future gateway-aware runner
//! would push the response back through the messaging gateway instead.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::registry::{CronRegistry, Job};
use crate::Result;

#[async_trait]
pub trait JobRunner: Send + Sync {
    /// Execute the given job once. Implementations decide where the
    /// resulting agent response goes (stdout, messaging gateway, etc.).
    async fn run_job(&self, job: &Job);
}

pub struct Scheduler {
    registry: CronRegistry,
    runner: Arc<dyn JobRunner>,
}

impl Scheduler {
    pub fn new(registry: CronRegistry, runner: Arc<dyn JobRunner>) -> Self {
        Self { registry, runner }
    }

    /// Run forever, firing jobs as they come due. Returns only on a fatal
    /// scheduling error.
    pub async fn run(self) -> Result<()> {
        let (tx, mut rx) = mpsc::channel::<Job>(32);
        for job in self.registry.jobs.iter().filter(|j| j.enabled).cloned() {
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Err(e) = run_job_forever(job, tx).await {
                    warn!(error = %e, "cron job task ended");
                }
            });
        }
        drop(tx);
        while let Some(job) = rx.recv().await {
            self.runner.run_job(&job).await;
        }
        Ok(())
    }
}

async fn run_job_forever(job: Job, tx: mpsc::Sender<Job>) -> Result<()> {
    let schedule = cron::Schedule::from_str(&job.schedule)
        .map_err(|e| crate::Error::BadSchedule(job.schedule.clone(), e.to_string()))?;
    loop {
        let now: DateTime<Utc> = Utc::now();
        let Some(next) = schedule.upcoming(Utc).next() else {
            warn!(name = %job.name, "cron schedule yielded no future fire time; stopping job");
            return Ok(());
        };
        let delay = (next - now).to_std().unwrap_or(std::time::Duration::from_secs(0));
        info!(name = %job.name, fires_at = %next, "sleeping until next cron fire");
        tokio::time::sleep(delay).await;
        if tx.send(job.clone()).await.is_err() {
            return Ok(());
        }
    }
}

/// One-shot helper used by `merlion cron run <name>`.
pub async fn run_once(job: &Job, runner: &dyn JobRunner) {
    runner.run_job(job).await;
}
