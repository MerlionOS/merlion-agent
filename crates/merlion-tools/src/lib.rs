//! Built-in tools shipped with Merlion Agent.

pub mod bash;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod ls;
pub mod read;
pub mod web_fetch;
pub mod write;

use merlion_core::ToolRegistry;

/// Register the default tool set. Mirrors hermes's `_HERMES_CORE_TOOLS`.
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
