//! `git` context dropdown.
//!
//! Fires on `git <sub>` + Space where `<sub>` is a ref-taking subcommand
//! (`checkout`, `switch`, `merge`, `rebase`, `cherry-pick`). Surfaces local
//! branches + tags as candidates via `git for-each-ref`.
//!
//! Dispatch: the key assignment `ActivateContextDropdown(Cd)` fans out through
//! `cd::try_activate` → `git::try_activate`. The `Cd` variant name is
//! historical — it's the "try each of our context pickers in order" binding.
//! Future follow-up phases (stash, files, remotes) will chain further.
//!
//! Guard chain mirrors cd:
//!   config.git_picker_enabled → not alt-screen → no existing overlay →
//!   no existing modal → trigger text check (strict via OSC 133 Input zone,
//!   fallback to last-two-tokens heuristic) → gather succeeds with non-empty
//!   result → install `PopupPicker` modal.
//!
//! Hard budget on the `for-each-ref` shell-out: 250 ms.

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

/// Git subcommands whose next argument is a ref (branch or tag). Ordered by
/// rough daily-use frequency. `cherry-pick` accepts commit SHAs too, but a
/// branch name is a valid target so we include it here; a separate commit
/// picker is a future follow-up.
const REF_SUBCOMMANDS: &[&str] = &["checkout", "switch", "merge", "rebase", "cherry-pick"];

const GIT_SHELL_BUDGET: Duration = Duration::from_millis(250);

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
    let matched = match split_input_zone_at_cursor(pane) {
        Some((left, right)) => {
            if !right.trim().is_empty() {
                return TriggerResult::PassThrough;
            }
            detect_git_ref_subcommand_strict(&left)
        }
        None => {
            let Some((left, _right)) = split_logical_line_at_cursor(pane) else {
                return TriggerResult::PassThrough;
            };
            detect_git_ref_subcommand_heuristic(&left)
        }
    };
    if matched.is_none() {
        return TriggerResult::PassThrough;
    }

    let cwd = pane
        .get_current_working_dir(mux::pane::CachePolicy::AllowStale)
        .and_then(|url| url.to_file_path().ok())
        .or_else(|| std::env::current_dir().ok());
    let Some(cwd) = cwd else {
        return TriggerResult::PassThrough;
    };

    // Empty output here means "not a git repo" OR "empty repo with no refs".
    // Either way, there's nothing to pick, so bail without a popup.
    let entries = gather_refs(&cwd);
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
    TriggerResult::Launched
}

/// Strict trigger: `left` is the OSC 133 Input zone up to the cursor. Accepts
/// iff the first token is `git`, the second token is a ref-taking subcommand,
/// and there is no third token (i.e. the user hasn't started typing an arg).
fn detect_git_ref_subcommand_strict(left: &str) -> Option<&'static str> {
    let mut toks = left.split_whitespace();
    if toks.next() != Some("git") {
        return None;
    }
    let sub = toks.next()?;
    if toks.next().is_some() {
        return None;
    }
    REF_SUBCOMMANDS.iter().copied().find(|s| *s == sub)
}

/// Heuristic trigger for shells without OSC 133: the line-left-of-cursor can
/// contain the shell prompt, so we match on the last two whitespace tokens.
/// Shares the cd heuristic's false-positive class (`echo git checkout` will
/// trigger here); users who install wezterm's shell integration get strict.
fn detect_git_ref_subcommand_heuristic(left: &str) -> Option<&'static str> {
    let toks: Vec<&str> = left.split_whitespace().collect();
    let n = toks.len();
    if n < 2 {
        return None;
    }
    if toks[n - 2] != "git" {
        return None;
    }
    let sub = toks[n - 1];
    REF_SUBCOMMANDS.iter().copied().find(|s| *s == sub)
}

