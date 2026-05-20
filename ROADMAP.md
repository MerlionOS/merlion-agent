# Merlion Roadmap

The Python hermes-agent is ~830,000 lines across many subsystems. Merlion is a
ground-up Rust reimplementation that intentionally trims scope. This document
maps what's done, what's next, and what's deliberately out of scope.

The unit of estimation is **Claude Code session hours** — the time the coding
agent spends building it, not human-pace estimates. Wall-clock time depends on
how the user chooses to space sessions; each unit below is "uninterrupted
session time the model needs to land the work, with reviews."

**Status legend:** ✅ done · 🟡 in progress · ⬜️ planned · ⛔️ out of scope

---

## Phase 0 — MVP (✅ done, ≈4 session hours)

Foundation: workspace, core types, agent loop, one provider, basic tools, CLI.

| # | Deliverable | Status |
|---|---|---|
| 0.1 | Cargo workspace + `crates/merlion-{core,llm,tools,config,session,cli}` | ✅ |
| 0.2 | `merlion-core`: `Message`, `Tool`, `Agent::run` with iteration budget | ✅ |
| 0.3 | `merlion-llm::OpenAiClient` — SSE chat-completions | ✅ |
| 0.4 | `merlion-tools`: `bash`, `read`, `write`, `edit`, `ls` | ✅ |
| 0.5 | `merlion-config`: YAML, `.env`, env overrides, 9 OpenAI-compatible presets | ✅ |
| 0.6 | `merlion-session`: SQLite + FTS5 | ✅ |
| 0.7 | `merlion-cli`: chat REPL + `model`/`config`/`doctor`/`sessions` | ✅ |

**Acceptance:** `merlion` can chat with any OpenAI-compatible endpoint, call
all 5 tools, persist sessions across runs, and search prior conversations.

---

## Phase 1 — Provider breadth (≈4 of 6 session hours done)

| # | Deliverable | Files | Est. | Status |
|---|---|---|---|---|
| 1.1 | Anthropic `/v1/messages` adapter | `crates/merlion-llm/src/anthropic.rs` | 2h | ✅ |
| 1.2 | Gemini `streamGenerateContent` adapter | `crates/merlion-llm/src/gemini.rs` | 2h | ✅ |
| 1.3 | Usage/cost accounting in `LlmResponse` + per-turn TUI footer | `merlion-core/src/llm.rs`, `merlion-cli/src/tui/render.rs` | 1h | ✅ |
| 1.4 | Retry with exponential backoff on 429/5xx | `merlion-llm/src/retry.rs` | 1h | ✅ |
| 1.5 | AWS Bedrock passthrough (hand-rolled SigV4, Anthropic-on-Bedrock) | `merlion-llm/src/bedrock.rs` | 2h | ✅ |
| 1.6 | Google Vertex passthrough (gcloud auth + Gemini wire) | `merlion-llm/src/vertex.rs` | 1h | ✅ |

**Acceptance:** every major frontier-lab model reachable with one config
change; usage shown in the CLI footer; transient errors auto-retry.

---

## Phase 2 — Tool surface (≈8 session hours)

This is where merlion stops being a toy. The goal is a tool set that lets the
agent actually complete coding tasks autonomously.

| # | Deliverable | Files | Est. | Status |
|---|---|---|---|---|
| 2.1 | `grep` (ripgrep-backed, POSIX `grep -rn` fallback) | `merlion-tools/src/grep.rs` | 1h | ✅ |
| 2.2 | `glob` (uses the `glob` crate, capped results) | `merlion-tools/src/glob.rs` | 0.5h | ✅ |
| 2.3 | `web_fetch` (reqwest + html2text, 256 KiB cap) | `merlion-tools/src/web_fetch.rs` | 1h | ✅ |
| 2.4 | `web_search` tool (Tavily backend) | `merlion-tools/src/web_search.rs` | 1.5h | ✅ |
| 2.5 | `task` — spawn a subagent with isolated message list + tools | `merlion-tools/src/task.rs` | 2h | ✅ |
| 2.6 | `ToolApprover` trait in core; CLI implements console prompter | `merlion-core/src/approval.rs`, `merlion-cli/src/approver.rs` | 1h | ✅ |
| 2.7 | "Always allow" approval persisted to `~/.merlion/approvals.yaml` | `merlion-cli/src/approver.rs` | 0.5h | ✅ |
| 2.8 | Tool-result truncation (`max_tool_result_chars` in `AgentOptions`, default 16 KiB). Disk overflow deferred — truncation alone covers the model-context-bloat goal | `merlion-core/src/agent.rs` | 0.5h | ✅ |

