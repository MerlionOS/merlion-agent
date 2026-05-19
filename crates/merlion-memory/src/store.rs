//! On-disk store implementation for [`MemoryStore`].

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::debug;

use crate::parse::{
    parse_index, parse_memory, remove_index_line, render_memory, upsert_index_line, validate_slug,
};
use crate::{Memory, MemoryRow, MemoryStore};

const INDEX_FILE: &str = "MEMORY.md";
const INDEX_HEADER: &str = "# Memory\n";

impl MemoryStore {
    /// Open a store rooted at `dir`. Creates the directory and a default
    /// `MEMORY.md` index if either is missing.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        if !dir.exists() {
            fs::create_dir_all(&dir)
                .with_context(|| format!("failed to create memory dir {}", dir.display()))?;
        }
        let index = dir.join(INDEX_FILE);
        if !index.exists() {
            fs::write(&index, INDEX_HEADER)
                .with_context(|| format!("failed to create {}", index.display()))?;
        }
        Ok(Self { dir })
    }

    /// Open the default store. Uses `$MERLION_HOME/memory` if set, else
    /// `~/.merlion/memory` via [`dirs::home_dir`].
    pub fn open_default() -> Result<Self> {
        let dir = if let Some(home) = std::env::var_os("MERLION_HOME") {
            PathBuf::from(home).join("memory")
        } else {
            let home = dirs::home_dir()
                .context("could not resolve home directory for default memory store")?;
            home.join(".merlion").join("memory")
        };
        Self::open(dir)
    }

    /// Path to the on-disk directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join(INDEX_FILE)
    }

    fn memory_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.md", name))
    }

    fn read_index(&self) -> Result<String> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(INDEX_HEADER.to_string());
        }
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
    }

    fn write_index(&self, text: &str) -> Result<()> {
        let path = self.index_path();
        fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))
    }

    /// List memories declared in `MEMORY.md`.
    pub fn list(&self) -> Result<Vec<MemoryRow>> {
        let text = self.read_index()?;
        Ok(parse_index(&text))
    }

    /// Read a single memory by slug. Fails if the file is missing or malformed.
    pub fn read(&self, name: &str) -> Result<Memory> {
        validate_slug(name)?;
        let path = self.memory_path(name);
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read memory `{}` at {}", name, path.display()))?;
        parse_memory(&raw).with_context(|| format!("failed to parse memory `{}`", name))
    }

    /// Write a memory to disk, creating or overwriting `<name>.md` and
    /// updating the index line in `MEMORY.md`.
    ///
    /// - `created_at` is preserved if the file already exists.
    /// - `updated_at` is always set to `Utc::now()`.
    pub fn write(&self, m: &Memory) -> Result<()> {
        validate_slug(&m.name)?;
        let path = self.memory_path(&m.name);

        let now = Utc::now();
        let created_at = if path.exists() {
            // Preserve the existing created_at, if we can parse it.
            match fs::read_to_string(&path)
                .ok()
                .and_then(|raw| parse_memory(&raw).ok())
            {
                Some(existing) => existing.created_at,
                None => m.created_at,
            }
        } else {
            // For new files honor whatever the caller passed (defaults to now in practice).
            m.created_at
        };

        let to_write = Memory {
            name: m.name.clone(),
            description: m.description.clone(),
            kind: m.kind.clone(),
            body: m.body.clone(),
            created_at,
            updated_at: now,
        };

        let rendered = render_memory(&to_write)?;
        fs::write(&path, rendered).with_context(|| {
            format!("failed to write memory `{}` at {}", m.name, path.display())
        })?;

        // Update the index. Use the slug itself as the "title" for now —
        // hand-edited titles in MEMORY.md are preserved by upsert_index_line
        // only when the file matches; since we replace the whole line on
        // match, we conservatively use the slug. Hooks come from description.
        let title = to_write.name.clone();
        let index_text = self.read_index()?;
        let new_index =
            upsert_index_line(&index_text, &to_write.name, &title, &to_write.description);
        self.write_index(&new_index)?;

        debug!(name = %to_write.name, "wrote memory");
        Ok(())
    }

    /// Delete a memory. Idempotent: missing files are not an error. The
    /// matching index line, if present, is stripped.
    pub fn delete(&self, name: &str) -> Result<()> {
        validate_slug(name)?;
        let path = self.memory_path(name);
        if path.exists() {
            fs::remove_file(&path).with_context(|| {
                format!("failed to remove memory `{}` at {}", name, path.display())
            })?;
        }
        let index_text = self.read_index()?;
        let new_index = remove_index_line(&index_text, name);
        if new_index != index_text {
            self.write_index(&new_index)?;
        }
        debug!(name = %name, "deleted memory");
        Ok(())
    }

    /// Build a context block suitable for system-prompt injection. Returns
    /// an empty string when there are no memories. Stops adding lines once
    /// the cumulative size would exceed `max_chars`.
    pub fn render_context_block(&self, max_chars: usize) -> Result<String> {
        let rows = self.list()?;
        if rows.is_empty() {
            return Ok(String::new());
        }
        let header = format!("# Persistent memory ({} entries)\n", rows.len());
        let mut out = String::new();
        if header.len() > max_chars {
            // Even the header doesn't fit — return empty rather than a half-built block.
            return Ok(String::new());
        }
        out.push_str(&header);
        for row in rows {
            let line = format!("- [{}] {}\n", row.name, row.hook);
            if out.len() + line.len() > max_chars {
                break;
            }
            out.push_str(&line);
        }
        Ok(out)
    }
}
