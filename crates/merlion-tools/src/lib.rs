//! Built-in tools shipped with Merlion Agent.

pub mod bash;
pub mod bash_docker;
pub mod bash_ssh;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod ls;
pub mod memory;
pub mod read;
pub mod skill_tools;
pub mod task;
pub mod web_fetch;
pub mod web_search;
pub mod write;

use std::sync::Arc;

use merlion_core::ToolRegistry;
use merlion_memory::MemoryStore;

/// Register tools that don't need any runtime configuration.
pub fn register_defaults(reg: &mut ToolRegistry) {
    reg.register(bash::Bash);
    reg.register(read::Read);
    reg.register(write::Write);
    reg.register(edit::Edit);
    reg.register(ls::Ls);
    reg.register(grep::Grep);
    reg.register(glob::Glob);
    reg.register(web_fetch::WebFetch);
    reg.register(web_search::WebSearch);
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

/// Register the sandboxed bash variants (`bash_docker`, `bash_ssh`). Opt-in
/// — most users want the host `bash` tool only. Sandbox tools are useful
/// when the agent is allowed to run shell commands but shouldn't touch the
/// host filesystem directly.
pub fn register_sandbox_bash(reg: &mut ToolRegistry) {
    reg.register(bash_docker::BashDocker);
    reg.register(bash_ssh::BashSsh);
}

/// Register the `task` tool, which spawns sub-agents that share this
/// `Agent`'s tools and LLM but start with fresh context. The returned
/// handle must be wired up via [`task::TaskTool::install_agent`] **after**
/// the `Agent` is constructed and wrapped in an `Arc` — otherwise the tool
/// will return an error on every call.
pub fn register_task_tool(reg: &mut ToolRegistry) -> Arc<task::TaskTool> {
    let tool = task::TaskTool::new();
    reg.register_arc(tool.clone());
    tool
}
