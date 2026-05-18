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
| 1.3 | Usage/cost accounting in `LlmResponse` + per-turn display | `merlion-core/src/llm.rs`, CLI | 1h | ⬜️ |
| 1.4 | Retry with exponential backoff on 429/5xx | `merlion-llm/src/retry.rs` | 1h | ⬜️ |
| 1.5 | AWS Bedrock passthrough (`anthropic.claude-*` on Bedrock) | `merlion-llm/src/bedrock.rs` | 2h | ⬜️ |
| 1.6 | Google Vertex passthrough (Vertex AI Gemini) | `merlion-llm/src/vertex.rs` | 1h | ⬜️ |

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
| 2.4 | `web_search` pluggable backend (Brave / Tavily / SerpAPI) | `merlion-tools/src/web_search.rs` | 1.5h | ⬜️ |
| 2.5 | `task` — spawn a subagent with isolated message list + tools | `merlion-tools/src/task.rs` | 2h | ⬜️ |
| 2.6 | `ToolApprover` trait in core; CLI implements console prompter | `merlion-core/src/approval.rs`, `merlion-cli/src/approver.rs` | 1h | ✅ |
| 2.7 | Command-pattern allowlist persisted to `~/.merlion/approvals.yaml` | `merlion-config` | 0.5h | ⬜️ |
| 2.8 | Tool-result truncation + overflow to `~/.merlion/tool_results/<id>` | `merlion-tools/src/storage.rs` | 0.5h | ⬜️ |

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
| 3.9 | Tab-complete for `/<skill>` in the REPL; agentskills.io compat doc | `merlion-cli`, docs | 1h | ⬜️ |

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
| 4.3 | HTTP+SSE transport | `merlion-mcp/src/http.rs` | 1.5h | ⬜️ |
| 4.4 | Server registry: `~/.merlion/mcp.yaml` | `merlion-mcp/src/registry.rs` | 1h | ✅ |
| 4.5 | `McpProxyTool` + autoload on chat startup | `merlion-mcp/src/proxy.rs`, `merlion-cli` | 1h | ✅ |
| 4.6 | OAuth flow for MCP servers that require it | `merlion-mcp/src/oauth.rs` | 1.5h | ⬜️ |
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
| 5.6 | Cross-platform session continuity | session join keys | 1.5h | ⬜️ |
| 5.7 | `merlion gateway {start,status}` | `merlion-cli` | 1h | ✅ |
| 5.8 | Voice transcription via Whisper API or local whisper.cpp | `merlion-gateway/src/voice.rs` | 1h | ⬜️ |

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
| 6.4 | Docker terminal backend (run shell commands in a container) | `merlion-tools/src/sandboxes/docker.rs` | 2h | ⬜️ |
| 6.5 | SSH terminal backend (run on a remote host) | `merlion-tools/src/sandboxes/ssh.rs` | 2h | ⬜️ |

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
| 7.8 | Light/dark theme via config | `merlion-cli/src/tui/theme.rs` | 1h | ⬜️ |

**Acceptance:** running `merlion` in a terminal feels modern — streaming,
collapsible tool output, no flicker, interrupt mid-stream.

---

## Phase 8 — Polish, packaging, distribution (≈2 of 6 session hours done)

Make merlion installable in one command from anywhere.

| # | Deliverable | Files | Est. | Status |
|---|---|---|---|---|
| 8.1 | GitHub Actions CI: build + test on linux/macos | `.github/workflows/ci.yml` | 1h | ✅ |
| 8.2 | Release workflow: `cargo dist`-style cross-compiled artifacts | `.github/workflows/release.yml` | 1.5h | ⬜️ |
| 8.3 | Homebrew formula (tap or core) | `Formula/merlion.rb` | 1h | ⬜️ |
| 8.4 | `cargo binstall` metadata | `merlion-cli/Cargo.toml` | 0.25h | ✅ |
| 8.5 | One-line installer: `curl ... \| bash` | `scripts/install.sh` | 1h | ✅ |
| 8.6 | `merlion update` subcommand (self-update) | `merlion-cli` | 1h | ⬜️ |
| 8.7 | `merlion doctor` deepened: probes for `rg`, `git`, MCP servers, gateway tokens, cron | `merlion-cli` | 0.25h | ✅ |

**Acceptance:** `brew install merlion` or `curl https://… | bash` lands a
working binary; `merlion update` self-upgrades to the latest release.

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

**≈32 session hours** of remaining model-time work to a feature-comparable
v1.0. Of the original ≈73-hour estimate, **≈41 h** has been delivered across
phases 0–8. The deferred items are mostly weight-class outliers: AWS/GCP SDK
adapters (Bedrock/Vertex), Discord/Slack Gateway WebSocket clients, Docker/
SSH terminal sandboxes — each large enough to warrant a dedicated session.

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
