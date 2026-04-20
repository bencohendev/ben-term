//! `git` context dropdown.
//!
//! Fires on `git <sub>` (or `git stash <action>`) + Space where `<sub>` /
//! `<action>` matches one of the picker categories below. Reuses the shared
//! `PopupPicker` popup; per-category gather logic lives in this file.
//!
//! Categories:
//!   - `Refs`    — `checkout` / `switch` / `merge` / `rebase` / `cherry-pick`
//!                 → local branches + tags via `for-each-ref`
//!   - `Stash`   — `stash pop` / `stash apply` / `stash drop` / `stash show`
//!                 → entries from `stash list`
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
use crate::termwindow::TermWindow;
use mux::pane::Pane;
use std::path::Path;
use std::process::Stdio;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Git subcommands whose next argument is a ref (branch or tag).
const REF_SUBCOMMANDS: &[&str] = &["checkout", "switch", "merge", "rebase", "cherry-pick"];

/// `git stash <action>` variants that take a stash ref as their next arg.
const STASH_ACTIONS: &[&str] = &["pop", "apply", "drop", "show"];

const GIT_SHELL_BUDGET: Duration = Duration::from_millis(250);

/// Which flavor of git picker to open. Trigger detection returns this so
/// `try_activate` can dispatch to the right gather function without repeating
/// the guard chain per category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitPickerKind {
    Refs,
    Stash,
}

impl GitPickerKind {
    fn gather(self, cwd: &Path) -> Vec<PickerEntry> {
        match self {
            Self::Refs => gather_refs(cwd),
            Self::Stash => gather_stash(cwd),
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

    // Empty output here means "not a git repo", "empty repo", or "no stashes".
    // Either way, there's nothing to pick, so bail without a popup.
    let entries = kind.gather(&cwd);
    if entries.is_empty() {
        return TriggerResult::PassThrough;
    }

    // Snapshot cursor viewport position for popup anchoring.
    let cursor = pane.get_cursor_position();
    let dims = pane.get_dimensions();
    let cursor_viewport_row = cursor.y - dims.physical_top;

    term_window.set_modal(Rc::new(PopupPicker::new(
        entries,
        pane.pane_id(),
        cursor.x,
        cursor_viewport_row,
    )));
    // Trigger Space was consumed by the KeyAssignment. Echo it back so the
    // command line reads `git <sub> ` and subsequent filter chars append
    // cleanly (see `PopupPicker::forward_to_pane`).
    let _ = pane.key_down(
        wezterm_term::KeyCode::Char(' '),
        wezterm_term::KeyModifiers::NONE,
    );
    TriggerResult::Launched
}

/// Strict trigger: `left` is the OSC 133 Input zone up to the cursor.
///
/// Accepts:
///   - `git <sub>` with no third token, where `<sub>` ∈ REF_SUBCOMMANDS → Refs
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
    REF_SUBCOMMANDS
        .contains(&sub)
        .then_some(GitPickerKind::Refs)
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
    // Ref form (2 trailing tokens: `git <sub>`).
    if n >= 2 && toks[n - 2] == "git" {
        return REF_SUBCOMMANDS
            .contains(&toks[n - 1])
            .then_some(GitPickerKind::Refs);
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
        assert_eq!(detect_git_picker_strict("git add"), None);
        assert_eq!(detect_git_picker_strict("git push"), None);
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
        assert_eq!(detect_git_picker_heuristic("$ git push"), None);
        assert_eq!(detect_git_picker_heuristic("git stash save"), None);
    }

    #[test]
    fn heuristic_rejects_short_lines() {
        assert_eq!(detect_git_picker_heuristic(""), None);
        assert_eq!(detect_git_picker_heuristic("git"), None);
        assert_eq!(detect_git_picker_heuristic("git stash"), None);
    }
}
