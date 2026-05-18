use std::io::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use merlion_config::{Config, Wire};
use merlion_core::{Agent, AgentEvent, AgentOptions, LlmClient, Message, ToolRegistry};
use merlion_llm::{AnthropicClient, GeminiClient, OpenAiClient};
use merlion_session::SessionDB;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

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

    match cli.command.unwrap_or(Command::Chat { session: None }) {
        Command::Chat { session } => chat(cfg, session).await,
        Command::Model { id } => model_cmd(cfg, id),
        Command::Config { action } => config_cmd(cfg, action),
        Command::Doctor => doctor(cfg),
        Command::Sessions { action } => sessions_cmd(action),
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

async fn chat(cfg: Config, resume: Option<String>) -> Result<()> {
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

    let mut tools = ToolRegistry::new();
    merlion_tools::register_defaults(&mut tools);

    let mut options = AgentOptions::default();
    options.model = provider.model.clone();
    options.temperature = cfg.model.temperature;
    options.max_tokens = cfg.model.max_tokens;
    options.max_iterations = cfg.max_iterations;

    let agent = Agent::new(client, tools, options);

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
        if let Some(prompt) = cfg.system_prompt.as_deref() {
            let m = Message::system(prompt);
            db.append_message(&session_id, &m)?;
            messages.push(m);
        } else {
            let m = Message::system(default_system_prompt());
            db.append_message(&session_id, &m)?;
            messages.push(m);
        }
    }

    println!("merlion — model {} · session {}", provider.model, &session_id[..8]);
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
                println!("/exit  — leave the session");
                println!("/new   — start a fresh session");
                println!("/usage — print message count");
                println!("/model — print active model");
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
            "/new" => {
                let new_id = uuid::Uuid::new_v4().to_string();
                db.create_session(&new_id, None)?;
                println!("new session: {new_id}");
                messages.clear();
                let m = Message::system(cfg.system_prompt.as_deref().unwrap_or(default_system_prompt()));
                db.append_message(&new_id, &m)?;
                messages.push(m);
                continue;
            }
            _ => {}
        }

        let user_msg = Message::user(trimmed.to_string());
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

fn default_system_prompt() -> &'static str {
    "You are Merlion, a coding agent. You have access to tools: bash, read, write, edit, ls. \
     Prefer small, verifiable steps. When you finish, stop calling tools and reply in plain text."
}
