use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub name: String,
    /// POSIX cron expression. `cron` crate accepts 5 fields (m h dom mon dow)
    /// with an optional leading seconds field.
    pub schedule: String,
    /// Natural-language prompt to send to the agent at each scheduled fire.
    pub prompt: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Where to send the result. `cli` = log to stdout. `telegram:<chat_id>`
    /// = send via the telegram gateway. Future: discord, slack, email.
    #[serde(default = "default_destination")]
    pub destination: String,
}

fn default_enabled() -> bool {
    true
}
fn default_destination() -> String {
    "cli".into()
}

impl Job {
    pub fn validate(&self) -> Result<()> {
        cron::Schedule::from_str(&self.schedule)
            .map_err(|e| Error::BadSchedule(self.schedule.clone(), e.to_string()))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CronRegistry {
    #[serde(default)]
    pub jobs: Vec<Job>,
}

impl CronRegistry {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        Ok(serde_yaml::from_str(&text)?)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(path, yaml)?;
        Ok(())
    }

    pub fn default_path() -> PathBuf {
        if let Ok(home) = std::env::var("MERLION_HOME") {
            return PathBuf::from(home).join("cron.yaml");
        }
        dirs::home_dir()
            .map(|h| h.join(".merlion").join("cron.yaml"))
            .unwrap_or_else(|| PathBuf::from(".merlion/cron.yaml"))
    }

    pub fn load_default() -> Result<Self> {
        Self::load(Self::default_path())
    }

    pub fn add(&mut self, job: Job) -> Result<()> {
        job.validate()?;
        if let Some(existing) = self.jobs.iter_mut().find(|j| j.name == job.name) {
            *existing = job;
        } else {
            self.jobs.push(job);
        }
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Option<Job> {
        if let Some(idx) = self.jobs.iter().position(|j| j.name == name) {
            Some(self.jobs.remove(idx))
        } else {
            None
        }
    }

    pub fn get(&self, name: &str) -> Option<&Job> {
        self.jobs.iter().find(|j| j.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_cron_passes_validation() {
        let j = Job {
            name: "daily".into(),
            schedule: "0 9 * * * *".into(), // sec min hour dom mon dow
            prompt: "summarize my email".into(),
            enabled: true,
            destination: "cli".into(),
        };
        j.validate().unwrap();
    }

    #[test]
    fn invalid_cron_rejected() {
        let j = Job {
            name: "bad".into(),
            schedule: "not a cron".into(),
            prompt: "x".into(),
            enabled: true,
            destination: "cli".into(),
        };
        assert!(j.validate().is_err());
    }

    #[test]
    fn add_replaces_by_name() {
        let mut r = CronRegistry::default();
        let j = Job {
            name: "x".into(),
            schedule: "0 0 * * * *".into(),
            prompt: "v1".into(),
            enabled: true,
            destination: "cli".into(),
        };
        r.add(j).unwrap();
        let j = Job {
            name: "x".into(),
            schedule: "0 0 * * * *".into(),
            prompt: "v2".into(),
            enabled: true,
            destination: "cli".into(),
        };
        r.add(j).unwrap();
        assert_eq!(r.jobs.len(), 1);
        assert_eq!(r.jobs[0].prompt, "v2");
    }
}
