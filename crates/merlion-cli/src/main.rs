use std::io::{IsTerminal, Write};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use merlion_config::{Config, Wire};
use merlion_core::{
    Agent, AgentEvent, AgentOptions, Curator, LlmClient, Message, ToolApprover, ToolRegistry,
};
use merlion_llm::{AnthropicClient, BedrockClient, GeminiClient, OpenAiClient, VertexClient};
use merlion_mcp::{
    make_exposed_name, McpClient, McpProxyTool, McpRegistry, ServerEntry, StdioTransport,
    TransportSpec,
};
use merlion_memory::MemoryStore;
use merlion_session::SessionDB;
use merlion_skills::SkillSet;
use merlion_tools::skill_tools::SkillToolsConfig;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

mod approver;
mod tui;

#[derive(Debug, Parser)]
#[command(name = "merlion", version, about = "Merlion Agent — Rust port of hermes-agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start an interactive chat (default when no subcommand is given).
    Chat {
        /// Resume a specific session id instead of starting a new one.
        #[arg(long)]
        session: Option<String>,
        /// Force the ratatui-based TUI even when stdout is not a TTY.
        #[arg(long)]
        tui: bool,
        /// Force the plain line-based REPL even when stdout is a TTY.
        #[arg(long)]
        no_tui: bool,
    },
    /// Print or set the active model. `merlion model openai:gpt-4o-mini`
    Model {
        /// New `provider:model` to switch to. Omit to print the current setting.
        id: Option<String>,
    },
    /// Show or edit the merged config.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Diagnose configuration and credentials.
    Doctor,
    /// List or search past sessions.
    Sessions {
        #[command(subcommand)]
        action: Option<SessionsAction>,
    },
    /// Manage MCP (Model Context Protocol) servers.
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Run the messaging gateway (Telegram + Discord).
    Gateway {
        #[command(subcommand)]
        action: GatewayAction,
    },
    /// Manage scheduled jobs.
    Cron {
        #[command(subcommand)]
        action: CronAction,
    },
    /// Check for a newer merlion release on GitHub. Pass --apply to actually
    /// download and swap the binary (Unix only).
    Update {
        /// Download the latest tarball and replace the running binary. The
        /// running process exits after a successful swap.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Debug, Subcommand)]
enum GatewayAction {
    /// Start the gateway daemon. Reads TELEGRAM_BOT_TOKEN +
    /// MERLION_GATEWAY_ALLOW_TELEGRAM from the environment.
    Start,
    /// Print expected env-var configuration.
    Status,
}

#[derive(Debug, Subcommand)]
enum CronAction {
    /// List configured jobs.
    List,
    /// Add a job: `merlion cron add daily-summary "0 0 9 * * *" "summarize my inbox"`.
    Add {
        name: String,
        schedule: String,
        prompt: String,
        #[arg(long, default_value = "cli")]
        destination: String,
    },
    /// Remove a job.
    Remove { name: String },
    /// Run a job once immediately (does not affect the schedule).
    Run { name: String },
    /// Run the scheduler in the foreground; fires jobs as they come due.
    Daemon,
}

#[derive(Debug, Subcommand)]
enum McpAction {
    /// List configured servers and their enabled state.
    List,
    /// Add a stdio server: `merlion mcp add fs -- npx -y @mcp/fs /tmp`.
    Add {
        /// Logical name (used to prefix the exposed tool names).
        name: String,
        /// Command + args, separated from the name by `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Add an HTTP server: `merlion mcp add-http my-srv https://mcp.example.com/`.
    AddHttp {
        name: String,
        url: String,
        /// Env var to read the bearer token from. Optional.
        #[arg(long)]
        bearer_env: Option<String>,
    },
    /// Remove a configured server.
    Remove { name: String },
    /// Enable a previously-disabled server.
    Enable { name: String },
    /// Disable a server without removing it.
    Disable { name: String },
    /// Connect to a server, run the initialize handshake, list its tools.
    Test { name: String },
    /// Run the OAuth2 PKCE flow against an HTTP server and cache the token
    /// in `~/.merlion/mcp-tokens.yaml`.
    Oauth {
        name: String,
        /// Repeatable; defaults to none if omitted.
        #[arg(long)]
        scope: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigAction {
    /// Print the merged config as YAML.
    Show,
    /// Print the path to the user config file.
    Path,
}

#[derive(Debug, Subcommand)]
enum SessionsAction {
    /// List recent sessions.
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// FTS5 search across all message content.
    Search { query: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env("MERLION_LOG").unwrap_or_else(|_| EnvFilter::new("warn")))
        .with_writer(std::io::stderr)
        .compact()
        .init();

    let cli = Cli::parse();
    let cfg = merlion_config::load().context("loading config")?;

    match cli
        .command
        .unwrap_or(Command::Chat { session: None, tui: false, no_tui: false })
    {
        Command::Chat { session, tui, no_tui } => chat(cfg, session, tui, no_tui).await,
        Command::Model { id } => model_cmd(cfg, id),
        Command::Config { action } => config_cmd(cfg, action),
        Command::Doctor => doctor(cfg),
        Command::Sessions { action } => sessions_cmd(action),
        Command::Mcp { action } => mcp_cmd(action).await,
        Command::Gateway { action } => gateway_cmd(cfg, action).await,
        Command::Cron { action } => cron_cmd(cfg, action).await,
        Command::Update { apply } => update_cmd(apply).await,
    }
}

async fn update_cmd(apply: bool) -> Result<()> {
    let api = "https://api.github.com/repos/MerlionOS/merlion-agent/releases/latest";
    let client = reqwest::Client::builder()
        .user_agent(concat!("merlion/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let resp = client.get(api).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "github releases api {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    let body: serde_json::Value = resp.json().await?;
    let latest = body.get("tag_name").and_then(|v| v.as_str()).unwrap_or("(unknown)");
    let current_full = format!("v{}", env!("CARGO_PKG_VERSION"));
    println!("installed:  {current_full}");
    println!("latest tag: {latest}");
    if latest == current_full {
        println!("already up to date.");
        return Ok(());
    }

    if !apply {
        println!();
        println!("To upgrade:");
        println!("  merlion update --apply        # download + swap the binary (Unix)");
        println!("  cargo binstall merlion --version {}", latest.trim_start_matches('v'));
        println!("  brew upgrade merlion          # if installed via Homebrew");
        println!(
            "  curl -fsSL https://raw.githubusercontent.com/MerlionOS/merlion-agent/main/scripts/install.sh | bash"
        );
        return Ok(());
    }

    // --apply path: download tarball, extract, swap.
    if cfg!(windows) {
        anyhow::bail!(
            "`merlion update --apply` is unix-only for now; on Windows please run the installer manually"
        );
    }
    let target = detect_target_triple().context("could not infer this binary's target triple")?;
    let assets = body.get("assets").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let asset_name = format!("merlion-{target}.tar.gz");
    let asset = assets
        .iter()
        .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(&asset_name))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no release asset named `{asset_name}` on tag {latest}; available: {:?}",
                assets.iter().filter_map(|a| a.get("name").and_then(|n| n.as_str())).collect::<Vec<_>>()
            )
        })?;
    let url = asset
        .get("browser_download_url")
        .and_then(|u| u.as_str())
        .ok_or_else(|| anyhow::anyhow!("asset missing browser_download_url"))?;

    println!("downloading {url}");
    let tarball = client.get(url).send().await?.error_for_status()?.bytes().await?;

    let tmp = tempfile::tempdir().context("create temp dir")?;
    let tar_path = tmp.path().join(&asset_name);
    std::fs::write(&tar_path, &tarball).context("write tarball")?;
    println!("extracting…");
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&tar_path)
        .current_dir(tmp.path())
        .status()
        .context("tar -xzf failed (is `tar` installed?)")?;
    if !status.success() {
        anyhow::bail!("tar -xzf exited with {status}");
    }

    let extracted = tmp.path().join(format!("merlion-{target}")).join("merlion");
    if !extracted.exists() {
        anyhow::bail!(
            "extracted layout unexpected — expected {} to exist",
            extracted.display()
        );
    }

    let current_exe = std::env::current_exe().context("current_exe")?;
    println!("replacing {} → {}", current_exe.display(), extracted.display());
    // rename across filesystems can fail; fall back to copy+remove.
    if std::fs::rename(&extracted, &current_exe).is_err() {
        std::fs::copy(&extracted, &current_exe).context("copy new binary into place")?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&current_exe)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&current_exe, perms)?;
    }
    println!("upgraded to {latest}. Restart any running merlion processes.");
    Ok(())
}

