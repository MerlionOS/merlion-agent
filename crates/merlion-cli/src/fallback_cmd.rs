//! `merlion fallback` — manage the provider fallback chain.
//!
//! The chain is stored at `~/.merlion/fallback.yaml` (see
//! [`merlion_config::FallbackChain`]). When non-empty, runtime LLM
//! construction sites in `main.rs` wrap their primary client with a
//! [`merlion_llm::FallbackLlmClient`] that transparently falls through to
//! the next provider on a retriable 429 / 5xx error.

use anyhow::{anyhow, Result};
use clap::Subcommand;
use merlion_config::{Config, FallbackChain, ModelConfig};

#[derive(Debug, Subcommand)]
pub enum FallbackAction {
    /// Print the current chain, numbered. The primary model from
    /// `config.yaml` is *not* shown here — only the fallback list.
    List,
    /// Append a `provider:model` id to the chain (no-op if already present).
    Add {
        /// e.g. `openrouter:anthropic/claude-sonnet-4`
        provider_model: String,
    },
    /// Remove a `provider:model` id from the chain by exact match.
    Remove { provider_model: String },
    /// Wipe the chain (deletes the saved chain entries, leaves the file
    /// in place with an empty list).
    Clear,
}

pub async fn run(action: FallbackAction) -> Result<()> {
    match action {
        FallbackAction::List => list(),
        FallbackAction::Add { provider_model } => add(provider_model),
        FallbackAction::Remove { provider_model } => remove(provider_model),
        FallbackAction::Clear => clear(),
    }
}

fn list() -> Result<()> {
    let chain = FallbackChain::load()?;
    if chain.chain.is_empty() {
        println!("(no fallback chain configured)");
        println!();
        println!("Add entries with: merlion fallback add <provider:model>");
        return Ok(());
    }
    println!("Fallback chain ({} entries):", chain.chain.len());
    let width = chain.chain.len().to_string().len();
    for (i, id) in chain.chain.iter().enumerate() {
        println!("  {:>width$}. {id}", i + 1, width = width);
    }
    Ok(())
}

fn add(provider_model: String) -> Result<()> {
    validate_provider_model(&provider_model)?;

    let mut chain = FallbackChain::load()?;
    if chain.chain.iter().any(|s| s == &provider_model) {
        println!("`{provider_model}` is already in the chain — nothing to do.");
        return Ok(());
    }
    chain.chain.push(provider_model.clone());
    let path = chain.save()?;
    println!(
        "Added `{provider_model}` to fallback chain ({}).",
        path.display()
    );
    Ok(())
}

fn remove(provider_model: String) -> Result<()> {
    let mut chain = FallbackChain::load()?;
    let before = chain.chain.len();
    chain.chain.retain(|s| s != &provider_model);
    if chain.chain.len() == before {
        return Err(anyhow!(
            "`{provider_model}` is not in the chain. Run `merlion fallback list` to see current entries."
        ));
    }
    let path = chain.save()?;
    println!(
        "Removed `{provider_model}` from fallback chain ({}).",
        path.display()
    );
    Ok(())
}

fn clear() -> Result<()> {
    let chain = FallbackChain { chain: Vec::new() };
    let path = chain.save()?;
    println!("Cleared fallback chain ({}).", path.display());
    Ok(())
}

