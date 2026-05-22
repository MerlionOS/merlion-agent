//! `merlion dump` — emit a compact, copy-pasteable plain-text snapshot of
//! the user's merlion setup for bug reports. No ANSI colors, no secrets:
//! API keys and tokens are reported as `set` / `MISSING`, never by value.

use std::path::Path;
use std::process::Command as StdCommand;

use anyhow::Result;
use chrono::{DateTime, Local};
use merlion_config::Config;

use crate::gateway_service;

/// Env vars probed for the "env-var presence" section. Each one prints as
/// `set` or `MISSING` — the value is never written to stdout.
const ENV_VARS: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "OPENROUTER_API_KEY",
    "GROQ_API_KEY",
    "DEEPSEEK_API_KEY",
    "MOONSHOT_API_KEY",
    "MINIMAX_API_KEY",
    "ZAI_API_KEY",
    "NOUS_API_KEY",
    "NOVITA_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "TELEGRAM_BOT_TOKEN",
    "DISCORD_BOT_TOKEN",
    "SLACK_APP_TOKEN",
    "SLACK_BOT_TOKEN",
    "TAVILY_API_KEY",
];

const GATEWAY_TOKENS: &[&str] = &[
    "TELEGRAM_BOT_TOKEN",
    "DISCORD_BOT_TOKEN",
    "SLACK_APP_TOKEN",
    "SLACK_BOT_TOKEN",
];

pub async fn run(cfg: Config) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let home = merlion_config::merlion_home();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unknown)".into());
    let build = git_short_hash().unwrap_or_else(|| "release".into());

    println!("── merlion-agent v{version} ──");
    println!("home:    {}", home.display());
    println!("exe:     {exe}");
    println!("build:   {build}");
    println!(
        "os:      {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    println!();
    println!("── config ──");
    println!("model.id:          {}", cfg.model.id);
    println!("model.base_url:    {}", opt_str(&cfg.model.base_url));
    let api_key_env = match cfg.resolve_provider() {
        Ok(p) => p.api_key_env,
        Err(_) => cfg
            .model
            .api_key_env
            .clone()
            .unwrap_or_else(|| "(unresolved)".into()),
    };
    println!("model.api_key_env: {api_key_env}");
    println!("system_prompt:     {}", opt_str(&cfg.system_prompt));
    println!("max_iterations:    {}", cfg.max_iterations);

    println!();
    println!("── env-var presence ──");
    for name in ENV_VARS {
        let present = std::env::var(name).is_ok();
        println!("{:<22} {}", name, if present { "set" } else { "MISSING" });
    }

    println!();
    println!("── credentials ──");
    let codex_auth = dirs::home_dir().map(|h| h.join(".codex/auth.json"));
    print_cred_line("~/.codex/auth.json", codex_auth.as_deref());
    let dotenv = home.join(".env");
    print_cred_line("~/.merlion/.env", Some(dotenv.as_path()));
    let auth_yaml = home.join("auth.yaml");
    print_cred_line("~/.merlion/auth.yaml", Some(auth_yaml.as_path()));

    println!();
    println!("── stores ──");
    let sessions_db = home.join("sessions.db");
    if sessions_db.exists() {
        let size = file_size_human(&sessions_db);
        let rows = match merlion_session::SessionDB::open(&sessions_db) {
            Ok(db) => match db.list_sessions(usize::MAX) {
                Ok(rs) => format!("{} rows", rs.len()),
                Err(_) => "rows unknown".into(),
            },
            Err(_) => "rows unknown".into(),
        };
        println!("sessions.db:  {} ({size}, {rows})", sessions_db.display());
    } else {
        println!("sessions.db:  (none yet)");
    }
    let memory_dir = home.join("memory");
    let mem_count = count_files(&memory_dir);
    println!("memory/:      {mem_count} files");
    let user_skills = home.join("skills/user");
    let user_count = count_files(&user_skills);
    let bundled = std::env::current_dir()
        .ok()
        .map(|p| p.join("skills"))
        .filter(|p| p.exists())
        .is_some();
    println!(
        "skills/:      user={user_count}, bundled={}",
        if bundled { "ok" } else { "none" }
    );

    println!();
    println!("── mcp ──");
    match merlion_mcp::McpRegistry::load_default() {
        Ok(reg) => {
            let count = reg.servers.len();
            let names: Vec<&str> = reg.servers.keys().map(|s| s.as_str()).collect();
            if count == 0 {
                println!("0 servers configured.");
            } else {
                println!("{count} servers configured. Names: {}", names.join(", "));
            }
        }
        Err(e) => println!("(error loading mcp.yaml: {e})"),
    }

    println!();
    println!("── gateway ──");
    let service_line = match gateway_service::service_status() {
        Ok(gateway_service::ServiceState::NotInstalled) => "not installed".to_string(),
        Ok(gateway_service::ServiceState::Stopped { .. }) => "installed, stopped".to_string(),
        Ok(gateway_service::ServiceState::Running { pid, .. }) => match pid {
            Some(p) => format!("running (pid {p})"),
            None => "running".into(),
        },
        Ok(gateway_service::ServiceState::Unknown { detail, .. }) => format!("unknown ({detail})"),
        Err(e) => format!("error ({e})"),
    };
    println!("service: {service_line}");
    for name in GATEWAY_TOKENS {
        let present = std::env::var(name).is_ok();
        println!(
            "{:<19} {}",
            format!("{name}:"),
            if present { "set" } else { "MISSING" }
        );
    }

    println!();
    println!("── cron ──");
    match merlion_cron::CronRegistry::load_default() {
        Ok(reg) => {
            let count = reg.jobs.len();
            let names: Vec<&str> = reg.jobs.iter().map(|j| j.name.as_str()).collect();
            if count == 0 {
                println!("0 jobs configured.");
            } else {
                println!("{count} jobs configured: {}", names.join(", "));
            }
        }
        Err(e) => println!("(error loading cron.yaml: {e})"),
    }

    println!();
    println!("── recent activity ──");
    let logs_dir = home.join("logs");
    print_log_tail(&logs_dir.join("gateway.log"), "logs/gateway.log", 5);
    print_recent_agent_log(&logs_dir, 5);

    Ok(())
}