/// Best-effort detection of `cargo` target triple for this binary. Used by
/// `merlion update --apply` to pick the right release tarball.
fn detect_target_triple() -> Option<String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu".into()),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu".into()),
        ("macos", "x86_64") => Some("x86_64-apple-darwin".into()),
        ("macos", "aarch64") => Some("aarch64-apple-darwin".into()),
        _ => None,
    }
}

fn model_cmd(mut cfg: Config, id: Option<String>) -> Result<()> {
    match id {
        None => {
            println!("{}", cfg.model.id);
        }
        Some(new_id) => {
            cfg.model.id = new_id;
            let path = merlion_config::save(&cfg)?;
            println!("Set model = {} ({})", cfg.model.id, path.display());
        }
    }
    Ok(())
}

fn config_cmd(cfg: Config, action: Option<ConfigAction>) -> Result<()> {
    match action.unwrap_or(ConfigAction::Show) {
        ConfigAction::Show => {
            print!("{}", serde_yaml::to_string(&cfg)?);
        }
        ConfigAction::Path => {
            let home = merlion_config::merlion_home();
            println!("{}", home.join("config.yaml").display());
        }
    }
    Ok(())
}

fn doctor(cfg: Config) -> Result<()> {
    use std::process::Command;

    println!("merlion-agent {}", env!("CARGO_PKG_VERSION"));
    let home = merlion_config::merlion_home();
    println!("home:    {}", home.display());

    // Core config
    println!("\n— config —");
    println!("model:        {}", cfg.model.id);
    let provider = cfg.resolve_provider()?;
    println!("base_url:     {}", provider.base_url);
    println!("api key env:  {}", provider.api_key_env);
    let has_key = std::env::var(&provider.api_key_env).is_ok();
    println!(
        "api key:      {}",
        if has_key { "found".into() } else { format!("MISSING ({})", provider.api_key_env) }
    );

    // External tools merlion shells out to
    println!("\n— external tools —");
    for tool in ["rg", "grep", "git", "bash", "curl"] {
        let found = Command::new("which").arg(tool).output().ok().filter(|o| o.status.success());
        match found {
            Some(o) => {
                let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
                println!("{:<6} {}", tool, path);
            }
            None => println!("{:<6} MISSING", tool),
        }
    }

    // Stores
    println!("\n— stores —");
    let session_db = home.join("sessions.db");
    println!("sessions.db   {}", if session_db.exists() { "ok" } else { "(none yet)" });
    let mem_dir = home.join("memory");
    println!("memory/       {}", if mem_dir.exists() { "ok" } else { "(none yet)" });
    let skills_dir = home.join("skills");
    let bundled_skills = std::env::current_dir().ok().map(|p| p.join("skills"));
    println!(
        "skills/       user={} bundled={}",
        if skills_dir.exists() { "ok" } else { "none" },
        bundled_skills.as_ref().filter(|p| p.exists()).map(|_| "ok").unwrap_or("none"),
    );

    // MCP
    println!("\n— mcp servers —");
    match merlion_mcp::McpRegistry::load_default() {
        Ok(reg) if reg.servers.is_empty() => {
            println!("(none configured — `merlion mcp add`)");
        }
        Ok(reg) => {
            for (name, entry) in &reg.servers {
                let status = if entry.enabled { "enabled " } else { "disabled" };
                println!("{status}  {name}");
            }
        }
        Err(e) => println!("(error loading mcp.yaml: {e})"),
    }

    // Gateway env
    println!("\n— gateway —");
    for (label, var) in [
        ("Telegram token", "TELEGRAM_BOT_TOKEN"),
        ("Discord token", "DISCORD_BOT_TOKEN"),
        ("Slack app token", "SLACK_APP_TOKEN"),
        ("Slack bot token", "SLACK_BOT_TOKEN"),
    ] {
        let set = std::env::var(var).is_ok();
        println!("{:<18} {}", format!("{label}:"), if set { "set" } else { "MISSING" });
    }

    // Cron
    println!("\n— cron —");
    match merlion_cron::CronRegistry::load_default() {
        Ok(reg) if reg.jobs.is_empty() => println!("(no jobs scheduled)"),
        Ok(reg) => {
            for j in &reg.jobs {
                let status = if j.enabled { "enabled " } else { "disabled" };
                println!("{status}  {}  ({})", j.name, j.schedule);
            }
        }
        Err(e) => println!("(error loading cron.yaml: {e})"),
    }

    Ok(())
}

