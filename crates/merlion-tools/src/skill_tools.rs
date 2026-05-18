//! Tools the agent uses to write and refine its own skills.
//!
//! `skill_create` writes a new flat-style skill file at
//! `<skills_dir>/<name>.md`. `skill_update` replaces the body of an
//! existing skill (front-matter is preserved). Both refuse to clobber
//! anything outside the configured skills directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use merlion_core::{Tool, ToolResult, ToolSchema};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::fs;

/// Shared configuration for the skill tools.
pub struct SkillToolsConfig {
    pub skills_dir: PathBuf,
}

impl SkillToolsConfig {
    pub fn new(skills_dir: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self { skills_dir: skills_dir.into() })
    }
}

pub struct SkillCreate {
    cfg: Arc<SkillToolsConfig>,
}

impl SkillCreate {
    pub fn new(cfg: Arc<SkillToolsConfig>) -> Self {
        Self { cfg }
    }
}

pub struct SkillUpdate {
    cfg: Arc<SkillToolsConfig>,
}

impl SkillUpdate {
    pub fn new(cfg: Arc<SkillToolsConfig>) -> Self {
        Self { cfg }
    }
}

#[derive(Debug, Deserialize)]
struct CreateArgs {
    name: String,
    description: String,
    body: String,
}

#[derive(Debug, Deserialize)]
struct UpdateArgs {
    name: String,
    #[serde(default)]
    description: Option<String>,
    body: String,
}

#[async_trait]
impl Tool for SkillCreate {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "skill_create".into(),
            description:
                "Create a new skill the user can invoke with `/<name>`. Writes \
                 `<merlion_home>/skills/<name>.md` with the given front-matter and body. \
                 Use this when you discover a repeatable workflow worth giving a name to."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "kebab-case slug, e.g. \"deploy\"" },
                    "description": { "type": "string", "description": "one-line summary the user sees in `/help skills`" },
                    "body": { "type": "string", "description": "markdown body — the instructions the model follows when the skill is invoked" }
                },
                "required": ["name", "description", "body"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: Value) -> ToolResult {
        let parsed: CreateArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, "skill_create", format!("invalid arguments: {e}")),
        };
        if let Err(reason) = validate_slug(&parsed.name) {
            return err(call_id, "skill_create", reason);
        }
        let path = self.cfg.skills_dir.join(format!("{}.md", parsed.name));
        if path.exists() {
            return err(
                call_id,
                "skill_create",
                format!("skill `{}` already exists; use skill_update to modify it", parsed.name),
            );
        }
        if let Err(e) = ensure_under(&self.cfg.skills_dir, &path) {
            return err(call_id, "skill_create", e);
        }
        let content = render_skill_file(&parsed.name, &parsed.description, &parsed.body);
        if let Err(e) = fs::create_dir_all(&self.cfg.skills_dir).await {
            return err(call_id, "skill_create", format!("mkdir: {e}"));
        }
        if let Err(e) = fs::write(&path, content).await {
            return err(call_id, "skill_create", format!("write {}: {e}", path.display()));
        }
        ok(call_id, "skill_create", format!("created skill `{}` at {}", parsed.name, path.display()))
    }
}

#[async_trait]
impl Tool for SkillUpdate {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "skill_update".into(),
            description:
                "Update an existing skill's body (and optionally its description). \
                 The skill must already exist. Use this to refine a skill based on \
                 lessons learned while using it."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "description": { "type": "string", "description": "if omitted, the existing description is preserved" },
                    "body": { "type": "string", "description": "new markdown body (replaces the old)" }
                },
                "required": ["name", "body"]
            }),
        }
    }

    async fn call(&self, call_id: &str, args: Value) -> ToolResult {
        let parsed: UpdateArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return err(call_id, "skill_update", format!("invalid arguments: {e}")),
        };
        if let Err(reason) = validate_slug(&parsed.name) {
            return err(call_id, "skill_update", reason);
        }
        let path = self.cfg.skills_dir.join(format!("{}.md", parsed.name));
        if !path.exists() {
            return err(
                call_id,
                "skill_update",
                format!("skill `{}` does not exist; use skill_create instead", parsed.name),
            );
        }
        if let Err(e) = ensure_under(&self.cfg.skills_dir, &path) {
            return err(call_id, "skill_update", e);
        }
        let existing = match fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) => return err(call_id, "skill_update", format!("read {}: {e}", path.display())),
        };
        let preserved_description = parsed
            .description
            .or_else(|| extract_description(&existing))
            .unwrap_or_else(|| String::from("(no description)"));
        let content = render_skill_file(&parsed.name, &preserved_description, &parsed.body);
        if let Err(e) = fs::write(&path, content).await {
            return err(call_id, "skill_update", format!("write {}: {e}", path.display()));
        }
        ok(call_id, "skill_update", format!("updated skill `{}` at {}", parsed.name, path.display()))
    }
}

fn validate_slug(s: &str) -> Result<(), String> {
    if s.is_empty()
        || !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || !s.chars().next().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false)
    {
        return Err(format!(
            "invalid skill name `{s}`: must match `^[a-z0-9][a-z0-9-]*$`"
        ));
    }
    Ok(())
}

/// Ensure `candidate` is inside `root` after canonicalizing the parts that
/// exist. Protects against `name = "../etc/passwd"` shenanigans.
fn ensure_under(root: &Path, candidate: &Path) -> Result<(), String> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let parent =
        candidate.parent().map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf())).ok_or_else(|| "no parent dir".to_string())?;
    if !parent.starts_with(&root) {
        return Err(format!(
            "refusing to write outside skills dir: {} not under {}",
            parent.display(),
            root.display()
        ));
    }
    Ok(())
}

fn render_skill_file(name: &str, description: &str, body: &str) -> String {
    let trimmed_body = body.trim_end_matches('\n');
    format!("---\nname: {name}\ndescription: {description}\n---\n\n{trimmed_body}\n")
}

fn extract_description(file: &str) -> Option<String> {
    let mut lines = file.lines();
    if lines.next()? != "---" {
        return None;
    }
    for line in lines {
        if line == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix("description:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

fn ok(call_id: &str, name: &str, content: String) -> ToolResult {
    ToolResult { tool_call_id: call_id.into(), name: name.into(), content, is_error: false }
}

fn err(call_id: &str, name: &str, msg: String) -> ToolResult {
    ToolResult { tool_call_id: call_id.into(), name: name.into(), content: msg, is_error: true }
}
