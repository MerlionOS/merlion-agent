//! `merlion hooks` — inspect and dry-run shell-script lifecycle hooks
//! declared under `hooks:` in `~/.merlion/config.yaml`.
//!
//! The hooks themselves are not yet wired into the agent loop. This command
//! exists so users can list what they've configured and pipe a synthetic
//! JSON payload through each hook to confirm the script behaves before the
//! runtime starts invoking it.

use anyhow::{anyhow, Result};
use clap::Subcommand;
use dialoguer::console::style;
use merlion_config::{Config, Hooks};
use serde_json::json;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Debug, Subcommand)]
pub enum HooksAction {
    /// Print configured hooks grouped by lifecycle event.
    List,
    /// Dry-run a hook against synthetic input. Pipes a fake JSON
    /// payload to the hook's stdin and prints the script's stdout/stderr.
    Test {
        /// Which lifecycle: before_tool | after_tool | session_start | session_end.
        event: String,
        /// Index of the hook within the list (0-based). Default 0.
        #[arg(short, long, default_value_t = 0)]
        index: usize,
    },
}

pub async fn run(cfg: Config, action: HooksAction) -> Result<()> {
    match action {
        HooksAction::Test { event, index } => test(&cfg.hooks, &event, index).await,
        HooksAction::List => {
            list(&cfg.hooks);
            Ok(())
        }
    }
}

fn list(hooks: &Hooks) {
    println!("{}", style("── hooks ──").cyan());
    print_event("before_tool", &hooks.before_tool);
    print_event("after_tool", &hooks.after_tool);
    print_event("session_start", &hooks.session_start);
    print_event("session_end", &hooks.session_end);
}

fn print_event(name: &str, entries: &[String]) {
    if entries.is_empty() {
        println!(
            "{}: {}",
            style(name).cyan(),
            style("(none configured)").dim()
        );
        return;
    }
    println!("{}:", style(name).cyan());
    let width = entries.len().to_string().len();
    for (i, cmd) in entries.iter().enumerate() {
        println!("  [{:>width$}] {cmd}", i, width = width);
    }
}

async fn test(hooks: &Hooks, event: &str, index: usize) -> Result<()> {
    let entries = lookup_event(hooks, event)?;
    if index >= entries.len() {
        return Err(anyhow!(
            "index {index} out of range for `{event}` (configured: {})",
            entries.len()
        ));
    }
    let cmd_line = &entries[index];
    let payload = synthetic_payload(event);
    let payload_str = serde_json::to_string(&payload)?;

    println!("{} {event}[{index}]: {}", style("running").cyan(), cmd_line);
    println!("{} {payload_str}", style("stdin →").dim());

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd_line)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("failed to spawn hook `{cmd_line}`: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(payload_str.as_bytes()).await?;
        stdin.shutdown().await?;
    }

    let output = child.wait_with_output().await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    println!(
        "{} {}",
        style("status:").cyan(),
        output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "<signal>".into())
    );
    print_boxed("stdout", &stdout);
    print_boxed("stderr", &stderr);
    Ok(())
}

fn lookup_event<'a>(hooks: &'a Hooks, event: &str) -> Result<&'a [String]> {
    Ok(match event {
        "before_tool" => &hooks.before_tool,
        "after_tool" => &hooks.after_tool,
        "session_start" => &hooks.session_start,
        "session_end" => &hooks.session_end,
        other => {
            return Err(anyhow!(
                "unknown hook event `{other}`. Expected one of: before_tool, after_tool, session_start, session_end."
            ));
        }
    })
}

/// Build a representative JSON payload for `event`. Shapes mirror the
/// docstrings on `merlion_config::Hooks` so a hook author can write against
/// the same fields they'll receive at runtime.
fn synthetic_payload(event: &str) -> serde_json::Value {
    match event {
        "before_tool" => json!({
            "tool": "bash",
            "args": { "command": "echo hello" }
        }),
        "after_tool" => json!({
            "tool": "bash",
            "result": "hello\n"
        }),
        "session_start" => json!({
            "session_id": "test-session-0000"
        }),
        "session_end" => json!({
            "session_id": "test-session-0000",
            "messages": 4
        }),
        _ => json!({}),
    }
}

