//! `merlion uninstall` — remove Merlion Agent from the system.
//!
//! Stops + uninstalls the launchd / systemd gateway daemon, deletes
//! `~/.merlion/`, and (best-effort) tells the user how to remove the
//! installed binary. Three escape hatches:
//!
//! - `--yes` — skip the interactive confirmation prompt.
//! - `--keep-data` — leave `~/.merlion/` in place (useful before a
//!   reinstall when you want to preserve config + sessions).
//! - `--keep-binary` — don't print binary-removal instructions.
//!
//! The running binary deliberately does *not* try to delete itself: a
//! process unlinking its own executable is a footgun (works on Unix, fails
//! oddly elsewhere, and can race with package managers). Instead we
//! classify the path and print the appropriate `brew uninstall` /
//! `cargo uninstall` / "delete manually" line for the user to run.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::Args;
use dialoguer::console::style;
use dialoguer::{theme::ColorfulTheme, Confirm};

use crate::gateway_service;

/// `merlion uninstall` arguments. Three independent "keep X" flags let the
/// user opt out of any subset of the uninstall steps; `--yes` skips the
/// final confirmation prompt for scripted use.
#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Skip the "are you sure?" prompt. Required when stdin isn't a TTY.
    #[arg(long)]
    pub yes: bool,
    /// Don't delete `~/.merlion/`. Useful when you plan to reinstall and
    /// want to preserve config, sessions, memories, and skills.
    #[arg(long)]
    pub keep_data: bool,
    /// Don't print instructions for removing the installed binary.
    #[arg(long)]
    pub keep_binary: bool,
}

/// How the running binary was installed — drives which removal command we
/// suggest. Extracted as an enum (rather than just a string) so the pure
/// classification logic can be unit-tested without touching the
/// filesystem or shelling out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryRemoval {
    /// Installed via `cargo install` into `~/.cargo/bin/`.
    Cargo,
    /// Installed via Homebrew under `/opt/homebrew` (Apple Silicon) or
    /// `/usr/local` (Intel).
    Brew,
    /// Anywhere else — we can't infer the package manager, so the user
    /// has to delete it themselves.
    Manual,
}

/// Classify `exe_path` into one of the [`BinaryRemoval`] strategies.
///
/// `cargo_bin_dir` is the user's `~/.cargo/bin` (passed in rather than
/// resolved internally so tests can pin it to a fixture). The classification
/// is purely prefix-based — we don't stat anything.
pub fn classify_binary_path(exe_path: &Path, cargo_bin_dir: Option<&Path>) -> BinaryRemoval {
    if let Some(cargo_bin) = cargo_bin_dir {
        if exe_path.starts_with(cargo_bin) {
            return BinaryRemoval::Cargo;
        }
    }
    if exe_path.starts_with("/opt/homebrew/") || exe_path.starts_with("/usr/local/") {
        return BinaryRemoval::Brew;
    }
    BinaryRemoval::Manual
}

/// Run the uninstall flow. See [`UninstallArgs`] for option semantics.
pub async fn run(args: UninstallArgs) -> Result<()> {
    let home = merlion_config::merlion_home();
    let service_state = gateway_service::service_status().ok();
    let service_installed = matches!(
        service_state.as_ref(),
        Some(gateway_service::ServiceState::Stopped { .. })
            | Some(gateway_service::ServiceState::Running { .. })
            | Some(gateway_service::ServiceState::Unknown { .. })
    );

    print_plan(&home, service_installed, &args);

    if !args.yes {
        if !std::io::stdin().is_terminal() {
            return Err(anyhow!(
                "`merlion uninstall` needs a TTY for confirmation.\n\
                 Re-run with --yes to skip the prompt in non-interactive contexts."
            ));
        }
        let theme = ColorfulTheme::default();
        let ok = Confirm::with_theme(&theme)
            .with_prompt("Proceed with uninstall?")
            .default(false)
            .interact()
            .context("reading confirmation")?;
        if !ok {
            println!();
            println!("{}", style("Aborted — nothing was changed.").dim());
            return Ok(());
        }
    }

    println!();
    section_header("Stopping gateway service");
    if service_installed {
        match gateway_service::uninstall() {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(error = %e, "gateway uninstall failed; continuing");
                println!(
                    "  {} {}",
                    style("!").yellow().bold(),
                    style(format!("gateway uninstall failed: {e}")).dim()
                );
            }
        }
    } else {
        println!("  {}", style("(no gateway service installed)").dim());
    }

    println!();
    section_header("Removing data directory");
    if args.keep_data {
        println!(
            "  {} {}",
            style("·").dim(),
            style(format!(
                "--keep-data set; leaving {} in place",
                home.display()
            ))
            .dim()
        );
    } else {
        remove_home(&home)?;
    }

    println!();
    section_header("Removing binary");
    if args.keep_binary {
        println!(
            "  {} {}",
            style("·").dim(),
            style("--keep-binary set; leaving the installed binary alone").dim()
        );
    } else {
        report_binary_removal()?;
    }

    println!();
    println!("{}", style("Uninstall complete.").green().bold());
    println!(
        "  {} {}",
        style("Reinstall with:").dim(),
        style("brew install MerlionOS/merlion/merlion-agent").bold()
    );
    Ok(())
}

