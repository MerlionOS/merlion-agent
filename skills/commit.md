---
name: commit
description: Create a focused git commit for the staged changes
---

# /commit

Make a single, focused git commit for the current changes.

1. `git status` and `git diff --staged` to see what will be committed.
   If nothing is staged, `git diff` to see unstaged changes — but DO NOT
   blanket-add everything. Ask the user which files to include if it's
   ambiguous.
2. Write a commit message that explains **why**, not what:
   - **Subject line:** under 70 characters, imperative mood ("Add", "Fix",
     "Refactor" — not "Added" / "Fixes" / "Refactoring").
   - **Body:** 1–3 sentences explaining the motivation. The "what" is
     visible in the diff; the "why" is what future you needs.
3. Match the repo's existing style — read `git log -5 --oneline` for tone.
4. **Never** commit files that may contain secrets (`.env`, `credentials.*`,
   `id_rsa`, `*.pem`).
5. **Never** `git add -A` or `git add .` without first listing what would
   be added.
6. Don't push unless the user explicitly asks.
