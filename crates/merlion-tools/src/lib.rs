//! Built-in tools shipped with Merlion Agent.

pub mod bash;
pub mod edit;
pub mod ls;
pub mod read;
pub mod write;

use merlion_core::ToolRegistry;

/// Register the default set of tools. Mirrors the `_HERMES_CORE_TOOLS` list
/// from hermes's `toolsets.py` but limited to the five tools currently
/// implemented in the MVP port.
pub fn register_defaults(reg: &mut ToolRegistry) {
    reg.register(bash::Bash::default());
    reg.register(read::Read::default());
    reg.register(write::Write::default());
    reg.register(edit::Edit::default());
    reg.register(ls::Ls::default());
}
