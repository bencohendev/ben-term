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
use crate::overlay::context_dropdowns::{at_shell_prompt, split_logical_line_at_cursor, TriggerResult};
use crate::overlay::start_overlay_pane;
use crate::termwindow::TermWindow;
use mux::pane::Pane;
use mux::termwiztermtab::TermWizTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WALK_BUDGET: Duration = Duration::from_millis(300);
const WALK_MAX_ENTRIES: usize = 500;
const ZOXIDE_MAX: usize = 20;

/// Entry point from `perform_key_assignment`. If every guard passes, launches
/// the picker as an overlay and returns `Launched` (key consumed). Otherwise
/// returns `PassThrough` so Space reaches the PTY normally.
pub fn try_activate(term_window: &mut TermWindow, pane: &Arc<dyn Pane>) -> TriggerResult {
    let cfg = config::configuration();
    if !cfg.cd_picker_enabled {
        return TriggerResult::PassThrough;
    }
    if !at_shell_prompt(pane) {
        return TriggerResult::PassThrough;
    }
    let Some((left, right)) = split_logical_line_at_cursor(pane) else {
        return TriggerResult::PassThrough;
    };
    if !is_cd_trigger(&left) || !right.trim().is_empty() {
        return TriggerResult::PassThrough;
    }

    let cwd = pane
        .get_current_working_dir(mux::pane::CachePolicy::AllowStale)
        .and_then(|url| url.to_file_path().ok());
    let home = dirs_home();
    let max_depth = cfg.cd_picker_max_depth.max(1) as usize;
    let include_home = cfg.cd_picker_include_home;

    let entries = gather_candidates(cwd.as_deref(), home.as_deref(), max_depth, include_home);

    let mux = mux::Mux::get();
    let Some(tab) = mux.get_active_tab_for_window(term_window.mux_window_id) else {
        return TriggerResult::PassThrough;
    };
    let pane_id = pane.pane_id();

    let (overlay, future) = start_overlay_pane(term_window, pane, move |_pid, term| {
        run_cd_picker(term, entries, pane_id)
    });
    term_window.assign_overlay(tab.tab_id(), overlay);
    promise::spawn::spawn(future).detach();
    TriggerResult::Launched
}

/// The picker itself: runs on the overlay thread. On select, it paste-injects
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
            let insertion = format!("cd {}", shell_quote(&entry.value));
            let _ = pane.send_paste(&insertion);
        }
    }
    Ok(())
}

fn is_cd_trigger(left: &str) -> bool {
    let trimmed = left.trim_start();
    trimmed == "cd"
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

    // 1. Pinned shortcuts — always first.
    push_entry(&mut seen, &mut out, "..".into(), "..".into());
    push_entry(&mut seen, &mut out, "-".into(), "-".into());
    if let Some(home) = home {
        push_entry(&mut seen, &mut out, "~".into(), home.display().to_string());
    }

    // 2. Walk cwd.
    if let Some(cwd) = cwd {
        walk_dirs(cwd, max_depth, start, &mut out, &mut seen);
    }

    // 3. Walk home (shallower) when cwd != home.
    if include_home {
        if let (Some(cwd), Some(home)) = (cwd, home) {
            if cwd != home {
                walk_dirs(home, 3, start, &mut out, &mut seen);
            }
        }
    }

    // 4. Zoxide frecent list.
    for path in zoxide_query() {
        if out.len() >= WALK_MAX_ENTRIES {
            break;
        }
        push_entry(&mut seen, &mut out, path.clone(), path);
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

fn walk_dirs(
    root: &Path,
    max_depth: usize,
    start: Instant,
    out: &mut Vec<PickerEntry>,
    seen: &mut std::collections::HashSet<String>,
) {
    for entry in walkdir::WalkDir::new(root)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_skippable_dir(e.file_name().to_str()))
    {
        if out.len() >= WALK_MAX_ENTRIES || start.elapsed() >= WALK_BUDGET {
            return;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_dir() {
            continue;
        }
        if entry.depth() == 0 {
            continue;
        }
        let path = entry.path();
        let display = path.display().to_string();
        if seen.insert(display.clone()) {
            // Show a relative path when possible — keeps labels short.
            let label = path
                .strip_prefix(root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| display.clone());
            out.push(PickerEntry {
                label,
                value: display,
            });
        }
    }
}

fn is_skippable_dir(name: Option<&str>) -> bool {
    matches!(
        name,
        Some(".git" | "node_modules" | "target" | ".svn" | ".hg" | ".venv" | "venv")
    )
}

fn zoxide_query() -> Vec<String> {
    let Ok(output) = std::process::Command::new("zoxide")
        .args(["query", "--list"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .take(ZOXIDE_MAX)
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
    fn trigger_matches_cd_with_whitespace_prefix() {
        assert!(is_cd_trigger("cd"));
        assert!(is_cd_trigger("  cd"));
        assert!(is_cd_trigger("\tcd"));
    }

    #[test]
    fn trigger_rejects_non_cd() {
        assert!(!is_cd_trigger("echo cd"));
        assert!(!is_cd_trigger("cde"));
        assert!(!is_cd_trigger("cd "));
        assert!(!is_cd_trigger(""));
        assert!(!is_cd_trigger("cdx"));
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