fn sessions_cmd(action: Option<SessionsAction>) -> Result<()> {
    let db = SessionDB::open_default()?;
    match action.unwrap_or(SessionsAction::List { limit: 20 }) {
        SessionsAction::List { limit } => {
            for row in db.list_sessions(limit)? {
                println!(
                    "{}  msgs={:<4}  {}  {}",
                    row.id,
                    row.message_count,
                    row.updated_at.format("%Y-%m-%d %H:%M"),
                    row.title.unwrap_or_else(|| "(untitled)".into())
                );
            }
        }
        SessionsAction::Search { query } => {
            for (sid, snip) in db.search(&query, 20)? {
                println!("{sid}\t{snip}");
            }
        }
    }
    Ok(())
}

async fn mcp_cmd(action: McpAction) -> Result<()> {
    let path = McpRegistry::default_path();
    let mut reg = McpRegistry::load(&path)
        .map_err(|e| anyhow::anyhow!("loading {}: {e}", path.display()))?;
    match action {
        McpAction::List => {
            if reg.servers.is_empty() {
                println!("(no MCP servers configured — see `merlion mcp add --help`)");
                return Ok(());
            }
            for (name, entry) in &reg.servers {
                let status = if entry.enabled { "enabled " } else { "disabled" };
                match &entry.transport {
                    TransportSpec::Stdio { command, args, .. } => {
                        let argline = args.iter().cloned().collect::<Vec<_>>().join(" ");
                        println!("{status}  {name}\t stdio: {command} {argline}");
                    }
                    TransportSpec::Http { url, bearer_env, oauth_client_id: _ } => {
                        let auth = bearer_env.as_deref().unwrap_or("(none)");
                        println!("{status}  {name}\t http:  {url} (auth env: {auth})");
                    }
                }
            }
        }
        McpAction::Add { name, command } => {
            if command.is_empty() {
                anyhow::bail!("missing command — usage: `merlion mcp add <name> -- <cmd> <args...>`");
            }
            let program = command[0].clone();
            let args: Vec<String> = command.into_iter().skip(1).collect();
            reg.add(&name, ServerEntry::stdio(program, args));
            reg.save(&path)
                .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
            println!("added MCP server `{name}` to {}", path.display());
        }
        McpAction::AddHttp { name, url, bearer_env } => {
            let mut entry = ServerEntry::http(url.clone());
            if let TransportSpec::Http { bearer_env: be, .. } = &mut entry.transport {
                *be = bearer_env;
            }
            reg.add(&name, entry);
            reg.save(&path).map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
            println!("added HTTP MCP server `{name}` ({url}) to {}", path.display());
        }
        McpAction::Remove { name } => match reg.remove(&name) {
            Some(_) => {
                reg.save(&path)
                    .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
                println!("removed `{name}`");
            }
            None => println!("(no server named `{name}`)"),
        },
        McpAction::Enable { name } => {
            toggle_server(&mut reg, &name, true)?;
            reg.save(&path)
                .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
            println!("enabled `{name}`");
        }
        McpAction::Disable { name } => {
            toggle_server(&mut reg, &name, false)?;
            reg.save(&path)
                .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
            println!("disabled `{name}`");
        }
        McpAction::Test { name } => {
            let entry = reg
                .servers
                .get(&name)
                .ok_or_else(|| anyhow::anyhow!("no server named `{name}` (try `merlion mcp list`)"))?;
            let client = connect_server(&name, entry).await?;
            let info = client.initialize().await.context("initialize handshake")?;
            let server_name = info
                .server_info
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or("(unknown)");
            println!("ok — server `{server_name}` (protocol {})", info.protocol_version);
            let tools = client.list_tools().await.context("list_tools")?;
            if tools.is_empty() {
                println!("(server exposes no tools)");
            } else {
                println!("{} tool(s):", tools.len());
                for t in tools {
                    let desc = t.description.as_deref().unwrap_or("");
                    println!("  - {} — {desc}", t.name);
                }
            }
            let _ = client.close().await;
        }
        McpAction::Oauth { name, scope } => {
            use merlion_mcp::{OauthFlow, TokenStore};
            let entry = reg.servers.get(&name).ok_or_else(|| {
                anyhow::anyhow!("no server named `{name}` (try `merlion mcp list`)")
            })?;
            let (url, client_id) = match &entry.transport {
                TransportSpec::Http { url, oauth_client_id, .. } => {
                    (url.clone(), oauth_client_id.clone())
                }
                TransportSpec::Stdio { .. } => {
                    anyhow::bail!("server `{name}` is stdio — OAuth applies to HTTP transports only");
                }
            };
            let flow = OauthFlow { server_url: url, static_client_id: client_id, scopes: scope };
            let tokens = flow
                .authorize()
                .await
                .map_err(|e| anyhow::anyhow!("oauth flow: {e}"))?;
            let store_path = TokenStore::default_path();
            let mut store = TokenStore::load(&store_path)
                .map_err(|e| anyhow::anyhow!("load token store: {e}"))?;
            let expires = tokens.expires_at;
            store.set(name.clone(), tokens);
            store
                .save(&store_path)
                .map_err(|e| anyhow::anyhow!("save token store: {e}"))?;
            println!(
                "saved token for `{name}` to {} (expires_at: {:?})",
                store_path.display(),
                expires
            );
        }
    }
    Ok(())
}

