# Merlion Agent

> A Rust port of [Nous Research's hermes-agent](https://github.com/NousResearch/hermes-agent),
> rebuilt as a small, statically-linked CLI agent.

Merlion is a from-scratch Rust reimplementation of the parts of hermes-agent that
matter most for everyday use: an interactive CLI, an OpenAI-compatible LLM client
that works with 200+ models, a small set of high-quality built-in tools, and a
SQLite-backed session store with FTS5 search.

It is not yet feature-complete. See [ROADMAP.md](ROADMAP.md) for what's in,
what's planned, and what's out of scope.

## Status — early MVP

What works today:

- `merlion` interactive chat (streaming, tool calls, tool output rendering)
- `merlion sessions list` / `sessions search "<query>"`
- `merlion model <id>` to switch providers
- `merlion config show` / `config path`
- `merlion doctor`
- Tools: `bash`, `read`, `write`, `edit`, `ls`, `grep`, `glob`, `web_fetch`,
  `memory`, `skill_create`, `skill_update`
- Tool approval gate for sensitive tools (bash / write / edit / web_fetch) —
  prompts y/N/always; bypass with `MERLION_AUTO_APPROVE=1`
- Persistent memory store under `~/.merlion/memory/` — per-memory `.md`
  files with YAML front-matter + `MEMORY.md` index, injected into the
  system prompt at session start
- Skill system: drop a `<name>.md` into `~/.merlion/skills/` or the
  bundled `./skills/` dir; invoke with `/<name>` in chat
- Periodic curator nudge (default every 20 user turns) reminds the model
  to save durable facts to memory
- Providers: OpenAI, OpenRouter, Nous Portal, NovitaAI, Moonshot, MiniMax,
  z.ai/GLM, Groq, DeepSeek (all via `POST /chat/completions`), plus
  **Anthropic** (`POST /v1/messages`, native) and **Gemini**
  (`POST /models/<m>:streamGenerateContent`, native).
- Persistent sessions in `~/.merlion/sessions.db` with FTS5 full-text search.

What's coming (see [ROADMAP.md](ROADMAP.md)):

- MCP integration
- Skills system
- Messaging gateway (Telegram / Discord / Slack first)
- Cron scheduler
- Sandboxed terminal backends (docker, ssh)
- ratatui-based TUI

## Build

```bash
cargo build --release
./target/release/merlion --help
```

Optionally symlink:

```bash
ln -s "$(pwd)/target/release/merlion" ~/.local/bin/merlion
```

## Quick start

```bash
# 1. Pick a model (any OpenAI-compatible provider)
merlion model openrouter:anthropic/claude-sonnet-4

# 2. Drop your API key into ~/.merlion/.env
mkdir -p ~/.merlion
echo OPENROUTER_API_KEY=sk-or-... >> ~/.merlion/.env

# 3. Chat
merlion
```

`merlion doctor` will tell you if anything is missing.

## Configuration

`~/.merlion/config.yaml`:

```yaml
model:
  id: openai:gpt-4o-mini       # provider:model — see Providers below
  temperature: null
  max_tokens: null
system_prompt: null            # null → built-in default
max_iterations: 32             # hard cap on assistant↔tool round-trips per turn
```

Secrets go in `~/.merlion/.env` (loaded automatically, never written by Merlion).

Project-local overrides: `./.merlion/config.yaml`.

Environment overrides: `MERLION_MODEL`, `MERLION_BASE_URL`, `MERLION_API_KEY_ENV`,
`MERLION_SYSTEM_PROMPT`, `MERLION_MAX_ITERATIONS`, `MERLION_HOME`.

### Providers

| Prefix       | Default base URL                                | API-key env         |
| ------------ | ----------------------------------------------- | ------------------- |
| `openai`     | `https://api.openai.com/v1`                     | `OPENAI_API_KEY`    |
| `openrouter` | `https://openrouter.ai/api/v1`                  | `OPENROUTER_API_KEY`|
| `nous`       | `https://inference-api.nousresearch.com/v1`     | `NOUS_API_KEY`      |
| `novita`     | `https://api.novita.ai/v3/openai`               | `NOVITA_API_KEY`    |
| `moonshot`   | `https://api.moonshot.ai/v1`                    | `MOONSHOT_API_KEY`  |
| `minimax`    | `https://api.minimaxi.chat/v1`                  | `MINIMAX_API_KEY`   |
| `zai`/`glm`  | `https://api.z.ai/api/paas/v4`                  | `ZAI_API_KEY`       |
| `groq`       | `https://api.groq.com/openai/v1`                | `GROQ_API_KEY`      |
| `deepseek`   | `https://api.deepseek.com/v1`                   | `DEEPSEEK_API_KEY`  |
| `anthropic`  | `https://api.anthropic.com/v1`                  | `ANTHROPIC_API_KEY` |
| `gemini`     | `https://generativelanguage.googleapis.com/v1beta` | `GEMINI_API_KEY` |

For anything else, set `model.base_url` and `model.api_key_env` explicitly.

## Architecture

Cargo workspace:

```
crates/
├── merlion-core      — message types, Tool trait, agent loop
├── merlion-llm       — OpenAI-compatible streaming client
├── merlion-tools     — bash, read, write, edit, ls
├── merlion-config    — YAML config + ~/.merlion home
├── merlion-session   — SQLite + FTS5 session store
└── merlion-cli       — `merlion` binary
```

The agent loop (`merlion-core::Agent::run`) streams assistant deltas, dispatches
tool calls in order, appends tool results to the conversation, and repeats until
the model stops calling tools or the iteration budget is exhausted. The CLI is
a thin shell around it.

## Relationship to hermes-agent

Merlion is a clean-room Rust reimplementation aimed at the same shape of
problem. It is not a translation — many of hermes's optional subsystems
(seven terminal backends, ~20 messaging platforms, batch trajectory generation,
trajectory compression for training) are intentionally out of scope here.

Hermes is MIT-licensed and so is Merlion. Conversation file formats, tool
semantics, and CLI affordances aim to be familiar to hermes users, but the
on-disk session format is **not** compatible — Merlion uses its own schema
under `~/.merlion`.

## License

MIT — see [LICENSE](LICENSE).
