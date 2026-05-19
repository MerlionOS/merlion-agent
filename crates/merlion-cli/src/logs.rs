//! Log file management for the `merlion` CLI.
//!
//! Provides two things:
//! 1. [`init_with_files`] — installs a `tracing` subscriber that fans out
//!    to stderr (current behavior) *and* to two daily-rotated files under
//!    `~/.merlion/logs/`:
//!      - `agent.log`  — INFO and above
//!      - `errors.log` — WARN and above
//! 2. [`run`] — the `merlion logs` subcommand. Prints the tail of the
//!    chosen log file to stdout, optionally streaming new lines via
//!    `tail -F` (Unix only).
//!
//! The wiring-spec block at the bottom of this file documents exactly how
//! `main.rs` should call into here. We intentionally do not edit
//! `main.rs` in this module so the integration is reviewable as a single
//! diff. Until that wiring lands the public functions look like dead
//! code to the bin crate, so we silence the lint at module scope.

#![allow(dead_code)]

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Subdirectory under `merlion_home()` where rotated log files live.
const LOG_DIRNAME: &str = "logs";
/// File-name *prefix* given to `tracing_appender`. With daily rotation
/// the actual files on disk are `agent.log.YYYY-MM-DD`.
const AGENT_LOG_PREFIX: &str = "agent.log";
const ERROR_LOG_PREFIX: &str = "errors.log";

/// Initialize tracing with a stderr layer + rolling-file layers that
/// write `~/.merlion/logs/agent.log` (INFO+) and `~/.merlion/logs/errors.log`
/// (WARN+).
///
/// Returns the [`WorkerGuard`] for the agent-log non-blocking writer.
/// **The caller must hold this in a `let _guard = ...;` binding for the
/// lifetime of the program** — when it drops, the background writer
/// thread shuts down and any buffered log lines are lost.
///
/// Filter precedence (matches the old behavior so existing setups keep
/// working):
///   - The stderr layer honors `MERLION_LOG` (defaulting to `warn`),
///     same as before.
///   - The file layers ignore `MERLION_LOG` and instead apply fixed
///     `INFO` / `WARN` floors so on-disk logs are always useful even
///     when the terminal is quiet.
pub fn init_with_files() -> Result<WorkerGuard> {
    let log_dir = log_dir();
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("create log dir {}", log_dir.display()))?;

    let agent_appender = rolling::daily(&log_dir, AGENT_LOG_PREFIX);
    let (agent_nb, guard) = tracing_appender::non_blocking(agent_appender);

    // errors.log stays synchronous: WARN+ traffic is low-volume, and using
    // a sync writer means we only have one WorkerGuard to track — matching
    // the signature this function is required to return. The MakeWriter
    // impl on RollingFileAppender already handles per-call file rotation.
    let error_appender = rolling::daily(&log_dir, ERROR_LOG_PREFIX);

    let stderr_filter =
        EnvFilter::try_from_env("MERLION_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .compact()
        .with_filter(stderr_filter);

    let agent_file_layer = tracing_subscriber::fmt::layer()
        .with_writer(agent_nb)
        .with_ansi(false)
        .with_filter(LevelFilter::INFO);

    let error_file_layer = tracing_subscriber::fmt::layer()
        .with_writer(make_writer(error_appender))
        .with_ansi(false)
        .with_filter(LevelFilter::WARN);

    tracing_subscriber::registry()
        .with(stderr_layer)
        .with(agent_file_layer)
        .with(error_file_layer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("tracing init: {e}"))?;

    Ok(guard)
}

/// Wrap a `RollingFileAppender` in a thin newtype so we can hand it to a
/// `fmt::Layer` as a `MakeWriter`. The appender already implements
/// `MakeWriter`, but going through a newtype keeps the type signatures
/// concrete and avoids confusing the trait solver when both file layers
/// are combined in the same Registry chain.
fn make_writer(appender: rolling::RollingFileAppender) -> RollingMakeWriter {
    RollingMakeWriter(appender)
}

struct RollingMakeWriter(rolling::RollingFileAppender);

impl<'a> MakeWriter<'a> for RollingMakeWriter {
    type Writer = <rolling::RollingFileAppender as MakeWriter<'a>>::Writer;
    fn make_writer(&'a self) -> Self::Writer {
        self.0.make_writer()
    }
}

