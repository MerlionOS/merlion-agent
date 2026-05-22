//! `merlion auth` — manage the pooled credential store at
//! `~/.merlion/auth.yaml`.
//!
//! The store keeps multiple API keys per provider so the runtime can rotate
//! when one hits a 429. `auth add` is the recommended way to register a key;
//! the first key added for a provider is also mirrored into `~/.merlion/.env`
//! so legacy code paths that read `<PROVIDER>_API_KEY` from the environment
//! keep working untouched.
//!
//! See the wiring spec at the bottom of this file for how this hooks into
//! `main.rs`.

// The Command variant + dispatch arm in `main.rs` are intentionally left for
// the wiring step at the bottom of this file. Until then, the public surface
// here is unreachable from the binary entry point — suppress dead-code so
// `cargo clippy -- -D warnings` stays green.
#![allow(dead_code)]

use std::fs::OpenOptions;
use std::io::{BufRead, Read, Write as _};

use anyhow::{Context, Result};
use clap::Subcommand;
use merlion_config::{
    ensure_home, redact_token, AuthPool, Config, CredentialState, ModelConfig, PooledCredential,
};

#[derive(Debug, Subcommand)]
pub enum AuthAction {
    /// List every credential in the pool, redacting tokens to their last
    /// four characters. Never prints the full token.
    List,
    /// Add (or replace) a credential for a provider.
    ///
    /// The first credential added for a provider is also appended to
    /// `~/.merlion/.env` under the provider's `*_API_KEY` env var so that
    /// existing env-var-reading code keeps working. Subsequent additions
    /// only touch `auth.yaml`; the pool then becomes the source of truth
    /// and the env var is left alone — edit it manually if you need to.
    Add {
        /// Provider prefix as understood by `Config::resolve_provider`
        /// (`openai`, `anthropic`, `gemini`, etc.).
        provider: String,
        /// Optional label. Defaults to `default` for the first key in a
        /// pool, then `n2`, `n3`, … for additional keys.
        #[arg(long)]
        label: Option<String>,
        /// Read the token from stdin instead of the command line. Safer in
        /// shared shells (the token doesn't end up in shell history).
        #[arg(long)]
        stdin: bool,
        /// Token as a positional arg. Mutually exclusive with `--stdin`.
        token: Option<String>,
    },
    /// Remove a single credential by `<provider> <label>`. The provider entry
    /// is dropped from the file once its pool is empty.
    Remove { provider: String, label: String },
    /// Clear `exhausted`/`disabled` flags for every credential in a provider's
    /// pool. Useful after the upstream provider resets quota at the top of
    /// the hour and you want merlion to pick the key up again.
    Reset { provider: String },
}

pub async fn run(action: AuthAction) -> Result<()> {
    match action {
        AuthAction::List => list(),
        AuthAction::Add {
            provider,
            label,
            stdin,
            token,
        } => add(&provider, label, stdin, token).await,
        AuthAction::Remove { provider, label } => remove(&provider, &label),
        AuthAction::Reset { provider } => reset(&provider),
    }
}

fn list() -> Result<()> {
    let pool = AuthPool::load().context("loading auth pool")?;
    if pool.pools.is_empty() {
        println!("(no credentials registered — run `merlion auth add <provider> <token>`)");
        return Ok(());
    }
    println!(
        "{:<14} {:<16} {:<10} {:<10} exhausted_at",
        "PROVIDER", "LABEL", "STATE", "TOKEN"
    );
    for (provider, creds) in &pool.pools {
        for c in creds {
            let exhausted = c
                .exhausted_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "-".to_string());
            println!(
                "{:<14} {:<16} {:<10} {:<10} {}",
                provider,
                c.label,
                state_label(c.state),
                redact_token(&c.token),
                exhausted
            );
        }
    }
    Ok(())
}

fn state_label(s: CredentialState) -> &'static str {
    match s {
        CredentialState::Ok => "ok",
        CredentialState::Exhausted => "exhausted",
        CredentialState::Disabled => "disabled",
    }
}

