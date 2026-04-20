# Git Picker

**Status:** Done — Phases 1–5 shipped (refs, stash, files, remotes, commits).

## Overview

A context dropdown that fires on `git <sub>` (or `git stash <action>`) + Space at a shell prompt and lists candidates appropriate to the subcommand. Reuses the v3 `PopupPicker` Modal popup shell shipped by [context-dropdowns.md](context-dropdowns.md); this spec covers trigger detection and candidate gathering specific to git.

The picker is organized around a single `GitPickerKind` enum — trigger detection returns which flavor to open, and one `try_activate` guards + dispatches to the per-kind gather function.

| Kind | Trigger | Candidate source |
|------|---------|------------------|
| `Refs` | `git checkout\|switch\|merge\|rebase` | `git for-each-ref --format=%(refname:short) refs/heads refs/tags` |
| `Stash` | `git stash pop\|apply\|drop\|show` | `git stash list`; value is the `stash@{N}` prefix |
| `Files` | `git add\|rm\|restore` | `git status --porcelain=v1 -z` (paths are cwd-relative via `-C`) |
| `Remotes` | `git push\|pull\|fetch` | `git remote` |
| `Commits` | `git cherry-pick\|reset\|show` | `git log --oneline -50`; value is the short sha |

## Scope

**Shipped:**
- Phase 1: ref-taking subcommands (branches + tags).
- Phase 2: stash picker.
- Phase 3: file picker.
- Phase 4: remote picker.
- Phase 5: commit-hash picker (`cherry-pick`, `reset`, `show`) backed by `git log --oneline -50`. `cherry-pick` moved out of `Refs` and `reset` moved out of `Files` — commit-ish is the dominant next-arg for both, and a user who wants the old behavior can Escape and type the ref/path manually.

**Optional (deferred until a real use case appears):**
- Remote branches in the `Refs` picker (`origin/foo`).
- Per-subcommand filtering in `Files` (e.g. `git rm` should exclude untracked; `git restore` should exclude staged). Currently all three share the full `status --porcelain` set.

## Decisions

### Shared (all phases)

