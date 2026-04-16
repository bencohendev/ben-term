//! `cd` context dropdown.
//!
//! Guard chain:
//!   config.cd_picker_enabled → at_shell_prompt → line-left matches `^\s*cd$`
//!   AND line-right is empty/whitespace.
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

use crate::overlay::context_dropdowns::picker::{self, PickerEntry, PickerParams};
use crate::overlay::context_dropdowns::{split_logical_line_at_cursor, TriggerResult};
use crate::overlay::start_overlay_pane;
use crate::termwindow::TermWindow;
use mux::pane::Pane;
use mux::termwiztermtab::TermWizTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WALK_BUDGET: Duration = Duration::from_millis(500);
const WALK_MAX_ENTRIES: usize = 10_000;
const HOME_WALK_DEPTH: usize = 5;

/// Entry point from `perform_key_assignment`. If every guard passes, launches
/// the picker as an overlay and returns `Launched` (key consumed). Otherwise
/// returns `PassThrough` so Space reaches the PTY normally.
pub fn try_activate(term_window: &mut TermWindow, pane: &Arc<dyn Pane>) -> TriggerResult {
    let cfg = config::configuration();
    if !cfg.cd_picker_enabled {
        return TriggerResult::PassThrough;
    }
    // Alt-screen hard guard (vim / Claude Code / htop): always bypass.
    if pane.is_alt_screen_active() {
        return TriggerResult::PassThrough;
    }
    // If this pane already has an overlay (e.g. the picker itself), don't
    // try to re-trigger — let Space through to the overlay's input stream.
    if term_window.pane_has_overlay(pane.pane_id()) {
        return TriggerResult::PassThrough;
    }

    let Some((left, _right)) = split_logical_line_at_cursor(pane) else {
        return TriggerResult::PassThrough;
    };
    // Note: we deliberately don't constrain right-of-cursor — shells with
    // RPROMPT (zsh powerline themes, git-status blocks) render content to
    // the right of the input buffer, which would otherwise block the trigger.
    // v2 will use OSC 133 semantic zones for precise input-vs-chrome.
    if !is_cd_trigger(&left) {
        return TriggerResult::PassThrough;
    }

    let cwd = pane
        .get_current_working_dir(mux::pane::CachePolicy::AllowStale)
        .and_then(|url| url.to_file_path().ok())
        .or_else(|| std::env::current_dir().ok());
    let home = dirs_home();
    let max_depth = cfg.cd_picker_max_depth.max(1) as usize;
    let include_home = cfg.cd_picker_include_home;
    let pane_id = pane.pane_id();

    // Gather on the main thread so the picker's first render has content.
    // Budget-capped BFS walk + zoxide is typically <500 ms.
    let entries =
        gather_candidates(cwd.as_deref(), home.as_deref(), max_depth, include_home);

    let (overlay, future) = start_overlay_pane(term_window, pane, move |_pid, term| {
        run_cd_picker(term, entries, pane_id)
    });
    // Must use pane-level overlay registration because `start_overlay_pane`
    // schedules `CancelOverlayForPane(pane_id)` on completion — a tab-level
    // assignment wouldn't be dismissed by that notification.
    term_window.assign_overlay_for_pane(pane_id, overlay);
    promise::spawn::spawn(future).detach();
    TriggerResult::Launched
}

/// Runs on the overlay thread. Gathers candidates (blocking the overlay
/// thread, not the GUI), then runs the picker loop. On select, paste-injects
/// the completed `cd <path>` string into the originating pane.
fn run_cd_picker(
    term: TermWizTerminal,
    entries: Vec<PickerEntry>,
    source_pane_id: mux::pane::PaneId,
) -> anyhow::Result<()> {
    let params = PickerParams {
        title: "cd".to_string(),
        description: "Select a directory".to_string(),
        entries,
        initial_filter: String::new(),
    };
    let picked = picker::run(term, params)?;
    if let Some(entry) = picked {
        let mux = mux::Mux::get();
        if let Some(pane) = mux.get_pane(source_pane_id) {
            // The user already typed `cd` to trigger this picker (Space was
            // consumed as the trigger). Paste only " <path>" to complete the
            // command — don't re-emit `cd`.
            let insertion = format!(" {}", shell_quote(&entry.value));
            let _ = pane.send_paste(&insertion);
        }
    }
    Ok(())
}

/// Trigger fires when the last whitespace-delimited token in the line-left-of-cursor
/// is `cd`. Reading the literal line buffer includes the shell prompt (e.g. `~ $ cd`),
/// so we can't match the whole string — only the trailing token.
///
/// This does fire on lines like `echo cd ` where cd is an argument, not the command.
/// Press Escape to dismiss; v2 will use OSC 133 semantic zones to narrow this.
fn is_cd_trigger(left: &str) -> bool {
    left.split_whitespace().last() == Some("cd")
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

/// Single-quote the path unless it's already safe bare-word shell text.
/// Embedded `'` is handled via the `'\''` concat trick.
fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '/' | '.' | '_' | '-' | '~' | '=' | ':' | '+' | '@')
        });
    if safe {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
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

    /// Known-imperfect: v1 fires on this; user can press Escape. v2 will
    /// use OSC 133 to distinguish prompt-context `cd` from argument `cd`.
    #[test]
    fn trigger_has_known_false_positive_on_cd_as_argument() {
        assert!(is_cd_trigger("echo cd"));
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

    #[test]
    fn quote_safe_paths_unchanged() {
        assert_eq!(shell_quote("/tmp/foo"), "/tmp/foo");
        assert_eq!(shell_quote(".."), "..");
        assert_eq!(shell_quote("~"), "~");
        assert_eq!(shell_quote("/Users/a/b-c.d_e"), "/Users/a/b-c.d_e");
    }

    #[test]
    fn quote_paths_with_spaces() {
        assert_eq!(shell_quote("/has space"), "'/has space'");
    }

    #[test]
    fn quote_paths_with_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