/// Arguments for the `merlion logs` subcommand. Kept as a plain struct
/// (no clap derive) so it can be tested without a clap dependency in
/// scope and so `main.rs` is free to expose its own flag spelling.
#[derive(Debug, Clone)]
pub struct LogsArgs {
    /// Follow the file (`tail -F`). Unix-only — on Windows this falls
    /// back to a single read and prints a warning.
    pub follow: bool,
    /// Show `errors.log` instead of `agent.log`.
    pub errors: bool,
    /// Drop lines older than this human-readable duration (e.g. `"1h"`,
    /// `"10m"`). Parsed via [`humantime::parse_duration`]. `None` means
    /// no time filter.
    pub since: Option<String>,
    /// How many lines from the tail to show before any `--follow` stream.
    pub lines: usize,
}

impl Default for LogsArgs {
    fn default() -> Self {
        Self {
            follow: false,
            errors: false,
            since: None,
            lines: 50,
        }
    }
}

/// Execute the `logs` subcommand: print the chosen log file's tail to
/// stdout, optionally following new writes.
pub async fn run(args: LogsArgs) -> Result<()> {
    let path = current_log_file(args.errors);
    if !path.exists() {
        eprintln!(
            "(no log file at {} yet — run merlion to generate one)",
            path.display()
        );
        return Ok(());
    }

    let since_cutoff = match args.since.as_deref() {
        Some(s) => Some(parse_since(s)?),
        None => None,
    };

    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let filtered: Vec<&str> = match since_cutoff {
        Some(cutoff) => text
            .lines()
            .filter(|line| line_after(line, cutoff).unwrap_or(true))
            .collect(),
        None => text.lines().collect(),
    };

    let start = filtered.len().saturating_sub(args.lines);
    for line in &filtered[start..] {
        println!("{line}");
    }

    if !args.follow {
        return Ok(());
    }

    if cfg!(windows) {
        eprintln!("(--follow is unix-only; printed snapshot above)");
        return Ok(());
    }

    let status = tokio::process::Command::new("tail")
        .arg("-F")
        .arg(&path)
        .status()
        .await
        .with_context(|| "spawn `tail -F` (is `tail` installed?)")?;
    if !status.success() {
        anyhow::bail!("tail -F exited with status {status}");
    }
    Ok(())
}

/// `~/.merlion/logs/`
fn log_dir() -> PathBuf {
    merlion_config::merlion_home().join(LOG_DIRNAME)
}

/// Resolve the *current* day's log file path. `tracing_appender` uses
/// `<prefix>.YYYY-MM-DD` as its on-disk name.
fn current_log_file(errors: bool) -> PathBuf {
    let prefix = if errors {
        ERROR_LOG_PREFIX
    } else {
        AGENT_LOG_PREFIX
    };
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    log_dir().join(format!("{prefix}.{today}"))
}

/// Parse `--since` flag values (`"1h"`, `"10m"`, `"30s"`, `"1d 2h"`) into
/// a UTC cutoff `DateTime`. Lines with timestamps earlier than the
/// cutoff are dropped from the output.
fn parse_since(s: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    let dur = humantime::parse_duration(s)
        .with_context(|| format!("invalid --since duration `{s}` (try `1h`, `10m`, `30s`)"))?;
    let cd = chrono::Duration::from_std(dur)
        .map_err(|e| anyhow::anyhow!("--since duration out of range: {e}"))?;
    Ok(chrono::Utc::now() - cd)
}

