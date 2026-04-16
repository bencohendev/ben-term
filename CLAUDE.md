# Project Rules

## Code Style

- Follow Rust idioms: use `Result`/`Option` properly, avoid `.unwrap()` in non-test code
- Avoid `unsafe` unless absolutely necessary — document why with a `// SAFETY:` comment
- Use `clippy` lint standards — `cargo clippy -- -D warnings` must pass
- Keep modules small and focused — one clear responsibility per file
- Prefer composition over inheritance (trait objects, generics)

## Architecture

- This is a WezTerm fork. Upstream code lives across many workspace crates — understand the crate you're modifying before changing it
- Custom platform features (overlays, pickers, autocomplete) should live in clearly separated modules from upstream code to make rebasing easier
- All features must work on both macOS and Linux — no platform-specific code without a cross-platform abstraction

## Testing & Verification

After adding, modifying, or fixing features, run these checks:

- **Build:** `cargo build --release`
- **Lint:** `cargo clippy --workspace -- -D warnings`
- **Tests:** `cargo test --workspace`

## Branching & Worktrees

- **BEFORE any Edit or Write call**, verify you are in a worktree — if not, call `EnterWorktree` first
- **Exception:** very small changes (doc tweaks, single-line fixes, config updates) can be committed directly on `main`
- All worktrees must be based off the latest `main`
- Use descriptive branch names (e.g., `worktree-fuzzy-cd-overlay`)

## Task Management

- Update tasks as they are completed

## Feature Workflow

### 1. Pick Work
- Use `/find-work` to surface available items
- User selects what to work on

### 2. Mark as Active
- Set the feature spec's `**Status:**` to `Active` (create the spec first if it doesn't exist)
- Commit the status change before starting implementation

### 3. Ensure a Spec Exists
- **If a spec exists:** read it, confirm scope with the user, then begin implementation
- **If no spec exists:** create one in `.readme/features/`, add to `index.md`, get user approval, then begin

### 4. Implement
- Enter a worktree, implement, and verify
- Run all checks (build, clippy, tests) before finishing

### 5. Verify
- Run `/verify` — all checks must pass before proceeding

### 6. Document
- Run `/update-docs` — audit and update all documentation layers
- Commit doc updates and merge to `main`

## Documentation

### Structure
- `.readme/index.md` — master index with implemented and planned feature tables
- `.readme/features/` — one file per feature
- `.readme/bugs/log.md` — bug tracking

### Feature Status
- Each spec has a `**Status:**` line: `Done`, `Active`, or `Not started`
- When new work is planned for a Done feature, set to `Active` and document new scope

### Keeping Index Tables Clean
- When a Planned item is completed, move to Implemented and **delete** from Planned
- Planned tables should only contain items that are actually still open