**Acceptance:** the agent can search a repo with `grep`, find files with
`glob`, fetch a URL, search the web, delegate a side-quest to a subagent, and
the human stays in control via the approval gate for any shell command.

---

## Phase 3 — Memory & skills (≈8 of 10 session hours done)

Hermes's killer feature: agent-curated long-term memory + skill creation.

| # | Deliverable | Files | Est. | Status |
|---|---|---|---|---|
| 3.1 | File-backed memory store with per-memory `.md` + `MEMORY.md` index | `crates/merlion-memory/` | 1.5h | ✅ |
| 3.2 | `memory` tool: list / read / write / delete via action arg | `merlion-tools/src/memory.rs` | 1h | ✅ |
| 3.3 | Periodic curator nudge (every N user turns) | `merlion-core/src/curator.rs` | 1.5h | ✅ |
| 3.4 | Skill loader (directory + flat layouts, two-root precedence) | `crates/merlion-skills/` | 1.5h | ✅ |
| 3.5 | Skill front-matter parser | `merlion-skills/src/parse.rs` | 0.5h | ✅ |
| 3.6 | `/<skill-name>` slash invocation; `/skills` + `/memory` commands | `merlion-cli` | 1h | ✅ |
| 3.7 | Skill-creation tool (`skill_create`) | `merlion-tools/src/skill_tools.rs` | 1h | ✅ |
| 3.8 | Skill self-improvement tool (`skill_update`) | `merlion-tools/src/skill_tools.rs` | 1h | ✅ |
| 3.9 | Tab-complete for `/<skill>` in TUI; agentskills.io compat doc | `merlion-cli`, `docs/skills.md` | 1h | ✅ |

**Acceptance:** when the user works on the same project across sessions,
merlion remembers their preferences and project facts; the agent can write a
skill mid-conversation and invoke it next time with `/<name>`.

---

## Phase 4 — MCP integration (≈6 of 8 session hours done)

Connect the agent to the wider ecosystem of MCP servers (filesystem, GitHub,
databases, etc.).

| # | Deliverable | Files | Est. | Status |
|---|---|---|---|---|
| 4.1 | MCP wire types + client (initialize, tools/list, tools/call) | `crates/merlion-mcp/src/{proto,client}.rs` | 1h | ✅ |
| 4.2 | Stdio transport (spawn server, JSON-RPC framing, pending map) | `merlion-mcp/src/stdio.rs` | 1.5h | ✅ |
| 4.3 | HTTP+SSE transport (JSON or text/event-stream response) | `merlion-mcp/src/http.rs` | 1.5h | ✅ |
| 4.4 | Server registry: `~/.merlion/mcp.yaml` | `merlion-mcp/src/registry.rs` | 1h | ✅ |
| 4.5 | `McpProxyTool` + autoload on chat startup | `merlion-mcp/src/proxy.rs`, `merlion-cli` | 1h | ✅ |
| 4.6 | OAuth2 PKCE flow + token cache + `merlion mcp oauth <name>` | `merlion-mcp/src/oauth.rs` | 1.5h | ✅ |
| 4.7 | `merlion mcp {list,add,remove,enable,disable,test}` subcommands | `merlion-cli` | 0.5h | ✅ |

**Acceptance:** `merlion mcp add filesystem ~/projects` adds a working
filesystem MCP server; its tools show up in `merlion doctor` and the agent
can call them transparently.

---

## Phase 5 — Messaging gateway (≈13 of 16 session hours done)

Talk to the agent from your phone. Hermes ships ~20 platforms; we ship the
three highest-value: Telegram (long-poll HTTP), Discord (serenity Gateway
WS), and Slack (Socket Mode WS over hand-rolled tokio-tungstenite — no
slack-morphism). `merlion gateway start` starts every configured platform
concurrently.

