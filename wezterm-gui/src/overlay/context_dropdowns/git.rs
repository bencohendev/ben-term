//! `git` context dropdown.
//!
//! Fires on `git <sub>` (or `git stash <action>`) + Space where `<sub>` /
//! `<action>` matches one of the picker categories below. Reuses the shared
//! `PopupPicker` popup; per-category gather logic lives in this file.
//!
//! Categories:
//! - `Refs` — `checkout` / `switch` / `merge` / `rebase`; candidates are
//!   local branches + tags from `for-each-ref`.
//! - `Stash` — `stash pop` / `stash apply` / `stash drop` / `stash show`;
//!   candidates from `stash list` (value is the `stash@{N}` prefix).
//! - `Files` — `add` / `rm` / `restore`; candidates are changed / untracked
//!   paths from `status --porcelain -z`.
//! - `Remotes` — `push` / `pull` / `fetch`; candidates are configured
//!   remote names from `git remote`.
//! - `Commits` — `cherry-pick` / `reset` / `show`; candidates are recent
//!   commits from `git log --oneline -50` (value is the short sha). Note
//!   that `git stash show` still routes to the Stash category via the
//!   3-token detection branch — only the bare 2-token `git show` lands
//!   here.
//!
//! Dispatch: the key assignment `ActivateContextDropdown(Cd)` fans out through
//! `cd::try_activate` → `git::try_activate`. The `Cd` variant name is
//! historical — it's the "try each of our context pickers in order" binding.
//!
//! Guard chain mirrors cd:
//!   config.git_picker_enabled → not alt-screen → no existing overlay →
//!   no existing modal → trigger text check (strict via OSC 133 Input zone,
//!   fallback to trailing-tokens heuristic) → gather succeeds with non-empty
//!   result → install `PopupPicker` modal.
//!
//! Hard budget on each shell-out: 250 ms.

use crate::overlay::context_dropdowns::popup_picker::{PickerEntry, PopupPicker};
use crate::overlay::context_dropdowns::{
    split_input_zone_at_cursor, split_logical_line_at_cursor, TriggerResult,
};
use crate::termwindow::{TermWindow, TermWindowNotif};
use mux::pane::Pane;
use std::path::Path;
use std::process::Stdio;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use window::WindowOps;

/// Git subcommands whose next argument is a ref (branch or tag).
const REF_SUBCOMMANDS: &[&str] = &["checkout", "switch", "merge", "rebase"];

/// `git stash <action>` variants that take a stash ref as their next arg.
const STASH_ACTIONS: &[&str] = &["pop", "apply", "drop", "show"];

/// Git subcommands whose next argument is a file path (a pathspec).
const FILE_SUBCOMMANDS: &[&str] = &["add", "rm", "restore"];

/// Git subcommands whose next argument is a remote name.
///
/// All three accept an optional remote as the first positional arg
/// (`git push origin`, `git pull upstream`, etc.). Pressing Space after the
/// bare subcommand offers the configured remotes; Escape falls back to
/// typing options like `-u` manually.
const REMOTE_SUBCOMMANDS: &[&str] = &["push", "pull", "fetch"];

/// Git subcommands whose next argument is a commit-ish (typically a sha).
///
/// `cherry-pick` and `reset` are here even though both accept refs too — for
/// `cherry-pick` the common case is picking a specific sha; for `reset` the
/// common non-bare case is moving HEAD to a past commit (`git reset <sha>`).
/// A user who wants to reset paths instead can Escape the popup and type
/// the path; a user who wants to cherry-pick a branch tip still has the
/// branch name visible in the recent log entries.
///
/// `show` only lands here in its 2-token form — `git stash show` is caught
/// earlier by the Stash branch of `detect_git_picker_strict`.
const COMMIT_SUBCOMMANDS: &[&str] = &["cherry-pick", "reset", "show"];

const GIT_SHELL_BUDGET: Duration = Duration::from_millis(250);

/// Which flavor of git picker to open. Trigger detection returns this so
/// `try_activate` can dispatch to the right gather function without repeating
/// the guard chain per category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitPickerKind {
    Refs,
    Stash,
    Files,
    Remotes,
    Commits,
}