async fn add(
    provider: &str,
    label: Option<String>,
    stdin: bool,
    token_arg: Option<String>,
) -> Result<()> {
    let token = match (stdin, token_arg) {
        (true, Some(_)) => {
            anyhow::bail!("--stdin and a positional token are mutually exclusive");
        }
        (true, None) => read_token_from_stdin()?,
        (false, Some(t)) => t,
        (false, None) => {
            anyhow::bail!(
                "no token supplied. Pass it as a positional arg or pipe via --stdin (preferred)."
            );
        }
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("token is empty");
    }

    let mut pool = AuthPool::load().context("loading auth pool")?;
    let was_empty_for_provider = pool
        .pools
        .get(provider)
        .map(|p| p.is_empty())
        .unwrap_or(true);

    let label = label.unwrap_or_else(|| auto_label(&pool, provider));
    let cred = PooledCredential::new(label.clone(), token.clone());
    pool.add(provider, cred);
    let path = pool.save().context("saving auth pool")?;

    println!(
        "Added {} credential `{}` ({}). Pool saved to {}.",
        provider,
        label,
        redact_token(&token),
        path.display()
    );

    // Mirror into ~/.merlion/.env only when this was the very first key for
    // this provider. After that the pool wins.
    if was_empty_for_provider {
        match provider_api_key_env(provider) {
            Ok(env_name) => {
                let home = ensure_home().context("ensuring ~/.merlion")?;
                let env_path = home.join(".env");
                if !env_already_sets(&env_path, &env_name).unwrap_or(false) {
                    append_env_line(&env_path, &env_name, &token)?;
                    println!(
                        "Wrote {}={} to {} so existing env-reading code keeps working.",
                        env_name,
                        redact_token(&token),
                        env_path.display()
                    );
                } else {
                    println!(
                        "{} already set in {} — left as-is.",
                        env_name,
                        env_path.display()
                    );
                }
            }
            Err(e) => {
                // Unknown provider: pool still gets saved, but we can't infer
                // the env var to mirror into. That's fine — user can set it
                // manually or rely solely on the pool.
                eprintln!("note: skipping .env mirror — {e}");
            }
        }
    }

    Ok(())
}

fn remove(provider: &str, label: &str) -> Result<()> {
    let mut pool = AuthPool::load().context("loading auth pool")?;
    match pool.remove(provider, label) {
        Some(c) => {
            let path = pool.save().context("saving auth pool")?;
            println!(
                "Removed {} credential `{}` ({}). Pool saved to {}.",
                provider,
                c.label,
                redact_token(&c.token),
                path.display()
            );
        }
        None => {
            anyhow::bail!("no {provider} credential labelled `{label}`");
        }
    }
    Ok(())
}

fn reset(provider: &str) -> Result<()> {
    let mut pool = AuthPool::load().context("loading auth pool")?;
    if !pool.pools.contains_key(provider) {
        anyhow::bail!("no credentials registered for {provider}");
    }
    pool.reset(provider);
    let path = pool.save().context("saving auth pool")?;
    println!(
        "Reset {} credentials back to ok. Pool saved to {}.",
        provider,
        path.display()
    );
    Ok(())
}

fn read_token_from_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("read token from stdin")?;
    Ok(buf)
}

