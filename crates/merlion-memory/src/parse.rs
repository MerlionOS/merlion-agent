//! Parsing helpers for memory files and the `MEMORY.md` index.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{Memory, MemoryRow, MemoryType};

/// On-disk YAML front-matter shape. Kept private; converted to/from [`Memory`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FrontMatter {
    pub name: String,
    pub description: String,
    pub metadata: FrontMatterMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FrontMatterMetadata {
    #[serde(rename = "type")]
    pub kind: MemoryType,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Split a raw memory file into `(front_matter_yaml, body)`.
///
/// Expects the file to begin with `---\n`, contain a closing `---` fence,
/// followed by an optional blank line and then the body.
pub(crate) fn split_front_matter(raw: &str) -> Result<(&str, &str)> {
    let rest = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))
        .ok_or_else(|| anyhow!("memory file is missing opening `---` front-matter fence"))?;

    // Find the closing fence at the start of a line.
    let mut search_start = 0usize;
    let close_rel = loop {
        let slice = &rest[search_start..];
        let idx = slice
            .find("\n---")
            .ok_or_else(|| anyhow!("memory file is missing closing `---` front-matter fence"))?;
        let abs = search_start + idx;
        // Ensure the `---` is followed by EOL or EOF (a line of its own).
        let after = &rest[abs + 4..];
        if after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n") {
            break abs;
        }
        search_start = abs + 1;
    };

    let yaml = &rest[..close_rel];
    // Skip the closing fence line.
    let after_fence = &rest[close_rel + 1..]; // drops leading '\n'
    let after_fence = after_fence
        .strip_prefix("---\n")
        .or_else(|| after_fence.strip_prefix("---\r\n"))
        .or_else(|| after_fence.strip_prefix("---"))
        .unwrap_or(after_fence);
    // Strip a single leading blank line if present.
    let body = after_fence
        .strip_prefix("\n")
        .or_else(|| after_fence.strip_prefix("\r\n"))
        .unwrap_or(after_fence);

    Ok((yaml, body))
}

/// Parse a memory file's raw contents into a [`Memory`].
pub(crate) fn parse_memory(raw: &str) -> Result<Memory> {
    let (yaml, body) = split_front_matter(raw)?;
    let fm: FrontMatter =
        serde_yaml::from_str(yaml).context("failed to parse memory front-matter as YAML")?;
    Ok(Memory {
        name: fm.name,
        description: fm.description,
        kind: fm.metadata.kind,
        body: body.to_string(),
        created_at: fm.metadata.created_at,
        updated_at: fm.metadata.updated_at,
    })
}

/// Serialize a memory back into the on-disk format.
pub(crate) fn render_memory(m: &Memory) -> Result<String> {
    let fm = FrontMatter {
        name: m.name.clone(),
        description: m.description.clone(),
        metadata: FrontMatterMetadata {
            kind: m.kind.clone(),
            created_at: m.created_at,
            updated_at: m.updated_at,
        },
    };
    let yaml = serde_yaml::to_string(&fm).context("failed to serialize memory front-matter")?;
    // `serde_yaml::to_string` already ends with a newline; ensure body is separated by one blank line.
    let mut out = String::with_capacity(yaml.len() + m.body.len() + 16);
    out.push_str("---\n");
    out.push_str(&yaml);
    out.push_str("---\n\n");
    out.push_str(&m.body);
    if !m.body.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Regex matching one row of the `MEMORY.md` index.
///
/// Captures: `title`, `file`, `hook`.
pub(crate) fn index_row_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^\s*-\s*\[(?P<title>[^\]]+)\]\((?P<file>[^)]+)\)\s*(?:—|--|-)\s*(?P<hook>.+?)\s*$",
        )
        .expect("index row regex compiles")
    })
}

/// Parse the rows of `MEMORY.md`, skipping non-matching lines.
pub(crate) fn parse_index(text: &str) -> Vec<MemoryRow> {
    let re = index_row_regex();
    text.lines()
        .filter_map(|line| {
            let caps = re.captures(line)?;
            let title = caps.name("title")?.as_str().trim().to_string();
            let file = caps.name("file")?.as_str().trim().to_string();
            let hook = caps.name("hook")?.as_str().trim().to_string();
            let name = file
                .strip_suffix(".md")
                .map(|s| s.to_string())
                .unwrap_or_else(|| file.clone());
            Some(MemoryRow {
                name,
                title,
                hook,
                file,
            })
        })
        .collect()
}

/// Slug validator: kebab-case, lowercase letters/digits, leading char must be alnum.
pub(crate) fn slug_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9-]*$").expect("slug regex compiles"))
}

pub(crate) fn validate_slug(name: &str) -> Result<()> {
    if slug_regex().is_match(name) {
        Ok(())
    } else {
        Err(anyhow!(
            "invalid memory slug `{}`: must match ^[a-z0-9][a-z0-9-]*$",
            name
        ))
    }
}

/// Truncate `s` to at most `max` chars (not bytes), preserving char boundaries.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Build a single index line for the given memory name + hook.
pub(crate) fn index_line_for(name: &str, title: &str, hook: &str) -> String {
    let hook = truncate_chars(hook, 120);
    format!("- [{}]({}.md) — {}", title, name, hook)
}

/// Update or append the index line for `name` in `index_text`. Returns the
/// rewritten index. Existing non-matching lines are preserved verbatim.
pub(crate) fn upsert_index_line(index_text: &str, name: &str, title: &str, hook: &str) -> String {
    let re = index_row_regex();
    let target_file = format!("{}.md", name);
    let mut lines: Vec<String> = index_text.lines().map(|l| l.to_string()).collect();
    let new_line = index_line_for(name, title, hook);
    let mut replaced = false;
    for line in lines.iter_mut() {
        if let Some(caps) = re.captures(line) {
            if caps.name("file").map(|m| m.as_str().trim()) == Some(target_file.as_str()) {
                *line = new_line.clone();
                replaced = true;
                break;
            }
        }
    }
    if !replaced {
        // Ensure file ends with newline before appending.
        lines.push(new_line);
    }
    let mut out = lines.join("\n");
    // Preserve trailing newline behavior: ensure file ends with `\n`.
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Remove the index line matching `<name>.md`, if present.
pub(crate) fn remove_index_line(index_text: &str, name: &str) -> String {
    let re = index_row_regex();
    let target_file = format!("{}.md", name);
    let kept: Vec<&str> = index_text
        .lines()
        .filter(|line| {
            if let Some(caps) = re.captures(line) {
                if caps.name("file").map(|m| m.as_str().trim()) == Some(target_file.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();
    let mut out = kept.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}