/// Shells out to `git for-each-ref` in the given directory, returning one
/// `PickerEntry` per short-named ref (`main`, `v1.2.3`, `feature/foo`, …).
/// Returns an empty Vec on any failure (not a git repo, binary missing,
/// budget exceeded) so the caller can fall through cleanly.
fn gather_refs(cwd: &Path) -> Vec<PickerEntry> {
    // GUI apps on macOS don't inherit the user's shell PATH, so mirror the
    // cd picker's zoxide fallback. Most binaries are on one of these paths.
    let candidates: &[&str] = &["git", "/opt/homebrew/bin/git", "/usr/local/bin/git"];

    for bin in candidates {
        let Ok(mut child) = std::process::Command::new(bin)
            .arg("-C")
            .arg(cwd)
            .args([
                "for-each-ref",
                "--format=%(refname:short)",
                "refs/heads",
                "refs/tags",
            ])
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
                    // Drain stdout.
                    let output = child.wait_with_output().ok();
                    if let Some(output) = output {
                        return String::from_utf8_lossy(&output.stdout)
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(|l| PickerEntry {
                                label: l.to_string(),
                                value: l.to_string(),
                            })
                            .collect();
                    }
                    return Vec::new();
                }
                Ok(Some(_)) => return Vec::new(), // non-zero exit
                Ok(None) => {
                    if start.elapsed() >= GIT_SHELL_BUDGET {
                        let _ = child.kill();
                        return Vec::new();
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return Vec::new(),
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_accepts_each_ref_subcommand() {
        for sub in REF_SUBCOMMANDS {
            let line = format!("git {sub}");
            assert_eq!(
                detect_git_ref_subcommand_strict(&line),
                Some(*sub),
                "strict should accept `{line}`"
            );
        }
    }

    #[test]
    fn strict_accepts_leading_whitespace() {
        assert_eq!(
            detect_git_ref_subcommand_strict("  git checkout"),
            Some("checkout")
        );
        assert_eq!(
            detect_git_ref_subcommand_strict("\tgit\tswitch"),
            Some("switch")
        );
    }

    #[test]
    fn strict_rejects_non_git_first_token() {
        assert_eq!(detect_git_ref_subcommand_strict(""), None);
        assert_eq!(detect_git_ref_subcommand_strict("git"), None);
        assert_eq!(detect_git_ref_subcommand_strict("gitcheckout"), None);
        assert_eq!(detect_git_ref_subcommand_strict("echo git checkout"), None);
    }

    #[test]
    fn strict_rejects_non_whitelisted_subcommand() {
        assert_eq!(detect_git_ref_subcommand_strict("git status"), None);
        assert_eq!(detect_git_ref_subcommand_strict("git branch"), None);
        assert_eq!(detect_git_ref_subcommand_strict("git add"), None);
        assert_eq!(detect_git_ref_subcommand_strict("git push"), None);
    }

    /// A third token means the user is mid-argument (`git checkout foo|`);
    /// the strict path should reject so Space doesn't stomp their typing.
    #[test]
    fn strict_rejects_when_arg_present() {
        assert_eq!(detect_git_ref_subcommand_strict("git checkout foo"), None);
        assert_eq!(detect_git_ref_subcommand_strict("git merge main dev"), None);
    }

    #[test]
    fn heuristic_accepts_with_leading_prompt_text() {
        assert_eq!(
            detect_git_ref_subcommand_heuristic("~ $ git checkout"),
            Some("checkout")
        );
        assert_eq!(
            detect_git_ref_subcommand_heuristic("user@host:~/dev $ git merge"),
            Some("merge")
        );
        assert_eq!(
            detect_git_ref_subcommand_heuristic("❯ git rebase"),
            Some("rebase")
        );
    }

    /// Same known false-positive class as cd's heuristic: without OSC 133
    /// we can't tell "echo git checkout" from a real command. Documented.
    #[test]
    fn heuristic_has_known_false_positive_on_git_as_argument() {
        assert_eq!(
            detect_git_ref_subcommand_heuristic("echo git checkout"),
            Some("checkout")
        );
    }

    #[test]
    fn heuristic_rejects_when_arg_present() {
        assert_eq!(
            detect_git_ref_subcommand_heuristic("git checkout foo"),
            None
        );
        assert_eq!(
            detect_git_ref_subcommand_heuristic("~ $ git switch dev"),
            None
        );
    }

    #[test]
    fn heuristic_rejects_non_whitelisted_subcommand() {
        assert_eq!(detect_git_ref_subcommand_heuristic("git status"), None);
        assert_eq!(detect_git_ref_subcommand_heuristic("$ git push"), None);
    }

    #[test]
    fn heuristic_rejects_short_lines() {
        assert_eq!(detect_git_ref_subcommand_heuristic(""), None);
        assert_eq!(detect_git_ref_subcommand_heuristic("git"), None);
    }
}