fn print_plan(home: &Path, service_installed: bool, args: &UninstallArgs) {
    println!();
    section_header("Merlion uninstall plan");
    println!(
        "  {}",
        style("The following actions will be performed:").dim()
    );
    println!();

    if service_installed {
        println!(
            "  {} stop and unregister the gateway daemon (launchd / systemd)",
            style("•").cyan()
        );
    } else {
        println!(
            "  {} {}",
            style("·").dim(),
            style("no gateway daemon installed — nothing to stop").dim()
        );
    }

    if args.keep_data {
        println!(
            "  {} {}",
            style("·").dim(),
            style(format!("--keep-data: leave {} in place", home.display())).dim()
        );
    } else {
        println!(
            "  {} delete {} (config, sessions, memories, skills)",
            style("•").cyan(),
            style(home.display().to_string()).bold()
        );
    }

    if args.keep_binary {
        println!(
            "  {} {}",
            style("·").dim(),
            style("--keep-binary: skip binary removal instructions").dim()
        );
    } else {
        println!(
            "  {} print instructions for removing the installed `merlion` binary",
            style("•").cyan()
        );
    }
    println!();
}

fn section_header(title: &str) {
    println!(
        "{} {}",
        style("◆").cyan().bold(),
        style(title).cyan().bold()
    );
}

fn remove_home(home: &Path) -> Result<()> {
    if !home.exists() {
        println!("  {}", style("(no ~/.merlion to remove)").dim());
        return Ok(());
    }
    std::fs::remove_dir_all(home).with_context(|| format!("removing {}", home.display()))?;
    println!(
        "  {} {} {}",
        style("✓").green().bold(),
        style("removed").dim(),
        style(home.display().to_string()).bold()
    );
    Ok(())
}

fn report_binary_removal() -> Result<()> {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "failed to resolve current_exe");
            println!(
                "  {} {}",
                style("!").yellow().bold(),
                style(format!("could not resolve current executable: {e}")).dim()
            );
            return Ok(());
        }
    };
    let cargo_bin = cargo_bin_dir();
    let strategy = classify_binary_path(&exe, cargo_bin.as_deref());
    match strategy {
        BinaryRemoval::Cargo => {
            println!(
                "  {} the running binary is at {}",
                style("·").dim(),
                style(exe.display().to_string()).bold()
            );
            println!(
                "  {} {}",
                style("→ run:").dim(),
                style("cargo uninstall merlion-agent").bold()
            );
        }
        BinaryRemoval::Brew => {
            println!(
                "  {} the running binary is at {}",
                style("·").dim(),
                style(exe.display().to_string()).bold()
            );
            println!(
                "  {} {}",
                style("→ run:").dim(),
                style("brew uninstall merlion-agent").bold()
            );
        }
        BinaryRemoval::Manual => {
            println!(
                "  {} the running binary is at {}",
                style("·").dim(),
                style(exe.display().to_string()).bold()
            );
            println!("  {} {}", style("→").dim(), style("delete manually").bold());
        }
    }
    Ok(())
}

