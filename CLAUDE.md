# merlion-agent — Instructions for AI coding assistants

This file is loaded by Claude Code (and compatible agents) when working in
this repo. Read it once at the start of a session.

## What this project is

merlion-agent is a **Rust port of [hermes-agent](https://github.com/NousResearch/hermes-agent)**,
a self-improving AI coding agent originally written in Python (~830k LOC).
This repo is a clean-room reimplementation aimed at the same shape of problem
with a much smaller, statically-linked footprint.

See [README.md](README.md) for user-facing docs and [ROADMAP.md](ROADMAP.md)
for the phased build plan with per-task session-hour estimates.

## Build & test

```bash
cargo build --workspace
cargo test --workspace
./target/debug/merlion doctor       # smoke test the binary
```

Tests must pass before any commit. If you add a feature, add a test for it.

## Workspace layout

8 crates, ordered by dependency depth (low to high):

```
crates/
├── merlion-core      — Message, Tool trait, Agent loop, ToolApprover, Curator
├── merlion-llm       — LlmClient impls: OpenAiClient, AnthropicClient, GeminiClient
├── merlion-config    — YAML config + ~/.merlion home + provider presets
├── merlion-session   — SQLite + FTS5 conversation store
├── merlion-memory    — MEMORY.md store (file per memory + index)
├── merlion-skills    — SKILL.md loader (agentskills.io-compatible)
├── merlion-tools     — bash, read, write, edit, ls, grep, glob, web_fetch, memory, skill_*
└── merlion-cli       — `merlion` binary with REPL + slash commands
```

`merlion-core` defines the contracts. Provider adapters and tools depend on
it. The CLI sits at the top and wires everything together.

## Conventions

### Tool authoring

A new tool is a single file in `crates/merlion-tools/src/<name>.rs` that
implements the `Tool` trait from `merlion-core::tool`. Mirror the shape of
`bash.rs` or `grep.rs`:

- `pub struct <Name>;` with `#[derive(Default)]`.
- `Args` deserialization struct with `#[derive(Deserialize)]`.
- `#[async_trait] impl Tool` providing `schema()` and `async fn call()`.
- **Never** bubble errors from `call()`. Always return a `ToolResult` —
  failures get `is_error: true` and the message in `content`. The agent
  loop relies on this so it can recover and try again.
- Register the tool in `crates/merlion-tools/src/lib.rs::register_defaults`.

### LLM adapter authoring

Each provider lives in `crates/merlion-llm/src/<name>.rs` and implements
`merlion-core::LlmClient`. Existing examples:

- `openai.rs` — chat-completions; covers 9 OpenAI-compatible providers.
- `anthropic.rs` — `/v1/messages` with content blocks.
- `gemini.rs` — `streamGenerateContent` with parts.

To add a provider, also wire its preset into `merlion-config`'s
`resolve_provider()` and add a `Wire` enum variant.

### Approval gate

Sensitive tools (bash, write, edit, web_fetch) go through `ToolApprover`
before dispatch. `merlion-core::AllowAllApprover` is the test default; the
CLI installs `ConsoleApprover` which prompts the user. If you add a new
tool that does anything irreversible, add its name to `SENSITIVE_TOOLS` in
`crates/merlion-cli/src/approver.rs`.

### Errors

Use `thiserror` for crate-public error enums (see `merlion-core::error::Error`).
Use `anyhow` only inside binaries (`merlion-cli`) and integration tests.

### Comments

Default to writing no comments. Only add one when the **why** is non-obvious:
a hidden invariant, a workaround for a specific bug, behavior that would
surprise a reader. Don't write "what" comments — well-named identifiers
already do that.

### Tests

Per-tool integration tests live in `crates/merlion-tools/tests/<tool>.rs`.
LLM adapter tests using a fixture HTTP server live in `crates/merlion-llm/
tests/<provider>_sse.rs`. Unit tests for pure logic go in `#[cfg(test)]
mod tests` inside the source file.

When you add an LLM adapter, write a fixture-server test that drives a
complete streaming exchange — see `crates/merlion-llm/tests/anthropic_sse.rs`
for the pattern.

## What NOT to do

- **Don't introduce shims for backwards compatibility unless asked.** This is
  pre-1.0 — break the API freely.
- **Don't write planning or summary `.md` files** unless they're required
  deliverables. ROADMAP.md tracks the plan; CLAUDE.md captures conventions;
  README.md is for users. Everything else clutters the repo.
- **Don't add dependencies casually.** Justify each one. Prefer
  `workspace.dependencies` over per-crate deps.
- **Don't reach for `bash -lc` from inside a tool.** Use
  `tokio::process::Command` directly with explicit args. Shell injection is
  a real concern; the tool args come from the LLM.
- **Don't bypass the approval gate.** Sensitive tools must go through
  `ToolApprover::approve()`. If you find yourself wanting to skip it,
  re-examine whether the operation should be a tool at all.
- **Don't commit `Cargo.lock` changes that aren't motivated by a Cargo.toml
  change** — those are usually accidental.

## Hermes parity expectations

The on-disk session format is intentionally **not** wire-compatible with
hermes. We use our own SQLite schema under `~/.merlion`. Conversation file
formats, tool semantics, and CLI affordances aim to feel familiar to
hermes users but are not a literal port.

If something is hard to port — large Python subsystems, plugin frameworks,
training-data tooling — first check [ROADMAP.md](ROADMAP.md) to see if it's
explicitly **out of scope**. The out-of-scope list includes batch
trajectory generation, browser tools, ACP integration, and most of the
~20 messaging-platform adapters.

## When in doubt

- Read `crates/merlion-core/src/agent.rs` to understand the agent loop.
- Read `crates/merlion-tools/src/bash.rs` as the canonical tool example.
- Read `crates/merlion-llm/src/openai.rs` as the canonical adapter example.
- Read `ROADMAP.md` to understand what's planned vs. shipped.