fn toggle_server(reg: &mut McpRegistry, name: &str, enabled: bool) -> Result<()> {
    let entry = reg
        .servers
        .get_mut(name)
        .ok_or_else(|| anyhow::anyhow!("no server named `{name}` (try `merlion mcp list`)"))?;
    entry.enabled = enabled;
    Ok(())
}

/// Connect to one configured server, list its tools, and register a
/// `McpProxyTool` for each into the given registry. Returns the live
/// [`McpClient`] so the caller can keep it alive for the duration of the
/// session (dropping it would kill the child process).
async fn autoload_server(
    server_name: &str,
    entry: &ServerEntry,
    tools: &mut ToolRegistry,
) -> Result<Arc<McpClient>> {
    let client = Arc::new(connect_server(server_name, entry).await?);
    client
        .initialize()
        .await
        .map_err(|e| anyhow::anyhow!("initialize: {e}"))?;
    let remote_tools = client
        .list_tools()
        .await
        .map_err(|e| anyhow::anyhow!("list_tools: {e}"))?;
    for t in remote_tools {
        let exposed = make_exposed_name(server_name, &t.name);
        let proxy = McpProxyTool::new(client.clone(), exposed, t);
        tools.register_arc(Arc::new(proxy));
    }
    Ok(client)
}

async fn connect_server(server_name: &str, entry: &ServerEntry) -> Result<McpClient> {
    use merlion_mcp::{HttpTransport, TokenStore};
    let transport: Box<dyn merlion_mcp::Transport> = match &entry.transport {
        TransportSpec::Stdio { command, args, env } => {
            let env_vec: Vec<(String, String)> =
                env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let t = StdioTransport::spawn(command, args, &env_vec)
                .await
                .map_err(|e| anyhow::anyhow!("spawn `{command}`: {e}"))?;
            Box::new(t)
        }
        TransportSpec::Http { url, bearer_env, oauth_client_id: _ } => {
            let mut t = HttpTransport::new(url.clone())
                .map_err(|e| anyhow::anyhow!("http transport `{url}`: {e}"))?;
            // Prefer a cached OAuth token if one exists for this server.
            let store = TokenStore::load(&TokenStore::default_path()).ok();
            if let Some(tok) = store.as_ref().and_then(|s| s.get(server_name)) {
                t = t.with_bearer(tok.access_token.clone());
            } else if let Some(env_var) = bearer_env.as_deref() {
                match std::env::var(env_var) {
                    Ok(tok) => t = t.with_bearer(tok),
                    Err(_) => {
                        eprintln!("warning: bearer env `{env_var}` not set; mcp `{url}` will go unauth");
                    }
                }
            }
            Box::new(t)
        }
    };
    Ok(McpClient::new(transport))
}

async fn gateway_cmd(cfg: Config, action: GatewayAction) -> Result<()> {
    match action {
        GatewayAction::Status => {
            let tg_tok = std::env::var("TELEGRAM_BOT_TOKEN").is_ok();
            let dc_tok = std::env::var("DISCORD_BOT_TOKEN").is_ok();
            let tg_allow = std::env::var("MERLION_GATEWAY_ALLOW_TELEGRAM").is_ok();
            let dc_allow = std::env::var("MERLION_GATEWAY_ALLOW_DISCORD").is_ok();
            let allow_all = std::env::var("MERLION_GATEWAY_ALLOW_ALL").is_ok();
            println!("Telegram:");
            println!("  TELEGRAM_BOT_TOKEN               {}", yes_no(tg_tok));
            println!("  MERLION_GATEWAY_ALLOW_TELEGRAM   {}", yes_no(tg_allow || allow_all));
            println!("Discord:");
            println!("  DISCORD_BOT_TOKEN                {}", yes_no(dc_tok));
            println!("  MERLION_GATEWAY_ALLOW_DISCORD    {}", yes_no(dc_allow || allow_all));
            println!();
            println!("Set `MERLION_GATEWAY_ALLOW_ALL=1` to admit any user (development only).");
            Ok(())
        }
        GatewayAction::Start => start_gateways(cfg).await,
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "set" } else { "MISSING" }
}

