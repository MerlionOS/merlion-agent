//! Persistent markdown memory store for the Merlion Agent.
//!
//! Manages a directory of small markdown files that the agent uses to
//! remember things across sessions. Each memory is a single file with YAML
//! front-matter and a markdown body; an index file (`MEMORY.md`) lists the
//! memories along with a one-line "hook" used to decide relevance.
//!
//! See the crate README / parent project docs for the on-disk format.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod parse;
mod store;

/// Handle to a memory directory on disk.
pub struct MemoryStore {
    pub(crate) dir: PathBuf,
}

/// The "type" tag stored in each memory's front-matter `metadata.type` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
}

/// A single memory: front-matter fields plus the markdown body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub name: String,
    pub description: String,
    pub kind: MemoryType,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One row parsed out of `MEMORY.md`.
#[derive(Debug, Clone)]
pub struct MemoryRow {
    pub name: String,
    pub title: String,
    pub hook: String,
    pub file: String,
}