impl GitPickerKind {
    fn gather(self, cwd: &Path) -> Vec<PickerEntry> {
        match self {
            Self::Refs => gather_refs(cwd),
            Self::Stash => gather_stash(cwd),
            Self::Files => gather_files(cwd),
            Self::Remotes => gather_remotes(cwd),
            Self::Commits => gather_commits(cwd),
        }
    }
}

/// Entry point from `perform_key_assignment` via the cd → git dispatch chain.
/// Returns `Launched` (key consumed) or `PassThrough` (Space reaches the PTY).
pub fn try_activate(term_window: &mut TermWindow, pane: &Arc<dyn Pane>) -> TriggerResult {
    let cfg = config::configuration();
    if !cfg.git_picker_enabled {
        return TriggerResult::PassThrough;
    }
    if pane.is_alt_screen_active() {
        return TriggerResult::PassThrough;
    }
    if term_window.pane_has_overlay(pane.pane_id()) {
        return TriggerResult::PassThrough;
    }
    if term_window.has_modal() {
        return TriggerResult::PassThrough;
    }

    // Prefer OSC 133 Input zone (exact command buffer, no prompt chrome).
    // Fall back to the logical-line heuristic for shells without integration.
    let kind = match split_input_zone_at_cursor(pane) {
        Some((left, right)) => {
            if !right.trim().is_empty() {
                return TriggerResult::PassThrough;
            }
            detect_git_picker_strict(&left)
        }
        None => {
            let Some((left, _right)) = split_logical_line_at_cursor(pane) else {
                return TriggerResult::PassThrough;
            };
            detect_git_picker_heuristic(&left)
        }
    };
    let Some(kind) = kind else {
        return TriggerResult::PassThrough;
    };

    let cwd = pane
        .get_current_working_dir(mux::pane::CachePolicy::AllowStale)
        .and_then(|url| url.to_file_path().ok())
        .or_else(|| std::env::current_dir().ok());
    let Some(cwd) = cwd else {
        return TriggerResult::PassThrough;
    };

    // Fast repo pre-check: pure stat syscalls walking up for `.git`. Avoids
    // flashing a popup that would immediately fail the shell-out for the
    // common "not in a git repo" case. Per-command empty results (e.g.
    // `git stash pop` in a repo with no stashes) are handled in the loader
    // callback below, which cancels the modal if the gather returns empty.
    if !is_git_repo(&cwd) {
        return TriggerResult::PassThrough;
    }

    // Snapshot cursor viewport position for popup anchoring.
    let cursor = pane.get_cursor_position();
    let dims = pane.get_dimensions();
    let cursor_viewport_row = cursor.y - dims.physical_top;
    let pane_id = pane.pane_id();

    // Open the popup instantly in a loading state — the git shell-out can
    // take up to GIT_SHELL_BUDGET (250 ms) and users perceive that as lag.
    // Gathering runs on a worker thread and results are swapped in via
    // `TermWindowNotif::Apply` when ready.
    let picker = PopupPicker::new_loading(pane_id, cursor.x, cursor_viewport_row);
    let picker_id = picker.id();
    term_window.set_modal(Rc::new(picker));
    // Trigger Space was consumed by the KeyAssignment. Echo it back so the
    // command line reads `git <sub> ` and subsequent filter chars append
    // cleanly (see `PopupPicker::forward_to_pane`).
    let _ = pane.key_down(
        wezterm_term::KeyCode::Char(' '),
        wezterm_term::KeyModifiers::NONE,
    );

    // Background gather. `PopupPicker` is `!Send` (lives in `Rc`), so we
    // post back to the main thread via `TermWindowNotif::Apply` and locate
    // the picker there via `downcast_ref`.
    let window = term_window.window.clone();
    promise::spawn::spawn_into_new_thread(move || -> anyhow::Result<()> {
        let entries = kind.gather(&cwd);
        if let Some(window) = window {
            window.notify(TermWindowNotif::Apply(Box::new(
                move |tw: &mut TermWindow| {
                    let Some(modal) = tw.get_modal() else {
                        return;
                    };
                    let Some(picker) = modal.downcast_ref::<PopupPicker>() else {
                        return;
                    };
                    // Guard against a stale result landing in a newer picker.
                    if picker.id() != picker_id {
                        return;
                    }
                    // Empty result in a repo means "this command has no
                    // candidates right now" (empty repo, no stashes, no
                    // remotes, clean worktree, etc.). Dismiss instead of
                    // leaving "(no matches)" stuck on screen — matches the
                    // prior sync behavior of bailing before showing the
                    // popup at all.
                    if entries.is_empty() {
                        tw.cancel_modal();
                    } else {
                        picker.set_entries_loaded(entries);
                        tw.invalidate_modal();
                    }
                },
            )));
        }
        Ok(())
    })
    .detach();

    TriggerResult::Launched
}

