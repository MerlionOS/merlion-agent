---
name: test-driven
description: Follow strict test-driven development for a coding task
---

# /test-driven

When working on the task at hand, follow strict TDD discipline:

1. **Write the failing test first.** State the behavior you want, in code.
   The test must compile and run, and it must fail for the right reason
   (assertion failure, not a missing function).
2. **Make it pass minimally.** No surrounding cleanup, no extra cases —
   just enough to flip red→green.
3. **Refactor if needed.** Only when green. Keep tests passing throughout.
4. **Repeat** for each behavior, smallest unit first.

Anti-patterns to avoid:
- Writing the implementation first, then "testing it" — that's a sanity
  check, not TDD.
- Writing five tests upfront — one at a time.
- Adding tests just to bump coverage — each test should specify a behavior
  someone would notice if regressed.

If you can't think of a failing test, that's a signal the requirement isn't
yet concrete enough — refine it before writing code.
