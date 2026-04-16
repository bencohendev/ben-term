---
name: verify
description: Run all verification checks (build, clippy, tests). Use after completing feature work or to check project health. Run /update-docs separately for documentation updates.
user_invocable: true
---

## Instructions

Run all verification checks for the current codebase. This skill focuses purely on code correctness — use `/update-docs` separately for documentation updates.

### Steps

1. **Build** — Run `cargo build --release`. Report pass/fail. If there are errors, list them.

2. **Clippy (lint)** — Run `cargo clippy --workspace -- -D warnings`. Report pass/fail. If there are warnings, list them.

3. **Tests** — Run `cargo test --workspace`. Report pass/fail. If there are failures, list the failing tests.

4. **Fix issues** — If any check failed:
   - Enter a worktree if code fixes are needed
   - Fix the issues
   - Re-run failing checks to confirm they pass
   - Merge fixes back

### Output Format

```
## Verification Results

| Check    | Result | Details          |
|----------|--------|------------------|
| Build    | Pass   |                  |
| Clippy   | Pass   |                  |
| Tests    | Pass   | 42 tests passed  |

### Issues Found
- Description of each issue and fix applied

### Status
All checks passing / N issues remain
```

If everything passes on the first run, keep the output short — just the table and a confirmation.

### Reminder

After verification passes, run `/update-docs` to audit and update all documentation layers.
