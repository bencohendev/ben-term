# Git Picker

**Status:** Active — Phases 1–4 shipped (refs, stash, files, remotes); commit-hash picker still planned.

## Overview

A context dropdown that fires on `git <sub>` (or `git stash <action>`) + Space at a shell prompt and lists candidates appropriate to the subcommand. Reuses the v3 `PopupPicker` Modal popup shell shipped by [context-dropdowns.md](context-dropdowns.md); this spec covers trigger detection and candidate gathering specific to git.

The picker is organized around a single `GitPickerKind` enum — trigger detection returns which flavor to open, and one `try_activate` guards + dispatches to the per-kind gather function.

| Kind | Trigger | Candidate source |
|------|---------|------------------|
| `Refs` | `git checkout\|switch\|merge\|rebase\|cherry-pick` | `git for-each-ref --format=%(refname:short) refs/heads refs/tags` |
| `Stash` | `git stash pop\|apply\|drop\|show` | `git stash list`; value is the `stash@{N}` prefix |
| `Files` | `git add\|rm\|reset\|restore` | `git status --porcelain=v1 -z` (paths are cwd-relative via `-C`) |
| `Remotes` | `git push\|pull\|fetch` | `git remote` |

## Scope

**Shipped:**
- Phase 1: ref-taking subcommands (branches + tags).
- Phase 2: stash picker.
- Phase 3: file picker.
- Phase 4: remote picker.

**Still planned (separate follow-ups):**
- Commit-hash picker for `git cherry-pick <sha>` / `git reset <sha>` / `git show <sha>` — candidates backed by `git log --oneline -50` or similar.
- Optional: remote branches in the `Refs` picker (`origin/foo`) when a real use case comes up.
- Optional: per-subcommand filtering in `Files` (e.g. `git rm` should exclude untracked; `git restore` should exclude staged). Currently all four share the full `status --porcelain` set.

## Decisions

### Shared (all phases)

| Decision | Choice | Why |
|----------|--------|-----|
| Render mechanism | Reuse `PopupPicker` | Already generic from the v3 refactor; `shell_quote` in `mod.rs` handles value quoting |
| Dispatch chain | cd::try_activate → git::try_activate on PassThrough; one `ActivateContextDropdown(Cd)` binding routes both | The Cd variant name is historical; chaining avoids touching the `KeyAssignment` enum |
| Config flag | `git_picker_enabled: bool` (default true) gates all git categories | Symmetry with `cd_picker_enabled`; no per-category flags until a real need appears |
| Kind modeling | Single `GitPickerKind` enum (`Refs`, `Stash`, `Files`, `Remotes`); trigger detection returns which kind to open | One guard chain, one `try_activate`, one modal install — per-kind code is just a subcommand list + gather fn |
| Strict trigger (OSC 133) | First token = `git`, second token classified into a kind, no third token (or for stash: second = `stash`, third classified, no fourth), right-of-cursor empty | Structurally identical to the cd v2 strict path; the 3-token stash form is the only shape deviation |
| Heuristic trigger (no OSC 133) | Last two (or three, for stash) whitespace tokens of line-left-of-cursor match the strict shape | Matches the cd heuristic; same `echo git checkout` false-positive class |
| Repo detection | Empty stdout or non-zero exit from the gather command → PassThrough (no picker) | Covers "not a git repo", "empty repo", and "no stashes / no remotes / clean worktree" without a separate `rev-parse` check |
| Budget | 250 ms wall-clock on each shell-out via shared `run_git` helper | All four commands are milliseconds on real repos; cap guards against stuck HEAD / network fs |
| Git binary lookup | `git`, `/opt/homebrew/bin/git`, `/usr/local/bin/git` — same fallback as `zoxide_query` in cd.rs | macOS GUI apps don't inherit shell PATH |
| Guards | Alt-screen / pane overlay / existing modal — identical to cd | Same failure modes |
| Paste on select | Value with defensive `shell_quote`, no leading space | Trigger Space is already echoed into the shell by the PTY-echo model (see context-dropdowns.md); leading space would double it |

### Per-kind