| # | Deliverable | Files | Est. | Status |
|---|---|---|---|---|
| 5.1 | `Gateway` trait + dispatcher in a new `merlion-gateway` crate | `crates/merlion-gateway/` | 2h | ✅ |
| 5.2 | Telegram adapter (long-polling) | `merlion-gateway/src/telegram.rs` | 3h | ✅ |
| 5.3 | Discord adapter (DM + @mention, serenity) | `merlion-gateway/src/discord.rs` | 3h | ✅ |
| 5.4 | Slack adapter (Socket Mode, tokio-tungstenite) | `merlion-gateway/src/slack.rs` | 3h | ✅ |
| 5.5 | Env-var per-platform allowlist (Allowlist::from_env) | `merlion-gateway/src/allowlist.rs` | 1.5h | ✅ |
| 5.6 | Cross-platform session continuity via 6-char join keys | `merlion-gateway/src/joinkeys.rs` | 1.5h | ✅ |
| 5.7 | `merlion gateway {start,status}` | `merlion-cli` | 1h | ✅ |
| 5.8 | Voice transcription via OpenAI Whisper (Telegram voice memos) | `merlion-gateway/src/telegram.rs` | 1h | ✅ |

**Acceptance:** one `merlion gateway start` process serves all three
platforms; the same conversation can move between Telegram and the CLI.

⛔️ Out of scope for now: WhatsApp, Signal, Matrix, Mattermost, Feishu,
WeCom, WeChat, QQ, Email, SMS, DingTalk, BlueBubbles, Yuanbao, Home
Assistant, WebHook, Generic API server. PRs welcome under
`merlion-gateway/src/platforms/`.

---

## Phase 6 — Cron + sandboxed execution (≈4 of 8 session hours done)

Scheduled runs + isolation from the host filesystem.

| # | Deliverable | Files | Est. | Status |
|---|---|---|---|---|
| 6.1 | Cron scheduler with cron-expression parsing, persisted job table | `crates/merlion-cron/` | 2h | ✅ |
| 6.2 | Job → messaging delivery (`telegram:<chat>` / `discord:<channel>`) | `merlion-cli` | 1h | ✅ |
| 6.3 | `merlion cron {add,list,remove,run,daemon}` subcommands | `merlion-cli` | 1h | ✅ |
| 6.4 | Docker terminal backend (run shell commands in a container) | `merlion-tools/src/bash_docker.rs` | 2h | ✅ |
| 6.5 | SSH terminal backend (run on a remote host) | `merlion-tools/src/bash_ssh.rs` | 2h | ✅ |

**Acceptance:** `merlion cron add "0 9 * * * 'check my email and summarize'"`
runs at 9am daily and delivers the result to Telegram.

⛔️ Out of scope: Modal, Daytona, Singularity, Vercel Sandbox.

---

## Phase 7 — TUI (≈8 of 12 session hours done)

Hermes's terminal UI is a meaningful UX win over a plain REPL. ratatui makes
this tractable in Rust.

| # | Deliverable | Files | Est. | Status |
|---|---|---|---|---|
| 7.1 | ratatui scaffolding: layout, event loop, render budget | `merlion-cli/src/tui/` | 2h | ✅ |
| 7.2 | Multiline editor widget (Ctrl+J newline, Enter submit) | `merlion-cli/src/tui/input.rs` | 2h | ✅ |
| 7.3 | Slash-command autocomplete (Tab) for `/<skill>` | `merlion-cli/src/tui/app.rs` | 1.5h | ✅ |
| 7.4 | Streaming output pane with interrupt-and-redirect (Ctrl+C → new input) | `merlion-cli/src/tui/app.rs` | 2h | ✅ |
| 7.5 | Tool-output collapsible panes (rendered inline; collapse TBD) | `merlion-cli/src/tui/render.rs` | 1.5h | ✅ |
| 7.6 | History scroll (keyboard via PgUp/PgDn/Esc) | `merlion-cli/src/tui/app.rs` | 1.5h | ✅ |
| 7.7 | `--tui` flag default-on when stdout is a TTY | `merlion-cli` | 0.5h | ✅ |
| 7.8 | Light/dark theme via `MERLION_THEME` env var | `merlion-cli/src/tui/theme.rs` | 1h | ✅ |

