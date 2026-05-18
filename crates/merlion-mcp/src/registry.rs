//! `~/.merlion/mcp.yaml` — the user's list of MCP servers.
//!
//! Format:
//! ```yaml
//! servers:
//!   filesystem:
//!     transport: stdio
//!     command: npx
//!     args: ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/projects"]
//!     env:
//!       FOO: bar
//!     enabled: true
//!   notes:
//!     transport: stdio
//!     command: /usr/local/bin/notes-mcp
//!     args: []
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpRegistry {
    #[serde(default)]
    pub servers: BTreeMap<String, ServerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    #[serde(flatten)]
    pub transport: TransportSpec,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum TransportSpec {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    // HTTP is Phase 4 future work.
}

impl McpRegistry {
    /// Load the registry from a yaml file. Missing file → empty registry.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let parsed: Self = serde_yaml::from_str(&text)?;
        Ok(parsed)
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

    /// Default location: `$MERLION_HOME/mcp.yaml` or `~/.merlion/mcp.yaml`.
    pub fn default_path() -> PathBuf {
        if let Ok(home) = std::env::var("MERLION_HOME") {
            return PathBuf::from(home).join("mcp.yaml");
        }
        dirs::home_dir()
            .map(|h| h.join(".merlion").join("mcp.yaml"))
            .unwrap_or_else(|| PathBuf::from(".merlion/mcp.yaml"))
    }

    pub fn load_default() -> Result<Self> {
        Self::load(Self::default_path())
    }

    pub fn add(&mut self, name: impl Into<String>, entry: ServerEntry) {
        self.servers.insert(name.into(), entry);
    }

    pub fn remove(&mut self, name: &str) -> Option<ServerEntry> {
        self.servers.remove(name)
    }

    pub fn enabled_servers(&self) -> impl Iterator<Item = (&String, &ServerEntry)> {
        self.servers.iter().filter(|(_, e)| e.enabled)
    }
}

impl ServerEntry {
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            transport: TransportSpec::Stdio { command: command.into(), args, env: Default::default() },
            enabled: true,
        }
    }
}

/// Parsed CLI shorthand: `"npx -y @modelcontextprotocol/server-filesystem /tmp"`.
/// First token is the command; the rest are args. Quoting is not supported —
/// users who need spaces in args should edit the yaml directly.
pub fn parse_stdio_command(s: &str) -> Result<TransportSpec> {
    let mut parts = s.split_whitespace();
    let command = parts
        .next()
        .ok_or_else(|| Error::Other("empty command".into()))?
        .to_string();
    let args = parts.map(|s| s.to_string()).collect();
    Ok(TransportSpec::Stdio { command, args, env: Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_yaml_loads_as_empty_registry() {
        let r = McpRegistry::load("/dev/null").unwrap();
        assert!(r.servers.is_empty());
    }

    #[test]
    fn roundtrip_through_yaml_preserves_servers() {
        let mut r = McpRegistry::default();
        r.add(
            "fs",
            ServerEntry::stdio("npx", vec!["-y".into(), "@mcp/fs".into(), "/tmp".into()]),
        );
        let yaml = serde_yaml::to_string(&r).unwrap();
        let back: McpRegistry = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.servers.len(), 1);
        let entry = back.servers.get("fs").unwrap();
        match &entry.transport {
            TransportSpec::Stdio { command, args, .. } => {
                assert_eq!(command, "npx");
                assert_eq!(args.len(), 3);
            }
        }
        assert!(entry.enabled, "default enabled should round-trip as true");
    }

    #[test]
    fn parse_stdio_command_splits_on_whitespace() {
        let spec = parse_stdio_command("npx -y @mcp/fs /tmp").unwrap();
        match spec {
            TransportSpec::Stdio { command, args, .. } => {
                assert_eq!(command, "npx");
                assert_eq!(args, vec!["-y", "@mcp/fs", "/tmp"]);
            }
        }
    }
}
