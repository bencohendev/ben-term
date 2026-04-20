//! `cd` context dropdown.
//!
//! Guard chain:
//!   config.cd_picker_enabled → not alt-screen → no existing overlay →
//!   no existing modal → trigger text check. Two trigger modes:
//!   - Strict (OSC 133 Input zone available): first token is `cd` AND
//!     right-of-cursor within the zone is empty/whitespace.
//!   - Fallback (no OSC 133): last whitespace-delimited token of the line
//!     left of cursor is `cd`.
//!
//! Candidates (matches the user's zsh widget at ~/.zshrc:184-202):
//!   1. Pinned shortcuts: `..`, `-`, `~`
//!   2. Walkdir of CWD, depth ≤ `cd_picker_max_depth`, skipping heavy dirs
//!   3. Walkdir of $HOME depth 3 when CWD != $HOME (if cd_picker_include_home)
//!   4. `zoxide query --list` top 20 (if the binary exists)
//!
//! Hard budget: 300 ms wall-clock or 500 entries across the dir walks.
//!
//! On select, `~` is resolved to the absolute home path before insertion to
//! avoid bracketed-paste expansion inconsistencies across shells.

use crate::overlay::context_dropdowns::popup_picker::{PickerEntry, PopupPicker};
use crate::overlay::context_dropdowns::{
    split_input_zone_at_cursor, split_logical_line_at_cursor, TriggerResult,
    MIN_LOADING_VISIBLE,
};
use crate::termwindow::{TermWindow, TermWindowNotif};
use mux::pane::Pane;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use window::WindowOps;

const WALK_BUDGET: Duration = Duration::from_millis(500);
const WALK_MAX_ENTRIES: usize = 10_000;
const HOME_WALK_DEPTH: usize = 5;