**Acceptance:** running `merlion` in a terminal feels modern — streaming,
collapsible tool output, no flicker, interrupt mid-stream.

---

## Phase 8 — Polish, packaging, distribution (≈2 of 6 session hours done)

Make merlion installable in one command from anywhere.

| # | Deliverable | Files | Est. | Status |
|---|---|---|---|---|
| 8.1 | GitHub Actions CI: build + test on linux/macos | `.github/workflows/ci.yml` | 1h | ✅ |
| 8.2 | Release workflow: cross-compiled tarballs (linux × arm/x86, mac × arm/x86) on `v*` tag | `.github/workflows/release.yml` | 1.5h | ✅ |
| 8.3 | Homebrew formula (tap-ready, sha256 placeholders) | `Formula/merlion.rb` | 1h | ✅ |
| 8.4 | `cargo binstall` metadata | `merlion-cli/Cargo.toml` | 0.25h | ✅ |
| 8.5 | One-line installer: `curl ... \| bash` | `scripts/install.sh` | 1h | ✅ |
| 8.6 | `merlion update [--apply]` — checks releases, optionally downloads + swaps the binary (Unix) | `merlion-cli` | 1h | ✅ |
| 8.7 | `merlion doctor` deepened: probes for `rg`, `git`, MCP servers, gateway tokens, cron | `merlion-cli` | 0.25h | ✅ |

**Acceptance:** `brew install merlion` or `curl https://… | bash` lands a
working binary; `merlion update` self-upgrades to the latest release.

---

## Phase 9 — Daily-use polish (v0.1.1, ≈4 session hours)

After v0.1.0 shipped, real-world use surfaced gaps versus hermes-agent's
42-subcommand surface. Most of hermes's subcommands are niche, but a
handful are the difference between "shipped" and "actually useful daily."

| # | Deliverable | Files | Est. | Status |
|---|---|---|---|---|
| 9.1 | `-z/--oneshot PROMPT` top-level flag — pipe-friendly: `git diff \| merlion -z "review"` | `merlion-cli/src/main.rs` | 0.5h | ✅ |
| 9.2 | `--continue` / `-c` to resume the most-recent session | `merlion-cli/src/main.rs`, `merlion-session` | 0.25h | ✅ |
| 9.3 | `status` alias for `doctor` + add Slack to `gateway status` | `merlion-cli/src/main.rs` | 0.1h | ✅ |
| 9.4 | `completion {bash,zsh,fish,powershell}` subcommand (clap_complete) | `merlion-cli/src/completion.rs` | 0.25h | ✅ |
| 9.5 | `logs` subcommand + actual log-file emission to `~/.merlion/logs/` | `merlion-cli/src/logs.rs`, tracing setup | 0.75h | ✅ |
| 9.6 | `setup` interactive wizard — pick provider, paste key, write config.yaml + .env | `merlion-cli/src/setup.rs` | 1h | ✅ |
| 9.7 | `skills` subcommands — `list`, `show <name>`, `delete <name>` | `merlion-cli/src/skills_cmd.rs` | 0.5h | ✅ |
| 9.8 | `version` subcommand (alongside `--version`) | `merlion-cli/src/main.rs` | 0.1h | ✅ |
| 9.9 | `tools` subcommand — list registered tools + per-platform enable/disable | `merlion-cli/src/tools_cmd.rs` | 0.5h | ✅ |
| 9.10 | `curator` subcommands — `status`, `pause`, `resume`, `run-now` | `merlion-cli/src/curator_cmd.rs` | 0.5h | ✅ |

**Acceptance:** `merlion --help` lists 15+ subcommands; `git diff | merlion
-z "review this"` works as a unix pipeline; `merlion completion zsh >>
~/.zshrc` enables tab-complete; `merlion setup` walks a first-time user
from zero to working `merlion doctor`.

---

## Phase 10 — Parity-on-the-useful-subset (v0.1.2, ≈3 session hours)

After v0.1.1 shipped, the `hermes --help` vs `merlion --help` diff still
shows hermes has 14 top-level flags (we have 4) and a few more subcommands
worth porting. Close the gap on what's high-leverage; explicitly leave
niche items deferred.

