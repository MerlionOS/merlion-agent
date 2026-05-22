//! `merlion debug share` — bundle redacted config + recent logs into a single
//! tar.gz so a user can attach it to a support request.
//!
//! Unlike hermes-agent's `debug share`, this does **not** upload anywhere. The
//! file lands on the user's disk; they choose where it goes from there. That
//! sidesteps a paste-service dependency and the auth that goes with it.
//!
//! Redaction strategy:
//!   - `config.yaml` is included verbatim. By design it stores only
//!     `api_key_env` names, never key material.
//!   - `.env` is rewritten to `.env.redacted`: each `KEY=value` line becomes
//!     `KEY=<REDACTED>` so the names of the env vars survive but values
//!     don't. Comments and blank lines pass through unchanged.
//!   - Logs are tail-clipped to the last N lines (default 1000). The tail is
//!     scanned for known secret prefixes (`sk-`, `xoxb-`, `xapp-`, plus
//!     Telegram-style `:AA...`) and any matching whitespace-delimited token
//!     is replaced with `<REDACTED>` before being written to the archive.
//!   - `~/.codex/auth.json` and similar out-of-tree credentials are never
//!     read in the first place — only files under `~/.merlion/` are touched.
//!
//! See the wiring spec at the bottom of this file for how this hooks into
//! `main.rs`.

#![allow(dead_code)]

use std::fs::File;
use std::io::{BufWriter, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::{Builder, Header};

#[derive(Debug, clap::Subcommand)]
pub enum DebugAction {
    /// Bundle redacted config + recent logs into a tar.gz for support.
    Share {
        /// Output path. Default: ~/.merlion/merlion-debug-<UTC timestamp>.tar.gz
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// How many lines of each log to include.
        #[arg(long, default_value_t = 1000)]
        lines: usize,
    },
}

pub async fn run(action: DebugAction) -> Result<()> {
    match action {
        DebugAction::Share { output, lines } => share(output, lines),
    }
}

fn share(output: Option<PathBuf>, lines: usize) -> Result<()> {
    let home = merlion_config::merlion_home();
    let output = output.unwrap_or_else(|| default_output_path(&home));
    let bytes = build_bundle(&home, lines)?;

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create output dir {}", parent.display()))?;
        }
    }
    std::fs::write(&output, &bytes)
        .with_context(|| format!("write bundle to {}", output.display()))?;

    println!(
        "Wrote debug bundle: {} ({})",
        output.display(),
        humanize_bytes(bytes.len() as u64),
    );
    println!();
    println!("Attach this file to your support request. Contents are redacted —");
    println!(".env values are stripped, only env-var names are preserved.");
    Ok(())
}

fn default_output_path(home: &Path) -> PathBuf {
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    home.join(format!("merlion-debug-{ts}.tar.gz"))
}

// ---------------------------------------------------------------------------
// Core impl — pure functions parameterised by `home` so tests can drive them
// against a tempdir without env mutation.
// ---------------------------------------------------------------------------

/// Build the gzip-compressed tar bundle entirely in memory and return the
/// raw bytes. Writing the file is the caller's job — easier to assert on the
/// archive contents in tests.
fn build_bundle(home: &Path, lines: usize) -> Result<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    {
        let encoder = GzEncoder::new(BufWriter::new(&mut buf), Compression::default());
        let mut builder = Builder::new(encoder);

        for (name, contents) in collect_entries(home, lines)? {
            let mut header = Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(chrono::Utc::now().timestamp() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, &name, contents.as_slice())
                .with_context(|| format!("append {name}"))?;
        }

        let encoder = builder.into_inner().context("finalise tar")?;
        encoder.finish().context("finalise gzip")?;
    }
    Ok(buf)
}

/// Returns the list of `(archive_path, contents)` pairs to put in the
/// tarball, in a stable order. Missing source files are silently skipped —
/// a fresh merlion install won't have logs or a fallback chain, and we don't
/// want that to fail the command.
fn collect_entries(home: &Path, lines: usize) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();

    out.push(("summary.txt".into(), build_summary(home).into_bytes()));

    if let Some(bytes) = read_if_exists(&home.join("config.yaml"))? {
        out.push(("config.yaml".into(), bytes));
    }

    if let Some(text) = read_if_exists_text(&home.join(".env"))? {
        out.push((".env.redacted".into(), redact_env(&text).into_bytes()));
    }

    if let Some(bytes) = tail_file_lines(&home.join("logs").join("gateway.log"), lines)? {
        out.push((
            "logs/gateway.log.tail".into(),
            redact_secrets(&String::from_utf8_lossy(&bytes)).into_bytes(),
        ));
    }
    if let Some(bytes) = tail_file_lines(&home.join("logs").join("gateway.error.log"), lines)? {
        out.push((
            "logs/gateway.error.log.tail".into(),
            redact_secrets(&String::from_utf8_lossy(&bytes)).into_bytes(),
        ));
    }
    if let Some((_path, bytes)) = newest_agent_log_tail(&home.join("logs"), lines)? {
        out.push((
            "logs/agent.log.tail".into(),
            redact_secrets(&String::from_utf8_lossy(&bytes)).into_bytes(),
        ));
    }

    if let Some(bytes) = read_if_exists(&home.join("mcp.yaml"))? {
        out.push(("mcp.yaml".into(), bytes));
    }
    if let Some(bytes) = read_if_exists(&home.join("fallback.yaml"))? {
        out.push(("fallback.yaml".into(), bytes));
    }

    Ok(out)
}