/// Start every gateway whose env-var config is present. Each gateway gets a
/// dedicated [`Dispatcher`] instance — they share the underlying [`Agent`]
/// and `SessionDB` (sessions are keyed on `(platform, user_id)` so they
/// can't collide), but the message-routing channels are per-platform.
async fn start_gateways(cfg: Config) -> Result<()> {
    use merlion_gateway::{Allowlist, DiscordGateway, Gateway, SlackGateway, TelegramGateway};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let provider = cfg.resolve_provider()?;
    let api_key = std::env::var(&provider.api_key_env).ok();
    let llm: Arc<dyn LlmClient> = match provider.wire {
        Wire::OpenAi => Arc::new(OpenAiClient::new(provider.base_url.clone(), api_key)?),
        Wire::Anthropic => Arc::new(AnthropicClient::new(provider.base_url.clone(), api_key)?),
        Wire::Gemini => Arc::new(GeminiClient::new(provider.base_url.clone(), api_key)?),
        Wire::Bedrock => Arc::new(BedrockClient::from_env()?),
        Wire::Vertex => Arc::new(VertexClient::from_env()?),
    };

    let mut tools = ToolRegistry::new();
    merlion_tools::register_defaults(&mut tools);

    let mut options = AgentOptions::default();
    options.model = provider.model.clone();
    options.temperature = cfg.model.temperature;
    options.max_tokens = cfg.model.max_tokens;
    options.max_iterations = cfg.max_iterations;
    let approver: Arc<dyn ToolApprover> = Arc::new(merlion_core::AllowAllApprover);
    let agent = Arc::new(Agent::new(llm, tools, options).with_approver(approver));

    let db = Arc::new(Mutex::new(SessionDB::open_default()?));
    let allowlist = Allowlist::from_env();
    let system_prompt = cfg.system_prompt.clone().unwrap_or_else(|| {
        "You are Merlion, a coding agent reachable over messaging platforms. \
         Reply succinctly; messaging UIs don't render tool output. \
         Use the `memory` tool to remember durable facts."
            .into()
    });

    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut started: Vec<&'static str> = Vec::new();

    if std::env::var("TELEGRAM_BOT_TOKEN").is_ok() {
        let (tx, rx, dispatcher_task) =
            spawn_dispatcher(&agent, &db, &system_prompt, &allowlist);
        tasks.push(dispatcher_task);
        let gw = Arc::new(TelegramGateway::from_env()?);
        let name = gw.name();
        started.push(name);
        tasks.push(tokio::spawn(async move {
            if let Err(e) = gw.run(tx, rx).await {
                eprintln!("telegram gateway exited: {e}");
            }
        }));
    }
    if std::env::var("DISCORD_BOT_TOKEN").is_ok() {
        let (tx, rx, dispatcher_task) =
            spawn_dispatcher(&agent, &db, &system_prompt, &allowlist);
        tasks.push(dispatcher_task);
        let gw = Arc::new(DiscordGateway::from_env()?);
        let name = gw.name();
        started.push(name);
        tasks.push(tokio::spawn(async move {
            if let Err(e) = gw.run(tx, rx).await {
                eprintln!("discord gateway exited: {e}");
            }
        }));
    }
    if std::env::var("SLACK_APP_TOKEN").is_ok() && std::env::var("SLACK_BOT_TOKEN").is_ok() {
        let (tx, rx, dispatcher_task) =
            spawn_dispatcher(&agent, &db, &system_prompt, &allowlist);
        tasks.push(dispatcher_task);
        let gw = Arc::new(SlackGateway::from_env()?);
        let name = gw.name();
        started.push(name);
        tasks.push(tokio::spawn(async move {
            if let Err(e) = gw.run(tx, rx).await {
                eprintln!("slack gateway exited: {e}");
            }
        }));
    }

    if started.is_empty() {
        anyhow::bail!(
            "no gateway env vars detected. Set TELEGRAM_BOT_TOKEN, DISCORD_BOT_TOKEN, or \
             SLACK_APP_TOKEN+SLACK_BOT_TOKEN. See `merlion gateway status`."
        );
    }
    println!("gateway started: {}", started.join(", "));

    // Wait for any task to exit; the rest will get cleaned up on drop.
    if !tasks.is_empty() {
        let _ = futures::future::select_all(tasks).await;
    }
    Ok(())
}

fn spawn_dispatcher(
    agent: &std::sync::Arc<Agent>,
    db: &std::sync::Arc<tokio::sync::Mutex<SessionDB>>,
    system_prompt: &str,
    allowlist: &merlion_gateway::Allowlist,
) -> (
    tokio::sync::mpsc::Sender<merlion_gateway::IncomingMessage>,
    tokio::sync::mpsc::Receiver<merlion_gateway::OutgoingMessage>,
    tokio::task::JoinHandle<()>,
) {
    use std::sync::Arc;
    use tokio::sync::mpsc;
    let (incoming_tx, incoming_rx) = mpsc::channel(64);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(64);
    let dispatcher = Arc::new(merlion_gateway::Dispatcher::new(
        agent.clone(),
        db.clone(),
        system_prompt.to_string(),
        allowlist.clone(),
    ));
    let task = tokio::spawn(async move {
        if let Err(e) = dispatcher.run(incoming_rx, outgoing_tx).await {
            eprintln!("dispatcher exited: {e}");
        }
    });
    (incoming_tx, outgoing_rx, task)
}

