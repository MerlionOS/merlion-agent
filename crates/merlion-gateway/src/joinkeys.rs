//! Cross-platform session continuity via short-lived "join keys".
//!
//! A user runs `/share` in the CLI; merlion mints a 6-character code keyed
//! against the active CLI session_id. The user then sends `/join <key>`
//! from Telegram / Discord / Slack — the gateway dispatcher looks up the
//! code, validates it hasn't expired, and persists a
//! `(platform, user_id) → session_id` binding so subsequent messages from
//! that account land in the CLI's session instead of the default
//! `gateway_<platform>_<user>` one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinKey {
    pub session_id: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct JoinKeyStore {
    /// Active short codes → session id + expiry. Consumed (removed) when a
    /// platform user redeems a code with `/join <key>`.
    #[serde(default)]
    pub keys: HashMap<String, JoinKey>,
    /// `(platform, user_id)` → adopted session_id. No expiry — once joined,
    /// stays joined until the user runs `/leave`. Key format: `"{platform}:{user_id}"`.
    #[serde(default)]
    pub bindings: HashMap<String, String>,
}

impl JoinKeyStore {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let store: Self = serde_yaml::from_str(&text)?;
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_yaml::to_string(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// `~/.merlion/join_keys.yaml`, honouring `MERLION_HOME` if set.
    pub fn default_path() -> PathBuf {
        let home = std::env::var("MERLION_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .map(|h| h.join(".merlion"))
                    .unwrap_or_else(|| PathBuf::from(".merlion"))
            });
        home.join("join_keys.yaml")
    }

    /// Generate a fresh 6-char key (uppercase alphanumeric, avoiding the
    /// look-alikes 0/O and 1/I) and register it pointing at `session_id`.
    /// Returns the key. Retries on the astronomically unlikely collision.
    pub fn mint(&mut self, session_id: String, ttl_seconds: i64) -> String {
        let expires_at = Utc::now() + Duration::seconds(ttl_seconds);
        for _ in 0..32 {
            let candidate = generate_key(6);
            if !self.keys.contains_key(&candidate) {
                self.keys.insert(
                    candidate.clone(),
                    JoinKey { session_id: session_id.clone(), expires_at },
                );
                return candidate;
            }
        }
        // Pathological case — overwrite an arbitrary slot rather than loop forever.
        let candidate = generate_key(6);
        self.keys
            .insert(candidate.clone(), JoinKey { session_id, expires_at });
        candidate
    }

    /// Look up an active key. Returns the bound `session_id` if the key
    /// exists and has not expired, removing it as a side-effect (one-shot).
    /// Expired keys are also removed. Subsequent calls return `None`.
    pub fn consume(&mut self, key: &str) -> Option<String> {
        let key = key.trim().to_uppercase();
        let entry = self.keys.remove(&key)?;
        if entry.expires_at < Utc::now() {
            return None;
        }
        Some(entry.session_id)
    }

    pub fn bind(&mut self, platform: &str, user_id: &str, session_id: String) {
        self.bindings.insert(binding_key(platform, user_id), session_id);
    }

    pub fn lookup_binding(&self, platform: &str, user_id: &str) -> Option<&str> {
        self.bindings
            .get(&binding_key(platform, user_id))
            .map(|s| s.as_str())
    }

    pub fn unbind(&mut self, platform: &str, user_id: &str) {
        self.bindings.remove(&binding_key(platform, user_id));
    }

    /// Drop any keys whose TTL has elapsed. Safe to call periodically; not
    /// load-bearing for correctness because `consume` re-checks expiry.
    pub fn gc(&mut self) {
        let now = Utc::now();
        self.keys.retain(|_, v| v.expires_at >= now);
    }
}

fn binding_key(platform: &str, user_id: &str) -> String {
    format!("{platform}:{user_id}")
}