/// Fast synchronous check for "is `cwd` inside a git worktree?". Walks up
/// the parent chain looking for a `.git` entry (directory in a normal repo,
/// file in a linked worktree — both count). Pure stat syscalls, typically
/// sub-millisecond even on a cold FS cache.
///
/// Used as a pre-flight gate before opening the git picker's loading popup:
/// without it, every `git checkout <space>` outside a repo would briefly
/// flash a popup that then cancels itself when the shell-out returns empty.
fn is_git_repo(cwd: &Path) -> bool {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return true;
        }
        dir = d.parent();
    }
    false
}

/// Strict trigger: `left` is the OSC 133 Input zone up to the cursor.
///
/// Accepts:
///   - `git <sub>` with no third token, where `<sub>` ∈ REF_SUBCOMMANDS → Refs
///   - `git <sub>` with no third token, where `<sub>` ∈ FILE_SUBCOMMANDS
///     → Files
///   - `git <sub>` with no third token, where `<sub>` ∈ REMOTE_SUBCOMMANDS
///     → Remotes
///   - `git stash <action>` with no fourth token, where `<action>` ∈
///     STASH_ACTIONS → Stash
///
/// Rejects anything else, including bare `git`, unknown subcommands, or any
/// trailing argument (which means the user is mid-argument — typing a name).
fn detect_git_picker_strict(left: &str) -> Option<GitPickerKind> {
    let mut toks = left.split_whitespace();
    if toks.next() != Some("git") {
        return None;
    }
    let sub = toks.next()?;
    if sub == "stash" {
        let action = toks.next()?;
        if toks.next().is_some() {
            return None;
        }
        return STASH_ACTIONS.contains(&action).then_some(GitPickerKind::Stash);
    }
    if toks.next().is_some() {
        return None;
    }
    classify_two_token(sub)
}

/// Map a bare subcommand string (e.g. `"checkout"`, `"add"`) to its picker
/// kind. Shared by the strict and heuristic detectors — both funnel into
/// the same subcommand→kind table once they've isolated the 2-token form.
fn classify_two_token(sub: &str) -> Option<GitPickerKind> {
    if REF_SUBCOMMANDS.contains(&sub) {
        return Some(GitPickerKind::Refs);
    }
    if FILE_SUBCOMMANDS.contains(&sub) {
        return Some(GitPickerKind::Files);
    }
    if REMOTE_SUBCOMMANDS.contains(&sub) {
        return Some(GitPickerKind::Remotes);
    }
    if COMMIT_SUBCOMMANDS.contains(&sub) {
        return Some(GitPickerKind::Commits);
    }
    None
}

/// Heuristic trigger for shells without OSC 133: the line-left-of-cursor can
/// contain the shell prompt, so we match on the trailing tokens (2 for refs,
/// 3 for stash). Shares the cd heuristic's false-positive class —
/// `echo git checkout` will trigger here; users with OSC 133 get strict.
fn detect_git_picker_heuristic(left: &str) -> Option<GitPickerKind> {
    let toks: Vec<&str> = left.split_whitespace().collect();
    let n = toks.len();
    // Stash form (3 trailing tokens: `git stash <action>`).
    if n >= 3 && toks[n - 3] == "git" && toks[n - 2] == "stash" {
        return STASH_ACTIONS
            .contains(&toks[n - 1])
            .then_some(GitPickerKind::Stash);
    }
    // 2-token form: `git <sub>` at the trailing edge.
    if n >= 2 && toks[n - 2] == "git" {
        return classify_two_token(toks[n - 1]);
    }
    None
}