| Decision | Choice | Why |
|----------|--------|-----|
| Render mechanism | Reuse `PopupPicker` | Already generic from the v3 refactor; `shell_quote` in `mod.rs` handles value quoting |
| Dispatch chain | cd::try_activate → git::try_activate on PassThrough; one `ActivateContextDropdown(Cd)` binding routes both | The Cd variant name is historical; chaining avoids touching the `KeyAssignment` enum |
| Config flag | `git_picker_enabled: bool` (default true) gates all git categories | Symmetry with `cd_picker_enabled`; no per-category flags until a real need appears |
| Kind modeling | Single `GitPickerKind` enum (`Refs`, `Stash`, `Files`, `Remotes`, `Commits`); trigger detection returns which kind to open | One guard chain, one `try_activate`, one modal install — per-kind code is just a subcommand list + gather fn |
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
| Refs | `checkout`, `switch`, `merge`, `rebase` | `for-each-ref --format=%(refname:short) refs/heads refs/tags` | label = value = short ref name (`main`, `v1.2.3`, `feature/foo`) |
| Stash | `pop`, `apply`, `drop`, `show` (under `git stash`) | `stash list` | label = full line (`stash@{0}: WIP on main: …`); value = `stash@{N}` prefix. `shell_quote` wraps the value in single quotes because of the `{}` — shells can't interpret it otherwise |
| Files | `add`, `rm`, `restore` | `status --porcelain=v1 -z`; pure `parse_porcelain` extracts records | label = `XY path` (git's 2-char status + path); value = path. Rename (R) / copy (C) records are two NUL-separated entries — the second (old path) is skipped so only the new name becomes a candidate |
| Remotes | `push`, `pull`, `fetch` | `remote` | label = value = remote name |
| Commits | `cherry-pick`, `reset`, `show` | `log --oneline -50`; pure `parse_log` extracts records | label = full oneline (`abc1234 subject`); value = short sha. `git stash show` still routes to `Stash` because the 3-token stash branch of the detector runs first |

Why `cherry-pick` and `reset` moved to `Commits`: for `cherry-pick`, picking a specific sha is the dominant case; for `reset`, moving HEAD (`git reset <sha>`) is the dominant non-bare use. A branch-based cherry-pick is still typeable manually, and `git reset <path>` / `git reset --hard` users Escape the popup. The routing is "help the common case; don't block the uncommon one".

Why no per-subcommand filtering in `Files` (e.g. hiding untracked from `git rm`): all three share the same `status --porcelain` candidate set, which is the simplest thing that works. If a real user case needs stricter filtering, it's a per-kind refinement without touching the shared pipeline.

## Architecture

### Files

Phase 1 created the initial `git.rs` and related wiring. Phases 2–5 only extended `git.rs` — no other files changed.

| File | Change |
|------|--------|
| `wezterm-gui/src/overlay/context_dropdowns/cd_picker.rs` | **Phase 1:** rename → `popup_picker.rs`; `CdPicker` → `PopupPicker` |
| `wezterm-gui/src/overlay/context_dropdowns/mod.rs` | **Phase 1:** `pub mod cd_picker` → `pub mod popup_picker`; add `pub mod git`; lift `shell_quote` as `pub(super)` |
| `wezterm-gui/src/overlay/context_dropdowns/cd.rs` | **Phase 1:** import `popup_picker::{PopupPicker, PickerEntry}`; delete the local `shell_quote` |
| `wezterm-gui/src/overlay/context_dropdowns/git.rs` | **Phase 1:** new file with guard chain, trigger detection, `gather_refs`, `try_activate`. **Phase 2+:** `GitPickerKind` enum, shared `run_git`, per-kind constants, `gather_stash` / `gather_files` (+ pure `parse_porcelain`) / `gather_remotes`, `classify_two_token`. **Phase 5:** adds `COMMIT_SUBCOMMANDS`, `GitPickerKind::Commits`, `gather_commits` (+ pure `parse_log`); removes `cherry-pick` from `REF_SUBCOMMANDS` and `reset` from `FILE_SUBCOMMANDS` |
| `wezterm-gui/src/termwindow/mod.rs` | **Phase 1:** chain cd::try_activate → git::try_activate on PassThrough inside the existing `ActivateContextDropdown(_)` block |
| `config/src/config.rs` | **Phase 1:** `pub git_picker_enabled: bool` default true |

### Reuse (from context-dropdowns shared infrastructure)

- `PopupPicker` + `PickerEntry` from `popup_picker.rs`
- `shell_quote` from `mod.rs`
- `split_input_zone_at_cursor` / `split_logical_line_at_cursor` / `TriggerResult` from `mod.rs`
- Guard chain pattern from `cd.rs` (alt-screen / overlay / modal)

## Verification

Unit tests (`cargo test --bin wezterm-gui context_dropdowns`) — **48 passing after Phase 5** (was 40 after Phase 4):

- `detect_git_picker_strict` / `detect_git_picker_heuristic` accept each kind's subcommands (refs, stash, files, remotes, commits) and reject bare `git`, unknown subcommands, mid-argument cases, and the `echo git …` argument form under strict. Heuristic has documented known false-positives on `echo git <sub>` (same class as cd).
- Strict + heuristic separately tested for the 3-token stash shape and the 2-token shape (refs / files / remotes / commits).
- `parse_porcelain` tests cover simple records, rename skipping, copy skipping, paths with spaces, and empty input.
- `parse_log` tests cover simple records, subjects with spaces, and empty input.
- `strict_git_stash_show_still_routes_to_stash` guards the `show`-overlap: `git stash show` → Stash, bare `git show` → Commits.
- `strict_moved_subcommands_route_to_commits` guards the Phase 5 routing change: `git cherry-pick` → Commits (not Refs), `git reset` → Commits (not Files).

Manual QA (inside wezterm) done so far:
- [x] Inside a git repo: `git checkout ` + Space → popup shows local branches + tags
- [x] Enter → buffer contains `git checkout <ref>` (no execution)
- [x] Filter typing echoes into the shell AND narrows the popup live (PTY-echo model)
- [x] Backspace past an empty filter backs out the trigger Space, closing the popup
- [x] `cd` + Space still opens the cd picker (cd dispatch untouched)

Left for further confirmation (user QA on their setup):
- [ ] `git switch`, `git merge`, `git rebase` all trigger (Refs kind — structurally identical to checkout)
- [ ] `git stash pop` / `apply` / `drop` / `show` + Space → popup shows stash entries; Enter pastes `'stash@{N}'` (single-quoted because of the braces)
- [ ] `git add` / `rm` / `restore` + Space → popup shows changed/untracked files with `XY path` labels; Enter pastes the path
- [ ] `git add foo.rs` → rename record shows only the new path, not both
- [ ] `git push` / `pull` / `fetch` + Space → popup shows configured remotes (`origin`, etc.)
- [ ] `git cherry-pick` / `reset` / `show` + Space → popup shows recent commits (`abc1234 subject`); Enter pastes the short sha
- [ ] `git stash show` + Space → still shows stash entries (Stash kind wins over Commits for this overlap)
- [ ] `git status` + Space → Space inserts normally, no popup
- [ ] `git checkout foo` + Space → no popup (strict path: third token present)
- [ ] `git stash pop stash@{0}` + Space → no popup (strict path: fourth token present)
- [ ] Outside a git repo (e.g. `cd /tmp`) → Space inserts normally, no popup
- [ ] Repo with no stashes / no remotes / no commits / clean worktree → PassThrough (empty gather result, no popup)
- [ ] `git_picker_enabled = false` in wezterm.lua → Space always falls through on `git <sub>`
- [ ] Large repo: popup opens without perceptible lag, fuzzy filter is snappy

Build & lint:
- `cargo build --release`
- `cargo test --bin wezterm-gui context_dropdowns` — 48/48 passing
- `cargo clippy --no-deps -p wezterm-gui` — our module clean (pre-existing `never_loop` error in `inputmap.rs:674` unchanged, out of scope per Phase 1 spec)

## Known limitations

- **Heuristic false-positive class**: without OSC 133, `echo git <sub>` + Space triggers the picker. Same class of issue as the cd heuristic's `echo cd`. Users who install wezterm's shell integration get the strict path, which rejects this case.
- **No remote branches in `Refs`**: `origin/foo` won't appear — only local branches + tags. Use `git checkout -b foo origin/foo` manually.
- **`Commits` caps at 50 entries**: `git log --oneline -50` is the candidate source. Older commits aren't pickable — type the sha manually.
- **`cherry-pick`/`reset` no longer surface refs/files on trigger**: Phase 5 moved them to Commits. A user who wants `git cherry-pick <branch>` or `git reset <path>` types the argument manually.
- **Files shows the full `status` set for all three subcommands**: `git rm` ideally would exclude untracked; `git restore` would exclude staged-only. Current picker doesn't distinguish — simplest thing that works.
- **`Files` path normalization**: git's `-C <cwd>` makes porcelain paths cwd-relative, so pasting them works uniformly. But if the pane cwd drifts between Space press and candidate gather, paths could mismatch — same window-of-staleness as the cd picker.