### Top-level flags (5)

| # | Flag | Why | Files | Est. | Status |
|---|---|---|---|---|---|
| 10.1 | `-m/--model MODEL` per-invocation override | `merlion -z "x" -m anthropic:claude-sonnet-4` without editing config | `merlion-cli/src/main.rs` | 0.25h | ✅ |
| 10.2 | `--provider PROVIDER` per-invocation override | Pair with 10.1; lets `-z` use a non-default provider | `merlion-cli/src/main.rs` | 0.15h | ✅ |
| 10.3 | `-s/--skills SKILLS` preload | Inject a skill body before the first user turn: `merlion -z "review" -s code-review` | `merlion-cli/src/main.rs` | 0.5h | ✅ |
| 10.4 | `--resume <ID>` top-level | Symmetric with `-c`: `merlion --resume abc123` works without `chat` subcommand | `merlion-cli/src/main.rs` | 0.15h | ✅ |
| 10.5 | `--yolo` flag (alias for `MERLION_AUTO_APPROVE=1`) | Discoverable; matches hermes's name | `merlion-cli/src/main.rs` | 0.1h | ✅ |

### Subcommands (3)

| # | Subcommand | Files | Est. | Status |
|---|---|---|---|---|
| 10.6 | `fallback {list,add,remove,clear}` + `FallbackLlmClient` runtime wrapper | `merlion-cli/src/fallback_cmd.rs` + `merlion-llm/src/fallback.rs` + `merlion-config` | 1h | ✅ |
| 10.7 | `auth {list,add,remove,reset}` — manage pooled API keys in `~/.merlion/auth.yaml` | `merlion-cli/src/auth_cmd.rs` + `merlion-config` | 0.75h | ✅ |
| 10.8 | `backup` / `import` — tar.gz of `~/.merlion/` for transfer | `merlion-cli/src/backup_cmd.rs` | 0.5h | ✅ |

### Cosmetic fix

| # | Item | Status |
|---|---|---|
| 10.9 | `Gateway` doc: "Telegram + Discord" → "Telegram + Discord + Slack" | ✅ |

**Acceptance:** `merlion --help` shows 8 top-level flags and ~19 subcommands;
`merlion -z "..." -m anthropic:claude-sonnet-4 -s code-review` runs as a
fully-overridden one-shot; `merlion fallback add openrouter:claude-sonnet-4`
configures a fallback chain that kicks in when the primary 429s.

**Still out of scope for v0.1.2** (defer further): `whatsapp`, `kanban`,
`dashboard`, `computer-use`, `lsp`, `acp`, `profile`, `insights`, `claw`,
`plugins`, `checkpoints`, `hooks`, `pairing` (CLI), `memory` (provider
switching), `dump`, `debug`, `webhook`, `uninstall`.

---

## Phase 11 — Gateway service lifecycle (v0.1.3, ≈1.5 session hours)

`hermes gateway status` reports on an installed background daemon
(launchd plist / systemd unit, with PID, last exit, and log paths).
`merlion gateway` shipped foreground-only through v0.1.2. Close that gap.

### Subcommands

| # | Subcommand | Files | Est. | Status |
|---|---|---|---|---|
| 11.1 | `gateway run` (rename of old foreground `start`) | `merlion-cli/src/main.rs` | 0.05h | ✅ |
| 11.2 | `gateway install` — write launchd plist or systemd --user unit and bootstrap/enable | `merlion-cli/src/gateway_service.rs` | 0.5h | ✅ |
| 11.3 | `gateway uninstall` — bootout/disable and remove | `merlion-cli/src/gateway_service.rs` | 0.15h | ✅ |
| 11.4 | `gateway start` / `stop` / `restart` for the installed service. `stop` is idempotent (launchctl exit 3 = no-op) | `merlion-cli/src/gateway_service.rs` | 0.3h | ✅ |
| 11.5 | `gateway logs [-f] [--errors] [-n N]` — tail `~/.merlion/logs/gateway{,.error}.log` | `merlion-cli/src/main.rs` | 0.15h | ✅ |
| 11.6 | `gateway status` enhanced: env-var matrix + service state (path, PID, last exit, log files); top-level `merlion status` also gets a service-state line | `merlion-cli/src/main.rs` + `gateway_service.rs` | 0.25h | ✅ |
| 11.7 | macOS launchd backend (`launchctl bootstrap/bootout/kickstart/kill/print`) + Linux systemd `--user` backend (`systemctl --user enable/start/stop/restart/show`) | `gateway_service.rs` | included above | ✅ |
| 11.8 | Plist + unit-file generators with unit tests; `current_exe()` baked into ProgramArguments/ExecStart so reinstalls track the binary that ran `install` | `gateway_service.rs` | included above | ✅ |

