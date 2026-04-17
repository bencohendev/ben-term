# Git Picker

**Status:** Active — Phase 1 (refs for checkout/switch/merge/rebase/cherry-pick) in progress

## Overview

A context dropdown that fires on `git <ref-taking-subcommand>` + Space at a shell prompt and lists local branches + tags. Reuses the v3 `PopupPicker` Modal popup shell shipped by [context-dropdowns.md](context-dropdowns.md); this spec covers trigger detection and candidate gathering specific to git.

Phase 1 covers the ref-taking subcommands — `checkout`, `switch`, `merge`, `rebase`, `cherry-pick`. Each wants the same candidate set (branches + tags) and follows the same paste pattern.

## Scope

**Phase 1 — this spec:** ref-taking git subcommands.

**Planned follow-ups (separate phases or specs):**
- Phase 2: stash picker (`git stash pop|apply|drop|show`)
- Phase 3: file picker (`git add|rm|reset|restore`)
- Phase 4: remote picker (`git push|pull|fetch`)
- Commit-hash picker for `git cherry-pick <sha>` / `git reset <sha>` — separate

## Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Render mechanism | Reuse `PopupPicker` (renamed from `CdPicker` during this spec) | Already generic; only `shell_quote` needed to be lifted out |
| Dispatch chain | cd::try_activate → git::try_activate on PassThrough; one `ActivateContextDropdown(Cd)` binding routes both | The Cd variant name is historical; chaining avoids touching the `KeyAssignment` enum |
| Config flag | `git_picker_enabled: bool` (default true), mirroring `cd_picker_enabled` | Symmetry; no depth/budget knobs needed (git is cheap) |
| Subcommand whitelist | `["checkout", "switch", "merge", "rebase", "cherry-pick"]` | Explicit and auditable; these are the ref-taking commands used daily |
| Strict trigger (OSC 133) | First token = `git`, second token in whitelist, no third token, right-of-cursor empty | Structurally identical to the cd v2 strict path |
| Heuristic trigger (no OSC 133) | Last two whitespace tokens of line-left-of-cursor match `git <sub>` with `<sub>` in whitelist | Matches cd heuristic — trailing tokens only; same known false-positive class as cd's `echo cd` case |
| Candidate source | `git -C <cwd> for-each-ref --format=%(refname:short) refs/heads refs/tags` | One cheap command; returns short names (`main` not `refs/heads/main`); covers branches + tags in one call |
| Remote branches | Excluded from Phase 1 | Noise-prone; add when a real user case comes up |
| Repo detection | Empty stdout or non-zero exit → PassThrough (no picker) | Covers "not a git repo" and "empty repo" without a separate `rev-parse` |
| Budget | 250 ms wall-clock on the shell-out | for-each-ref is milliseconds on real repos; cap guards against stuck HEAD / network fs |
| Git binary lookup | `git`, `/opt/homebrew/bin/git`, `/usr/local/bin/git` — same fallback as `zoxide_query` in cd.rs | macOS GUI apps don't inherit shell PATH |
| Paste on select | ` <ref>` with defensive `shell_quote` | Git ref names forbid spaces and quotes in practice; shell_quote is a safety net |
| Guards | Alt-screen / pane overlay / existing modal — identical to cd | Same failure modes |

## Architecture

### Files (during implementation)

| File | Change |
|------|--------|
| `wezterm-gui/src/overlay/context_dropdowns/cd_picker.rs` | Rename → `popup_picker.rs`; `CdPicker` → `PopupPicker` |
| `wezterm-gui/src/overlay/context_dropdowns/mod.rs` | `pub mod cd_picker` → `pub mod popup_picker`; add `pub mod git`; lift `shell_quote` from cd.rs as `pub(super)` |
| `wezterm-gui/src/overlay/context_dropdowns/cd.rs` | Import `popup_picker::{PopupPicker, PickerEntry}`; delete the local `shell_quote` |
| `wezterm-gui/src/overlay/context_dropdowns/git.rs` | **New**. Guard chain, trigger detection, `gather_refs`, `try_activate` |
| `wezterm-gui/src/termwindow/mod.rs` | Chain cd::try_activate → git::try_activate on PassThrough inside the existing `ActivateContextDropdown(_)` block |
| `config/src/config.rs` | `pub git_picker_enabled: bool` default true |

### Reuse (from context-dropdowns shared infrastructure)

- `PopupPicker` + `PickerEntry` from `popup_picker.rs`
- `shell_quote` from `mod.rs`
- `split_input_zone_at_cursor` / `split_logical_line_at_cursor` / `TriggerResult` from `mod.rs`
- Guard chain pattern from `cd.rs` (alt-screen / overlay / modal)

## Verification

Unit tests (`cargo test --bin wezterm-gui context_dropdowns`):
- `detect_git_ref_subcommand_strict` accepts `"git checkout"`, `"git switch"`, `"git merge"`, `"git rebase"`, `"git cherry-pick"`, leading whitespace variants; rejects `"git"`, `"git checkout foo"`, `"git status"`, `"gitcheckout"`, `"echo git checkout"`, `""`
- `detect_git_ref_subcommand_heuristic` accepts the same cases preceded by arbitrary prompt text (`"~ $ git checkout"`, `"user@host:~ $ git merge"`); rejects third-token-present and non-whitelisted subcommands

Manual QA (inside wezterm):
- [ ] Inside a git repo: `git checkout ` + Space → popup shows local branches + tags
- [ ] Enter → buffer contains `git checkout <ref>` (no execution)
- [ ] `git switch`, `git merge`, `git rebase`, `git cherry-pick` all trigger
- [ ] `git status` + Space → Space inserts normally, no popup
- [ ] `git checkout foo` + Space → no popup (strict path: third token present)
- [ ] Outside a git repo (e.g. `cd /tmp`) → Space inserts normally, no popup
- [ ] `cd` + Space still opens the cd picker (cd dispatch untouched)
- [ ] `git_picker_enabled = false` in wezterm.lua → Space always falls through on `git <sub>`
- [ ] Large repo: popup opens without perceptible lag, fuzzy filter is snappy

Build & lint:
- `cargo build --bin wezterm-gui`
- `cargo test --bin wezterm-gui context_dropdowns` — existing cd tests must still pass
- `cargo clippy --no-deps -p wezterm-gui` — our module clean (pre-existing workspace debt in `inputmap.rs` unchanged)

## Known Phase 1 limitations

- **Heuristic false-positive class**: without OSC 133, `echo git checkout` + Space triggers the picker. Same class of issue as the cd heuristic's `echo cd`. Users who install wezterm's shell integration get the strict path, which rejects this case.
- **No remote branches**: `origin/foo` won't appear in the list. Use `git checkout -b foo origin/foo` manually, or ask for a spec update.
- **No commit-hash picker**: `git cherry-pick abc123` still requires typing the hash. A separate picker with `git log --oneline -50`-backed candidates is a reasonable follow-up.