/// Best-effort lookup for `~/.cargo/bin`. Respects `CARGO_HOME` when set,
/// falls back to `$HOME/.cargo/bin`. Returns `None` when we can't even
/// figure out a home directory — in that case `classify_binary_path` skips
/// the cargo check entirely.
fn cargo_bin_dir() -> Option<PathBuf> {
    if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        if !cargo_home.is_empty() {
            return Some(PathBuf::from(cargo_home).join("bin"));
        }
    }
    dirs::home_dir().map(|h| h.join(".cargo").join("bin"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn classify_recognises_cargo_bin() {
        let cargo_bin = PathBuf::from("/home/alice/.cargo/bin");
        let exe = PathBuf::from("/home/alice/.cargo/bin/merlion");
        assert_eq!(
            classify_binary_path(&exe, Some(&cargo_bin)),
            BinaryRemoval::Cargo
        );
    }

    #[test]
    fn classify_recognises_apple_silicon_brew() {
        let exe = PathBuf::from("/opt/homebrew/bin/merlion");
        assert_eq!(classify_binary_path(&exe, None), BinaryRemoval::Brew);
    }

    #[test]
    fn classify_recognises_intel_brew() {
        let exe = PathBuf::from("/usr/local/bin/merlion");
        assert_eq!(classify_binary_path(&exe, None), BinaryRemoval::Brew);
    }

    #[test]
    fn classify_falls_back_to_manual() {
        let exe = PathBuf::from("/somewhere/exotic/merlion");
        assert_eq!(classify_binary_path(&exe, None), BinaryRemoval::Manual);
    }

    #[test]
    fn classify_prefers_cargo_over_brew_when_both_match() {
        // Pathological setup: cargo_bin lives under /usr/local. The cargo
        // check runs first so we should still suggest `cargo uninstall`.
        let cargo_bin = PathBuf::from("/usr/local/cargo/bin");
        let exe = PathBuf::from("/usr/local/cargo/bin/merlion");
        assert_eq!(
            classify_binary_path(&exe, Some(&cargo_bin)),
            BinaryRemoval::Cargo
        );
    }

    #[test]
    fn classify_ignores_unrelated_cargo_bin() {
        let cargo_bin = PathBuf::from("/home/alice/.cargo/bin");
        let exe = PathBuf::from("/opt/homebrew/bin/merlion");
        assert_eq!(
            classify_binary_path(&exe, Some(&cargo_bin)),
            BinaryRemoval::Brew
        );
    }

    #[test]
    fn classify_target_debug_is_manual() {
        // Running from `cargo run` inside the workspace — the dev binary
        // lives under `target/debug/`. No package manager owns it.
        let cargo_bin = PathBuf::from("/home/alice/.cargo/bin");
        let exe = PathBuf::from("/home/alice/code/merlion-agent/target/debug/merlion");
        assert_eq!(
            classify_binary_path(&exe, Some(&cargo_bin)),
            BinaryRemoval::Manual
        );
    }
}

// -----------------------------------------------------------------------------
// WIRING SPEC — apply to `crates/merlion-cli/src/main.rs`.
//
// 1. Add a module declaration near the other `mod` lines at the top of main.rs:
//
//        mod uninstall_cmd;
//
// 2. Add a new variant to the `Command` enum:
//
//        /// Remove Merlion Agent from the system.
//        ///
//        /// Stops and unregisters the gateway daemon (if installed), deletes
//        /// `~/.merlion/`, and prints the appropriate command to remove the
//        /// installed binary (`brew uninstall` / `cargo uninstall` /
//        /// "delete manually") based on where the running binary lives.
//        ///
//        /// Use `--keep-data` to preserve config + sessions for a later
//        /// reinstall, `--keep-binary` to skip binary-removal instructions,
//        /// and `--yes` to skip the interactive confirmation prompt.
//        Uninstall(uninstall_cmd::UninstallArgs),
//
// 3. Add a dispatch arm in the `match cli.command.unwrap_or(...)` block in
//    `main()`:
//
//        Command::Uninstall(args) => uninstall_cmd::run(args).await,
//
// 4. No new `use` lines are required at the top of `main.rs` — the args
//    struct is referenced through the `uninstall_cmd::` module path, and
//    `run` returns `anyhow::Result<()>` like the other async dispatch arms.
// -----------------------------------------------------------------------------
