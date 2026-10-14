//! Inline HSV color picker shown when the user presses `Space` on an
//! editing row in the theme builder.
//!
//! Keyboard-driven:
//!   - Left/Right: sat -/+
//!   - Up/Down: val +/- (up brightens)
//!   - Shift+Left/Right: hue -/+
//!   - Enter: commit current HSV as the row's hex
//!   - Esc: close picker, restore the hex buffer that was
//!     in place before the picker opened
//!
//! Mouse-driven (milestone 6):
//!   - Click inside SV grid: set (sat, val) at the cell under the cursor
//!   - Click inside hue bar: set hue at the cell under the cursor
//!   - Drag from either: continuous update; clamped to the captured
//!     rect's edges when the pointer moves outside
//!   - Release: ends drag capture; no commit (Enter still commits)
//!
//! Render: 16×8 SV grid + 24-cell hue bar, all solid-bg Element rects.
//! Cursor cells show a contrasting marker so the active (sat, val) and
//! hue positions are visible against their color.

use crate::termwindow::box_model::{
    BorderColor, BoxDimension, ComputedElement, ComputedElementContent, DisplayType, Element,
    ElementColors, ElementContent, InheritableColor,
};
use config::{Dimension, RgbaColor};
use std::ops::Deref;
use std::rc::Rc;
use wezterm_font::LoadedFont;
use window::color::LinearRgba;
use window::RectF;

const SV_COLS: usize = 16;
const SV_ROWS: usize = 8;
const HUE_CELLS: usize = 24;

const SAT_STEP: f32 = 1.0 / (SV_COLS as f32 - 1.0);
const VAL_STEP: f32 = 1.0 / (SV_ROWS as f32 - 1.0);
const HUE_STEP: f32 = 360.0 / HUE_CELLS as f32;

/// Picker state held while the picker is active. Lives inside the
/// builder's `RefCell<Option<PickerState>>` — `Some` means the picker
/// overlay is showing.
#[derive(Debug, Clone)]
pub struct PickerState {
    pub hue: f32,
    pub sat: f32,
    pub val: f32,
    /// Edit buffer at the moment the picker opened. Restored on Esc so
    /// any in-progress hex typing isn't lost by a cancelled pick.
    pub saved_buffer: String,
}

impl PickerState {
    /// Seed from the user's edit buffer (if parseable) or a fallback hex.
    /// The `saved_buffer` retains whatever the user had typed so Esc can
    /// put it back untouched.
    pub fn from_buffer(buffer: &str, fallback_hex: Option<&str>) -> Self {
        let hex = buffer.trim();
        let rgb = parse_hex_to_rgb(hex)
            .or_else(|| fallback_hex.and_then(parse_hex_to_rgb))
            .unwrap_or((255, 0, 0));
        let (h, s, v) = rgb_to_hsv(rgb.0, rgb.1, rgb.2);
        Self {
            hue: h,
            sat: s,
            val: v,
            saved_buffer: buffer.to_string(),
        }
    }

    /// Current picker color as a `#rrggbb` string.
    pub fn to_hex(&self) -> String {
        let (r, g, b) = hsv_to_rgb(self.hue, self.sat, self.val);
        format!("#{:02x}{:02x}{:02x}", r, g, b)
    }

    pub fn nudge_sat(&mut self, delta: f32) {
        self.sat = (self.sat + delta).clamp(0.0, 1.0);
    }

    pub fn nudge_val(&mut self, delta: f32) {
        self.val = (self.val + delta).clamp(0.0, 1.0);
    }

    pub fn nudge_hue(&mut self, delta: f32) {
        self.hue = (self.hue + delta).rem_euclid(360.0);
    }

    pub fn sat_step() -> f32 {
        SAT_STEP
    }

    pub fn val_step() -> f32 {
        VAL_STEP
    }

    pub fn hue_step() -> f32 {
        HUE_STEP
    }
}

/// Recorded pixel bounds of the picker's interactive regions, captured
/// during the builder's `compute()` call and consulted by the mouse
/// handlers. Coordinates are in window-absolute pixels (same space as
/// `MouseEvent::coords`).
#[derive(Debug, Clone, Copy)]
pub struct PickerLayout {
    pub sv_rect: RectF,
    pub hue_rect: RectF,
}

/// Which of the picker's interactive regions a pixel point falls inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerHit {
    Sv,
    Hue,
    None,
}

