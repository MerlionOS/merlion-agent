//! Per-platform user allowlist driven by env vars.
//!
//! Default is **deny** — if no env var is set for a platform, no user
//! is admitted. `MERLION_GATEWAY_ALLOW_ALL=1` is an explicit insecure
//! bypass for development.
//!
//! Per-platform variables (comma-separated user ids):
//!   MERLION_GATEWAY_ALLOW_TELEGRAM
//!   MERLION_GATEWAY_ALLOW_DISCORD
//!   MERLION_GATEWAY_ALLOW_SLACK

use std::collections::{HashMap, HashSet};

use crate::User;

#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    per_platform: HashMap<String, HashSet<String>>,
    allow_all: bool,
}

impl Allowlist {
    pub fn from_env() -> Self {
        let allow_all = std::env::var("MERLION_GATEWAY_ALLOW_ALL")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false);
        let mut per_platform = HashMap::new();
        for platform in ["telegram", "discord", "slack"] {
            let key = format!("MERLION_GATEWAY_ALLOW_{}", platform.to_uppercase());
            if let Ok(val) = std::env::var(&key) {
                let ids: HashSet<String> = val
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                per_platform.insert(platform.into(), ids);
            }
        }
        Self {
            per_platform,
            allow_all,
        }
    }

    pub fn permits(&self, user: &User) -> bool {
        if self.allow_all {
            return true;
        }
        self.per_platform
            .get(&user.platform)
            .map(|ids| ids.contains(&user.id))
            .unwrap_or(false)
    }

    /// Returns true if the user is admitted by an explicit allowlist entry
    /// (not by `MERLION_GATEWAY_ALLOW_ALL`).
    pub fn explicitly_allows(&self, user: &User) -> bool {
        self.per_platform
            .get(&user.platform)
            .map(|ids| ids.contains(&user.id))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(platform: &str, id: &str) -> User {
        User {
            platform: platform.into(),
            id: id.into(),
            display_name: "x".into(),
        }
    }

    #[test]
    fn default_denies_everyone() {
        let a = Allowlist::default();
        assert!(!a.permits(&user("telegram", "1")));
    }

    #[test]
    fn allow_all_admits_anyone() {
        let a = Allowlist {
            allow_all: true,
            ..Default::default()
        };
        assert!(a.permits(&user("telegram", "1")));
        assert!(a.permits(&user("discord", "99")));
    }

    #[test]
    fn per_platform_lookup() {
        let mut a = Allowlist::default();
        a.per_platform.insert(
            "telegram".into(),
            ["123".to_string(), "456".to_string()].into_iter().collect(),
        );
        assert!(a.permits(&user("telegram", "123")));
        assert!(!a.permits(&user("telegram", "789")));
        assert!(!a.permits(&user("discord", "123")));
    }
}
