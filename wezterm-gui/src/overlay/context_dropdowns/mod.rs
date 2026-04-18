//! ben-term custom: Context Dropdowns.
//!
//! Native overlay pickers triggered by typing a recognized command + space
//! at a shell prompt. v1 ships `cd` only; future commands (git, npm, ssh,
//! docker, kill, env) add sibling modules and reuse the shared guard chain
//! and picker loop from here.
//!
//! Isolated from upstream overlay code to keep rebases clean.

pub mod cd;
pub mod git;
pub mod popup_picker;

use mux::pane::Pane;
use std::sync::Arc;
use wezterm_term::SemanticType;

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

/// If the shell has OSC 133 integration installed, return the portion of the
/// Input zone (the actual command line, excluding prompt chrome and RPROMPT)
/// to the left and right of the cursor.
///
/// Returns `None` when no Input zone contains the cursor — callers should
/// fall back to the heuristic `split_logical_line_at_cursor` for shells
/// without OSC 133.
///
/// Why this matters: the Input zone precisely marks where the user's command
/// starts and ends, so the picker can check the FIRST token of the command
/// (rejecting `echo cd ` where `cd` is an argument) and enforce that the
/// cursor is at the trailing edge of the zone (rejecting `cd fo|o` where
/// content follows the cursor inside the command buffer).
pub fn split_input_zone_at_cursor(pane: &Arc<dyn Pane>) -> Option<(String, String)> {
    let cursor = pane.get_cursor_position();
    let zones = pane.get_semantic_zones().ok()?;
    let zone = zones.into_iter().find(|z| {
        z.semantic_type == SemanticType::Input
            && z.start_y <= cursor.y
            && cursor.y <= z.end_y
    })?;

    let logical = pane.get_logical_lines(zone.start_y..zone.end_y + 1);
    let line = logical.into_iter().find(|l| l.contains_y(cursor.y))?;
    let logical_len = line.logical.len();

    // Map the zone's physical (x, y) corners onto the logical line so
    // soft-wrapped commands are handled correctly.
    let zone_start_lx = if line.contains_y(zone.start_y) {
        line.xy_to_logical_x(zone.start_x, zone.start_y)
            .min(logical_len)
    } else {
        0
    };
    let zone_end_lx = if line.contains_y(zone.end_y) {
        // end_x is the inclusive last column of the zone; +1 makes it
        // exclusive so `columns_as_str(..zone_end_lx)` stops past it.
        line.xy_to_logical_x(zone.end_x.saturating_add(1), zone.end_y)
            .min(logical_len)
    } else {
        logical_len
    };

    let cursor_lx = line
        .xy_to_logical_x(cursor.x, cursor.y)
        .clamp(zone_start_lx, zone_end_lx);

    let left = line.logical.columns_as_str(zone_start_lx..cursor_lx);
    let right = line.logical.columns_as_str(cursor_lx..zone_end_lx);
    Some((left, right))
}

/// Single-quote the value unless it's already safe bare-word shell text.
/// Embedded `'` is handled via the `'\''` concat trick.
///
/// Shared by all context-dropdown pickers that paste a value into the shell.
/// Applied defensively even for inputs that shouldn't need it (e.g. git ref
/// names forbid spaces and quotes) so a pathological candidate source can't
/// produce shell injection.
pub(super) fn shell_quote(s: &str) -> String {
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
    fn quote_safe_paths_unchanged() {
        assert_eq!(shell_quote("/tmp/foo"), "/tmp/foo");
        assert_eq!(shell_quote(".."), "..");
        assert_eq!(shell_quote("~"), "~");
        assert_eq!(shell_quote("/Users/a/b-c.d_e"), "/Users/a/b-c.d_e");
        assert_eq!(shell_quote("main"), "main");
        assert_eq!(shell_quote("feature/foo"), "feature/foo");
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
