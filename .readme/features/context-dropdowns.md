# Context Dropdowns

**Status:** Active

## Overview

Native overlay pickers that appear inline when the user types a recognized command and presses space at a shell prompt. Each picker surfaces relevant, context-aware candidates (directories, git branches, npm scripts, etc.) and inserts the completed command into the shell buffer. Works regardless of shell (zsh / bash / fish), across panes and tabs, without relying on `fzf` subprocesses or shell-specific widgets.

## Scope

**v1 — this spec:** `cd` picker only.

**Planned follow-ups (separate future specs):** `git checkout` / `git stash`, `npm run` / `cargo`, `ssh`, `docker`, `kill`, `env`.

The v1 module layout (`wezterm-gui/src/overlay/context_dropdowns/`) is designed so each follow-up picker adds a sibling file (`git.rs`, `npm.rs`, …) reusing shared trigger-parsing and guard-chain infrastructure from `mod.rs`.

## Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Trigger | Auto on Space when line-left-of-cursor matches `^\s*cd$` | Matches the user's existing zsh `cd<space>` widget; zero extra keystrokes |
| Trigger placement | `KeyAssignment` dispatch, not raw keyevent intercept | Avoids IME / dead-key / compose edge cases; reuses `PerformAssignmentResult::Unhandled` for clean fall-through |
| Alt-screen guard | Picker never fires in alt-screen mode | Keeps Claude Code / vim / htop safe — space passes through to the TUI |
| At-prompt guard | `pane.get_current_working_dir()` must be `Some` | Proxy for "shell is at a prompt" — avoids firing inside `read -p` loops in scripts |
| After select | Insert `cd /path` with NO trailing newline | User reviews and presses Enter; safer for v2 pickers where commands are destructive |
| `~` handling | Resolve to absolute `$HOME` before insert | Avoids bracketed-paste expansion inconsistency across shells |
| Path quoting | Shell-quote paths with spaces / special chars | Correctness |
| Candidate gather | Sync pinned entries; async `walkdir` + zoxide stream in | Picker appears instantly; heavier sources don't block UI thread |
| Walk budget | 300 ms OR 500 entries, whichever hits first | Prevents huge monorepos from stalling the picker |
| Dir walker | `walkdir` crate (already a direct workspace dep) | Avoid adding `ignore` as a direct dep just for v1 |
| Config nesting | Flat fields (`cd_picker_enabled`, etc.) | Premature to nest with one picker; nest when v2 lands |

## Architecture

### New module (custom, isolated from upstream per rebasing strategy)

```
wezterm-gui/src/overlay/context_dropdowns/
  mod.rs   — trigger-line parsing, guard chain, shared helpers
  cd.rs    — cd-specific candidates + insertion
```

### Files modified

| File | Change |
|------|--------|
| `config/src/keyassignment.rs` | New `ActivateContextDropdown(ContextDropdownKind)` variant; `ContextDropdownKind::Cd` |
| `config/src/config.rs` | New fields: `cd_picker_enabled` (default true), `cd_picker_max_depth` (default 5), `cd_picker_include_home` (default true) |
| `wezterm-gui/src/overlay/mod.rs` | `pub mod context_dropdowns;` |
| `wezterm-gui/src/commands.rs` | Entry in `derive_command_from_key_assignment` for the new assignment; default binding Space → `ActivateContextDropdown(Cd)` |
| `wezterm-gui/src/termwindow/mod.rs` | Dispatch new assignment in `perform_key_assignment`; return `Unhandled` on guard-chain miss |

### Reused (don't reimplement)

- Overlay lifecycle — `wezterm-gui/src/overlay/mod.rs` `start_overlay`
- Fuzzy picker UI — `wezterm-gui/src/overlay/selector.rs` (`InputSelector`, nucleo matcher)
- Logical-line reading — `mux/src/pane.rs` `get_logical_lines`, `LogicalLine::xy_to_logical_x`
- Alt-screen guard — `mux/src/pane.rs` `is_alt_screen_active`
- CWD lookup — `Pane::get_current_working_dir(CachePolicy::AllowStale)`
- Text insertion — `Pane::send_paste` (bracketed-paste aware)

## Verification

- [ ] `cd` + Space at zsh prompt → picker opens with `..`, `-`, `~` immediately; other entries stream in
- [ ] `cd` + Space inside `vim` (alt-screen) → Space inserts normally, no picker
- [ ] `cd` + Space inside Claude Code (alt-screen) → Space inserts normally, no picker
- [ ] `echo cd` + Space → no picker (regex mismatch)
- [ ] `cd foo` then cursor-left + Space → no picker (right-of-cursor not empty)
- [ ] Pick an entry → buffer shows `cd /path`, NO execution
- [ ] Pick `~` → buffer shows `cd /Users/bencohen` (resolved)
- [ ] Pick path with spaces → buffer shows `cd '/path with spaces'` (quoted)
- [ ] Press Escape in picker → picker closes, buffer unchanged, no stray space
- [ ] Works with `zoxide` absent (fallback to pinned + walk only)
- [ ] Large monorepo cwd → picker appears <100ms, walker cuts off at 300ms / 500 entries
- [ ] `cd_picker_enabled = false` in `wezterm.lua` → Space always inserts normally
- [ ] Two panes: trigger in A, switch to B before async done → no crash, no stray entries in B
- [ ] Works with `bash` and `fish` (not just `zsh`)
- [ ] `cargo build --bin wezterm-gui` passes
- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
