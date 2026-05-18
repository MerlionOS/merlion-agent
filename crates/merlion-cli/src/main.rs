use std::io::{IsTerminal, Write};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use merlion_config::{Config, Wire};
use merlion_core::{
    Agent, AgentEvent, AgentOptions, Curator, LlmClient, Message, ToolApprover, ToolRegistry,
};
use merlion_llm::{AnthropicClient, GeminiClient, OpenAiClient};
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
    /// Remove a configured server.
    Remove { name: String },
    /// Enable a previously-disabled server.
    Enable { name: String },
    /// Disable a server without removing it.
    Disable { name: String },
    /// Connect to a server, run the initialize handshake, list its tools.
    Test { name: String },
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
    println!("merlion-agent {}", env!("CARGO_PKG_VERSION"));
    let home = merlion_config::merlion_home();
    println!("home:   {}", home.display());
    println!("model:  {}", cfg.model.id);
    let provider = cfg.resolve_provider()?;
    println!("base_url: {}", provider.base_url);
    println!("api key env: {}", provider.api_key_env);
    let has_key = std::env::var(&provider.api_key_env).is_ok();
    println!(
        "api key:   {}",
        if has_key { "found".to_string() } else { format!("MISSING ({})", provider.api_key_env) }
    );
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
            let client = connect_server(entry).await?;
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
    let client = Arc::new(connect_server(entry).await?);
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

async fn connect_server(entry: &ServerEntry) -> Result<McpClient> {
    let transport: Box<dyn merlion_mcp::Transport> = match &entry.transport {
        TransportSpec::Stdio { command, args, env } => {
            let env_vec: Vec<(String, String)> =
                env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let t = StdioTransport::spawn(command, args, &env_vec)
                .await
                .map_err(|e| anyhow::anyhow!("spawn `{command}`: {e}"))?;
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
    use merlion_gateway::{Allowlist, DiscordGateway, Gateway, TelegramGateway};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let provider = cfg.resolve_provider()?;
    let api_key = std::env::var(&provider.api_key_env).ok();
    let llm: Arc<dyn LlmClient> = match provider.wire {
        Wire::OpenAi => Arc::new(OpenAiClient::new(provider.base_url.clone(), api_key)?),
        Wire::Anthropic => Arc::new(AnthropicClient::new(provider.base_url.clone(), api_key)?),
        Wire::Gemini => Arc::new(GeminiClient::new(provider.base_url.clone(), api_key)?),
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

    if started.is_empty() {
        anyhow::bail!(
            "no gateway env vars detected. Set TELEGRAM_BOT_TOKEN and/or DISCORD_BOT_TOKEN.\n\
             See `merlion gateway status`."
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
        println!(
            "[cron {}] @ {} → {}",
            job.name,
            chrono::Utc::now().to_rfc3339(),
            reply
        );
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
    let agent = Agent::new(client, tools, options).with_approver(approver);
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
                println!("/usage        — print message count");
                println!("/model        — print active model");
                println!("/skills       — list available skills");
                println!("/memory       — list memories");
                println!("/<skill-name> — invoke a skill (prepends its body as a turn)");
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