| Kind | Subcommand list | Gather | Label / value |
|------|-----------------|--------|---------------|
| Refs | `checkout`, `switch`, `merge`, `rebase`, `cherry-pick` | `for-each-ref --format=%(refname:short) refs/heads refs/tags` | label = value = short ref name (`main`, `v1.2.3`, `feature/foo`) |
| Stash | `pop`, `apply`, `drop`, `show` (under `git stash`) | `stash list` | label = full line (`stash@{0}: WIP on main: …`); value = `stash@{N}` prefix. `shell_quote` wraps the value in single quotes because of the `{}` — shells can't interpret it otherwise |
| Files | `add`, `rm`, `reset`, `restore` | `status --porcelain=v1 -z`; pure `parse_porcelain` extracts records | label = `XY path` (git's 2-char status + path); value = path. Rename (R) / copy (C) records are two NUL-separated entries — the second (old path) is skipped so only the new name becomes a candidate |
| Remotes | `push`, `pull`, `fetch` | `remote` | label = value = remote name |

Why `reset` is in `Files` even though `git reset` is legitimate on its own: pressing Space after `git reset` is a reasonable "I'm about to type a path" signal. If the user wanted the bare form they'd press Enter, not Space. If they want to type `--hard` or `HEAD~1`, Escape dismisses the popup.

Why no per-subcommand filtering in `Files` (e.g. hiding untracked from `git rm`): all four share the same `status --porcelain` candidate set, which is the simplest thing that works. If a real user case needs stricter filtering, it's a per-kind refinement without touching the shared pipeline.

## Architecture

### Files

Phase 1 created the initial `git.rs` and related wiring. Phases 2–4 only extended `git.rs` — no other files changed.

| File | Change |
|------|--------|
| `wezterm-gui/src/overlay/context_dropdowns/cd_picker.rs` | **Phase 1:** rename → `popup_picker.rs`; `CdPicker` → `PopupPicker` |
| `wezterm-gui/src/overlay/context_dropdowns/mod.rs` | **Phase 1:** `pub mod cd_picker` → `pub mod popup_picker`; add `pub mod git`; lift `shell_quote` as `pub(super)` |
| `wezterm-gui/src/overlay/context_dropdowns/cd.rs` | **Phase 1:** import `popup_picker::{PopupPicker, PickerEntry}`; delete the local `shell_quote` |
| `wezterm-gui/src/overlay/context_dropdowns/git.rs` | **Phase 1:** new file with guard chain, trigger detection, `gather_refs`, `try_activate`. **Phase 2+:** adds `GitPickerKind` enum, shared `run_git`, per-kind constants (`STASH_ACTIONS`, `FILE_SUBCOMMANDS`, `REMOTE_SUBCOMMANDS`), `gather_stash` / `gather_files` (+ pure `parse_porcelain`) / `gather_remotes`, and `classify_two_token` to fold Refs/Files/Remotes into a shared 2-token path |
| `wezterm-gui/src/termwindow/mod.rs` | **Phase 1:** chain cd::try_activate → git::try_activate on PassThrough inside the existing `ActivateContextDropdown(_)` block |
| `config/src/config.rs` | **Phase 1:** `pub git_picker_enabled: bool` default true |

### Reuse (from context-dropdowns shared infrastructure)

- `PopupPicker` + `PickerEntry` from `popup_picker.rs`
- `shell_quote` from `mod.rs`
- `split_input_zone_at_cursor` / `split_logical_line_at_cursor` / `TriggerResult` from `mod.rs`
- Guard chain pattern from `cd.rs` (alt-screen / overlay / modal)

## Verification

Unit tests (`cargo test --bin wezterm-gui context_dropdowns`) — **40 passing after Phase 4** (was 30 after Phase 2, 37 after Phase 3):

- `detect_git_picker_strict` / `detect_git_picker_heuristic` accept each kind's subcommands (refs, stash, files, remotes) and reject bare `git`, unknown subcommands, mid-argument cases, and the `echo git …` argument form under strict. Heuristic has documented known false-positives on `echo git <sub>` (same class as cd).
- Strict + heuristic separately tested for the 3-token stash shape and the 2-token shape (refs / files / remotes).
- `parse_porcelain` tests cover simple records, rename skipping, copy skipping, paths with spaces, and empty input.

Manual QA (inside wezterm) done so far:
- [x] Inside a git repo: `git checkout ` + Space → popup shows local branches + tags
- [x] Enter → buffer contains `git checkout <ref>` (no execution)
- [x] Filter typing echoes into the shell AND narrows the popup live (PTY-echo model)
- [x] Backspace past an empty filter backs out the trigger Space, closing the popup
- [x] `cd` + Space still opens the cd picker (cd dispatch untouched)

Left for further confirmation (user QA on their setup):
- [ ] `git switch`, `git merge`, `git rebase`, `git cherry-pick` all trigger (Refs kind — structurally identical to checkout)
- [ ] `git stash pop` / `apply` / `drop` / `show` + Space → popup shows stash entries; Enter pastes `'stash@{N}'` (single-quoted because of the braces)
- [ ] `git add` / `rm` / `reset` / `restore` + Space → popup shows changed/untracked files with `XY path` labels; Enter pastes the path
- [ ] `git add foo.rs` → rename record shows only the new path, not both
- [ ] `git push` / `pull` / `fetch` + Space → popup shows configured remotes (`origin`, etc.)
- [ ] `git status` + Space → Space inserts normally, no popup
- [ ] `git checkout foo` + Space → no popup (strict path: third token present)
- [ ] `git stash pop stash@{0}` + Space → no popup (strict path: fourth token present)
- [ ] Outside a git repo (e.g. `cd /tmp`) → Space inserts normally, no popup
- [ ] Repo with no stashes / no remotes / clean worktree → PassThrough (empty gather result, no popup)
- [ ] `git_picker_enabled = false` in wezterm.lua → Space always falls through on `git <sub>`
- [ ] Large repo: popup opens without perceptible lag, fuzzy filter is snappy

Build & lint:
- `cargo build --release`
- `cargo test --bin wezterm-gui context_dropdowns` — 40/40 passing
- `cargo clippy --no-deps -p wezterm-gui` — our module clean (pre-existing `never_loop` error in `inputmap.rs:674` unchanged, out of scope per Phase 1 spec)

## Known limitations

- **Heuristic false-positive class**: without OSC 133, `echo git <sub>` + Space triggers the picker. Same class of issue as the cd heuristic's `echo cd`. Users who install wezterm's shell integration get the strict path, which rejects this case.
- **No remote branches in `Refs`**: `origin/foo` won't appear — only local branches + tags. Use `git checkout -b foo origin/foo` manually.
- **No commit-hash picker**: `git cherry-pick abc123` / `git reset abc123` / `git show abc123` still require typing the hash. A separate picker with `git log --oneline -50`-backed candidates is the natural next step.
- **Files shows the full `status` set for all four subcommands**: `git rm` ideally would exclude untracked; `git restore` would exclude staged-only. Current picker doesn't distinguish — simplest thing that works.
- **`Files` path normalization**: git's `-C <cwd>` makes porcelain paths cwd-relative, so pasting them works uniformly. But if the pane cwd drifts between Space press and candidate gather, paths could mismatch — same window-of-staleness as the cd picker.