fn opt_str(opt: &Option<String>) -> String {
    match opt {
        Some(s) if !s.is_empty() => s.clone(),
        _ => "(none)".into(),
    }
}

fn git_short_hash() -> Option<String> {
    let out = StdCommand::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn print_cred_line(label: &str, path: Option<&Path>) {
    let line = match path {
        Some(p) if p.exists() => match std::fs::metadata(p) {
            Ok(md) => format!("present ({} bytes)", md.len()),
            Err(_) => "present".into(),
        },
        _ => "missing".into(),
    };
    println!("{:<22} {line}", label);
}

fn file_size_human(path: &Path) -> String {
    let bytes = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return "unknown size".into(),
    };
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} B")
    }
}

fn count_files(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .count()
}

fn print_log_tail(path: &Path, label: &str, n: usize) {
    if !path.exists() {
        println!("{label}: (none)");
        println!();
        return;
    }
    let mtime = mtime_display(path).unwrap_or_else(|| "unknown".into());
    println!("{label}:  last modified {mtime} (last {n} lines below)");
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(n);
            for line in &lines[start..] {
                println!("{line}");
            }
        }
        Err(e) => println!("(error reading log: {e})"),
    }
    println!();
}

fn print_recent_agent_log(logs_dir: &Path, n: usize) {
    let Ok(rd) = std::fs::read_dir(logs_dir) else {
        println!("logs/agent.log.<date>: (none)");
        return;
    };
    let mut newest: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    for entry in rd.flatten() {
        let p = entry.path();
        let name = match p.file_name().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        if !name.starts_with("agent.log") {
            continue;
        }
        let mtime = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if newest.as_ref().map(|(_, t)| mtime > *t).unwrap_or(true) {
            newest = Some((p, mtime));
        }
    }
    match newest {
        Some((p, _)) => {
            let label = format!(
                "logs/{}",
                p.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("agent.log")
            );
            print_log_tail(&p, &label, n);
        }
        None => println!("logs/agent.log.<date>: (none)"),
    }
}

fn mtime_display(path: &Path) -> Option<String> {
    let md = std::fs::metadata(path).ok()?;
    let modified = md.modified().ok()?;
    let dt: DateTime<Local> = modified.into();
    Some(dt.format("%Y-%m-%dT%H:%M").to_string())
}

// -----------------------------------------------------------------------------
// WIRING SPEC — apply to `crates/merlion-cli/src/main.rs`.
//
// 1. Add a module declaration near the other `mod` lines at the top of main.rs:
//
//        mod dump_cmd;
//
// 2. Add a new variant to the `Command` enum:
//
//        /// Print a compact, copy-pasteable plain-text snapshot of the
//        /// user's merlion setup — config, env-var presence (no values),
//        /// credential file presence, store sizes, MCP servers, gateway
//        /// service state, cron jobs, and recent log tails. Designed to be
//        /// dropped into a bug report. API keys, tokens, and password-shaped
//        /// values are NEVER written to stdout.
//        Dump,
//
// 3. Add a dispatch arm in the `match cli.command.unwrap_or(...)` block in
//    `main()`, alongside the other simple commands:
//
//        Command::Dump => dump_cmd::run(cfg).await,
//
// 4. No new dependencies are required — this module reuses
//    `merlion_config`, `merlion_session`, `merlion_mcp`, `merlion_cron`,
//    `dirs`, `chrono`, and `crate::gateway_service`, all of which are
//    already in `crates/merlion-cli/Cargo.toml`.
// -----------------------------------------------------------------------------