/// Best-effort check: does this log line's leading RFC3339-ish timestamp
/// fall on or after `cutoff`? Returns `None` when the line has no parseable
/// timestamp — callers treat that as "keep the line" so untimestamped
/// continuation lines aren't silently dropped.
fn line_after(line: &str, cutoff: chrono::DateTime<chrono::Utc>) -> Option<bool> {
    // tracing's default fmt prefixes each line with an RFC3339 timestamp
    // and a space (e.g. `2026-05-19T07:21:30.123456Z  INFO ...`). We
    // take the first whitespace-delimited token and try to parse it.
    let ts_token = line.split_whitespace().next()?;
    let parsed = chrono::DateTime::parse_from_rfc3339(ts_token).ok()?;
    Some(parsed.with_timezone(&chrono::Utc) >= cutoff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_args_default_is_50_lines_agent() {
        let a = LogsArgs::default();
        assert_eq!(a.lines, 50);
        assert!(!a.follow);
        assert!(!a.errors);
        assert!(a.since.is_none());
    }

    #[test]
    fn current_log_file_picks_agent_or_errors() {
        let agent = current_log_file(false);
        let errors = current_log_file(true);
        assert!(
            agent
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap()
                .starts_with("agent.log."),
            "agent path = {}",
            agent.display()
        );
        assert!(
            errors
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap()
                .starts_with("errors.log."),
            "errors path = {}",
            errors.display()
        );
        assert_eq!(agent.parent(), errors.parent());
    }

    #[test]
    fn log_dir_honors_merlion_home_env() {
        let tmp = tempfile::tempdir().unwrap();
        // Safety: tests in this module run single-threaded by default
        // under `cargo test` per-binary; this env-var dance only affects
        // this test. We restore it on the way out.
        let prev = std::env::var("MERLION_HOME").ok();
        // SAFETY: single-threaded test invocation.
        unsafe {
            std::env::set_var("MERLION_HOME", tmp.path());
        }
        let d = log_dir();
        assert_eq!(d, tmp.path().join(LOG_DIRNAME));
        // SAFETY: single-threaded test invocation.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("MERLION_HOME", v),
                None => std::env::remove_var("MERLION_HOME"),
            }
        }
    }

    #[test]
    fn parse_since_accepts_humantime() {
        let cutoff = parse_since("1h").unwrap();
        let now = chrono::Utc::now();
        let diff = now - cutoff;
        assert!(
            diff.num_seconds() >= 3599 && diff.num_seconds() <= 3601,
            "expected ~1h ago, got {} seconds",
            diff.num_seconds()
        );
    }

    #[test]
    fn parse_since_rejects_garbage() {
        assert!(parse_since("not-a-duration").is_err());
    }

    #[test]
    fn line_after_respects_cutoff() {
        let cutoff = chrono::DateTime::parse_from_rfc3339("2026-05-19T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let before = "2026-05-18T23:59:59Z  INFO before";
        let after = "2026-05-19T00:00:01Z  INFO after";
        let untimestamped = "  ... continuation line";
        assert_eq!(line_after(before, cutoff), Some(false));
        assert_eq!(line_after(after, cutoff), Some(true));
        assert_eq!(line_after(untimestamped, cutoff), None);
    }
}

// =============================================================================
// WIRING SPEC — how `main.rs` should adopt this module.
// =============================================================================
//
// 1. Add a `mod logs;` declaration alongside the existing `mod approver;`
//    and `mod tui;` in `main.rs`.
//
// 2. Replace the existing tracing init block at the top of `main()`:
//
//        tracing_subscriber::fmt()
//            .with_env_filter(
//                EnvFilter::try_from_env("MERLION_LOG")
//                    .unwrap_or_else(|_| EnvFilter::new("warn")),
//            )
//            .with_writer(std::io::stderr)
//            .compact()
//            .init();
//
//    with:
//
//        let _guard = logs::init_with_files()?;
//
//    The `_guard` binding MUST live until the end of `main()`. If you
//    drop it earlier (e.g. by writing `logs::init_with_files()?;`
//    without binding) the background log-writer thread shuts down and
//    any buffered lines are lost. Naming it `_guard` (with the leading
//    underscore) is intentional — it's a sentinel, not something the
//    rest of main reads.
//
// 3. The `use tracing_subscriber::EnvFilter;` import at the top of
//    `main.rs` is now unused and should be deleted. No other new `use`
//    statements are required by main.rs itself — `logs::*` is reached
//    via the module path.
//
// 4. Add a new variant to the `Command` enum (between `Update` and the
//    closing `}` is fine):
//
//        /// Tail merlion's on-disk log files (`~/.merlion/logs/`).
//        Logs {
//            /// Follow the file like `tail -F` (Unix only).
//            #[arg(short, long)]
//            follow: bool,
//            /// Show errors.log (WARN+) instead of agent.log (INFO+).
//            #[arg(long)]
//            errors: bool,
//            /// Only show entries newer than this duration. e.g. `1h`, `10m`.
//            #[arg(long)]
//            since: Option<String>,
//            /// How many lines from the tail to show. Default 50.
//            #[arg(short = 'n', long, default_value_t = 50)]
//            lines: usize,
//        },
//
// 5. Add a dispatch arm in the `match cli.command.unwrap_or(...)` block:
//
//        Command::Logs { follow, errors, since, lines } => {
//            logs::run(logs::LogsArgs { follow, errors, since, lines }).await
//        }
//
// 6. Nothing else in `main.rs` needs to change. The new `logs`
//    subcommand reads from disk only; it does not need config, an
//    LlmClient, the session DB, or any tools.
//
// =============================================================================
