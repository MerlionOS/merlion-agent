use clap::CommandFactory;
use clap_complete::Shell;

pub fn emit<C: CommandFactory>(shell: Shell, bin_name: &str, out: &mut impl std::io::Write) {
    let mut cmd = C::command();
    clap_complete::generate(shell, &mut cmd, bin_name, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    #[command(name = "merlion-test")]
    #[allow(dead_code)]
    struct DummyCli {
        #[arg(long)]
        flag: bool,
    }

    #[test]
    fn emits_bash_completion_containing_bin_name() {
        let mut out: Vec<u8> = Vec::new();
        emit::<DummyCli>(Shell::Bash, "merlion", &mut out);
        let text = String::from_utf8(out).expect("bash completion is utf8");
        assert!(
            text.contains("merlion"),
            "bash completion should mention bin name `merlion`, got:\n{text}"
        );
    }

    #[test]
    fn emits_zsh_completion_containing_bin_name() {
        let mut out: Vec<u8> = Vec::new();
        emit::<DummyCli>(Shell::Zsh, "merlion", &mut out);
        let text = String::from_utf8(out).expect("zsh completion is utf8");
        assert!(
            text.contains("merlion"),
            "zsh completion should mention bin name `merlion`, got:\n{text}"
        );
    }
}

// =============================================================================
// WIRING SPEC — parent agent: apply these edits to `crates/merlion-cli/src/main.rs`.
// =============================================================================
//
// 1. MODULE DECLARATION
//    Add this alongside the existing `mod approver;` / `mod tui;` lines near
//    the top of `main.rs`:
//
//        mod completion;
//
// 2. NEW `use` STATEMENTS
//    Add to the top-of-file imports (next to the other `use clap::...` line):
//
//        use clap_complete::Shell;
//
//    No other new imports are required — `completion::emit` is referenced via
//    its module path and `Cli` is already in scope.
//
// 3. NEW `Command` ENUM VARIANT
//    Insert this variant into the `Command` enum (alongside `Update`, etc.):
//
//        /// Print a shell-completion script to stdout.
//        /// Example: `merlion completion bash > /etc/bash_completion.d/merlion`
//        Completion {
//            /// Target shell: bash, zsh, fish, powershell, or elvish.
//            shell: Shell,
//        },
//
// 4. DISPATCH ARM
//    Add this arm to the `match cli.command.unwrap_or(...)` block in `main`,
//    alongside the existing `Command::Update { apply } => ...` arm:
//
//        Command::Completion { shell } => {
//            completion::emit::<Cli>(shell, "merlion", &mut std::io::stdout());
//            Ok(())
//        }
//
// 5. NO OTHER CHANGES
//    Do NOT touch the `Cli::parse()` call, the tokio runtime, or any other
//    subcommand. The new variant is self-contained and synchronous.
// =============================================================================