**Acceptance:** end-to-end on macOS — `merlion gateway install` writes
`~/Library/LaunchAgents/ai.merlion.gateway.plist`, `launchctl print
gui/<uid>/ai.merlion.gateway` shows it running with the merlion binary as
the program; `merlion gateway status` reports running + PID; `stop` /
`start` / `restart` / `uninstall` round-trip cleanly. Tokens are picked up
from `~/.merlion/.env` since launchd does not inherit the user shell env.

---

## Phase 12 — Catalog-driven model picker (v0.1.4, ≈1.5 session hours)

`hermes model` walks an interactive picker: friendly provider labels
("OpenAI Codex", "Anthropic", "OpenRouter (100+ models)") then a curated
list of models per provider, with "Enter custom" + "Keep current"
escapes. `merlion model` v0.1.3 was bare-bones: print id, or set id from
positional arg. Close the gap.

| # | Item | Files | Est. | Status |
|---|---|---|---|---|
| 12.1 | `setup::CATALOG` — `ProviderEntry { prefix, label, api_key_env, models }`; 13 providers × 3–5 popular models, ordered with Anthropic/OpenAI/OpenRouter/Gemini first | `merlion-cli/src/setup.rs` | 0.5h | ✅ |
| 12.2 | `model_cmd::wizard` — provider menu (friendly labels, marks current) → model menu (catalog list, "Enter custom…", "Keep current") → API key prompt | `merlion-cli/src/model_cmd.rs` | 0.5h | ✅ |
| 12.3 | `model_cmd::set_shortcut` — `merlion model <provider:model>` keeps the silent set, but follow-prompts for the API key only if its env var is missing | `merlion-cli/src/model_cmd.rs` | 0.15h | ✅ |
| 12.4 | `setup::run` rewired to the same catalog menu (was free-text Input). Single source of truth | `merlion-cli/src/setup.rs` | 0.15h | ✅ |
| 12.5 | Tests: every catalog prefix round-trips through `Config::resolve_provider`; every entry has ≥1 model; first model matches existing `default_model_for` so defaults don't shift silently | `merlion-cli/src/setup.rs` | 0.2h | ✅ |

**Deliberately out of scope:** the OAuth/Nous-portal login branch
(`hermes model --portal-url …`) — that's Nous-internal infrastructure.
The catalog model lists will go stale (frontier providers ship a new
model every few weeks); users can always pick "Enter custom…" or use
`merlion model <provider:model>` to override.

**Acceptance:** `merlion model` (no arg) launches a TUI picker that
matches hermes's shape. `merlion model anthropic:claude-opus-4-7`
silently switches without prompting if `ANTHROPIC_API_KEY` is already
set, and prompts for it otherwise. `merlion setup` first-run flow uses
the same picker.

---

## Phase 12.1 — Model picker non-TTY fallback (v0.1.5, ≈0.15 session hours)

After v0.1.4 shipped, `merlion model` (no arg) inside Claude Code's bash
tool (or any piped context) errored with "Error: provider picker /
Caused by: IO error: not a terminal" — dialoguer's Select needs a real
TTY for arrow keys. Detect non-TTY at wizard entry and fall back to
v0.1.3 behavior: print the current id, plus a hint with `merlion model
<provider:model>` examples.

| # | Item | Status |
|---|---|---|
| 12.1.1 | `std::io::stdin().is_terminal()` guard at `model_cmd::wizard` entry; print current model + non-TTY hint instead of erroring | ✅ |