impl PickerLayout {
    pub fn hit(&self, pt: (f32, f32)) -> PickerHit {
        let p = euclid::point2(pt.0, pt.1);
        if self.sv_rect.contains(p) {
            PickerHit::Sv
        } else if self.hue_rect.contains(p) {
            PickerHit::Hue
        } else {
            PickerHit::None
        }
    }

    /// Walk a computed modal tree to recover the picker's pixel bounds.
    /// We don't want to hard-index into the tree (the outer structure
    /// changes when the preview pane is hidden) so we look for a
    /// container whose children match the picker's distinctive shape: a
    /// run of 8 consecutive rows with exactly `SV_COLS` children each,
    /// followed (somewhere in the trailing children) by a row with
    /// exactly `HUE_CELLS` children. Only one element in the theme
    /// builder matches that shape, so false positives are effectively
    /// impossible.
    pub fn from_computed_elements(roots: &[ComputedElement]) -> Option<Self> {
        for root in roots {
            if let Some(layout) = find_picker_layout(root) {
                return Some(layout);
            }
        }
        None
    }
}

fn find_picker_layout(node: &ComputedElement) -> Option<PickerLayout> {
    let kids = match &node.content {
        ComputedElementContent::Children(k) => k,
        _ => return None,
    };
    if let Some(layout) = layout_from_picker_children(kids) {
        return Some(layout);
    }
    for kid in kids {
        if let Some(layout) = find_picker_layout(kid) {
            return Some(layout);
        }
    }
    None
}

fn layout_from_picker_children(kids: &[ComputedElement]) -> Option<PickerLayout> {
    if kids.len() < 9 {
        return None;
    }
    // 8 consecutive SV rows at the head, each with exactly SV_COLS children.
    for kid in kids.iter().take(8) {
        match &kid.content {
            ComputedElementContent::Children(grand) if grand.len() == SV_COLS => {}
            _ => return None,
        }
    }
    // SV rect spans all 8 rows' outer bounds. `euclid::Rect::union` merges
    // them; initial fold seed is the first row.
    let sv_rect = kids
        .iter()
        .take(8)
        .map(|k| k.bounds)
        .reduce(|a, b| a.union(&b))?;
    // Hue row: first element in the tail that has exactly HUE_CELLS children.
    let hue_rect = kids
        .iter()
        .skip(8)
        .find(|k| matches!(&k.content, ComputedElementContent::Children(g) if g.len() == HUE_CELLS))?
        .bounds;
    Some(PickerLayout { sv_rect, hue_rect })
}

/// Update the picker's `(sat, val)` from a pixel coordinate inside (or
/// near) the SV rect. Out-of-rect coordinates are clamped to the rect's
/// edges so a drag that leaves the grid still tracks cleanly.
pub fn sv_point_to_state(state: &mut PickerState, rect: RectF, pt: (f32, f32)) {
    let w = rect.width().max(1.0);
    let h = rect.height().max(1.0);
    let fx = ((pt.0 - rect.min_x()) / w).clamp(0.0, 1.0);
    let fy = ((pt.1 - rect.min_y()) / h).clamp(0.0, 1.0);
    // y grows downward but val grows upward — brightest row is the top.
    state.sat = fx;
    state.val = 1.0 - fy;
}

/// Update the picker's hue from a pixel coordinate inside (or near) the
/// hue rect. Clamped to the rect's horizontal edges; hue never wraps on
/// mouse input (users who want wrap-around can Shift-arrow).
pub fn hue_point_to_state(state: &mut PickerState, rect: RectF, pt: (f32, f32)) {
    let w = rect.width().max(1.0);
    let fx = ((pt.0 - rect.min_x()) / w).clamp(0.0, 1.0);
    // Clamp the top end just below 360° so (360° wraps to 0°) stays off.
    state.hue = (fx * 360.0).min(359.999);
}

/// Parse `#rgb` / `#rrggbb` (case-insensitive). Returns None for anything
/// else (including alpha-carrying `#rrggbbaa` — the picker only operates
/// on opaque colors, alpha is out of scope for 4b).
fn parse_hex_to_rgb(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim().trim_start_matches('#');
    match s.len() {
        3 => {
            let parse = |c: char| u8::from_str_radix(&c.to_string(), 16).ok();
            let mut chars = s.chars();
            let r = parse(chars.next()?)?;
            let g = parse(chars.next()?)?;
            let b = parse(chars.next()?)?;
            Some((r * 17, g * 17, b * 17))
        }
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some((r, g, b))
        }
        _ => None,
    }
}