/// Entry point from `perform_key_assignment`. If every guard passes, installs
/// the picker as a `Modal` popup and returns `Launched` (key consumed).
/// Otherwise returns `PassThrough` so Space reaches the PTY normally.
pub fn try_activate(term_window: &mut TermWindow, pane: &Arc<dyn Pane>) -> TriggerResult {
    let cfg = config::configuration();
    if !cfg.cd_picker_enabled {
        return TriggerResult::PassThrough;
    }
    // Alt-screen hard guard (vim / Claude Code / htop): always bypass.
    if pane.is_alt_screen_active() {
        return TriggerResult::PassThrough;
    }
    // If this pane already has an overlay (e.g. copy mode / quickselect),
    // don't steal Space — let the overlay's input stream consume it.
    if term_window.pane_has_overlay(pane.pane_id()) {
        return TriggerResult::PassThrough;
    }
    // Double-trigger / stacking guard. Incidentally hedges the keystroke
    // batching bug: duplicate Space events can't spawn a second modal.
    if term_window.has_modal() {
        return TriggerResult::PassThrough;
    }

    // Prefer the OSC 133 Input zone when the shell emits semantic prompt
    // markers — it gives us the exact command-line text with no prompt or
    // RPROMPT contamination, enabling strict first-token and trailing-edge
    // checks. Fall back to the line-heuristic for shells without integration.
    match split_input_zone_at_cursor(pane) {
        Some((left, right)) => {
            if !is_cd_command_start(&left) || !right.trim().is_empty() {
                return TriggerResult::PassThrough;
            }
        }
        None => {
            let Some((left, _right)) = split_logical_line_at_cursor(pane) else {
                return TriggerResult::PassThrough;
            };
            // Without OSC 133, right-of-cursor is unreliable — shells with
            // RPROMPT render chrome to the right of the input buffer, which
            // would otherwise block the trigger. Last-token match only.
            if !is_cd_trigger(&left) {
                return TriggerResult::PassThrough;
            }
        }
    }

    let cwd = pane
        .get_current_working_dir(mux::pane::CachePolicy::AllowStale)
        .and_then(|url| url.to_file_path().ok())
        .or_else(|| std::env::current_dir().ok());
    let home = dirs_home();
    let max_depth = cfg.cd_picker_max_depth.max(1) as usize;
    let include_home = cfg.cd_picker_include_home;
    let pane_id = pane.pane_id();

    // Snapshot the cursor's viewport position so the popup can anchor just
    // below and left-aligned to the typed `cd ` insertion point. Input is
    // captured while the modal is open, so this snapshot stays valid.
    let cursor = pane.get_cursor_position();
    let dims = pane.get_dimensions();
    let cursor_viewport_row = cursor.y - dims.physical_top;

    // Open the popup instantly in a "loading" state — the candidate gather
    // (zoxide subprocess + BFS walk of $HOME) can take a few hundred ms and
    // users perceive that as lag. Gathering runs on a worker thread and the
    // results are swapped in via `TermWindowNotif::Apply` when ready.
    let open_time = Instant::now();
    let picker = PopupPicker::new_loading(pane_id, cursor.x, cursor_viewport_row);
    let picker_id = picker.id();
    term_window.set_modal(Rc::new(picker));
    // The Space that triggered us was consumed by the KeyAssignment. Echo it
    // back into the shell so the command line reads `cd ` and subsequent
    // filter chars append correctly (see `PopupPicker::forward_to_pane`).
    let _ = pane.key_down(
        wezterm_term::KeyCode::Char(' '),
        wezterm_term::KeyModifiers::NONE,
    );

    // Background gather. Values captured into the closure must be `Send`;
    // `PopupPicker` itself is `!Send` (it lives in `Rc`), so we don't touch
    // it from this thread — we post a closure back to the main thread
    // through `TermWindowNotif::Apply` which then locates the picker via
    // `get_modal().downcast_ref()`.
    let window = term_window.window.clone();
    let cwd_buf = cwd.map(|p| p.to_path_buf());
    let home_buf = home;
    // `.detach()` so the task runs to completion even though we don't await
    // it — dropping the returned `Task` would cancel the gather.
    promise::spawn::spawn_into_new_thread(move || -> anyhow::Result<()> {
        let entries = gather_candidates(
            cwd_buf.as_deref(),
            home_buf.as_deref(),
            max_depth,
            include_home,
        );
        // Hold the Loading placeholder for at least one visible paint even
        // if the gather beat the first vsync (rare for cd — the walk budget
        // is 500 ms — but kept symmetric with git.rs for consistent UX).
        let elapsed = open_time.elapsed();
        if elapsed < MIN_LOADING_VISIBLE {
            std::thread::sleep(MIN_LOADING_VISIBLE - elapsed);
        }
        if let Some(window) = window {
            window.notify(TermWindowNotif::Apply(Box::new(
                move |tw: &mut TermWindow| {
                    // Guard against a stale result landing in a newer picker:
                    // if the user Escaped and reopened cd before we finished,
                    // the current modal (if any) has a different id and we
                    // silently drop our results.
                    if let Some(modal) = tw.get_modal() {
                        if let Some(picker) = modal.downcast_ref::<PopupPicker>() {
                            if picker.id() == picker_id {
                                picker.set_entries_loaded(entries);
                                tw.invalidate_modal();
                            }
                        }
                    }
                },
            )));
        }
        Ok(())
    })
    .detach();

    TriggerResult::Launched
}

/// Fallback trigger for shells without OSC 133: fires when the last
/// whitespace-delimited token in the line-left-of-cursor is `cd`. Reading
/// the literal line buffer includes the shell prompt (e.g. `~ $ cd`), so
/// we can't match the whole string — only the trailing token.
///
/// This fires on lines like `echo cd ` where cd is an argument, not the
/// command. Press Escape to dismiss; installing wezterm's shell integration
/// enables the strict `is_cd_command_start` path which avoids this.
fn is_cd_trigger(left: &str) -> bool {
    left.split_whitespace().last() == Some("cd")
}

/// Strict trigger for OSC 133-enabled shells: `left` is the text of the
/// Input zone (actual command buffer) up to the cursor. The command line
/// starts with `cd` iff the first whitespace-delimited token is `cd`.
/// Lines like `echo cd` correctly reject because the first token is `echo`.
fn is_cd_command_start(left: &str) -> bool {
    let mut toks = left.split_whitespace();
    toks.next() == Some("cd") && toks.next().is_none()
}

