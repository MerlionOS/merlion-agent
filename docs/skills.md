# Skills

Skills are small markdown files the user can invoke with `/<slug>` in chat.
Each one acts as a named, reusable instruction the agent follows for the
turn it's invoked on.

## File format — `agentskills.io`-compatible

A skill is a markdown file with a YAML front-matter header:

```markdown
---
name: review
description: Pre-landing code review of the current branch against main
---

# /review

Instructions the agent reads when the user types `/review` in chat:

1. Identify the base branch via `git symbolic-ref refs/remotes/origin/HEAD`,
   fall back to `main`.
2. `git diff <base>...HEAD` to see what would land.
3. Report findings as a numbered list with file:line refs.
4. Do not modify code.
```

**Front-matter fields:**

| Field | Required | Description |
|---|---|---|
| `name` | optional[1] | Kebab-case slug, e.g. `review`, `test-driven` |
| `description` | required | One-line summary shown in `/help skills` and the system-prompt manifest |

[1] If omitted, derived from the filename (or directory name, for the
directory layout below).

**Body:** plain markdown. Becomes the user-turn content when the skill is
invoked, with any extra text the user typed after `/<slug>` appended as
`User-supplied arguments: <text>`.

## Two on-disk layouts

```
skills_root/
├── review.md                  # flat — slug "review"
└── test-driven/               # directory — slug "test-driven"
    └── SKILL.md
```

The flat layout is easiest. The directory layout is useful when a skill
ships with supporting files (templates, example data, scripts the agent
should read). Directory layout matches the [agentskills.io](https://agentskills.io)
convention exactly — skills exported from there should drop into
`~/.merlion/skills/` and work without modification.

## Skill roots

merlion looks in two places at startup, in this order:

1. `./skills/` — bundled with the merlion source tree. Useful when developing.
2. `$MERLION_HOME/skills/` (or `~/.merlion/skills/`) — user-installed.

The second root wins on collision: a user `~/.merlion/skills/review.md`
overrides the bundled `./skills/review.md`. This matches how the rest of
merlion's config layers work.

## Compatibility notes

Skills are wire-compatible with [agentskills.io](https://agentskills.io) in
both directions:

- **Importing into merlion:** drop the skill's directory into
  `~/.merlion/skills/` (or extract a `.skill` archive there). merlion will
  parse the front-matter and surface it as `/<name>` automatically.

- **Exporting from merlion:** any `~/.merlion/skills/<name>/` directory or
  `<name>.md` file is already in the agentskills.io format. To package,
  `tar czf <name>.skill.tar.gz <name>/` and upload.

Differences from the standard:

- merlion does not (yet) read or respect agentskills.io's `model_directives`,
  `parameters`, or `version` front-matter fields. These parse and are ignored.
- Skills can't currently install dependencies on first use — bring your own
  scripts in the directory. (Future Phase 3.x work.)
- merlion's `skill_create` and `skill_update` tools both write the flat
  layout, not the directory layout.

## See also

- [skill_create / skill_update tools](../README.md#tools) — let the agent
  write its own skills mid-conversation.
- `merlion-skills/src/parse.rs` — the front-matter parser.
- `merlion-skills/tests/load.rs` — fixture-based round-trip tests covering
  both layouts.