/// Compute a default label for a new credential. First slot is `default`,
/// then `n2`, `n3`, …, skipping labels already in use.
fn auto_label(pool: &AuthPool, provider: &str) -> String {
    let existing: Vec<&str> = pool
        .pools
        .get(provider)
        .map(|v| v.iter().map(|c| c.label.as_str()).collect())
        .unwrap_or_default();
    if existing.is_empty() {
        return "default".to_string();
    }
    let mut n = 2usize;
    loop {
        let candidate = format!("n{n}");
        if !existing.iter().any(|e| *e == candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Resolve the `<PROVIDER>_API_KEY` env-var name for a provider using the
/// existing `Config::resolve_provider` mapping — that way `auth add` and
/// `merlion model` agree on which env var to consult.
fn provider_api_key_env(provider: &str) -> Result<String> {
    let cfg = Config {
        model: ModelConfig {
            id: format!("{provider}:placeholder"),
            base_url: None,
            api_key_env: None,
            temperature: None,
            max_tokens: None,
        },
        system_prompt: None,
        max_iterations: 32,
        hooks: Default::default(),
    };
    let resolved = cfg.resolve_provider()?;
    Ok(resolved.api_key_env)
}

fn append_env_line(path: &std::path::Path, key: &str, value: &str) -> Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {} for append", path.display()))?;
    let needs_leading_newline = path.metadata().map(|m| m.len() > 0).unwrap_or(false)
        && !file_ends_with_newline(path).unwrap_or(true);
    if needs_leading_newline {
        writeln!(f).ok();
    }
    writeln!(f, "{key}={value}").with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn file_ends_with_newline(path: &std::path::Path) -> Result<bool> {
    let text = std::fs::read_to_string(path)?;
    Ok(text.ends_with('\n'))
}

/// True when `key=` appears at the start of any line in `path`. Used to
/// decide whether to skip the .env mirror so we don't pile up duplicate
/// lines on repeated `auth add` invocations.
fn env_already_sets(path: &std::path::Path, key: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = std::io::BufReader::new(f);
    let prefix = format!("{key}=");
    for line in reader.lines().map_while(Result::ok) {
        if line.trim_start().starts_with(&prefix) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_label_first_slot_is_default() {
        let pool = AuthPool::default();
        assert_eq!(auto_label(&pool, "openai"), "default");
    }

    #[test]
    fn auto_label_second_slot_is_n2() {
        let mut pool = AuthPool::default();
        pool.add("openai", PooledCredential::new("default", "sk-1"));
        assert_eq!(auto_label(&pool, "openai"), "n2");
    }

    #[test]
    fn auto_label_skips_collisions() {
        let mut pool = AuthPool::default();
        pool.add("openai", PooledCredential::new("default", "sk-1"));
        pool.add("openai", PooledCredential::new("n2", "sk-2"));
        pool.add("openai", PooledCredential::new("n3", "sk-3"));
        assert_eq!(auto_label(&pool, "openai"), "n4");
    }

    #[test]
    fn provider_api_key_env_maps_known_providers() {
        assert_eq!(provider_api_key_env("openai").unwrap(), "OPENAI_API_KEY");
        assert_eq!(
            provider_api_key_env("anthropic").unwrap(),
            "ANTHROPIC_API_KEY"
        );
        assert_eq!(provider_api_key_env("gemini").unwrap(), "GEMINI_API_KEY");
        assert!(provider_api_key_env("not-a-real-provider").is_err());
    }

    #[test]
    fn env_already_sets_detects_existing_key() {
        let tmp = std::env::temp_dir().join(format!(
            "merlion-auth-cmd-env-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let env_path = tmp.join(".env");
        std::fs::write(&env_path, "OTHER=1\nOPENAI_API_KEY=sk-abc\n").unwrap();
        assert!(env_already_sets(&env_path, "OPENAI_API_KEY").unwrap());
        assert!(!env_already_sets(&env_path, "ANTHROPIC_API_KEY").unwrap());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn append_env_line_creates_and_appends() {
        let tmp = std::env::temp_dir().join(format!(
            "merlion-auth-cmd-append-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let env_path = tmp.join(".env");
        append_env_line(&env_path, "FOO", "bar").unwrap();
        let text = std::fs::read_to_string(&env_path).unwrap();
        assert_eq!(text, "FOO=bar\n");
        std::fs::remove_dir_all(&tmp).ok();
    }
}

// -----------------------------------------------------------------------------
// Wiring spec — apply to `crates/merlion-cli/src/main.rs`.
//
// 1. Add a module declaration alongside the other `mod` lines at the top:
//
//        mod auth_cmd;
//
// 2. Add a new variant to the `Command` enum:
//
//        /// Manage pooled API credentials (list / add / remove / reset).
//        /// Stored at `~/.merlion/auth.yaml`; the runtime rotates keys when
//        /// one hits 429.
//        Auth {
//            #[command(subcommand)]
//            action: auth_cmd::AuthAction,
//        },
//
//    `AuthAction` already derives `clap::Subcommand` in this file, so no
//    extra clap derives are needed in `main.rs`.
//
// 3. Add a dispatch arm in the `match cli.command.unwrap_or(...)` block in
//    `main`:
//
//        Command::Auth { action } => auth_cmd::run(action).await,
//
// -----------------------------------------------------------------------------
