//! `merlion tools` — inspect the tools merlion would hand to the model.
//!
//! Lists tool name + schema description, grouped by which registration
//! helper exposes them. MCP-server-provided tools are intentionally
//! omitted because building those requires live network/stdio
//! connections, and this command is a read-only inspector.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Subcommand;
use merlion_core::ToolRegistry;
use merlion_memory::MemoryStore;
use merlion_tools::skill_tools::SkillToolsConfig;

#[derive(Debug, Subcommand)]
pub enum ToolsAction {
    /// List every built-in tool merlion would register at chat start,
    /// grouped by registration helper.
    List,
}

pub async fn run(action: ToolsAction) -> Result<()> {
    match action {
        ToolsAction::List => list().await,
    }
}

async fn list() -> Result<()> {
    let sections = build_sections().context("constructing tool registries")?;
    for (heading, names) in sections {
        println!("# {heading}");
        if names.is_empty() {
            println!("  (none)");
        } else {
            let width = names.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
            for (name, desc) in names {
                let first_line = desc.lines().next().unwrap_or("").trim();
                println!("  {name:<width$}  {first_line}", name = name, width = width);
            }
        }
        println!();
    }
    Ok(())
}

/// Build the (section heading, tools-in-that-section) list. Each section is
/// constructed by populating a *fresh* registry with one helper so that
/// `register_defaults` tools don't show up under "Memory", etc. We use a
/// throwaway tempdir for the helpers that need on-disk state — nothing is
/// ever written into it.
/// (tool_name, description) pair.
type ToolEntry = (String, String);
/// One labeled section: heading + the tools in it.
type Section = (&'static str, Vec<ToolEntry>);

fn build_sections() -> Result<Vec<Section>> {
    let scratch = tempfile::tempdir().context("create scratch tempdir for tool inspection")?;

    let core = {
        let mut reg = ToolRegistry::new();
        merlion_tools::register_defaults(&mut reg);
        schemas(&reg)
    };

    let memory = {
        let mut reg = ToolRegistry::new();
        let store = Arc::new(
            MemoryStore::open(scratch.path().join("memory"))
                .context("open scratch memory store")?,
        );
        merlion_tools::register_memory(&mut reg, store);
        schemas(&reg)
    };

    let skills = {
        let mut reg = ToolRegistry::new();
        let cfg = SkillToolsConfig::new(scratch.path().join("skills"));
        merlion_tools::register_skill_tools(&mut reg, cfg);
        schemas(&reg)
    };

    let sandbox = {
        let mut reg = ToolRegistry::new();
        merlion_tools::register_sandbox_bash(&mut reg);
        schemas(&reg)
    };

    let subagent = {
        let mut reg = ToolRegistry::new();
        let _handle = merlion_tools::register_task_tool(&mut reg);
        schemas(&reg)
    };

    Ok(vec![
        ("Core", core),
        ("Memory", memory),
        ("Skills", skills),
        ("Sandbox", sandbox),
        ("Subagent", subagent),
    ])
}

fn schemas(reg: &ToolRegistry) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = reg
        .schemas()
        .into_iter()
        .map(|s| (s.name, s.description))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_sections_covers_every_helper_and_returns_nonempty_for_each() {
        let sections = build_sections().expect("build_sections");
        let names: Vec<&'static str> = sections.iter().map(|(h, _)| *h).collect();
        assert_eq!(
            names,
            vec!["Core", "Memory", "Skills", "Sandbox", "Subagent"]
        );
        for (heading, tools) in &sections {
            assert!(
                !tools.is_empty(),
                "section `{heading}` should have at least one tool",
            );
            for (name, desc) in tools {
                assert!(!name.is_empty(), "tool in `{heading}` has empty name");
                assert!(
                    !desc.is_empty(),
                    "tool `{name}` in `{heading}` has empty description",
                );
            }
        }
    }

    #[test]
    fn core_section_contains_canonical_tools() {
        let sections = build_sections().expect("build_sections");
        let core: &Vec<(String, String)> = &sections
            .iter()
            .find(|(h, _)| *h == "Core")
            .expect("Core section")
            .1;
        let names: Vec<&str> = core.iter().map(|(n, _)| n.as_str()).collect();
        for required in ["bash", "read", "write", "edit", "ls", "grep", "glob"] {
            assert!(
                names.contains(&required),
                "Core section missing `{required}`; got {names:?}",
            );
        }
    }
}

// -----------------------------------------------------------------------------
// Wiring spec — apply to `crates/merlion-cli/src/main.rs`.
//
// 1. Add a module declaration near the other `mod` lines at the top:
//
//        mod tools_cmd;
//
// 2. Add a new variant to the `Command` enum:
//
//        /// Inspect the built-in tools merlion exposes to the model.
//        Tools {
//            #[command(subcommand)]
//            action: tools_cmd::ToolsAction,
//        },
//
//    `ToolsAction` already derives `clap::Subcommand` in this file, so no
//    extra clap derives are needed in `main.rs`.
//
// 3. Add a dispatch arm in the `match cli.command.unwrap_or(...)` block in
//    `main`:
//
//        Command::Tools { action } => tools_cmd::run(action).await,
//
// -----------------------------------------------------------------------------
