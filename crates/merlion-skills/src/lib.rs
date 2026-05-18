//! Skill discovery + parsing for Merlion Agent.
//!
//! A "skill" is a small markdown document the user can invoke with
//! `/<slug>` in chat to inject specialized instructions into the
//! conversation. The on-disk format matches [agentskills.io] so skills
//! are portable across agents.
//!
//! [agentskills.io]: https://agentskills.io
//!
//! # Layout
//!
//! Two layouts are supported per skills root:
//!
//! 1. **Directory skill:** `<root>/<slug>/SKILL.md`
//! 2. **Flat skill:** `<root>/<slug>.md`
//!
//! Each file starts with YAML front-matter delimited by `---` lines and
//! is followed by a markdown body which is passed verbatim to the model
//! when the skill is invoked.
//!
//! # Layering
//!
//! [`SkillSet::load`] takes one or more roots. Later roots override
//! earlier ones by slug, so bundled skills can be shadowed by the
//! user's personal skills in `~/.merlion/skills/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

mod loader;
mod parse;

/// A single skill parsed from disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Slug derived from the filename or directory name. Always matches
    /// `^[a-z0-9][a-z0-9-]*$`.
    pub name: String,
    /// One-line summary from the front-matter, shown in `/help skills`.
    pub description: String,
    /// Markdown body following the front-matter, passed verbatim to the
    /// model when the skill is invoked.
    pub body: String,
    /// Path the skill was loaded from. Useful for error messages and
    /// hot-reload.
    pub source_path: PathBuf,
}

/// A collection of skills keyed by slug. Construct via [`SkillSet::load`]
/// or [`SkillSet::load_default`].
#[derive(Debug, Clone, Default)]
pub struct SkillSet {
    skills: BTreeMap<String, Skill>,
}

impl SkillSet {
    /// Walk one or more roots and load every skill found. Later roots
    /// override earlier ones by name, so user skills shadow bundled
    /// ones.
    ///
    /// Missing roots are silently skipped — only an empty roots list
    /// or a genuinely broken filesystem produces an error.
    pub fn load(roots: &[impl AsRef<Path>]) -> Result<Self> {
        let mut skills: BTreeMap<String, Skill> = BTreeMap::new();

        for root in roots {
            let root = root.as_ref();
            for candidate in loader::discover(root) {
                if !loader::is_valid_slug(&candidate.slug) {
                    tracing::warn!(
                        skill.slug = %candidate.slug,
                        source = %candidate.path.display(),
                        "skipping skill: slug does not match ^[a-z0-9][a-z0-9-]*$",
                    );
                    continue;
                }

                let raw = match std::fs::read_to_string(&candidate.path) {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::warn!(
                            source = %candidate.path.display(),
                            error = %err,
                            "skipping skill: failed to read file",
                        );
                        continue;
                    }
                };

                match parse::parse_skill(&candidate.slug, &candidate.path, &raw) {
                    Ok(skill) => {
                        if let Some(prev) = skills.insert(candidate.slug.clone(), skill) {
                            tracing::debug!(
                                skill.slug = %candidate.slug,
                                replaced = %prev.source_path.display(),
                                with = %candidate.path.display(),
                                "overriding earlier skill from previous root",
                            );
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            skill.slug = %candidate.slug,
                            source = %candidate.path.display(),
                            error = %err,
                            "skipping skill: parse error",
                        );
                    }
                }
            }
        }

        Ok(SkillSet { skills })
    }

    /// Convenience: load from bundled `./skills/` + `~/.merlion/skills/`
    /// (or `$MERLION_HOME/skills/` if set). Both roots may be absent.
    pub fn load_default() -> Result<Self> {
        let mut roots: Vec<PathBuf> = Vec::new();

        // Bundled skills relative to the current working directory.
        roots.push(PathBuf::from("skills"));

        // User skills under `$MERLION_HOME` or `~/.merlion`.
        let user_home = std::env::var_os("MERLION_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".merlion")));

        if let Some(home) = user_home {
            roots.push(home.join("skills"));
        }

        Self::load(&roots)
    }

    /// Look up a skill by slug.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Slugs of all loaded skills, in deterministic (alphabetical) order.
    pub fn names(&self) -> Vec<&str> {
        self.skills.keys().map(|s| s.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Skill)> {
        self.skills.iter()
    }

    /// Render a help index for `/help skills` — one line per skill, in
    /// the form `  /<slug>  <description>`.
    pub fn help_index(&self) -> String {
        if self.skills.is_empty() {
            return "No skills loaded.".to_string();
        }

        // Align descriptions to the widest slug for readability.
        let width = self.skills.keys().map(|s| s.len()).max().unwrap_or(0);

        let mut out = String::new();
        for (slug, skill) in &self.skills {
            use std::fmt::Write;
            let _ = writeln!(
                out,
                "  /{slug:<width$}  {desc}",
                slug = slug,
                width = width,
                desc = skill.description,
            );
        }
        // Drop the trailing newline so callers can append cleanly.
        if out.ends_with('\n') {
            out.pop();
        }
        out
    }
}