async fn cron_cmd(cfg: Config, action: CronAction) -> Result<()> {
    use merlion_cron::{CronRegistry, Job};
    let path = CronRegistry::default_path();
    let mut reg = CronRegistry::load(&path).map_err(|e| anyhow::anyhow!("loading cron: {e}"))?;
    match action {
        CronAction::List => {
            if reg.jobs.is_empty() {
                println!("(no jobs configured)");
                return Ok(());
            }
            for j in &reg.jobs {
                let status = if j.enabled { "enabled " } else { "disabled" };
                println!("{status}  {}\t{}\t→ {}\n  prompt: {}", j.name, j.schedule, j.destination, j.prompt);
            }
            Ok(())
        }
        CronAction::Add { name, schedule, prompt, destination } => {
            let job = Job { name: name.clone(), schedule, prompt, enabled: true, destination };
            reg.add(job).map_err(|e| anyhow::anyhow!("add: {e}"))?;
            reg.save(&path).map_err(|e| anyhow::anyhow!("save: {e}"))?;
            println!("added cron job `{name}`");
            Ok(())
        }
        CronAction::Remove { name } => {
            if reg.remove(&name).is_some() {
                reg.save(&path).map_err(|e| anyhow::anyhow!("save: {e}"))?;
                println!("removed `{name}`");
            } else {
                println!("(no job named `{name}`)");
            }
            Ok(())
        }
        CronAction::Run { name } => {
            use merlion_cron::scheduler::JobRunner;
            let job = reg
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no job named `{name}`"))?;
            let runner = build_cli_runner(cfg).await?;
            runner.run_job(&job).await;
            Ok(())
        }
        CronAction::Daemon => {
            let runner = build_cli_runner(cfg).await?;
            let scheduler = merlion_cron::Scheduler::new(reg, std::sync::Arc::new(runner));
            scheduler.run().await.map_err(|e| anyhow::anyhow!("scheduler: {e}"))?;
            Ok(())
        }
    }
}

/// Build a cron JobRunner that constructs an Agent from `cfg`, runs the
/// prompt, and prints the response to stdout. (Destination-aware delivery
/// is a Phase 5.6 follow-up.)
async fn build_cli_runner(cfg: Config) -> Result<CliJobRunner> {
    let provider = cfg.resolve_provider()?;
    let api_key = std::env::var(&provider.api_key_env).ok();
    let llm: Arc<dyn LlmClient> = match provider.wire {
        Wire::OpenAi => Arc::new(OpenAiClient::new(provider.base_url.clone(), api_key)?),
        Wire::Anthropic => Arc::new(AnthropicClient::new(provider.base_url.clone(), api_key)?),
        Wire::Gemini => Arc::new(GeminiClient::new(provider.base_url.clone(), api_key)?),
        Wire::Bedrock => Arc::new(BedrockClient::from_env()?),
        Wire::Vertex => Arc::new(VertexClient::from_env()?),
    };
    let mut tools = ToolRegistry::new();
    merlion_tools::register_defaults(&mut tools);
    let mut options = AgentOptions::default();
    options.model = provider.model.clone();
    options.max_iterations = cfg.max_iterations;
    let approver: Arc<dyn ToolApprover> = Arc::new(merlion_core::AllowAllApprover);
    let agent = Agent::new(llm, tools, options).with_approver(approver);
    let system_prompt = cfg
        .system_prompt
        .clone()
        .unwrap_or_else(|| "You are Merlion, running a scheduled task non-interactively.".into());
    Ok(CliJobRunner { agent: Arc::new(agent), system_prompt })
}

struct CliJobRunner {
    agent: Arc<Agent>,
    system_prompt: String,
}

#[async_trait::async_trait]
impl merlion_cron::scheduler::JobRunner for CliJobRunner {
    async fn run_job(&self, job: &merlion_cron::Job) {
        let mut messages =
            vec![Message::system(&self.system_prompt), Message::user(&job.prompt)];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
        let agent = self.agent.clone();
        let task = tokio::spawn(async move {
            let _ = agent.run(&mut messages, tx).await;
            messages
        });
        let mut reply = String::new();
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::AssistantMessage(m) = ev {
                if let Some(c) = m.content {
                    reply.push_str(&c);
                }
            }
        }
        let _ = task.await;

        if reply.trim().is_empty() {
            reply = "(merlion produced no text reply)".into();
        }
        let when = chrono::Utc::now().to_rfc3339();

        match deliver_cron_result(&job.destination, &job.name, &reply).await {
            Ok(true) => {
                tracing::info!(job = %job.name, dest = %job.destination, "cron result delivered");
            }
            Ok(false) | Err(_) => {
                // Fall back to stdout if delivery failed or destination is "cli".
                println!("[cron {}] @ {when} → {reply}", job.name);
            }
        }
    }
}

/// Push the cron result to its configured destination. Returns Ok(true) if
/// a non-CLI destination accepted the message, Ok(false) for the "cli"
/// destination (caller prints to stdout), or Err on transport failure.
///
/// Supported destinations:
/// - `cli` — print to stdout (the default).
/// - `telegram:<chat_id>` — POST sendMessage with TELEGRAM_BOT_TOKEN.
/// - `discord:<channel_id>` — POST messages with DISCORD_BOT_TOKEN.
async fn deliver_cron_result(destination: &str, job_name: &str, reply: &str) -> Result<bool> {
    let prefix = format!("[cron {job_name}]\n");
    let body = format!("{prefix}{reply}");
    if destination == "cli" {
        return Ok(false);
    }
    if let Some(chat_id) = destination.strip_prefix("telegram:") {
        let token = std::env::var("TELEGRAM_BOT_TOKEN")
            .context("TELEGRAM_BOT_TOKEN not set; cannot deliver to telegram")?;
        let url = format!("https://api.telegram.org/bot{token}/sendMessage");
        let resp = reqwest::Client::new()
            .post(&url)
            .json(&serde_json::json!({"chat_id": chat_id, "text": body}))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("telegram sendMessage {}: {}", resp.status(), resp.text().await.unwrap_or_default());
        }
        Ok(true)
    } else if let Some(channel_id) = destination.strip_prefix("discord:") {
        let token = std::env::var("DISCORD_BOT_TOKEN")
            .context("DISCORD_BOT_TOKEN not set; cannot deliver to discord")?;
        let url = format!("https://discord.com/api/v10/channels/{channel_id}/messages");
        let resp = reqwest::Client::new()
            .post(&url)
            .header("Authorization", format!("Bot {token}"))
            .json(&serde_json::json!({"content": body}))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("discord postMessage {}: {}", resp.status(), resp.text().await.unwrap_or_default());
        }
        Ok(true)
    } else {
        anyhow::bail!(
            "unknown cron destination `{destination}` — use `cli`, `telegram:<chat_id>`, or `discord:<channel_id>`"
        )
    }
}