fn print_boxed(label: &str, body: &str) {
    let trimmed = body.trim_end_matches('\n');
    println!("{}", style(format!("┌─ {label}")).dim());
    if trimmed.is_empty() {
        println!("{}", style("│ (empty)").dim());
    } else {
        for line in trimmed.lines() {
            println!("{} {line}", style("│").dim());
        }
    }
    println!("{}", style("└─").dim());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_payload_shapes_match_doc_comments() {
        let p = synthetic_payload("before_tool");
        assert!(p.get("tool").is_some());
        assert!(p.get("args").is_some());

        let p = synthetic_payload("after_tool");
        assert!(p.get("tool").is_some());
        assert!(p.get("result").is_some());

        let p = synthetic_payload("session_start");
        assert!(p.get("session_id").is_some());

        let p = synthetic_payload("session_end");
        assert!(p.get("session_id").is_some());
        assert!(p.get("messages").is_some());
    }

    #[test]
    fn synthetic_payload_unknown_event_is_empty_object() {
        let p = synthetic_payload("nope");
        assert!(p.is_object());
        assert_eq!(p.as_object().unwrap().len(), 0);
    }

    #[test]
    fn lookup_event_returns_slice_for_each_known_event() {
        let hooks = Hooks {
            before_tool: vec!["a".into()],
            after_tool: vec!["b".into(), "c".into()],
            session_start: vec![],
            session_end: vec!["d".into()],
        };
        assert_eq!(lookup_event(&hooks, "before_tool").unwrap().len(), 1);
        assert_eq!(lookup_event(&hooks, "after_tool").unwrap().len(), 2);
        assert_eq!(lookup_event(&hooks, "session_start").unwrap().len(), 0);
        assert_eq!(lookup_event(&hooks, "session_end").unwrap().len(), 1);
    }

    #[test]
    fn lookup_event_rejects_unknown_name() {
        let hooks = Hooks::default();
        let err = lookup_event(&hooks, "bogus").unwrap_err();
        assert!(err.to_string().contains("unknown hook event"));
    }

    #[tokio::test]
    async fn test_errors_when_index_out_of_range() {
        let hooks = Hooks {
            before_tool: vec!["echo hi".into()],
            ..Hooks::default()
        };
        let err = test(&hooks, "before_tool", 5).await.unwrap_err();
        assert!(err.to_string().contains("out of range"));
    }

    #[tokio::test]
    async fn test_errors_on_unknown_event_before_spawning() {
        let hooks = Hooks::default();
        let err = test(&hooks, "not_a_real_event", 0).await.unwrap_err();
        assert!(err.to_string().contains("unknown hook event"));
    }
}

// -----------------------------------------------------------------------------
// WIRING SPEC — apply to `crates/merlion-cli/src/main.rs`.
//
// 1. Add a module declaration near the other `mod` lines at the top of main.rs:
//
//        mod hooks_cmd;
//
// 2. Add a new variant to the `Command` enum:
//
//        /// Inspect and dry-run shell-script lifecycle hooks declared
//        /// under `hooks:` in `~/.merlion/config.yaml`.
//        ///
//        /// Hooks are not yet executed by the agent loop — that wiring is
//        /// tracked separately. This command exists so users can list what
//        /// they've configured and pipe a synthetic JSON payload through
//        /// each hook to confirm the script behaves.
//        Hooks {
//            #[command(subcommand)]
//            action: hooks_cmd::HooksAction,
//        },
//
//    `HooksAction` already derives `clap::Subcommand` in this file, so no
//    extra clap derives are needed in `main.rs`.
//
// 3. Add a dispatch arm in the `match cli.command.unwrap_or(...)` block in
//    `main`:
//
//        Command::Hooks { action } => hooks_cmd::run(cfg, action).await,
//
//    The dispatch consumes `cfg` (the loaded `merlion_config::Config`). If
//    `cfg` is borrowed by surrounding code in that match arm, clone it
//    (`cfg.clone()`) before handing it in — the borrow checker, not the
//    contract here, dictates which.
// -----------------------------------------------------------------------------