/// Confirm the id parses as a known provider preset by feeding it through
/// `Config::resolve_provider`. This catches typos like `openroute:...` at
/// `merlion fallback add` time rather than at runtime fallthrough.
fn validate_provider_model(id: &str) -> Result<()> {
    let probe = Config {
        model: ModelConfig {
            id: id.to_string(),
            base_url: None,
            api_key_env: None,
            temperature: None,
            max_tokens: None,
        },
        ..Config::default()
    };
    probe
        .resolve_provider()
        .map_err(|e| anyhow!("invalid provider id `{id}`: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_known_providers() {
        validate_provider_model("openrouter:anthropic/claude-sonnet-4").unwrap();
        validate_provider_model("anthropic:claude-opus-4-7").unwrap();
        validate_provider_model("openai:gpt-4o-mini").unwrap();
        validate_provider_model("gemini:gemini-1.5-pro").unwrap();
    }

    #[test]
    fn validate_rejects_unknown_provider() {
        let err = validate_provider_model("nonesuch:foo").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nonesuch") || msg.contains("invalid provider id"));
    }
}

// -----------------------------------------------------------------------------
// WIRING SPEC — apply to `crates/merlion-cli/src/main.rs`.
//
// 1. Add a module declaration near the other `mod` lines at the top of main.rs:
//
//        mod fallback_cmd;
//
// 2. Add a new variant to the `Command` enum:
//
//        /// Manage the provider fallback chain.
//        ///
//        /// When the primary LLM (from config.yaml) returns a retriable
//        /// 429 / 5xx error after exhausting its own retries, merlion will
//        /// transparently fall through to the next provider in this chain.
//        /// Stored at `~/.merlion/fallback.yaml`.
//        Fallback {
//            #[command(subcommand)]
//            action: fallback_cmd::FallbackAction,
//        },
//
// 3. Add a dispatch arm in the `match cli.command.unwrap_or(...)` block in
//    `main()`:
//
//        Command::Fallback { action } => fallback_cmd::run(action).await,
//
// 4. Add the helper below at module scope in main.rs. It wraps a primary
//    `Arc<dyn LlmClient>` with `FallbackLlmClient` when the user's chain is
//    non-empty; otherwise returns the primary unchanged. Constructing each
//    fallback entry mirrors the existing per-wire match used by `chat()`,
//    `oneshot_cmd()`, `gateway_cmd()`, and `build_cli_runner()`:
//
//    ```rust
//    use merlion_config::{FallbackChain, ModelConfig};
//    use merlion_llm::FallbackLlmClient;
//
//    /// Wrap `primary` with a fallback chain loaded from
//    /// `~/.merlion/fallback.yaml`. The primary is `cfg.model.id`; each
//    /// entry in the chain is a `"provider:model"` string resolved through
//    /// the same `ModelConfig` -> `resolve_provider` -> wire-typed client
//    /// path that the four LLM construction sites use today.
//    ///
//    /// Returns `primary` unchanged when the chain is empty or fails to
//    /// load (we log the error but don't fail the whole command — a broken
//    /// fallback file should not prevent chat from starting).
//    fn wrap_with_fallback(primary: Arc<dyn LlmClient>, cfg: &Config) -> Arc<dyn LlmClient> {
//        let chain_cfg = match FallbackChain::load() {
//            Ok(c) => c,
//            Err(e) => {
//                tracing::warn!(error = %e, "failed to load fallback chain; using primary only");
//                return primary;
//            }
//        };
//        if chain_cfg.chain.is_empty() {
//            return primary;
//        }
//
//        let mut clients: Vec<Arc<dyn LlmClient>> = Vec::new();
//        let mut names: Vec<String> = vec![cfg.model.id.clone()];
//        for entry in &chain_cfg.chain {
//            let mut entry_cfg = cfg.clone();
//            entry_cfg.model = ModelConfig {
//                id: entry.clone(),
//                base_url: None,
//                api_key_env: None,
//                temperature: cfg.model.temperature,
//                max_tokens: cfg.model.max_tokens,
//            };
//            let provider = match entry_cfg.resolve_provider() {
//                Ok(p) => p,
//                Err(e) => {
//                    tracing::warn!(entry = %entry, error = %e, "skipping invalid fallback entry");
//                    continue;
//                }
//            };
//            let api_key = std::env::var(&provider.api_key_env).ok();
//            let client: Result<Arc<dyn LlmClient>> = (|| -> Result<Arc<dyn LlmClient>> {
//                Ok(match provider.wire {
//                    Wire::OpenAi => Arc::new(OpenAiClient::new(provider.base_url.clone(), api_key)?),
//                    Wire::Anthropic => Arc::new(AnthropicClient::new(provider.base_url.clone(), api_key)?),
//                    Wire::Gemini => Arc::new(GeminiClient::new(provider.base_url.clone(), api_key)?),
//                    Wire::Bedrock => Arc::new(BedrockClient::from_env()?),
//                    Wire::Vertex => Arc::new(VertexClient::from_env()?),
//                })
//            })();
//            match client {
//                Ok(c) => {
//                    clients.push(c);
//                    names.push(entry.clone());
//                }
//                Err(e) => {
//                    tracing::warn!(entry = %entry, error = %e, "failed to build fallback client; skipping");
//                }
//            }
//        }
//
//        if clients.is_empty() {
//            return primary;
//        }
//        Arc::new(FallbackLlmClient::new(primary, clients, names))
//    }
//    ```
//
// 5. Wrap each of the four LLM construction sites with `wrap_with_fallback`.
//    Search for the existing `let client: Arc<dyn LlmClient> = match provider.wire {`
//    and `let llm: Arc<dyn LlmClient> = match provider.wire {` blocks — there
//    are four of them in `chat()`, `oneshot_cmd()`, `gateway_cmd()`, and
//    `build_cli_runner()`. After the match, add one line:
//
//        let client = wrap_with_fallback(client, &cfg);
//        // (or `let llm = wrap_with_fallback(llm, &cfg);` for the sites that
//        // bind to `llm`)
//
//    Note `build_cli_runner` only has `cfg` in scope under that name —
//    confirm before adapting. The CLI runner constructs its `llm` from
//    `cfg.resolve_provider()`, so `cfg` is available.
//
// 6. No new clap derives are required in main.rs — `FallbackAction` already
//    derives `clap::Subcommand` in this file.
// -----------------------------------------------------------------------------