async fn chat(cfg: Config, resume: Option<String>, want_tui: bool, no_tui: bool) -> Result<()> {
    let provider = cfg.resolve_provider()?;
    let api_key = std::env::var(&provider.api_key_env).ok();
    if api_key.is_none() {
        eprintln!(
            "warning: env var `{}` is not set — requests will be sent without an Authorization header.",
            provider.api_key_env
        );
    }
    let client: Arc<dyn LlmClient> = match provider.wire {
        Wire::OpenAi => Arc::new(OpenAiClient::new(provider.base_url.clone(), api_key)?),
        Wire::Anthropic => Arc::new(AnthropicClient::new(provider.base_url.clone(), api_key)?),
        Wire::Gemini => Arc::new(GeminiClient::new(provider.base_url.clone(), api_key)?),
        Wire::Bedrock => Arc::new(BedrockClient::from_env()?),
        Wire::Vertex => Arc::new(VertexClient::from_env()?),
    };

    let home = merlion_config::merlion_home();
    let memory_store = Arc::new(MemoryStore::open(home.join("memory"))?);
    let skills_dir = home.join("skills");
    std::fs::create_dir_all(&skills_dir).ok();
    let bundled_skills_dir = std::env::current_dir().ok().map(|p| p.join("skills"));
    let skill_roots: Vec<std::path::PathBuf> = bundled_skills_dir
        .into_iter()
        .filter(|p| p.exists())
        .chain(std::iter::once(skills_dir.clone()))
        .collect();
    let skills = SkillSet::load(&skill_roots).unwrap_or_else(|e| {
        eprintln!("warning: failed to load skills: {e}");
        SkillSet::load(&[skills_dir.clone()]).unwrap_or_default()
    });
    let skill_cfg = SkillToolsConfig::new(skills_dir.clone());

    let mut tools = ToolRegistry::new();
    merlion_tools::register_defaults(&mut tools);
    merlion_tools::register_memory(&mut tools, memory_store.clone());
    merlion_tools::register_skill_tools(&mut tools, skill_cfg);
    let task_tool = merlion_tools::register_task_tool(&mut tools);

    // Connect to configured MCP servers and inject their tools. Connection
    // failures are logged, not fatal — one broken server shouldn't take down
    // the agent.
    let mut mcp_clients: Vec<Arc<McpClient>> = Vec::new();
    let mcp_registry = McpRegistry::load_default().unwrap_or_else(|e| {
        eprintln!("warning: could not load MCP registry: {e}");
        McpRegistry::default()
    });
    for (server_name, entry) in mcp_registry.enabled_servers() {
        match autoload_server(server_name, entry, &mut tools).await {
            Ok(client) => mcp_clients.push(client),
            Err(e) => eprintln!("warning: MCP server `{server_name}` failed to load: {e}"),
        }
    }

    let mut options = AgentOptions::default();
    options.model = provider.model.clone();
    options.temperature = cfg.model.temperature;
    options.max_tokens = cfg.model.max_tokens;
    options.max_iterations = cfg.max_iterations;

    let approver: Arc<dyn ToolApprover> = Arc::new(approver::ConsoleApprover::new());
    let agent = Arc::new(Agent::new(client, tools, options).with_approver(approver));
    task_tool.install_agent(&agent);
    let mut curator = Curator::default();

    let db = SessionDB::open_default()?;
    let session_id = match resume {
        Some(id) => id,
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            db.create_session(&id, None)?;
            id
        }
    };
    let mut messages = db.load_messages(&session_id)?;
    if messages.is_empty() {
        let prompt = build_initial_system_prompt(&cfg, &memory_store, &skills);
        let m = Message::system(prompt);
        db.append_message(&session_id, &m)?;
        messages.push(m);
    }

    let is_tty = std::io::stdout().is_terminal();
    let use_tui = if want_tui {
        true
    } else if no_tui {
        false
    } else {
        is_tty
    };
    if use_tui {
        return tui::run(
            &cfg,
            &agent,
            &mut messages,
            &session_id,
            &skills,
            &memory_store,
            &db,
            &mut curator,
        )
        .await;
    }

    println!(
        "merlion — model {} · session {} · {} skills · {} memories · {} MCP server(s)",
        provider.model,
        &session_id[..8],
        skills.len(),
        memory_store.list().map(|v| v.len()).unwrap_or(0),
        mcp_clients.len(),
    );
    println!("Type your message, blank line to end, /exit to quit, /help for commands.");

    let mut rl = rustyline::DefaultEditor::new()?;
    loop {
        let input = match rl.readline("you> ") {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Eof) | Err(rustyline::error::ReadlineError::Interrupted) => {
                break;
            }
            Err(e) => return Err(e.into()),
        };
        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(trimmed);
        match trimmed {
            "/exit" | "/quit" => break,
            "/help" => {
                println!("/exit         — leave the session");
                println!("/new          — start a fresh session");
                println!("/share        — mint a join key to continue this session from a messaging platform");
                println!("/usage        — print message count");
                println!("/model        — print active model");
                println!("/skills       — list available skills");
                println!("/memory       — list memories");
                println!("/<skill-name> — invoke a skill (prepends its body as a turn)");
                continue;
            }
            "/share" => {
                let path = merlion_gateway::JoinKeyStore::default_path();
                let mut store = merlion_gateway::JoinKeyStore::load(&path).unwrap_or_default();
                store.gc();
                let key = store.mint(session_id.clone(), 600);
                if let Err(e) = store.save(&path) {
                    eprintln!("warning: failed to persist join key to {}: {e}", path.display());
                }
                println!("Join key: {key}");
                println!("Send `/join {key}` from Telegram/Discord/Slack within 10 minutes to");
                println!("continue this conversation on that platform.");
                continue;
            }
            "/usage" => {
                println!("messages: {}", messages.len());
                continue;
            }
            "/model" => {
                println!("{}", provider.model);
                continue;
            }
            "/skills" => {
                print!("{}", skills.help_index());
                if !skills.help_index().ends_with('\n') {
                    println!();
                }
                continue;
            }
            "/memory" => {
                match memory_store.list() {
                    Ok(rows) if rows.is_empty() => println!("(no memories saved)"),
                    Ok(rows) => {
                        for r in rows {
                            println!("{} — {}", r.name, r.hook);
                        }
                    }
                    Err(e) => eprintln!("error: {e}"),
                }
                continue;
            }
            "/new" => {
                let new_id = uuid::Uuid::new_v4().to_string();
                db.create_session(&new_id, None)?;
                println!("new session: {new_id}");
                messages.clear();
                let m = Message::system(build_initial_system_prompt(&cfg, &memory_store, &skills));
                db.append_message(&new_id, &m)?;
                messages.push(m);
                continue;
            }
            _ => {}
        }

        // Slash-command skill invocation: /<skill-name> or /<skill-name> <extra text>.
        let user_text = if let Some(rest) = trimmed.strip_prefix('/') {
            let (name, extra) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            match skills.get(name) {
                Some(skill) => {
                    println!("\x1b[2m(invoking skill `{}`)\x1b[0m", skill.name);
                    let extra = extra.trim();
                    if extra.is_empty() {
                        skill.body.clone()
                    } else {
                        format!("{}\n\n---\nUser-supplied arguments: {extra}", skill.body)
                    }
                }
                None => {
                    eprintln!("unknown slash command: /{name} (try /help)");
                    continue;
                }
            }
        } else {
            trimmed.to_string()
        };

        curator.record_user_turn();
        let user_text = if let Some(nudge) = curator.nudge_if_due() {
            format!("<system-reminder>{nudge}</system-reminder>\n\n{user_text}")
        } else {
            user_text
        };

        let user_msg = Message::user(user_text);
        db.append_message(&session_id, &user_msg)?;
        messages.push(user_msg);

        let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
        let mut snapshot = messages.clone();
        let agent_ref = &agent;

        let run_fut = async {
            let res = agent_ref.run(&mut snapshot, tx).await;
            (res, snapshot)
        };
        let render_fut = async {
            print!("merlion> ");
            std::io::stdout().flush().ok();
            while let Some(ev) = rx.recv().await {
                match ev {
                    AgentEvent::AssistantDelta(s) => {
                        print!("{s}");
                        std::io::stdout().flush().ok();
                    }
                    AgentEvent::AssistantMessage(_) => {
                        println!();
                    }
                    AgentEvent::ToolCallStart { name, arguments, .. } => {
                        let preview = preview_args(&arguments);
                        println!("\x1b[2m· tool {name} {preview}\x1b[0m");
                    }
                    AgentEvent::ToolCallFinish { is_error, content, .. } => {
                        let head = content.lines().next().unwrap_or("").chars().take(120).collect::<String>();
                        let tag = if is_error { "ERR" } else { "ok" };
                        println!("\x1b[2m  ↪ {tag}: {head}\x1b[0m");
                        print!("merlion> ");
                        std::io::stdout().flush().ok();
                    }
                    AgentEvent::IterationBudgetExhausted => {
                        println!("\n[iteration budget exhausted]");
                    }
                    AgentEvent::Done => {}
                    AgentEvent::Usage(_) => {}
                }
            }
        };

        let ((res, new_messages), _) = tokio::join!(run_fut, render_fut);

        for m in new_messages.iter().skip(messages.len()) {
            db.append_message(&session_id, m)?;
        }
        messages = new_messages;

        if let Err(e) = res {
            eprintln!("error: {e}");
        }
    }

    Ok(())
}

