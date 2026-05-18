---
name: review
description: Pre-landing code review of the current branch against main
---

# /review

Run a pre-landing review of the changes on the current branch.

1. Identify the base branch (try `git symbolic-ref refs/remotes/origin/HEAD`,
   fall back to `main`).
2. `git diff <base>...HEAD` to see all changes that would be merged.
3. `git log <base>..HEAD --oneline` to see the commit history.
4. Review the diff for:
   - **Correctness** — does the code do what the commit messages claim?
   - **Safety** — SQL injection, command injection, path traversal, secrets in code.
   - **Conditional side effects** — code that runs unconditionally in a function
     whose name implies it's gated (e.g. an `if let Ok(...)` whose `Err` arm
     silently does nothing dangerous).
   - **Trust boundaries** — when input from the model is fed into a shell
     or a SQL query.
   - **Test coverage** — are the new behaviors tested? If not, propose tests.
5. Report findings as a numbered list with file:line refs.
6. Do **not** modify code; this is a read-only review.