/// 6-char unambiguous code: omits 0/O and 1/I/L. Uses nanosecond entropy
/// XOR'd with a per-byte counter so we don't need to pull in `rand`.
fn generate_key(len: usize) -> String {
    const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
    let now = Utc::now();
    // Nanosecond timestamp gives ~30 bits of entropy per call; we mix in
    // an additional source-of-instability (the address of a stack value)
    // to keep keys distinct under tight loops.
    let stamp = now.timestamp_nanos_opt().unwrap_or_else(|| now.timestamp_micros());
    let mut state = (stamp as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(&stamp as *const _ as u64);
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let idx = (state >> 33) as usize % ALPHABET.len();
        out.push(ALPHABET[idx] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn mint_then_consume_roundtrip() {
        let mut store = JoinKeyStore::default();
        let key = store.mint("session-abc".into(), 600);
        assert_eq!(key.len(), 6);
        assert!(key.chars().all(|c| c.is_ascii_alphanumeric()));
        let resolved = store.consume(&key);
        assert_eq!(resolved.as_deref(), Some("session-abc"));
    }

    #[test]
    fn consume_is_idempotent_after_success() {
        let mut store = JoinKeyStore::default();
        let key = store.mint("session-xyz".into(), 600);
        assert!(store.consume(&key).is_some());
        assert!(store.consume(&key).is_none());
    }

    #[test]
    fn expired_key_returns_none() {
        let mut store = JoinKeyStore::default();
        let key = store.mint("session-old".into(), 600);
        // Backdate the entry to be safely in the past.
        let entry = store.keys.get_mut(&key).unwrap();
        entry.expires_at = Utc::now() - Duration::seconds(1);
        assert!(store.consume(&key).is_none());
    }

    #[test]
    fn unknown_key_returns_none() {
        let mut store = JoinKeyStore::default();
        assert!(store.consume("ZZZZZZ").is_none());
    }

    #[test]
    fn consume_is_case_insensitive_and_trims() {
        let mut store = JoinKeyStore::default();
        let key = store.mint("session-lc".into(), 600);
        let lower = key.to_lowercase();
        let padded = format!("  {lower}  ");
        assert_eq!(store.consume(&padded).as_deref(), Some("session-lc"));
    }

    #[test]
    fn bind_lookup_unbind_roundtrip() {
        let mut store = JoinKeyStore::default();
        assert!(store.lookup_binding("telegram", "42").is_none());
        store.bind("telegram", "42", "session-shared".into());
        assert_eq!(store.lookup_binding("telegram", "42"), Some("session-shared"));
        // Another user / platform stays independent.
        assert!(store.lookup_binding("discord", "42").is_none());
        store.unbind("telegram", "42");
        assert!(store.lookup_binding("telegram", "42").is_none());
    }

    #[test]
    fn yaml_load_save_roundtrip() {
        let tmp = tempdir();
        let path = tmp.join("join_keys.yaml");
        let mut store = JoinKeyStore::default();
        let key = store.mint("session-persist".into(), 600);
        store.bind("telegram", "42", "session-persist".into());
        store.save(&path).unwrap();

        let loaded = JoinKeyStore::load(&path).unwrap();
        assert!(loaded.keys.contains_key(&key));
        assert_eq!(
            loaded.lookup_binding("telegram", "42"),
            Some("session-persist")
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn load_missing_file_returns_default() {
        let tmp = tempdir();
        let path = tmp.join("does-not-exist.yaml");
        let loaded = JoinKeyStore::load(&path).unwrap();
        assert!(loaded.keys.is_empty());
        assert!(loaded.bindings.is_empty());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn gc_removes_expired_keys_only() {
        let mut store = JoinKeyStore::default();
        let fresh = store.mint("fresh".into(), 600);
        let stale = store.mint("stale".into(), 600);
        store.keys.get_mut(&stale).unwrap().expires_at = Utc::now() - Duration::seconds(1);
        store.gc();
        assert!(store.keys.contains_key(&fresh));
        assert!(!store.keys.contains_key(&stale));
    }

    /// Cheap temp-dir helper that avoids pulling in a dev-dep.
    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir();
        let unique = format!(
            "merlion-joinkeys-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let p = base.join(unique);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