/// HSV → RGB. Inputs: hue 0..360, sat 0..1, val 0..1. Output: 0..255 per
/// channel. Standard textbook formula; picks the hexant by integer-
/// flooring `h/60` and blends with chroma.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (rf, gf, bf) = match h as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((rf + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((gf + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((bf + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

/// RGB → HSV. Inverse of `hsv_to_rgb` within rounding error. Uses the
/// standard formulation with max/min/delta.
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g.max(b));
    let min = r.min(g.min(b));
    let delta = max - min;

    let h = if delta == 0.0 {
        0.0
    } else if (max - r).abs() < f32::EPSILON {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if (max - g).abs() < f32::EPSILON {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };

    let s = if max == 0.0 { 0.0 } else { delta / max };
    let v = max;

    (h.rem_euclid(360.0), s, v)
}

/// Build the picker UI as a single `Element` ready to be pushed into the
/// builder's element list. Layout: SV grid on top, hue bar below, hex
/// readout as a trailing text line.
pub fn render_picker(
    state: &PickerState,
    font: &Rc<LoadedFont>,
    solid_fg: &InheritableColor,
) -> Element {
    let mut rows: Vec<Element> = Vec::new();

    // Map picker state to the nearest cell index so the cursor sits on a
    // concrete cell. Indexes are also useful for the hue cursor.
    let sv_col = ((state.sat / SAT_STEP).round() as usize).min(SV_COLS - 1);
    let sv_row = ((1.0 - state.val) / VAL_STEP)
        .round()
        .clamp(0.0, (SV_ROWS - 1) as f32) as usize;
    let hue_col = ((state.hue / HUE_STEP).round() as usize) % HUE_CELLS;

    for row_idx in 0..SV_ROWS {
        let val = 1.0 - (row_idx as f32 * VAL_STEP);
        let mut cells: Vec<Element> = Vec::new();
        for col_idx in 0..SV_COLS {
            let sat = col_idx as f32 * SAT_STEP;
            let (r, g, b) = hsv_to_rgb(state.hue, sat, val);
            let is_cursor = row_idx == sv_row && col_idx == sv_col;
            cells.push(swatch_cell(font, r, g, b, 2, is_cursor));
        }
        rows.push(
            Element::new(font, ElementContent::Children(cells))
                .display(DisplayType::Block)
                .padding(BoxDimension {
                    left: Dimension::Cells(0.25),
                    right: Dimension::Cells(0.25),
                    top: Dimension::Cells(0.),
                    bottom: Dimension::Cells(0.),
                }),
        );
    }

    // Spacer between SV grid and hue bar.
    rows.push(
        Element::new(font, ElementContent::Text(" ".into()))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: LinearRgba::TRANSPARENT.into(),
                text: solid_fg.clone(),
            })
            .display(DisplayType::Block),
    );

    // Hue bar: 24 cells wide.
    let mut hue_cells: Vec<Element> = Vec::new();
    for col_idx in 0..HUE_CELLS {
        let h = col_idx as f32 * HUE_STEP;
        let (r, g, b) = hsv_to_rgb(h, 1.0, 1.0);
        let is_cursor = col_idx == hue_col;
        hue_cells.push(swatch_cell(font, r, g, b, 2, is_cursor));
    }
    rows.push(
        Element::new(font, ElementContent::Children(hue_cells))
            .display(DisplayType::Block)
            .padding(BoxDimension {
                left: Dimension::Cells(0.25),
                right: Dimension::Cells(0.25),
                top: Dimension::Cells(0.),
                bottom: Dimension::Cells(0.),
            }),
    );

    // Readout: current hex + HSV triple, to make keyboard nudging
    // feel concrete.
    let readout = format!(
        "  {}   H {:>3.0}°  S {:>3.0}%  V {:>3.0}%",
        state.to_hex(),
        state.hue,
        state.sat * 100.0,
        state.val * 100.0,
    );
    rows.push(
        Element::new(font, ElementContent::Text(readout))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: LinearRgba::TRANSPARENT.into(),
                text: solid_fg.clone(),
            })
            .padding(BoxDimension {
                left: Dimension::Cells(0.25),
                right: Dimension::Cells(0.25),
                top: Dimension::Cells(0.3),
                bottom: Dimension::Cells(0.),
            })
            .display(DisplayType::Block),
    );

    Element::new(font, ElementContent::Children(rows))
        .display(DisplayType::Block)
        .padding(BoxDimension {
            left: Dimension::Cells(0.),
            right: Dimension::Cells(0.),
            top: Dimension::Cells(0.25),
            bottom: Dimension::Cells(0.25),
        })
}

