use crate::termwindow::box_model::ComputedElement;
use crate::TermWindow;
use config::keyassignment::KeyAssignment;
use downcast_rs::{impl_downcast, Downcast};
use std::cell::Ref;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent};

/// Outcome of `Modal::mouse_press_pixel`. Distinguishes "don't consume"
/// from "consume but don't track further motion" from "consume and
/// capture the mouse for a drag interaction." The theme builder's HSV
/// picker uses `Capture` for SV-grid / hue-bar presses so drag events
/// route to it until release; other modals stay on `Passthrough` /
/// `Consume`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalPressResult {
    /// The event is not for this modal. Dispatch continues to the pane
    /// underneath.
    Passthrough,
    /// The modal consumed the press. No drag follow-up is expected.
    Consume,
    /// The modal consumed the press and wants subsequent drag/release
    /// events until the mouse button comes back up.
    Capture,
}

pub trait Modal: Downcast {
    fn perform_assignment(
        &self,
        _assignment: &KeyAssignment,
        _term_window: &mut TermWindow,
    ) -> bool {
        false
    }
    fn mouse_event(&self, event: MouseEvent, term_window: &mut TermWindow) -> anyhow::Result<()>;
    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<bool>;
    fn computed_element(
        &self,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<Ref<'_, [ComputedElement]>>;
    fn reconfigure(&self, term_window: &mut TermWindow);

    /// Called on every window-level mouse press (pixel coords relative to
    /// the window origin). Default: pass through. ben-term popup pickers
    /// return `Consume` for click-away dismiss; the theme builder's HSV
    /// picker returns `Capture` on presses inside the SV grid / hue bar
    /// so follow-up motion routes through `mouse_drag_pixel`.
    fn mouse_press_pixel(
        &self,
        _point: (f32, f32),
        _term_window: &TermWindow,
    ) -> ModalPressResult {
        ModalPressResult::Passthrough
    }

    /// Called on each mouse-motion event while this modal holds the
    /// capture (set via a preceding `mouse_press_pixel` that returned
    /// `Capture`). Default no-op; only modals that return `Capture` need
    /// to override this.
    fn mouse_drag_pixel(&self, _point: (f32, f32), _term_window: &TermWindow) {}

    /// Called when the captured mouse button is released. Default no-op.
    /// The capture flag is cleared by the dispatcher regardless of what
    /// this method does.
    fn mouse_release_pixel(&self, _point: (f32, f32), _term_window: &TermWindow) {}
}
impl_downcast!(Modal);