fn preview_args(v: &serde_json::Value) -> String {
    let s = v.to_string();
    let max = 80;
    if s.len() <= max {
        s
    } else {
        format!("{}…", &s[..max])
    }
}

const DEFAULT_SYSTEM_PROMPT: &str =
    "You are Merlion, a coding agent. You have access to tools: bash, read, write, edit, ls, \
     grep, glob, web_fetch, memory, skill_create, skill_update. Prefer small, verifiable steps. \
     Use the `memory` tool to remember durable facts about the user, their project, or their \
     preferences — these persist across sessions. Create a skill with `skill_create` when you \
     discover a repeatable workflow worth naming. When you finish, stop calling tools and reply \
     in plain text.";

fn build_initial_system_prompt(cfg: &Config, memory: &MemoryStore, skills: &SkillSet) -> String {
    let base = cfg.system_prompt.as_deref().unwrap_or(DEFAULT_SYSTEM_PROMPT);
    let mut out = String::from(base);
    let mem_block = memory.render_context_block(2048).unwrap_or_default();
    if !mem_block.trim().is_empty() {
        out.push_str("\n\n");
        out.push_str(&mem_block);
    }
    if !skills.is_empty() {
        out.push_str("\n\n# Available skills\n");
        out.push_str(
            "The user can invoke any of these with `/<name>` and you will see the skill body \
             appended to their next message. You can also reference them when suggesting next \
             steps.\n",
        );
        out.push_str(&skills.help_index());
    }
    out
}