**Acceptance:** `merlion model` piped, captured, or invoked from Claude
Code's bash tool prints `provider:model` and an explanatory hint with
exit 0 — no `IO error: not a terminal` regression.

---

## Phase 12.2 — Catalog refresh (v0.1.6, ≈0.1 session hours)

User feedback after v0.1.5: the OpenAI picker didn't list gpt-5 family
models (gpt-5.5, gpt-5.4, gpt-5.4-mini, gpt-5.3-codex, …) that hermes
shows. Curated catalog had drifted relative to current OpenAI ids.

| # | Item | Status |
|---|---|---|
| 12.2.1 | OpenAI catalog: prepend gpt-5.5 / gpt-5.4 / gpt-5.4-mini / gpt-5.3-codex / gpt-5.3-codex-spark / gpt-5.2 ahead of legacy gpt-4o/o1 entries. gpt-5.5 is the new wizard default | ✅ |
| 12.2.2 | OpenRouter catalog: prepend openai/gpt-5.5 + openai/gpt-5.4-mini | ✅ |
| 12.2.3 | Tests + fallback updated: `default_model_for("openai")` now `gpt-5.5`, and the unknown-provider fallback updated accordingly | ✅ |

The catalog will always go stale; the "Enter custom model name…" item
in the wizard and `merlion model <provider:model>` continue to accept
arbitrary strings, so future drift is recoverable without a release.

---

## Phase 13 — `merlion setup` parity polish (v0.1.7, ≈2 session hours)

`hermes setup` is a sectional wizard: welcome banner, "you already have
config — Enter to keep" preamble, Configuration Location panel, then
sections for Inference Provider / Messaging Gateway / Agent, each with
a credential health check and a 3-way prompt when keys exist. `merlion
setup` was a flat 4-step prompt chain. Close the gap.

### Visual layout
| # | Item | Status |
|---|---|---|
| 13.1 | Welcome banner with magenta border + `⚕` glyph; "Ctrl+C to exit" hint | ✅ |
| 13.2 | `◆ Section` headers (cyan, bold) for every step | ✅ |
| 13.3 | "Reconfigure" preamble — "You already have Merlion configured ✓ / press Enter to keep" or "First-time setup…" for fresh installs | ✅ |
| 13.4 | "Configuration Location" panel listing Config file / Secrets file / Data folder up front | ✅ |
| 13.5 | "Next steps" footer with copy-pasteable `merlion doctor` / `merlion` lines | ✅ |

### Sections
| # | Item | Status |
|---|---|---|
| 13.6 | `section_inference_provider` — shows current model + provider + API-key health; 3-way prompt (Skip / Change model / Re-enter key) when key is already set; catalog-driven picker otherwise | ✅ |
| 13.7 | `section_gateway` — Telegram / Discord / Slack each with ✓/◐/· status, opt-in `Configure {name}?` prompt, hidden-input prompts for tokens, plain Input for the user-id allowlist (Slack also asks for `SLACK_BOT_TOKEN`) | ✅ |
| 13.8 | `section_agent` — shows current system-prompt preview (truncated to 70 chars) and `max_iterations`; prompts for system-prompt edit | ✅ |

### Plumbing
| # | Item | Status |
|---|---|---|
| 13.9 | `Section` enum (`Full`, `Model`, `Gateway`, `Agent`) + `pub async fn run(section, quick)` orchestrator | ✅ |
| 13.10 | `merlion setup [model\|gateway\|agent]` subcommand picker via `clap::ValueEnum` | ✅ |
| 13.11 | `--quick` flag — section skips when its values are already set | ✅ |
| 13.12 | Non-TTY guard at `run` entry — fail fast with a hint to `merlion model <id>` / `merlion config edit` instead of erroring inside dialoguer | ✅ |

**Deliberately out of scope:** the OAuth login branch (`hermes setup
model` → "Reauthenticate (new OAuth login)") — Nous-portal infra. The
3-way prompt covers the credential management UX without the OAuth
spawn-browser flow.

**Acceptance:** `merlion setup` in a real terminal shows the banner +
Config Location panel + 3 cyan-headed sections. `merlion setup gateway`
jumps straight to the gateway section. `merlion setup --quick` only
prompts where something is missing. Non-TTY callers get a clear hint
instead of "IO error: not a terminal".

