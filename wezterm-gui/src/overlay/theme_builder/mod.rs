//! Interactive theme builder (rich theming milestone 4a).
//!
//! Entry point invoked by the `ActivateThemeBuilder` key assignment.
//! The modal itself lives in `builder.rs`; file-system plumbing for
//! resolving the active scheme's on-disk TOML + writing zone edits
//! lives in `file_ops.rs`.

pub mod builder;
pub mod file_ops;

use crate::termwindow::TermWindow;
use std::rc::Rc;

/// Open the theme builder modal for the currently-active scheme.
///
/// Called from `ActivateThemeBuilder` dispatch in
/// `wezterm-gui/src/termwindow/mod.rs`. Does nothing if a modal is
/// already installed (e.g. the command palette is already open) so
/// two modals never stack.
pub fn try_activate(term_window: &mut TermWindow) {
    if term_window.has_modal() {
        return;
    }

    let builder = match builder::ThemeBuilder::new(term_window) {
        Ok(b) => b,
        Err(err) => {
            log::warn!("theme builder: not activating — {:#}", err);
            return;
        }
    };

    term_window.set_modal(Rc::new(builder));
}
