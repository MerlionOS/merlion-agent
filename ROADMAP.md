# Merlion Roadmap

The Python hermes-agent is ~830,000 lines across many subsystems. Merlion is a
ground-up Rust reimplementation that intentionally trims scope. This document
maps what's done, what's next, and what's deliberately out of scope.

The unit of estimation here is **Claude Code session hours** — the time the
coding agent spends building it, not human-pace estimates.

## Phase 0 — MVP (done)

- [x] Cargo workspace and crate split
- [x] `merlion-core`: messages, tool trait, agent loop with iteration budget
- [x] `merlion-llm`: OpenAI-compatible streaming client (SSE)
- [x] `merlion-tools`: bash, read, write, edit, ls
- [x] `merlion-config`: `~/.merlion/config.yaml`, `.env`, env overrides,
  provider presets for OpenAI / OpenRouter / Nous / Novita / Moonshot /
  MiniMax / z.ai / Groq / DeepSeek
- [x] `merlion-session`: SQLite + FTS5 session store
- [x] `merlion-cli`: chat REPL with streaming, slash commands, `model`,
  `config`, `doctor`, `sessions list`/`sessions search`

## Phase 1 — Provider breadth (≈4–6 session hours)

- [x] Anthropic native adapter (`/v1/messages`) — `anthropic:` provider
- [x] Gemini native adapter (`streamGenerateContent`) — `gemini:` provider
- [ ] Bedrock + Vertex passthroughs
- [ ] Usage / cost accounting per response
- [ ] Retry with backoff on 429 / 5xx (already partly handled by reqwest)

## Phase 2 — Tool surface (≈6–10 session hours)

- [ ] `grep` (ripgrep-style — likely shells out to `rg`)
- [ ] `find` / `glob`
- [ ] `task` — spawn subagent with isolated context
- [ ] `web_fetch` — HTTP GET with readability-style extraction
- [ ] `web_search` — pluggable provider (Brave/Tavily/SerpAPI)
- [ ] Tool approval / allowlist callback (mirrors hermes `approval.py`)
- [ ] Tool-result truncation + on-disk overflow (hermes `tool_result_storage`)

## Phase 3 — Memory & skills (≈8–12 session hours)

- [ ] MEMORY.md + USER.md style memory store
- [ ] `memory` tool (read/write/forget)
- [ ] Skill discovery from `~/.merlion/skills/` and bundled `skills/`
- [ ] `/<skill>` slash invocation
- [ ] Compatibility with the [agentskills.io](https://agentskills.io) format
- [ ] Curator-style nudges to persist learning

## Phase 4 — MCP integration (≈6–8 session hours)

- [ ] Stdio + HTTP MCP transports
- [ ] Tool injection from connected MCP servers
- [ ] OAuth flow for MCP servers that need it

## Phase 5 — Messaging gateway (≈12–20 session hours)

- [ ] `gateway` subcommand, per-platform adapters
- [ ] Telegram, Discord, Slack first (highest user value)
- [ ] DM pairing and per-user allowlists
- [ ] Conversation continuity across CLI ↔ messaging

## Phase 6 — Cron + remote execution (≈6–8 session hours)

- [ ] Cron scheduler (tokio-cron-scheduler)
- [ ] Job → message-platform delivery
- [ ] Sandboxed terminal backends: docker, ssh
  - Modal / Daytona / Vercel Sandbox / Singularity deferred

## Phase 7 — TUI (≈8–12 session hours)

- [ ] ratatui-based terminal UI
- [ ] Multiline editing, slash autocomplete, history scroll
- [ ] Streaming output with interrupt-and-redirect

## Out of scope (at least for now)

These are real features in hermes that Merlion is **not** planning to port,
because they're tangential to the core agent loop or require an enormous amount
of infrastructure:

- Batch trajectory generation
- Trajectory compression for training
- ACP adapter (VS Code / Zed / JetBrains integration via the Agent Client
  Protocol) — re-evaluate if there's user demand
- The full ~20-platform messaging matrix (we ship 3, others welcome as PRs)
- Browser tool (Camoufox / CDP) — large surface area; reconsider in Phase 8
- Honcho-style dialectic user modeling
- Image-gen and TTS plugins

## Sequencing

Phases 1–4 are the highest-leverage next steps. Phases 5–7 are user-visible but
much larger; they should be done once the core engine is stable and tested.

If you want to contribute, the easiest places to start are Phase 1 (Anthropic
adapter) and Phase 2 (`grep`/`find` tools).
