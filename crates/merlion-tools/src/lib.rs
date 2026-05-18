//! Built-in tools shipped with Merlion Agent.

pub mod bash;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod ls;
pub mod memory;
pub mod read;
pub mod skill_tools;
pub mod web_fetch;
pub mod write;

use std::sync::Arc;

use merlion_core::ToolRegistry;
use merlion_memory::MemoryStore;

/// Register tools that don't need any runtime configuration.
pub fn register_defaults(reg: &mut ToolRegistry) {
    reg.register(bash::Bash::default());
    reg.register(read::Read::default());
    reg.register(write::Write::default());
    reg.register(edit::Edit::default());
    reg.register(ls::Ls::default());
    reg.register(grep::Grep::default());
    reg.register(glob::Glob::default());
    reg.register(web_fetch::WebFetch::default());
}

/// Register the `memory` tool against a specific store.
pub fn register_memory(reg: &mut ToolRegistry, store: Arc<MemoryStore>) {
    reg.register(memory::MemoryTool::new(store));
}

/// Register `skill_create` and `skill_update` against a specific skills dir.
pub fn register_skill_tools(reg: &mut ToolRegistry, cfg: Arc<skill_tools::SkillToolsConfig>) {
    reg.register(skill_tools::SkillCreate::new(cfg.clone()));
    reg.register(skill_tools::SkillUpdate::new(cfg));
}