/// Runs `git <args>` in `cwd` with a hard wall-clock cap. Returns stdout on
/// success. None on any failure path (missing binary, non-zero exit, budget
/// exceeded, spawn error). Mirrors the zoxide fallback from cd.rs — macOS
/// GUI apps don't inherit the user's shell PATH, so the PATH-less candidates
/// cover typical Homebrew installs.
fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let bins: &[&str] = &["git", "/opt/homebrew/bin/git", "/usr/local/bin/git"];

    for bin in bins {
        let Ok(mut child) = std::process::Command::new(bin)
            .arg("-C")
            .arg(cwd)
            .args(args)
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
        else {
            continue;
        };

        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    let output = child.wait_with_output().ok()?;
                    return Some(String::from_utf8_lossy(&output.stdout).into_owned());
                }
                Ok(Some(_)) => return None,
                Ok(None) => {
                    if start.elapsed() >= GIT_SHELL_BUDGET {
                        let _ = child.kill();
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return None,
            }
        }
    }
    None
}

/// Local branches + tags as short names (`main`, `v1.2.3`, `feature/foo`).
/// Empty on non-repos, empty repos, or shell-out failure.
fn gather_refs(cwd: &Path) -> Vec<PickerEntry> {
    let Some(stdout) = run_git(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/tags",
        ],
    ) else {
        return Vec::new();
    };
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| PickerEntry {
            label: l.to_string(),
            value: l.to_string(),
        })
        .collect()
}

/// Stash entries. Each `git stash list` line looks like:
///   `stash@{0}: WIP on main: abc123 Commit message`
/// Value is the `stash@{N}` prefix (which needs shell-quoting since it
/// contains `{}`); label is the whole line so the user can disambiguate.
fn gather_stash(cwd: &Path) -> Vec<PickerEntry> {
    let Some(stdout) = run_git(cwd, &["stash", "list"]) else {
        return Vec::new();
    };
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let colon = l.find(':')?;
            let stash_ref = &l[..colon];
            if !stash_ref.starts_with("stash@{") || !stash_ref.ends_with('}') {
                return None;
            }
            Some(PickerEntry {
                label: l.to_string(),
                value: stash_ref.to_string(),
            })
        })
        .collect()
}

/// Configured remote names (`origin`, `upstream`, etc.) from `git remote`.
/// Repos without remotes return empty, which dispatch treats as "no popup".
fn gather_remotes(cwd: &Path) -> Vec<PickerEntry> {
    let Some(stdout) = run_git(cwd, &["remote"]) else {
        return Vec::new();
    };
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| PickerEntry {
            label: l.to_string(),
            value: l.to_string(),
        })
        .collect()
}

/// Changed / untracked files from `git status --porcelain=v1 -z`. Paths are
/// reported relative to the current directory (git's `-C` sets cwd), so
/// pasting them straight into the shell works without any path-normalizing.
fn gather_files(cwd: &Path) -> Vec<PickerEntry> {
    let Some(stdout) = run_git(cwd, &["status", "--porcelain=v1", "-z"]) else {
        return Vec::new();
    };
    parse_porcelain(stdout.as_bytes())
}

/// Recent commits from `git log --oneline -50`. Each line is `<short-sha>
/// <subject>` — label is the full line so the user sees the subject; value
/// is the short sha, which is what `cherry-pick` / `reset` / `show` expect
/// as their next positional arg.
fn gather_commits(cwd: &Path) -> Vec<PickerEntry> {
    let Some(stdout) = run_git(cwd, &["log", "--oneline", "-50"]) else {
        return Vec::new();
    };
    parse_log(&stdout)
}

/// Parse `git log --oneline` output into picker entries. Extracted as a
/// pure function so we can unit-test the parser without shelling out.
fn parse_log(stdout: &str) -> Vec<PickerEntry> {
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let sha = l.split_whitespace().next()?;
            Some(PickerEntry {
                label: l.to_string(),
                value: sha.to_string(),
            })
        })
        .collect()
}