/// One colored cell in the SV grid or hue bar. `width_chars` controls the
/// visual width per cell (each char is narrow; 2 chars gives a more
/// square cell given typical terminal font aspect ratios).
fn swatch_cell(
    font: &Rc<LoadedFont>,
    r: u8,
    g: u8,
    b: u8,
    width_chars: usize,
    is_cursor: bool,
) -> Element {
    let rgba = RgbaColor::from((r, g, b));
    let bg: LinearRgba = rgba.deref().to_linear();
    // Pick a contrasting marker color so the cursor dot stays visible
    // against light AND dark swatches.
    let marker_is_black = relative_luminance(r, g, b) > 0.5;
    let marker_color: LinearRgba = if marker_is_black {
        RgbaColor::from((0u8, 0, 0)).deref().to_linear()
    } else {
        RgbaColor::from((255u8, 255, 255)).deref().to_linear()
    };

    let text = if is_cursor {
        // Center a dot in the cell. Pad to the target width.
        let mut s = String::with_capacity(width_chars);
        let center = width_chars / 2;
        for i in 0..width_chars {
            s.push(if i == center { '●' } else { ' ' });
        }
        s
    } else {
        " ".repeat(width_chars)
    };

    Element::new(font, ElementContent::Text(text)).colors(ElementColors {
        border: BorderColor::default(),
        bg: bg.into(),
        text: marker_color.into(),
    })
}

