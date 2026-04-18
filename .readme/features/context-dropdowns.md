# Context Dropdowns

**Status:** Active — v1/v2/v3 shipped for `cd`; `PopupPicker` (shared popup shell), PTY-echo input model, and instant-open / async-loading candidate gather shipped alongside git picker Phase 1. Spec now primarily covers shared infrastructure + the cd picker; per-command pickers live in their own specs (see [git-picker.md](git-picker.md)).

## Overview

Native overlay pickers that appear inline when the user types a recognized command and presses space at a shell prompt. Each picker surfaces relevant, context-aware candidates (directories, git branches, npm scripts, etc.) and inserts the completed command into the shell buffer. Works regardless of shell (zsh / bash / fish), across panes and tabs, without relying on `fzf` subprocesses or shell-specific widgets.

## Scope

**v1 — this spec:** `cd` picker only.

**Planned follow-ups (separate future specs):** `npm run` / `cargo`, `ssh`, `docker`, `kill`, `env`. (`git` pickers now live in [git-picker.md](git-picker.md); Phase 1 — ref-taking subcommands — shipped.)

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
| Candidate gather | Async on worker thread, modal opens instantly with "Loading…" placeholder | The ~100–500 ms zoxide + BFS walk was perceptible lag between Space and popup appearing. Results swap in via `TermWindowNotif::Apply` when ready. Prior v1–v3 behavior was sync-on-main-thread |
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

Known v1 imperfections (v2 resolves via OSC 133 semantic zones):

- `echo cd ` + Space triggers the picker (cd as argument, not command). Escape dismisses.
- `cd fo|o` with cursor mid-line triggers the picker (right-of-cursor check was dropped for RPROMPT compatibility).

## v2 — OSC 133 strict trigger

