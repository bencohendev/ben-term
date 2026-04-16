# Context Dropdowns

**Status:** Active — v1 (cd) shipped; follow-ups not started

## Overview

Native overlay pickers that appear inline when the user types a recognized command and presses space at a shell prompt. Each picker surfaces relevant, context-aware candidates (directories, git branches, npm scripts, etc.) and inserts the completed command into the shell buffer. Works regardless of shell (zsh / bash / fish), across panes and tabs, without relying on `fzf` subprocesses or shell-specific widgets.

## Scope

**v1 — this spec:** `cd` picker only.

**Planned follow-ups (separate future specs):** `git checkout` / `git stash`, `npm run` / `cargo`, `ssh`, `docker`, `kill`, `env`.

The v1 module layout (`wezterm-gui/src/overlay/context_dropdowns/`) is designed so each follow-up picker adds a sibling file (`git.rs`, `npm.rs`, …) reusing shared trigger-parsing and guard-chain infrastructure from `mod.rs`.

## Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Trigger | Auto on Space when last whitespace-delimited token in line-left-of-cursor is `cd` | Works across all shell prompts (PS1 text precedes `cd` in the line buffer); zero extra keystrokes |
| Trigger placement | `KeyAssignment` dispatch, not raw keyevent intercept | Avoids IME / dead-key / compose edge cases; reuses `PerformAssignmentResult::Unhandled` for clean fall-through |
| Alt-screen guard | Picker never fires in alt-screen mode | Keeps Claude Code / vim / htop safe — space passes through to the TUI |
| Right-of-cursor check | Intentionally dropped | Shells with RPROMPT (powerline themes, git-status blocks) render text to the right of user input, which would otherwise permanently block the trigger |
| Overlay re-entry guard | Skip trigger if pane already has an overlay | Keystrokes inside the picker go to the picker's own input stream, not back through the trigger |
| After select | Paste ` /path` (leading space, no newline) | User already typed `cd`; we only append the argument. User confirms with Enter |
| `~` handling | Resolve to absolute `$HOME` before insert | Avoids bracketed-paste expansion inconsistency across shells |
| Path quoting | Shell-quote paths with spaces / special chars | Correctness |
| Candidate gather | Sync on main thread before mounting overlay | Keeps initial render non-blank; BFS walker + zoxide is typically <500 ms |
| Dir walker | Hand-rolled BFS over `std::fs::read_dir` | `walkdir`'s DFS exhausts the budget on the first huge subtree (`~/Desktop`, `~/Documents`); BFS guarantees depth-N coverage before any depth-N+1 descent |
| Walk scope | $HOME depth 5 always; CWD depth 1-3 if outside $HOME | Projects live in $HOME; depth 5 covers typical monorepo nesting |
| Walk budget | 500 ms wall-clock OR 10 000 entries | Bounded cost; caps huge homes |
| Noise skipping | VCS / build dirs universally; system dirs only at root `/`; `Library`/`Applications`/`Pictures` at $HOME top level | Keeps results relevant without hardcoding user-specific paths |
| Zoxide integration | Shell out to `zoxide query --list` with hardcoded fallback paths for Homebrew | macOS GUI apps don't inherit shell PATH |
| Overlay registration | `assign_overlay_for_pane(pane_id, overlay)` — NOT `assign_overlay(tab_id, ...)` | `start_overlay_pane` dismisses via `CancelOverlayForPane(pane_id)`; mismatched storage = picker never closes |
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

Confirmed working in manual QA (v1):

- [x] `cd` + Space at zsh prompt → picker opens with `..`, `-`, `~` and matched entries
- [x] Pick an entry → buffer shows `cd /path`, NO execution
- [x] Arrow keys / typing filter / Escape / Enter all work
- [x] Typing `dev/repos` surfaces every direct child under `~/dev/repos` (BFS walker)
- [x] `cargo build --bin wezterm-gui` passes
- [x] `cargo test --bin wezterm-gui context_dropdowns` passes (7/7)
- [x] `cargo clippy --no-deps -p wezterm-gui` — my module is clean

Left for the user to confirm on their setup:

- [ ] `cd` + Space inside `vim` (alt-screen) → Space inserts normally, no picker
- [ ] `cd` + Space inside Claude Code (alt-screen) → Space inserts normally, no picker
- [ ] Pick `~` → buffer shows `cd /Users/bencohen` (resolved)
- [ ] Pick path with spaces → buffer shows `cd '/path with spaces'` (quoted)
- [ ] `cd_picker_enabled = false` in `wezterm.lua` → Space always inserts normally
- [ ] Works with `bash` / `fish` shells

Known v1 imperfections (v2 will resolve via OSC 133 semantic zones):

- `echo cd ` + Space triggers the picker (cd as argument, not command). Escape dismisses.
- `cd fo|o` with cursor mid-line triggers the picker (right-of-cursor check was dropped for RPROMPT compatibility).

Pre-existing workspace-wide limitation:

- `cargo clippy --workspace -- -D warnings` fails on unrelated upstream crates (`wezterm-char-props`, `config/src/background.rs`, etc.). This is pre-existing debt in the WezTerm fork and out of scope for this feature.