/// Brief 10-ish-line summary: version, OS, model id, redacted config presence,
/// env-var presence (names only). Kept self-contained so we don't depend on
/// a `dump_cmd::build_summary` that doesn't exist yet.
fn build_summary(home: &Path) -> String {
    let mut s = String::new();
    s.push_str("merlion debug bundle\n");
    s.push_str(&format!("generated: {}\n", chrono::Utc::now().to_rfc3339()));
    s.push_str(&format!("merlion version: {}\n", env!("CARGO_PKG_VERSION")));
    s.push_str(&format!(
        "os: {} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    s.push_str(&format!("home: {}\n", home.display()));

    let cfg_path = home.join("config.yaml");
    s.push_str(&format!("config.yaml present: {}\n", cfg_path.exists()));
    if let Ok(Some(text)) = read_if_exists_text(&cfg_path) {
        let model_id = parse_model_id(&text).unwrap_or_else(|| "(unparsed)".into());
        s.push_str(&format!("model.id: {model_id}\n"));
    }

    s.push_str(&format!(".env present: {}\n", home.join(".env").exists()));
    s.push_str(&format!(
        "mcp.yaml present: {}\n",
        home.join("mcp.yaml").exists()
    ));
    s.push_str(&format!(
        "fallback.yaml present: {}\n",
        home.join("fallback.yaml").exists()
    ));

    let logs_dir = home.join("logs");
    s.push_str(&format!("logs dir present: {}\n", logs_dir.exists()));

    s
}

/// Pull `model.id: <value>` out of a config.yaml without round-tripping
/// through serde — the summary is best-effort and we'd rather degrade
/// gracefully than fail on a malformed file.
fn parse_model_id(yaml: &str) -> Option<String> {
    let mut in_model = false;
    for raw in yaml.lines() {
        let line = raw.trim_end();
        if line.starts_with("model:") {
            in_model = true;
            continue;
        }
        if in_model {
            if !line.starts_with(' ') && !line.starts_with('\t') && !line.is_empty() {
                in_model = false;
                continue;
            }
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("id:") {
                return Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
            }
        }
    }
    None
}

fn read_if_exists(path: &Path) -> Result<Option<Vec<u8>>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(Some(bytes))
}

fn read_if_exists_text(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(Some(text))
}

/// Return the last `n` lines of a file as raw bytes, or `None` if the file
/// doesn't exist. Reads the whole file — these logs are bounded by the
/// rotating appender so this is fine in practice and avoids a reverse-seek
/// implementation.
fn tail_file_lines(path: &Path, n: usize) -> Result<Option<Vec<u8>>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut text = String::new();
    File::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .read_to_string(&mut text)
        .with_context(|| format!("read {}", path.display()))?;
    let tail: Vec<&str> = text.lines().rev().take(n).collect();
    let mut joined = String::new();
    for line in tail.iter().rev() {
        joined.push_str(line);
        joined.push('\n');
    }
    Ok(Some(joined.into_bytes()))
}

