//! ben-term custom: Context Dropdowns.
//!
//! Native overlay pickers triggered by typing a recognized command + space
//! at a shell prompt. v1 ships `cd` only; future commands (git, npm, ssh,
//! docker, kill, env) add sibling modules and reuse the shared guard chain
//! and picker loop from here.
//!
//! Isolated from upstream overlay code to keep rebases clean.

pub mod cd;
pub mod picker;

use mux::pane::{CachePolicy, Pane};
use std::sync::Arc;

/// Outcome of a trigger attempt. Mirrors `PerformAssignmentResult` semantics
/// so it can be returned directly from `perform_key_assignment` dispatch.
pub enum TriggerResult {
    /// Picker launched; the keypress is consumed.
    Launched,
    /// A guard failed; the caller should let the keypress fall through
    /// to the PTY as if no assignment existed.
    PassThrough,
}

/// Read the logical line at the cursor and return the text to the left of
/// the cursor and the text to the right (both on the same logical line).
///
/// Uses `get_logical_lines` + `xy_to_logical_x` so soft-wrapped prompts
/// (where a long PS1 wraps across physical rows) still yield the full
/// command-line buffer.
pub fn split_logical_line_at_cursor(pane: &Arc<dyn Pane>) -> Option<(String, String)> {
    let cursor = pane.get_cursor_position();
    let logical = pane.get_logical_lines(cursor.y..cursor.y + 1);
    let line = logical.into_iter().find(|l| l.contains_y(cursor.y))?;
    let lx = line.xy_to_logical_x(cursor.x, cursor.y);
    let logical_len = line.logical.len();
    let lx = lx.min(logical_len);
    let left = line.logical.columns_as_str(0..lx);
    let right = line.logical.columns_as_str(lx..logical_len);
    Some((left, right))
}

/// Cheap preflight shared by every context dropdown:
///   1. Not in an alt-screen TUI (vim / Claude Code / htop).
///   2. Shell appears to be at a prompt (CWD resolvable via OSC 7 or procinfo).
///
/// Specific pickers add their own line-regex check on top of this.
pub fn at_shell_prompt(pane: &Arc<dyn Pane>) -> bool {
    if pane.is_alt_screen_active() {
        return false;
    }
    pane.get_current_working_dir(CachePolicy::AllowStale).is_some()
}