/// Perceived brightness via the Rec. 601 luma coefficients. Used only to
/// pick a readable cursor marker — not color-accurate enough for anything
/// but "is this cell roughly bright or dark".
fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    0.299 * r + 0.587 * g + 0.114 * b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_rgb_round_trips_primary_colors() {
        // Pure red, green, blue should round-trip through the converters.
        for (r, g, b) in [(255u8, 0, 0), (0, 255, 0), (0, 0, 255)] {
            let (h, s, v) = rgb_to_hsv(r, g, b);
            let (r2, g2, b2) = hsv_to_rgb(h, s, v);
            assert_eq!((r, g, b), (r2, g2, b2), "round-trip failed");
        }
    }

    #[test]
    fn hsv_rgb_round_trips_grey() {
        let (h, s, v) = rgb_to_hsv(128, 128, 128);
        assert!(s.abs() < 0.01);
        let (r, g, b) = hsv_to_rgb(h, s, v);
        assert!((r as i32 - 128).abs() <= 1);
        assert!((g as i32 - 128).abs() <= 1);
        assert!((b as i32 - 128).abs() <= 1);
    }

    #[test]
    fn hsv_hue_wraps() {
        let (r1, g1, b1) = hsv_to_rgb(0.0, 1.0, 1.0);
        let (r2, g2, b2) = hsv_to_rgb(360.0, 1.0, 1.0);
        assert_eq!((r1, g1, b1), (r2, g2, b2));
    }

    #[test]
    fn parse_hex_handles_short_and_long_form() {
        assert_eq!(parse_hex_to_rgb("#abc"), Some((0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_hex_to_rgb("#aabbcc"), Some((0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_hex_to_rgb("  abcdef  "), Some((0xab, 0xcd, 0xef)));
        assert_eq!(parse_hex_to_rgb(""), None);
        assert_eq!(parse_hex_to_rgb("#zzz"), None);
    }

    #[test]
    fn state_to_hex_formats_six_digits() {
        let s = PickerState::from_buffer("#ff0000", None);
        assert_eq!(s.to_hex(), "#ff0000");
    }

    #[test]
    fn state_seeds_from_buffer_falls_back_to_default() {
        let s = PickerState::from_buffer("not-a-color", Some("#00ff00"));
        assert!(
            (s.sat - 1.0).abs() < 0.01
                && (s.val - 1.0).abs() < 0.01
                && (s.hue - 120.0).abs() < 1.0
        );
    }

    #[test]
    fn nudge_sat_clamps_to_unit_range() {
        let mut s = PickerState::from_buffer("#ff0000", None);
        s.nudge_sat(1.5);
        assert_eq!(s.sat, 1.0);
        s.nudge_sat(-5.0);
        assert_eq!(s.sat, 0.0);
    }

    #[test]
    fn nudge_hue_wraps_mod_360() {
        let mut s = PickerState::from_buffer("#ff0000", None);
        s.nudge_hue(-30.0);
        assert!((s.hue - 330.0).abs() < 0.01);
        s.nudge_hue(60.0);
        assert!((s.hue - 30.0).abs() < 0.01);
    }

    fn layout(sv: RectF, hue: RectF) -> PickerLayout {
        PickerLayout {
            sv_rect: sv,
            hue_rect: hue,
        }
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> RectF {
        euclid::rect(x, y, w, h)
    }

    #[test]
    fn hit_dispatches_by_region() {
        let l = layout(rect(10., 20., 100., 80.), rect(10., 110., 100., 10.));
        assert_eq!(l.hit((50., 50.)), PickerHit::Sv);
        assert_eq!(l.hit((50., 115.)), PickerHit::Hue);
        // Above SV, outside both rects.
        assert_eq!(l.hit((50., 5.)), PickerHit::None);
        // Between SV and hue, outside both rects.
        assert_eq!(l.hit((50., 105.)), PickerHit::None);
    }

    #[test]
    fn sv_point_center_of_rect_is_mid_sat_mid_val() {
        let mut s = PickerState::from_buffer("#000000", None);
        sv_point_to_state(&mut s, rect(0., 0., 100., 100.), (50., 50.));
        assert!((s.sat - 0.5).abs() < 0.01);
        assert!((s.val - 0.5).abs() < 0.01);
    }

    #[test]
    fn sv_point_top_left_is_zero_sat_full_val() {
        let mut s = PickerState::from_buffer("#000000", None);
        sv_point_to_state(&mut s, rect(0., 0., 100., 100.), (0., 0.));
        assert!(s.sat.abs() < 0.01);
        assert!((s.val - 1.0).abs() < 0.01);
    }

    #[test]
    fn sv_point_bottom_right_is_full_sat_zero_val() {
        let mut s = PickerState::from_buffer("#000000", None);
        sv_point_to_state(&mut s, rect(0., 0., 100., 100.), (100., 100.));
        assert!((s.sat - 1.0).abs() < 0.01);
        assert!(s.val.abs() < 0.01);
    }

    #[test]
    fn sv_point_outside_rect_clamps() {
        let mut s = PickerState::from_buffer("#000000", None);
        // Way above and to the left: clamps to top-left.
        sv_point_to_state(&mut s, rect(10., 10., 100., 100.), (-50., -50.));
        assert!(s.sat.abs() < 0.01);
        assert!((s.val - 1.0).abs() < 0.01);
        // Way below and to the right: clamps to bottom-right.
        sv_point_to_state(&mut s, rect(10., 10., 100., 100.), (1000., 1000.));
        assert!((s.sat - 1.0).abs() < 0.01);
        assert!(s.val.abs() < 0.01);
    }

    #[test]
    fn hue_point_scans_0_to_near_360() {
        let mut s = PickerState::from_buffer("#ff0000", None);
        let r = rect(0., 0., 360., 10.);
        hue_point_to_state(&mut s, r, (0., 5.));
        assert!(s.hue.abs() < 0.1);
        hue_point_to_state(&mut s, r, (180., 5.));
        assert!((s.hue - 180.).abs() < 0.1);
        // At exact right edge, hue should clamp just under 360 so it
        // doesn't wrap back to zero.
        hue_point_to_state(&mut s, r, (360., 5.));
        assert!(s.hue < 360.0);
        assert!(s.hue > 359.0);
    }

    #[test]
    fn hue_point_clamps_outside_rect() {
        let mut s = PickerState::from_buffer("#ff0000", None);
        let r = rect(100., 0., 100., 10.);
        hue_point_to_state(&mut s, r, (50., 5.)); // left of rect
        assert!(s.hue.abs() < 0.1);
        hue_point_to_state(&mut s, r, (999., 5.)); // right of rect
        assert!(s.hue > 359.0);
    }
}