/// Find the most-recently-modified `agent.log*` file under `logs/` and return
/// its tail. Useful because the agent log rotates daily — the latest file
/// is the one a user's most recent failure landed in.
fn newest_agent_log_tail(logs_dir: &Path, n: usize) -> Result<Option<(PathBuf, Vec<u8>)>> {
    if !logs_dir.exists() {
        return Ok(None);
    }
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    for entry in
        std::fs::read_dir(logs_dir).with_context(|| format!("read_dir {}", logs_dir.display()))?
    {
        let entry = entry.context("dir entry")?;
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !name.starts_with("agent.log") {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        let take = match &best {
            None => true,
            Some((_, prev)) => mtime > *prev,
        };
        if take {
            best = Some((path, mtime));
        }
    }
    match best {
        None => Ok(None),
        Some((path, _)) => {
            let bytes = tail_file_lines(&path, n)?.unwrap_or_default();
            Ok(Some((path, bytes)))
        }
    }
}

/// Replace every `KEY=value` line in a dotenv file with `KEY=<REDACTED>`.
/// Comments (`#`), blank lines, and lines without `=` pass through unchanged.
/// The output preserves the original line order so a user can diff the
/// shape of their `.env` against a working install.
fn redact_env(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if let Some(eq) = line.find('=') {
            let (key, _value) = line.split_at(eq);
            out.push_str(key);
            out.push_str("=<REDACTED>");
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Walk a blob of (presumably) log text and replace any whitespace-delimited
/// token that looks like a known-shape API key with `<REDACTED>`. This is a
/// belt-and-braces pass — we never deliberately log secrets, but a stray
/// `Authorization: Bearer sk-...` would otherwise leak.
fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for tok in line.split_inclusive(char::is_whitespace) {
            out.push_str(&redact_token(tok));
        }
    }
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn redact_token(tok: &str) -> String {
    // Strip trailing whitespace from the token so we can match on the value,
    // then put the whitespace back. Most callers pass tokens via
    // split_inclusive so the suffix is at most one whitespace char.
    let trail_len = tok.chars().rev().take_while(|c| c.is_whitespace()).count();
    let core_len = tok.len()
        - tok
            .chars()
            .rev()
            .take(trail_len)
            .map(|c| c.len_utf8())
            .sum::<usize>();
    let core = &tok[..core_len];
    let trail = &tok[core_len..];

    if looks_like_secret(core) {
        format!("<REDACTED>{trail}")
    } else {
        tok.to_string()
    }
}

fn looks_like_secret(s: &str) -> bool {
    // Strip surrounding punctuation a token might be wrapped in inside a log
    // line (quotes, brackets, commas, colons, semicolons).
    let trimmed = s.trim_matches(|c: char| {
        c == '"'
            || c == '\''
            || c == '`'
            || c == ','
            || c == ';'
            || c == ':'
            || c == ')'
            || c == ']'
            || c == '}'
            || c == '('
            || c == '['
            || c == '{'
    });
    if trimmed.len() < 12 {
        return false;
    }
    if trimmed.starts_with("sk-") || trimmed.starts_with("xoxb-") || trimmed.starts_with("xapp-") {
        return true;
    }
    // Telegram bot tokens look like `<digits>:AA<base64ish>`. Match on the
    // `:AA` infix followed by 30+ chars from the bot-token alphabet.
    if let Some(pos) = trimmed.find(":AA") {
        let after = &trimmed[pos + 3..];
        if after.len() >= 30
            && after
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return true;
        }
    }
    false
}

fn humanize_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if n >= MB {
        format!("{:.2} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{} KB", n / KB)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::collections::HashMap;
    use std::fs;
    use tar::Archive;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn extract(bytes: &[u8]) -> HashMap<String, Vec<u8>> {
        let mut out = HashMap::new();
        let mut archive = Archive::new(GzDecoder::new(bytes));
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().into_owned();
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).unwrap();
            out.insert(path.to_string_lossy().into_owned(), buf);
        }
        out
    }

    #[test]
    fn redact_env_strips_values_but_keeps_keys() {
        let input = "\
# a comment, keep me
OPENAI_API_KEY=sk-leakleakleakleak
SLACK_BOT_TOKEN=xoxb-supersecret
EMPTY=

PLAINKEY=plainvalue
weird line with no equals
";
        let out = redact_env(input);
        assert!(out.contains("OPENAI_API_KEY=<REDACTED>"));
        assert!(out.contains("SLACK_BOT_TOKEN=<REDACTED>"));
        assert!(out.contains("EMPTY=<REDACTED>"));
        assert!(out.contains("PLAINKEY=<REDACTED>"));
        assert!(out.contains("# a comment, keep me"));
        assert!(out.contains("weird line with no equals"));
        assert!(!out.contains("sk-leak"));
        assert!(!out.contains("xoxb-supersecret"));
        assert!(!out.contains("plainvalue"));
    }

    #[test]
    fn redact_secrets_catches_inline_tokens() {
        let log = "2024-01-01T00:00:00Z INFO using key sk-abcdefghijklmnopqrstuvwxyz for openai\nplain line\n";
        let redacted = redact_secrets(log);
        assert!(redacted.contains("<REDACTED>"));
        assert!(!redacted.contains("sk-abcdefghijklmnopqrstuvwxyz"));
        assert!(redacted.contains("plain line"));
    }

    #[test]
    fn share_bundle_redacts_env_and_includes_expected_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        write_file(
            &home.join("config.yaml"),
            "model:\n  id: openai:gpt-4o-mini\n",
        );
        write_file(
            &home.join(".env"),
            "OPENAI_API_KEY=sk-leak\nANTHROPIC_API_KEY=ant-secret\n",
        );
        write_file(&home.join("mcp.yaml"), "servers: {}\n");
        write_file(&home.join("fallback.yaml"), "chain: []\n");
        write_file(
            &home.join("logs").join("gateway.log"),
            "line1\nline2\nleak sk-abcdefghijklmnopqrstuv\n",
        );
        write_file(&home.join("logs").join("gateway.error.log"), "err1\nerr2\n");
        write_file(
            &home.join("logs").join("agent.log.2026-05-20"),
            "old agent log\n",
        );
        write_file(
            &home.join("logs").join("agent.log.2026-05-22"),
            "newest agent log\n",
        );

        let bytes = build_bundle(&home, 100).unwrap();
        let entries = extract(&bytes);

        assert!(entries.contains_key("summary.txt"));
        let summary = std::str::from_utf8(&entries["summary.txt"]).unwrap();
        assert!(summary.contains("merlion debug bundle"));
        assert!(summary.contains("openai:gpt-4o-mini"));

        assert_eq!(
            std::str::from_utf8(&entries["config.yaml"]).unwrap(),
            "model:\n  id: openai:gpt-4o-mini\n"
        );

        let redacted_env = std::str::from_utf8(&entries[".env.redacted"]).unwrap();
        assert!(redacted_env.contains("OPENAI_API_KEY=<REDACTED>"));
        assert!(redacted_env.contains("ANTHROPIC_API_KEY=<REDACTED>"));
        assert!(!redacted_env.contains("sk-leak"));
        assert!(!redacted_env.contains("ant-secret"));

        let gw = std::str::from_utf8(&entries["logs/gateway.log.tail"]).unwrap();
        assert!(gw.contains("line1"));
        assert!(gw.contains("<REDACTED>"));
        assert!(!gw.contains("sk-abcdefghijklmnopqrstuv"));

        assert!(entries.contains_key("logs/gateway.error.log.tail"));

        let agent = std::str::from_utf8(&entries["logs/agent.log.tail"]).unwrap();
        assert!(
            agent.contains("newest agent log"),
            "expected most-recent agent log, got: {agent}"
        );

        assert!(entries.contains_key("mcp.yaml"));
        assert!(entries.contains_key("fallback.yaml"));
    }

    #[test]
    fn share_bundle_tolerates_missing_optional_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        // Only config.yaml — no .env, no logs, no mcp, no fallback.
        write_file(&home.join("config.yaml"), "model:\n  id: openai:foo\n");

        let bytes = build_bundle(&home, 100).unwrap();
        let entries = extract(&bytes);

        assert!(entries.contains_key("summary.txt"));
        assert!(entries.contains_key("config.yaml"));
        assert!(!entries.contains_key(".env.redacted"));
        assert!(!entries.contains_key("logs/gateway.log.tail"));
        assert!(!entries.contains_key("mcp.yaml"));
    }

    #[test]
    fn tail_file_lines_returns_last_n() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.log");
        let mut s = String::new();
        for i in 0..50 {
            s.push_str(&format!("line {i}\n"));
        }
        fs::write(&path, &s).unwrap();
        let tail = tail_file_lines(&path, 5).unwrap().unwrap();
        let tail_str = String::from_utf8(tail).unwrap();
        assert!(tail_str.contains("line 45"));
        assert!(tail_str.contains("line 49"));
        assert!(!tail_str.contains("line 44"));
    }

    #[test]
    fn parse_model_id_handles_typical_yaml() {
        let y = "model:\n  id: anthropic:claude-sonnet-4\n  temperature: 0.2\nmax_iterations: 32\n";
        assert_eq!(
            parse_model_id(y).as_deref(),
            Some("anthropic:claude-sonnet-4")
        );
    }
}

// -----------------------------------------------------------------------------
// WIRING SPEC — apply to `crates/merlion-cli/src/main.rs`.
//
// 1. Add a module declaration near the other `mod` lines at the top of main.rs:
//
//        mod debug_cmd;
//
// 2. Add a new variant to the `Command` enum:
//
//        /// Diagnostics for support requests.
//        ///
//        /// `merlion debug share` packages a redacted snapshot of your
//        /// `~/.merlion/` (config, env-var names only, log tails) into a
//        /// single tar.gz you can attach to a bug report. Nothing is
//        /// uploaded — the file lands on your disk and you choose what to
//        /// do with it.
//        Debug {
//            #[command(subcommand)]
//            action: debug_cmd::DebugAction,
//        },
//
// 3. Add a dispatch arm in the `match cli.command.unwrap_or(...)` block in
//    `main()`:
//
//        Command::Debug { action } => debug_cmd::run(action).await,
//
// 4. No new clap derives are required in main.rs — `DebugAction` already
//    derives `clap::Subcommand` in this file.
// -----------------------------------------------------------------------------
