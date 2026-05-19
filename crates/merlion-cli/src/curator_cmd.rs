//! `merlion curator` — inspect and control the memory-curator nudge loop.
//!
//! The curator state lives in `~/.merlion/curator.yaml` as a small file:
//!
//! ```yaml
//! paused: true
//! ```
//!
//! For now only `Pause`/`Resume` touch that file. The chat loop in
//! `main.rs::chat` currently always runs the curator unconditionally — wiring
//! it to honor `curator.yaml` (skip `record_user_turn` / `nudge_if_due` when
//! `paused: true`) is **follow-up work** and not part of this change.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use merlion_core::Curator;
use serde::{Deserialize, Serialize};

#[derive(Debug, Subcommand)]
pub enum CuratorAction {
    /// Print the current interval and whether the curator is paused.
    Status,
    /// Pause the curator. Writes `paused: true` to `~/.merlion/curator.yaml`.
    /// The chat loop is expected to consult this flag (follow-up work).
    Pause,
    /// Resume the curator (deletes the pause flag).
    Resume,
    /// Emit the nudge text to stdout — useful for previewing what the model
    /// would see in a `<system-reminder>` block.
    RunNow,
}

pub async fn run(action: CuratorAction) -> Result<()> {
    match action {
        CuratorAction::Status => status(),
        CuratorAction::Pause => pause(),
        CuratorAction::Resume => resume(),
        CuratorAction::RunNow => run_now(),
    }
}

/// On-disk representation of the curator's persistent state. Tiny on purpose
/// — anything more elaborate belongs in `config.yaml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CuratorState {
    #[serde(default)]
    paused: bool,
}

fn state_path() -> PathBuf {
    merlion_config::merlion_home().join("curator.yaml")
}

fn load_state(path: &Path) -> Result<CuratorState> {
    if !path.exists() {
        return Ok(CuratorState::default());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let parsed: CuratorState =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    Ok(parsed)
}

fn save_state(path: &Path, state: &CuratorState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let yaml = serde_yaml::to_string(state).context("serialize curator state")?;
    std::fs::write(path, yaml).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn status() -> Result<()> {
    let path = state_path();
    let state = load_state(&path)?;
    let curator = Curator::default();
    let interval = curator.interval();
    println!("interval:     {interval} user turns");
    let next = if interval == 0 {
        "(disabled)".to_string()
    } else {
        format!("after {interval} more user turns (counter resets on every nudge)")
    };
    println!("next nudge:   {next}");
    println!("paused:       {}", if state.paused { "yes" } else { "no" });
    println!("state file:   {}", path.display());
    Ok(())
}

fn pause() -> Result<()> {
    let path = state_path();
    let mut state = load_state(&path)?;
    state.paused = true;
    save_state(&path, &state)?;
    println!("curator paused ({})", path.display());
    println!("note: the chat loop does not yet honor this flag — wiring it up is follow-up work.");
    Ok(())
}

fn resume() -> Result<()> {
    let path = state_path();
    let mut state = load_state(&path)?;
    state.paused = false;
    save_state(&path, &state)?;
    println!("curator resumed ({})", path.display());
    Ok(())
}

fn run_now() -> Result<()> {
    // Force a nudge by giving a Curator with interval 1 a single user turn.
    let mut c = Curator::new(1);
    c.record_user_turn();
    let nudge = c
        .nudge_if_due()
        .expect("Curator::new(1) should always nudge after one turn");
    println!("{nudge}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_state_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("curator.yaml");

        // Missing file → default state.
        let loaded = load_state(&path).unwrap();
        assert!(!loaded.paused);

        // Persist `paused: true`.
        save_state(&path, &CuratorState { paused: true }).unwrap();
        let loaded = load_state(&path).unwrap();
        assert!(loaded.paused);

        // Persist `paused: false`.
        save_state(&path, &CuratorState { paused: false }).unwrap();
        let loaded = load_state(&path).unwrap();
        assert!(!loaded.paused);
    }

    #[test]
    fn state_path_lives_under_merlion_home() {
        let path = state_path();
        assert!(path.ends_with("curator.yaml"));
        let home = merlion_config::merlion_home();
        assert!(path.starts_with(home));
    }
}

// -----------------------------------------------------------------------------
// Wiring spec — apply to `crates/merlion-cli/src/main.rs`.
//
// 1. Add a module declaration near the other `mod` lines at the top:
//
//        mod curator_cmd;
//
// 2. Add a new variant to the `Command` enum:
//
//        /// Inspect or control the memory-curator nudge loop.
//        Curator {
//            #[command(subcommand)]
//            action: curator_cmd::CuratorAction,
//        },
//
//    `CuratorAction` already derives `clap::Subcommand` in this file, so no
//    extra clap derives are needed in `main.rs`.
//
// 3. Add a dispatch arm in the `match cli.command.unwrap_or(...)` block in
//    `main`:
//
//        Command::Curator { action } => curator_cmd::run(action).await,
//
// 4. Follow-up (NOT part of this change): in `chat()` the user-turn loop
//    currently always calls `curator.record_user_turn()` /
//    `curator.nudge_if_due()`. To honor `curator.yaml`, read it once at the
//    top of `chat()` and skip those two calls when `paused: true`. The TUI
//    path needs the same treatment.
//
// -----------------------------------------------------------------------------
