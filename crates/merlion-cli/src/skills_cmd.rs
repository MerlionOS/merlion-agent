//! `merlion skills` — inspect and manage the skill library.
//!
//! See the wiring spec at the bottom of this file for how this hooks into
//! `main.rs`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use merlion_skills::SkillSet;

#[derive(Debug, Subcommand)]
pub enum SkillsAction {
    /// List every loaded skill (bundled + user) with one-line descriptions.
    List,
    /// Print the full body of a single skill.
    Show {
        /// Slug of the skill, e.g. `code-review` (no leading slash).
        name: String,
    },
    /// Delete a user skill from `~/.merlion/skills/`. Bundled skills are
    /// read-only and cannot be deleted from here.
    Delete {
        /// Slug of the skill to delete.
        name: String,
    },
}

pub async fn run(action: SkillsAction) -> Result<()> {
    match action {
        SkillsAction::List => list(),
        SkillsAction::Show { name } => show(&name),
        SkillsAction::Delete { name } => delete(&name),
    }
}

fn list() -> Result<()> {
    let skills = SkillSet::load_default().context("loading skills")?;
    let index = skills.help_index();
    print!("{index}");
    if !index.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn show(name: &str) -> Result<()> {
    let skills = SkillSet::load_default().context("loading skills")?;
    let skill = skills
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("no skill named `{name}` (try `merlion skills list`)"))?;
    println!("# {} — {}", skill.name, skill.description);
    println!("source: {}", skill.source_path.display());
    println!();
    print!("{}", skill.body);
    if !skill.body.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn delete(name: &str) -> Result<()> {
    let user_dir = merlion_config::merlion_home().join("skills");
    let skills = SkillSet::load_default().context("loading skills")?;
    let skill = skills
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("no skill named `{name}` (try `merlion skills list`)"))?;

    if !is_under(&skill.source_path, &user_dir) {
        anyhow::bail!(
            "refusing to delete bundled skill `{name}` (loaded from {}). Only skills under {} can be deleted via this command.",
            skill.source_path.display(),
            user_dir.display(),
        );
    }

    // For a directory-style skill (`<user_dir>/<name>/SKILL.md`), remove the
    // whole skill directory; for a flat skill (`<user_dir>/<name>.md`),
    // remove just the file.
    let target: PathBuf = skill
        .source_path
        .parent()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some(name) && is_under(p, &user_dir))
        .map(Path::to_path_buf)
        .unwrap_or_else(|| skill.source_path.clone());

    if target.is_dir() {
        std::fs::remove_dir_all(&target)
            .with_context(|| format!("removing {}", target.display()))?;
    } else {
        std::fs::remove_file(&target).with_context(|| format!("removing {}", target.display()))?;
    }
    println!("deleted `{name}` ({})", target.display());
    Ok(())
}

/// True when `path` is the same as `root` or lives anywhere underneath it.
/// Falls back to a lexical comparison when the filesystem can't canonicalize
/// either path (e.g. the target was just deleted in a parallel session).
fn is_under(path: &Path, root: &Path) -> bool {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let r = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    p.starts_with(&r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_under_matches_exact_and_nested_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let nested = root.join("a").join("b.md");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "x").unwrap();
        assert!(is_under(&nested, root));
        assert!(is_under(root, root));
    }

    #[test]
    fn is_under_rejects_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("a");
        let sibling = tmp.path().join("b").join("c.md");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(sibling.parent().unwrap()).unwrap();
        std::fs::write(&sibling, "x").unwrap();
        assert!(!is_under(&sibling, &root));
    }
}

// -----------------------------------------------------------------------------
// Wiring spec — apply to `crates/merlion-cli/src/main.rs`.
//
// 1. Add a module declaration near the other `mod` lines at the top:
//
//        mod skills_cmd;
//
// 2. Add a new variant to the `Command` enum:
//
//        /// Manage Merlion skills (list / show / delete).
//        Skills {
//            #[command(subcommand)]
//            action: skills_cmd::SkillsAction,
//        },
//
//    `SkillsAction` already derives `clap::Subcommand` in this file, so no
//    extra clap derives are needed in `main.rs`.
//
// 3. Add a dispatch arm in the `match cli.command.unwrap_or(...)` block in
//    `main`:
//
//        Command::Skills { action } => skills_cmd::run(action).await,
//
// -----------------------------------------------------------------------------