When the shell emits OSC 133 semantic-prompt markers (via wezterm's shell integration — see `docs/shell-integration.md` and `assets/shell-integration/`), the `cd` picker switches to a strict trigger path that resolves both known v1 imperfections.

### How it works

wezterm already parses OSC 133 into `SemanticZone`s (`Prompt` / `Input` / `Output`) reachable via `pane.get_semantic_zones()`. A new helper `split_input_zone_at_cursor` (in `overlay/context_dropdowns/mod.rs`) locates the `Input` zone containing the cursor and returns the zone's text to the left and right of the cursor — bypassing the PS1 prompt and any RPROMPT chrome entirely.

The trigger then runs two strict checks on the Input zone contents:

1. **First-token match** — the command's first whitespace-delimited token must be `cd`, with no second token. `echo cd` has `echo` as first token → rejected.
2. **Trailing-edge cursor** — text to the right of the cursor within the zone must be empty / whitespace. `cd fo|o` has `o` to the right → rejected.

### Fallback when OSC 133 is absent

If no Input zone is found at the cursor (shell has no integration installed), the picker falls back to the v1 line-heuristic (last-token-is-`cd` check). Users without shell integration keep the v1 behavior, accepting its known false-positives; users with shell integration get the stricter v2.

### Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| OSC 133 detection | Look for `SemanticType::Input` zone containing cursor | Presence = integration active; absence = unintegrated shell → fall back |
| Fallback policy | v1 heuristic when OSC 133 absent | Keeps the picker working for unintegrated shells; strict behavior only enabled automatically when the data supports it |
| Strict-path checks | First token AND right-of-cursor in zone | Both v1 imperfections come from not knowing command-vs-argument or zone bounds — OSC 133 gives us both |
| No new config flag | Detection is automatic | One less knob; the behavior is strictly better when data is available |

### Verification

- [x] Unit tests: `is_cd_command_start` accepts `cd`, `  cd`, `cd `, rejects `echo cd`, `cd foo`, `cdx`
- [x] `cargo build --bin wezterm-gui` passes
- [x] `cargo test --bin wezterm-gui context_dropdowns` passes

Left for the user to confirm on their setup (after sourcing wezterm's shell integration):

- [ ] `echo cd ` + Space → no picker (strict path rejects; v1 fallback would have fired)
- [ ] `cd fo|o` with cursor mid-argument → no picker
- [ ] `cd` + Space at prompt → picker opens (strict path accepts)
- [ ] Without shell integration (fresh shell, no `__wezterm_set_user_var` sourcing) → v1 heuristic still fires on `cd` + Space

Pre-existing workspace-wide limitation:

- `cargo clippy --workspace -- -D warnings` fails on unrelated upstream crates (`wezterm-char-props`, `config/src/background.rs`, etc.). This is pre-existing debt in the WezTerm fork and out of scope for this feature.

## v3 — Modal popup rendering

v1 and v2 ran the picker inside a full-pane `TermWizTerminal` overlay (`start_overlay_pane`) which visually replaced the entire shell pane. v3 rebuilds it as a floating popup on top of wezterm's existing `Modal` trait — the same render path the built-in command palette uses. The shell prompt and surrounding output stay visible behind the picker.

### How it works

`CdPicker` implements `Modal` in `wezterm-gui/src/overlay/context_dropdowns/cd_picker.rs`. On trigger, `cd::try_activate` snapshots the cursor's viewport `(col, row)` from `pane.get_cursor_position()` / `pane.get_dimensions().physical_top`, gathers candidates synchronously as before, then calls `term_window.set_modal(Rc::new(CdPicker::new(entries, pane_id, cursor_col, cursor_viewport_row)))`.

Paint integration is entirely in upstream `paint_modal` (`render/paint.rs`), which composites the picker's `ComputedElement` at zindex 100 after the pane render pass. Keystrokes bypass the PTY while the modal is installed because `TermWindow::key_event_impl` routes to `modal.key_down` when one exists. The picker's own `key_down` handles arrows / filter editing / Enter / Escape / Ctrl-C+G+U and returns `Ok(true)` to consume them.

### Positioning & sizing

```
anchor_x = min(
    padding_left + cursor_col * cell_w,
    padding_left + avail_pixel_width - desired_pixel_width
).max(padding_left)
anchor_y = top_bar_height + padding_top + os_border.top + (cursor_viewport_row + 1) * cell_h
width   = 50 cols * cell_w         (clamped to available columns)
border  = 1px, color = command_palette_fg_color
corners = 0.5 cells, rounded (TOP_*_ROUNDED_CORNER / BOTTOM_*_ROUNDED_CORNER polys)
padding = 0.5 cell horizontal, 0.25 cell vertical
colors  = reuses command_palette_{bg,fg}_color
font    = reuses command_palette_font
```

Anchored below and left-aligned to the cursor so the popup feels like a contextual dropdown instead of a centered palette. Width is fixed at 50 cols — path labels remain legible without dominating the pane. Clamped against the right edge on narrow windows.

### Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Render mechanism | Implement `Modal` trait | Only popup-capable primitive in the codebase; upstream already composites `Modal`s at zindex 100 with no pane replacement |
| Parallel vs. shared | Parallel to `palette.rs`, zero shared code | Keeps our divergence isolated so upstream palette changes don't ripple in |
| Anchor | Below + left-aligned to shell cursor | Matches "dropdown" affordance; follows the typed `cd ` insertion point |
| Anchor clamp | Right-edge clamp, cursor-row clamp to `max(0)` | Avoid clipping off-screen when the cursor is near the right or scrolled above viewport |
| Width | Fixed 50 cols (clamped to available) | Reads comfortably; leaves the shell context visible on both sides |
| Style | Reuse `command_palette_{fg,bg}_color` / font | No new user-visible config; matches wezterm popup aesthetic |
| Filter cursor | Keep v2's full editor (Left/Right/Home/End/Delete/Ctrl-{U,A,E,B,F,G,C,K,J,N,P}) | User-visible regression if dropped. **Superseded by PTY-echo — see section below.** |
| Cursor display | Underscore at insertion point inside the header `>` line | Trivial; no separate caret primitive. **Superseded by PTY-echo — the shell cursor is the real cursor; popup no longer renders a header.** |
| Double-trigger guard | `TermWindow::has_modal()` check in `try_activate` | Duplicate Space events can't stack a second modal on top of the first |
| Source pane | `CdPicker` stores `PaneId` at construction; resolves via `Mux::get().get_pane` on Enter | Pane identity is stable for the modal's lifetime |
| Deleted code | Retired `picker.rs` (raw-mode input loop on `TermWizTerminal`) | No longer needed — Modal path owns input and render |

### Files modified

| File | Change |
|------|--------|
| `wezterm-gui/src/overlay/context_dropdowns/cd_picker.rs` | **New**. `CdPicker` + `PickerEntry` + `Modal` impl + all filter-cursor tests |
| `wezterm-gui/src/overlay/context_dropdowns/cd.rs` | Swap `start_overlay_pane` block for `term_window.set_modal(…)`. Add `has_modal` guard. Drop `run_cd_picker`, `TermWizTerminal`, `promise::spawn` imports. `shell_quote` becomes `pub(super)` so the Modal's `Enter` handler can use it |
| `wezterm-gui/src/overlay/context_dropdowns/mod.rs` | `pub mod picker` → `pub mod cd_picker` |
| `wezterm-gui/src/overlay/context_dropdowns/picker.rs` | **Deleted** (~360 lines) |
| `wezterm-gui/src/termwindow/mod.rs` | Minimal upstream exposure: fields `fonts`, `show_tab_bar`, `render_state` become `pub`; `invalidate_modal` becomes `pub`; new `pub fn has_modal()` and `pub fn terminal_size()`. All mirror the access upstream Modal siblings (palette/charselect/paneselect) already have inside the module |

### Verification

- [x] `cargo build --bin wezterm-gui` passes
- [x] `cargo test --bin wezterm-gui context_dropdowns` — 20/20 passing (all v1 + v2 tests preserved; `enter_resolves_selected_entry` and `selected_value_is_none_when_no_matches` added)
- [x] `cargo clippy --no-deps -p wezterm-gui` clean on our module (pre-existing workspace debt in `inputmap.rs` unchanged, out of scope)
- [x] Manual QA: popup opens below the shell cursor, half-width, rounded border visible; shell prompt + output still visible around it; Enter pastes `cd <path>`; Escape dismisses; Resize while open relayouts

### Known follow-ups (out of scope for v3)

- Click-away dismiss (palette doesn't do it either — would add in `Modal::mouse_event`)
- If cursor is near the bottom of the window, the popup may extend below the visible grid. Could flip to anchor *above* cursor on that edge case — deferred until it bites
- Pane cursor stays visible under the popup (palette precedent). Fine for now; can be hidden via a cursor-visibility hook if it visually clashes

## Shared `PopupPicker` — PTY-echo input model (shipped alongside git picker Phase 1)

During the git picker work the popup's input handling was reworked so the shell remains the "real" cursor. This change applies to every picker that uses `PopupPicker` (cd, git, future phases).

### Behavior

- The popup no longer renders a `> <filter>` header. Only the candidate list is shown.
- The trigger (`cd` / `git <sub>`) re-sends the consumed Space to the PTY, so the shell command line shows e.g. `cd ` with a trailing space once the popup opens.
- Typed filter characters are both (a) appended to the popup's internal filter used for fuzzy ranking, and (b) forwarded to the PTY via `Pane::key_down` so they echo into the shell's command line in real time.
- Backspace/Delete likewise pop the filter and forward a Backspace to the PTY, keeping the command line and popup filter in sync. On an empty filter, Backspace closes the popup AND still forwards so the user can keep rubbing out the trigger text (`cd ` then `cd` etc.).
- Ctrl-U clears the filter and sends N Backspaces to the PTY.
- Enter sends N Backspaces (to rub out the filter chars we echoed) then pastes `shell_quote(value)` — no leading space since it's already in the shell from the trigger.
- Escape closes the popup and leaves whatever the user typed visible in the shell.

### Code changes

- Dropped `filter_cursor` field + all positional editing (`filter_left/right/home/end/filter_delete` + Ctrl-A/E/B/F handlers). The popup is append-only now; positional edits happen in the shell.
- Added `PopupPicker::forward_to_pane(key, mods)` and `PopupPicker::send_backspaces(n)` helpers.
- `cd::try_activate` and `git::try_activate` now call `pane.key_down(KeyCode::Char(' '), KeyModifiers::NONE)` immediately after `set_modal`.
- Enter handler pastes without the leading space.

### Why this is better

- "Where is the cursor?" has one answer: the shell. No dual-cursor confusion.
- The command line is the source of truth — if the user Escapes, they still see what they typed and can edit / submit it manually.
- Backspace past the trigger naturally backs out the whole interaction one character at a time.

## Instant-open / async-loading candidate gather

Originally `cd::try_activate` gathered candidates synchronously on the main thread before calling `set_modal`, so users perceived a ~100–500 ms stall between pressing Space and the popup appearing. The zoxide subprocess + `$HOME` BFS walk (up to 500 ms budget, 10 000 entries) is the dominant cost. Moving it off the critical path makes the popup feel instantaneous.

### Behavior

- Popup opens immediately on Space with an empty candidate list and a `Loading…` placeholder row (instead of the false-negative `(no matches)`).
- A worker thread runs `gather_candidates`; when it finishes, it posts a closure back to the main thread via `window.notify(TermWindowNotif::Apply(...))`. The closure downcasts the current modal to `PopupPicker` and calls `set_entries_loaded` to swap entries in + invalidate cached match list + trigger a repaint.
- Any filter text the user typed during the load is preserved — `set_entries_loaded` clears the cached `MatchResults` so the first render after the swap re-fuzzies the fresh entries against the already-typed filter.
- If the user dismisses the popup (Escape) or re-triggers cd before the gather returns, the stale results are silently discarded. Each `PopupPicker` has a monotonic `id` (atomic counter); the background closure captures the id it was spawned for and only applies results when the currently-installed modal's id matches.

### Code changes

- `PopupPicker` gains:
  - `entries: RefCell<Vec<PickerEntry>>` (was a plain `Vec`) so a deferred loader can replace them in place.
  - `loading: RefCell<bool>` — renders `Loading…` when true + entries empty.
  - `id: u64` + `fn id(&self)` — per-activation identity for stale-result guard.
  - `fn new_loading(...)` — constructs in loading state with no entries.
  - `fn set_entries_loaded(entries)` — swaps in entries, clears `loading`, invalidates `matches`/`element` caches.
- `cd::try_activate` now calls `set_modal` immediately, echoes the trigger space, and spawns `promise::spawn::spawn_into_new_thread` for the gather. The notify closure locates the picker via `TermWindow::get_modal().downcast_ref::<PopupPicker>()` and only applies if `picker.id() == captured_id`.
- `TermWindow::get_modal` is now `pub` (like `invalidate_modal` / `has_modal`) so the notify closure can access the installed modal.

### Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Main-thread mutation only | `RefCell` on `entries`/`loading`; cross-thread work hops back via `TermWindowNotif::Apply` | `PopupPicker` lives in `Rc<dyn Modal>` which is `!Send`. The notify closure runs on the main thread so RefCell is safe; no new locking primitives needed |
| Stale-result guard | Per-activation monotonic `id` compared before applying | User can Escape and reopen cd while the first gather is in flight; the id mismatch makes the old result a no-op instead of corrupting the new picker |
| Loading placeholder | Single `Loading…` row (not a spinner) | Gather completes in ≤500 ms budget, so an animated spinner is overkill; a static label is zero render cost |
| No cancellation | Background task always runs to completion | The gather is short, bounded, and side-effect-free. Building a cancellation channel for ≤500 ms of work is over-engineering |

### Verification

- [x] `cargo build --bin wezterm-gui` passes
- [x] `cargo build --release --bin wezterm-gui` passes
- [x] `cargo test --bin wezterm-gui context_dropdowns` — 28/28 passing
- [x] `cargo clippy --no-deps -p wezterm-gui -- -D warnings` — our module clean (pre-existing `bind-instead-of-map` error in `wezterm-gui/build.rs` is unchanged upstream debt, out of scope)

Left for the user to confirm on their setup:

- [ ] `cd` + Space → popup appears instantly (no perceptible lag), briefly shows `Loading…`, then populates
- [ ] Type filter chars while `Loading…` is shown → when entries arrive, list is already filtered to match
- [ ] `cd` + Space, Escape quickly, `cd` + Space again → no stale results bleed into the second popup