fn gather_candidates(
    cwd: Option<&Path>,
    home: Option<&Path>,
    max_depth: usize,
    include_home: bool,
) -> Vec<PickerEntry> {
    let start = Instant::now();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<PickerEntry> = Vec::new();

    // 1. Pinned shortcuts.
    push_entry(&mut seen, &mut out, "..".into(), "..".into());
    push_entry(&mut seen, &mut out, "-".into(), "-".into());
    if let Some(home) = home {
        push_entry(&mut seen, &mut out, "~".into(), home.display().to_string());
    }

    // 2. Zoxide frecent list.
    for path in zoxide_query() {
        push_entry(&mut seen, &mut out, path.clone(), path);
    }

    // 3. Walk $HOME — where user projects live. This is the primary
    //    fuzzy-search source, regardless of current cwd.
    if include_home {
        if let Some(home) = home {
            walk_dirs(home, HOME_WALK_DEPTH, start, &mut out, &mut seen);
        }
    }

    // 4. If cwd is outside $HOME and we have budget, walk cwd too — lets
    //    the user navigate deeper into whatever workspace they're in now.
    if start.elapsed() < WALK_BUDGET {
        if let (Some(cwd), Some(home)) = (cwd, home) {
            if !cwd.starts_with(home) {
                walk_dirs(cwd, max_depth.min(3), start, &mut out, &mut seen);
            } else {
                // Inside $HOME already covered by step 3; just pull immediate
                // children of cwd for quick drill-down.
                walk_dirs(cwd, 1, start, &mut out, &mut seen);
            }
        } else if let Some(cwd) = cwd {
            walk_dirs(cwd, 1, start, &mut out, &mut seen);
        }
    }

    out
}

fn push_entry(
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<PickerEntry>,
    label: String,
    value: String,
) {
    if seen.insert(value.clone()) {
        out.push(PickerEntry { label, value });
    }
}

/// Breadth-first directory walker.
///
/// walkdir's DFS exhausts budget on the first huge subtree before reaching
/// later-sorted siblings. For a cd picker, we want "every directory at depth
/// N" before any descent to depth N+1 — a user's `~/dev/repos/*` should not
/// be starved by `~/Desktop/**` burning the time budget first.
fn walk_dirs(
    root: &Path,
    max_depth: usize,
    start: Instant,
    out: &mut Vec<PickerEntry>,
    seen: &mut std::collections::HashSet<String>,
) {
    use std::collections::VecDeque;
    let root_is_fs_root = root == Path::new("/");
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0));

    while let Some((dir, depth)) = queue.pop_front() {
        if out.len() >= WALK_MAX_ENTRIES || start.elapsed() >= WALK_BUDGET {
            return;
        }
        if depth >= max_depth {
            continue;
        }
        let Ok(children) = std::fs::read_dir(&dir) else {
            continue;
        };
        for child in children.flatten() {
            let Ok(ft) = child.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let name = child.file_name();
            let name_str = name.to_str();
            if is_universal_skippable_dir(name_str) {
                continue;
            }
            // Top-level filtering: depth == 0 means `dir` is `root`, so the
            // child we're about to push is at effective depth 1 from root.
            if depth == 0 {
                if root_is_fs_root && is_system_top_level_dir(name_str) {
                    continue;
                }
                if is_home_top_level_noise(name_str) {
                    continue;
                }
            }
            let path = child.path();
            let display = path.display().to_string();
            if seen.insert(display.clone()) {
                let label = path
                    .strip_prefix(root)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| display.clone());
                out.push(PickerEntry {
                    label,
                    value: display,
                });
            }
            if depth + 1 < max_depth {
                queue.push_back((path, depth + 1));
            }
        }
    }
}

fn is_universal_skippable_dir(name: Option<&str>) -> bool {
    matches!(
        name,
        Some(".git" | "node_modules" | "target" | ".svn" | ".hg" | ".venv" | "venv")
    )
}

fn is_system_top_level_dir(name: Option<&str>) -> bool {
    matches!(
        name,
        Some(
            "System" | "private" | "usr" | "bin" | "sbin" | "opt" | "tmp" | "var" | "etc" | "dev"
                | "Applications" | "Library" | "cores" | "Volumes" | "Network"
        )
    )
}