/// Parse `git status --porcelain=v1 -z` output into picker entries.
///
/// Porcelain v1 `-z` format:
///   `XY<space>PATH\0` for normal records.
///   `RX<space>NEW_PATH\0OLD_PATH\0` for renames (and `CX` for copies) —
///   two NUL-separated records where the second holds the pre-rename name.
///
/// Extracted as a pure function so we can unit-test the parser without
/// shelling out to a real git repo. The caller of `gather_files` owns the
/// subprocess; this just handles bytes.
fn parse_porcelain(bytes: &[u8]) -> Vec<PickerEntry> {
    let records: Vec<&[u8]> = bytes
        .split(|&b| b == 0)
        .filter(|r| !r.is_empty())
        .collect();

    let mut out = Vec::new();
    let mut i = 0;
    while i < records.len() {
        let record = records[i];
        // Valid record is at minimum `XY<space>c` — 4 bytes with a 1-char
        // path. Skip anything shorter.
        if record.len() < 4 {
            i += 1;
            continue;
        }
        let xy = &record[..2];
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        let xy_str = String::from_utf8_lossy(xy).into_owned();
        out.push(PickerEntry {
            label: format!("{xy_str} {path}"),
            value: path,
        });
        // Rename (R) / copy (C) records are followed by an extra record
        // holding the old path. Skip it so the pre-rename name doesn't
        // surface as a separate candidate.
        let is_rename_or_copy = xy[0] == b'R' || xy[0] == b'C';
        i += if is_rename_or_copy { 2 } else { 1 };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_accepts_each_ref_subcommand() {
        for sub in REF_SUBCOMMANDS {
            let line = format!("git {sub}");
            assert_eq!(
                detect_git_picker_strict(&line),
                Some(GitPickerKind::Refs),
                "strict should accept `{line}`"
            );
        }
    }

    #[test]
    fn strict_accepts_each_stash_action() {
        for action in STASH_ACTIONS {
            let line = format!("git stash {action}");
            assert_eq!(
                detect_git_picker_strict(&line),
                Some(GitPickerKind::Stash),
                "strict should accept `{line}`"
            );
        }
    }

    #[test]
    fn strict_accepts_leading_whitespace() {
        assert_eq!(
            detect_git_picker_strict("  git checkout"),
            Some(GitPickerKind::Refs)
        );
        assert_eq!(
            detect_git_picker_strict("\tgit\tswitch"),
            Some(GitPickerKind::Refs)
        );
        assert_eq!(
            detect_git_picker_strict("  git stash pop"),
            Some(GitPickerKind::Stash)
        );
    }

    #[test]
    fn strict_rejects_non_git_first_token() {
        assert_eq!(detect_git_picker_strict(""), None);
        assert_eq!(detect_git_picker_strict("git"), None);
        assert_eq!(detect_git_picker_strict("gitcheckout"), None);
        assert_eq!(detect_git_picker_strict("echo git checkout"), None);
        assert_eq!(detect_git_picker_strict("echo git stash pop"), None);
    }

    #[test]
    fn strict_rejects_non_whitelisted_subcommand() {
        assert_eq!(detect_git_picker_strict("git status"), None);
        assert_eq!(detect_git_picker_strict("git branch"), None);
        assert_eq!(detect_git_picker_strict("git log"), None);
        assert_eq!(detect_git_picker_strict("git diff"), None);
    }

    /// `git stash` alone doesn't take a ref — reject. `git stash list` / `save`
    /// / `push` aren't picker targets either (they don't consume a stash ref).
    #[test]
    fn strict_rejects_bare_stash_and_unknown_action() {
        assert_eq!(detect_git_picker_strict("git stash"), None);
        assert_eq!(detect_git_picker_strict("git stash list"), None);
        assert_eq!(detect_git_picker_strict("git stash save"), None);
        assert_eq!(detect_git_picker_strict("git stash push"), None);
    }

    /// A third token (or fourth for stash) means the user is mid-argument;
    /// the strict path should reject so Space doesn't stomp their typing.
    #[test]
    fn strict_rejects_when_arg_present() {
        assert_eq!(detect_git_picker_strict("git checkout foo"), None);
        assert_eq!(detect_git_picker_strict("git merge main dev"), None);
        assert_eq!(detect_git_picker_strict("git stash pop stash@{0}"), None);
    }

    #[test]
    fn heuristic_accepts_with_leading_prompt_text() {
        assert_eq!(
            detect_git_picker_heuristic("~ $ git checkout"),
            Some(GitPickerKind::Refs)
        );
        assert_eq!(
            detect_git_picker_heuristic("user@host:~/dev $ git merge"),
            Some(GitPickerKind::Refs)
        );
        assert_eq!(
            detect_git_picker_heuristic("❯ git stash apply"),
            Some(GitPickerKind::Stash)
        );
        assert_eq!(
            detect_git_picker_heuristic("~ $ git stash drop"),
            Some(GitPickerKind::Stash)
        );
    }

    /// Same known false-positive class as cd's heuristic: without OSC 133
    /// we can't tell "echo git checkout" from a real command. Documented.
    #[test]
    fn heuristic_has_known_false_positive_on_git_as_argument() {
        assert_eq!(
            detect_git_picker_heuristic("echo git checkout"),
            Some(GitPickerKind::Refs)
        );
        assert_eq!(
            detect_git_picker_heuristic("echo git stash pop"),
            Some(GitPickerKind::Stash)
        );
    }

    #[test]
    fn heuristic_rejects_when_arg_present() {
        assert_eq!(detect_git_picker_heuristic("git checkout foo"), None);
        assert_eq!(detect_git_picker_heuristic("~ $ git switch dev"), None);
        assert_eq!(
            detect_git_picker_heuristic("git stash pop stash@{0}"),
            None
        );
    }

    #[test]
    fn heuristic_rejects_non_whitelisted_subcommand() {
        assert_eq!(detect_git_picker_heuristic("git status"), None);
        assert_eq!(detect_git_picker_heuristic("$ git log"), None);
        assert_eq!(detect_git_picker_heuristic("git stash save"), None);
    }

    #[test]
    fn heuristic_rejects_short_lines() {
        assert_eq!(detect_git_picker_heuristic(""), None);
        assert_eq!(detect_git_picker_heuristic("git"), None);
        assert_eq!(detect_git_picker_heuristic("git stash"), None);
    }

    #[test]
    fn strict_accepts_each_file_subcommand() {
        for sub in FILE_SUBCOMMANDS {
            let line = format!("git {sub}");
            assert_eq!(
                detect_git_picker_strict(&line),
                Some(GitPickerKind::Files),
                "strict should accept `{line}`"
            );
        }
    }

    #[test]
    fn heuristic_accepts_each_file_subcommand() {
        for sub in FILE_SUBCOMMANDS {
            let line = format!("~ $ git {sub}");
            assert_eq!(
                detect_git_picker_heuristic(&line),
                Some(GitPickerKind::Files),
                "heuristic should accept `{line}`"
            );
        }
    }

    /// Modified + untracked file — one record each, no rename handling.
    #[test]
    fn parse_porcelain_simple_records() {
        let input = b" M foo.rs\0?? new.rs\0";
        let entries = parse_porcelain(input);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, " M foo.rs");
        assert_eq!(entries[0].value, "foo.rs");
        assert_eq!(entries[1].label, "?? new.rs");
        assert_eq!(entries[1].value, "new.rs");
    }

    /// Rename: the second NUL-separated record is the old name — it must
    /// NOT become a separate picker entry.
    #[test]
    fn parse_porcelain_rename_skips_old_name() {
        let input = b"R  new.rs\0old.rs\0 M unrelated.rs\0";
        let entries = parse_porcelain(input);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "R  new.rs");
        assert_eq!(entries[0].value, "new.rs");
        assert_eq!(entries[1].label, " M unrelated.rs");
        assert_eq!(entries[1].value, "unrelated.rs");
    }

    /// Copy records (C in index status) follow the same two-record shape
    /// as renames — the old path should be skipped too.
    #[test]
    fn parse_porcelain_copy_skips_source() {
        let input = b"C  dest.rs\0src.rs\0";
        let entries = parse_porcelain(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].value, "dest.rs");
    }

    /// Paths can contain whitespace — porcelain -z emits them unquoted
    /// so the whole byte range after `XY ` is the path.
    #[test]
    fn parse_porcelain_path_with_space() {
        let input = b" M has space.rs\0";
        let entries = parse_porcelain(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].value, "has space.rs");
    }

    /// Empty stdout (clean worktree): zero entries, no crash.
    #[test]
    fn parse_porcelain_empty_input() {
        assert!(parse_porcelain(b"").is_empty());
        assert!(parse_porcelain(b"\0\0").is_empty());
    }

    #[test]
    fn strict_accepts_each_remote_subcommand() {
        for sub in REMOTE_SUBCOMMANDS {
            let line = format!("git {sub}");
            assert_eq!(
                detect_git_picker_strict(&line),
                Some(GitPickerKind::Remotes),
                "strict should accept `{line}`"
            );
        }
    }

    #[test]
    fn heuristic_accepts_each_remote_subcommand() {
        for sub in REMOTE_SUBCOMMANDS {
            let line = format!("~ $ git {sub}");
            assert_eq!(
                detect_git_picker_heuristic(&line),
                Some(GitPickerKind::Remotes),
                "heuristic should accept `{line}`"
            );
        }
    }

    /// Remote-taking subcommands still reject a trailing argument (the user
    /// is mid-type: `git push ori|`).
    #[test]
    fn strict_rejects_remote_with_arg_present() {
        assert_eq!(detect_git_picker_strict("git push origin"), None);
        assert_eq!(detect_git_picker_strict("git pull upstream main"), None);
    }

    #[test]
    fn strict_accepts_each_commit_subcommand() {
        for sub in COMMIT_SUBCOMMANDS {
            let line = format!("git {sub}");
            assert_eq!(
                detect_git_picker_strict(&line),
                Some(GitPickerKind::Commits),
                "strict should accept `{line}`"
            );
        }
    }

    #[test]
    fn heuristic_accepts_each_commit_subcommand() {
        for sub in COMMIT_SUBCOMMANDS {
            let line = format!("~ $ git {sub}");
            assert_eq!(
                detect_git_picker_heuristic(&line),
                Some(GitPickerKind::Commits),
                "heuristic should accept `{line}`"
            );
        }
    }

    #[test]
    fn strict_rejects_commit_with_arg_present() {
        assert_eq!(detect_git_picker_strict("git cherry-pick abc1234"), None);
        assert_eq!(detect_git_picker_strict("git reset HEAD~1"), None);
        assert_eq!(detect_git_picker_strict("git show v1.0"), None);
    }

    /// `git stash show` must still route to the Stash kind even though `show`
    /// is now a Commits subcommand — the 3-token stash branch runs first.
    #[test]
    fn strict_git_stash_show_still_routes_to_stash() {
        assert_eq!(
            detect_git_picker_strict("git stash show"),
            Some(GitPickerKind::Stash)
        );
        assert_eq!(
            detect_git_picker_heuristic("~ $ git stash show"),
            Some(GitPickerKind::Stash)
        );
    }

    /// `cherry-pick` moved from Refs → Commits; `reset` moved from Files →
    /// Commits. Regression-guard the new routing.
    #[test]
    fn strict_moved_subcommands_route_to_commits() {
        assert_eq!(
            detect_git_picker_strict("git cherry-pick"),
            Some(GitPickerKind::Commits)
        );
        assert_eq!(
            detect_git_picker_strict("git reset"),
            Some(GitPickerKind::Commits)
        );
    }

    #[test]
    fn parse_log_simple() {
        let input = "abc1234 fix the bug\ndef5678 add feature\n";
        let entries = parse_log(input);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "abc1234 fix the bug");
        assert_eq!(entries[0].value, "abc1234");
        assert_eq!(entries[1].label, "def5678 add feature");
        assert_eq!(entries[1].value, "def5678");
    }

    /// Subjects routinely contain spaces — the label keeps the whole line,
    /// the value is only the first token (the sha).
    #[test]
    fn parse_log_subject_with_spaces() {
        let input = "abc1234 subject with many spaces and words\n";
        let entries = parse_log(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].value, "abc1234");
        assert_eq!(entries[0].label, "abc1234 subject with many spaces and words");
    }

    #[test]
    fn parse_log_empty_input() {
        assert!(parse_log("").is_empty());
        assert!(parse_log("\n\n").is_empty());
    }

    /// A plain directory with no ancestors containing `.git` is not a repo.
    #[test]
    fn is_git_repo_rejects_non_repo_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_git_repo(tmp.path()));
    }

    /// A directory containing `.git` (as a dir — the normal repo shape) is
    /// recognized without needing to actually initialize a repo.
    #[test]
    fn is_git_repo_accepts_dir_with_dotgit() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        assert!(is_git_repo(tmp.path()));
    }

    /// Linked worktrees (`git worktree add`) have a `.git` FILE pointing at
    /// the parent's gitdir, not a directory. Must still count as a repo.
    #[test]
    fn is_git_repo_accepts_dotgit_as_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert!(is_git_repo(tmp.path()));
    }

    /// The walk-up behavior: being N levels below a repo root still counts.
    #[test]
    fn is_git_repo_finds_ancestor_dotgit() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        let deep = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        assert!(is_git_repo(&deep));
    }
}