---

## Summary — remaining work to v1

Adding up the unchecked items:

| Phase | Remaining | Cumulative |
|---|---:|---:|
| 1 (Provider breadth — Bedrock/Vertex/usage display) | 5 h  | 5 h  |
| 2 (Tool surface — web_search ✅, task tool, allowlist disk, truncation ✅) | 3 h  | 8 h  |
| 3 (Memory & skills — tab-complete, agentskills.io doc) | 1 h  | 9 h  |
| 4 (MCP integration — HTTP transport, OAuth) | 3 h  | 12 h |
| 5 (Gateway — Discord, Slack, voice, cross-platform session continuity) | 9 h  | 21 h |
| 6 (Sandboxes — Docker, SSH; cron→messaging delivery) | 5 h  | 26 h |
| 7 (TUI — themes, tab-complete) | 2 h  | 28 h |
| 8 (Packaging — release artifacts, Homebrew, self-update) | 4 h  | 32 h |

**Status update:** the originally-estimated ≈73h of roadmap work is now
**substantially complete**. The remaining roadmap items are minor polish
(Modal/Daytona/Singularity sandboxes — explicitly out of scope; release
artifacts are already wired and just need a real `v*` tag push to publish).

Recent runs added Bedrock + Vertex via hand-rolled SigV4 and gcloud
shellout respectively — no heavyweight AWS/GCP SDKs needed.

---

## ⛔️ Explicitly out of scope (for v1.0)

These are real features in hermes that Merlion is **not** planning to port,
because they're tangential to the core agent loop or require an enormous amount
of infrastructure relative to user impact:

- **Batch trajectory generation** + trajectory compression for training the
  next generation of tool-calling models. Hermes ships this; it's a research
  tool, not an end-user feature.
- **ACP adapter** (VS Code / Zed / JetBrains integration via the Agent Client
  Protocol). Reconsider in v1.x if there's user demand.
- **The full ~20-platform messaging matrix.** We ship 3 in Phase 5; the rest
  are PR-welcome but won't block v1.0.
- **Browser tool** (Camoufox / CDP). Large surface area; reconsider after
  Phase 7.
- **Honcho-style dialectic user modeling.** Memory in Phase 3 is the
  simpler MEMORY.md/USER.md model.
- **Image-generation and TTS plugins.** Out of scope.
- **Modal / Daytona / Singularity / Vercel Sandbox** terminal backends.
  Docker + SSH (Phase 6) cover the common case.
- **Honcho, Mem0, Supermemory** memory-provider plugins. File-backed memory
  is the v1.0 baseline; plugins reconsidered later.

---

## Sequencing logic

The ordering above is roughly highest-leverage first:

1. **Provider breadth** (Phase 1) — broaden the audience first; people use what
   they have keys for.
2. **Tool surface** (Phase 2) — without `grep`/`glob`/`web_fetch`, the agent is
   noticeably worse than competitors at real coding tasks.
3. **Memory & skills** (Phase 3) — this is hermes's differentiator; once tools
   are good, memory makes merlion *useful across sessions*, not just *useful
   in one session*.
4. **MCP** (Phase 4) — opens the ecosystem; cheap relative to value.
5. **Gateway** (Phase 5) — the "lives where you do" promise; big chunk of work
   but high user-visible payoff.
6. **Cron + sandboxes** (Phase 6) — unlocks unattended runs.
7. **TUI** (Phase 7) — quality-of-life polish. Could be reordered earlier if
   feedback prioritizes it.
8. **Packaging** (Phase 8) — only meaningful once the features are in.

If you want to skip ahead — for instance, jump to Phase 5 (gateway) before
finishing Phase 2 — it's tractable but the gateway version will be missing
tools that the CLI version has, so chat sessions from Telegram will feel
weaker than CLI ones until Phase 2 lands.

---

## Contributing

The easiest places to start:

- Phase 1 leftovers: usage/cost accounting (1.3), retry/backoff (1.4)
- Phase 2: any single tool (1h each)
- Phase 8: GitHub Actions CI (8.1)

Each task above is sized to one Claude Code session. Pick one, file an issue,
and PR.