fn is_home_top_level_noise(name: Option<&str>) -> bool {
    matches!(
        name,
        Some("Library" | "Applications" | "Pictures" | "Music" | "Movies" | "Public")
    )
}

fn zoxide_query() -> Vec<String> {
    // GUI apps on macOS don't inherit the user's shell PATH, so `zoxide` on
    // bare PATH often fails. Fall back to common Homebrew install locations.
    let candidates: &[&str] = &["zoxide", "/opt/homebrew/bin/zoxide", "/usr/local/bin/zoxide"];
    let output = candidates.iter().find_map(|bin| {
        std::process::Command::new(bin)
            .args(["query", "--list"])
            .output()
            .ok()
            .filter(|o| o.status.success())
    });
    let Some(output) = output else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_matches_cd_after_prompt_text() {
        assert!(is_cd_trigger("cd"));
        assert!(is_cd_trigger("  cd"));
        assert!(is_cd_trigger("\tcd"));
        assert!(is_cd_trigger("~ $ cd"));
        assert!(is_cd_trigger("user@host:~/dev $ cd"));
        assert!(is_cd_trigger("❯ cd"));
    }

    #[test]
    fn trigger_rejects_non_cd_token() {
        assert!(!is_cd_trigger("cde"));
        assert!(!is_cd_trigger(""));
        assert!(!is_cd_trigger("cdx"));
        assert!(!is_cd_trigger("$ pwd"));
        assert!(!is_cd_trigger("cd foo"));
    }

    /// The heuristic fallback fires on this — it's the known limitation of
    /// the OSC-133-less path. The strict `is_cd_command_start` test below
    /// covers the v2 path that correctly rejects it.
    #[test]
    fn heuristic_has_known_false_positive_on_cd_as_argument() {
        assert!(is_cd_trigger("echo cd"));
    }

    #[test]
    fn strict_accepts_bare_cd() {
        assert!(is_cd_command_start("cd"));
        assert!(is_cd_command_start("  cd"));
        assert!(is_cd_command_start("cd "));
        assert!(is_cd_command_start("cd\t"));
        assert!(is_cd_command_start("\tcd  "));
    }

    /// The core v2 fix: an OSC 133 Input zone containing `echo cd` is
    /// correctly recognized as NOT the start of a `cd` command.
    #[test]
    fn strict_rejects_cd_as_argument() {
        assert!(!is_cd_command_start("echo cd"));
        assert!(!is_cd_command_start("  sudo cd"));
    }

    #[test]
    fn strict_rejects_partial_cd_word() {
        assert!(!is_cd_command_start("cde"));
        assert!(!is_cd_command_start("cdfoo"));
        assert!(!is_cd_command_start(""));
        assert!(!is_cd_command_start("   "));
    }

    /// After Space, the Input zone-left of cursor is typically `cd` with no
    /// trailing args. If the user typed more before Space, the second token
    /// means we're mid-argument and the picker should not open.
    #[test]
    fn strict_rejects_cd_with_argument_already_typed() {
        assert!(!is_cd_command_start("cd foo"));
        assert!(!is_cd_command_start("cd /tmp"));
    }

    /// Verifies the default Space binding actually reaches default_key_assignments.
    /// If this fails, `commands.rs` is not emitting the binding and no keypress
    /// will ever dispatch to our handler.
    #[test]
    fn default_space_binding_is_registered() {
        use config::keyassignment::{ContextDropdownKind, KeyAssignment};
        let cfg = config::configuration();
        let assignments = crate::commands::CommandDef::default_key_assignments(&cfg);
        let found = assignments.iter().any(|(_mods, _code, action)| {
            matches!(
                action,
                KeyAssignment::ActivateContextDropdown(ContextDropdownKind::Cd)
            )
        });
        assert!(
            found,
            "No default binding emitted for ActivateContextDropdown(Cd). \
             Assignments dumped: {:?}",
            assignments
                .iter()
                .filter(|(_, _, a)| matches!(
                    a,
                    KeyAssignment::ActivateContextDropdown(_)
                ))
                .collect::<Vec<_>>()
        );
    }

}
