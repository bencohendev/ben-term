# Work Lifecycle — ben-term

This repo uses the work lifecycle workflow. Open Claude Code (or any agent) and run `/find-work` to see the kanban, or `/work-on <#>` to drive an Issue end-to-end.

## Skills

| Skill | When to use |
|---|---|
| `/find-work` | See what's Active, Planned, Blocked across projects |
| `/add-work <text>` | File a new Issue, attach to board with Status = Planned |
| `/work-on [#]` | **Single hand-off.** Drives the full lifecycle on an Issue. Resumable based on Issue + PR state. With no argument, shows the kanban and prompts you to pick. |
| `/start-work <#>` | (escape hatch) Flip Status → Active, suggest branch, print phases |
| `/verify-work` | (escape hatch) Run verify commands; no state change |
| `/finish-work <#>` | (escape hatch) Verify → confirm PR merged → Status → Done → close Issue |

## Phases an agent walks through during `/work-on`

1. **Build/implement** — work happens in a worktree (preferred) or on a feature branch (suggested name: `issue-<#>-<slug>`). See root `CLAUDE.md` for the worktree convention.
2. **Document (only if warranted)** — write an ADR at `docs/decisions/NNN-<slug>.md` *only if* the work made a non-obvious decision. Skip otherwise.
3. **Verify** — `cargo check --workspace` + `cargo clippy --workspace -- -D warnings` must pass. For larger changes, also run `cargo build --release` and `cargo test --workspace` (per root `CLAUDE.md`).
4. **PR + review** — open the PR; the agent stops here. Re-run `/work-on <#>` after merge to flip Status → Done.

## When to write an ADR

Write an ADR when the work involves at least one of:

- A library swap, schema migration, or third-party API trade-off.
- A non-obvious architectural choice (e.g. "use blob: URL not data URI because Safari rejects 2MB+ data URIs").
- A constraint that isn't visible in the code (legal, performance budget, third-party API limits, upstream-merge compatibility).
- Work that will likely span multiple sessions and benefits from a frozen rationale.

**Skip the ADR for:** bug fixes, dep bumps, renames, single-file features that follow established patterns, copy/UI tweaks. Most work doesn't get an ADR — that's the point.

ADRs are *immutable*. Once written, they record a decision at a point in time. Don't edit them; if a decision is reversed later, write a new ADR superseding it.

ADR template lives at `docs/decisions/_template.md`. Naming: `docs/decisions/NNN-<slug>.md` where `NNN` is the next zero-padded number.

## State definitions

- **Planned** — Issue is filed and on the board, no work started.
- **Active** — A branch (or worktree) exists and work is in progress.
- **Blocked** — Work has started but is waiting on something external.
- **Done** — PR merged to `main`.
- **Deferred** — Work is on the board but not currently planned.

## Verify

The configured `verify-work` commands are deliberately fast: `cargo check --workspace` (typecheck) and `cargo clippy --workspace -- -D warnings` (lints). Both must pass before `/finish-work` will close out an Issue.

The fuller battery from root `CLAUDE.md` — `cargo build --release` and `cargo test --workspace` — is slow on a wezterm-sized workspace and is left to manual runs for non-trivial changes.

## Branching / worktrees

This is a wezterm fork. The root `CLAUDE.md` documents the worktree-first convention (`EnterWorktree` before any Edit/Write for non-trivial work) and how `upstream` (the wezterm remote) is wired. Read it before starting work.

## Cold-start orientation

When entering this repo for the first time (or first time in a while), read `.work/repo-map.md` if present — a hierarchical view of source files with their top-level exports. Regenerate via `/refresh-repo-map`. Keep in mind ben-term is a large workspace; the map is most useful for the fork-specific crates rather than every upstream wezterm crate.

## Where things live

- **Issues, labels, board state** — GitHub (source of truth for state).
- **Lifecycle config** — `.work/config.json` (board IDs, verify commands, ADR dir).
- **Repo map** — `.work/repo-map.md` (manual via `/refresh-repo-map`).
- **This file** — workflow conventions for this repo.
- **ADRs** — `docs/decisions/NNN-<slug>.md`.
- **Project rules** — root `CLAUDE.md` (Rust style, worktree convention, upstream merge policy).
