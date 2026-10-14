# Work Lifecycle — ben-term

Issues on GitHub, Status on the shared **Active Work** board (also read by the dashboard). Run `/find-work` to see this repo's slice, `/add-work` to file Issues, `/work-on <#>` to take one to a PR.

## Verify

`verifyCommands` are deliberately fast: `cargo check --workspace` (typecheck) and `cargo clippy --workspace -- -D warnings` (lints). Both must pass before the PR.

The fuller battery from root `CLAUDE.md` — `cargo build --release` and `cargo test --workspace` — is slow on a wezterm-sized workspace and is left to manual runs for non-trivial changes (the repo's `/verify` skill runs it).

## Branching / worktrees

This is a wezterm fork. The root `CLAUDE.md` documents the worktree-first convention (`EnterWorktree` before any Edit/Write for non-trivial work) and how `upstream` (the wezterm remote) is wired. Read it before starting work.

## Flow

1. **Filed**: by `/add-work`, the dashboard, or GitHub's web UI. The issue templates and `gh issue create --project` put it on the board; the board sets **Planned**.
2. **Started**: `/work-on <#>` sets **Active**, creates a worktree (`../ben-term-issue-<#>`, branch `issue-<#>-<slug>`), builds, and runs `cargo check --workspace` · `cargo clippy --workspace -- -D warnings`.
3. **PR**: opened with `Closes #<#>` (one line per Issue). The agent stops here; it merges only when asked.
4. **Merged**: GitHub closes the Issue and the board sets **Done**. Nothing else to do.

## When to write an ADR

Write one at `docs/decisions/NNN-<slug>.md` (from `_template.md`) when the work involves at least one of:

- A library swap, schema migration, or third-party API trade-off.
- A non-obvious architectural choice.
- A constraint the code can't show (legal, performance budget, third-party limits).
- Work spanning several sessions that benefits from a frozen rationale.

Skip it for bug fixes, dep bumps, renames, pattern-following features and copy/UI tweaks. ADRs are immutable: supersede, don't edit. Take the next number from `origin/main` right before committing, since parallel sessions can race for it.

## Statuses

**Planned** filed, not started · **Active** in progress · **Blocked** waiting on something external · **Done** merged · **Deferred** parked. Don't rename them: the dashboard reads these names.
