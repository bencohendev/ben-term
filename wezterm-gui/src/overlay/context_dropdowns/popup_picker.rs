//! Shared popup picker for context dropdowns, rendered as a `Modal`.
//!
//! Originally shipped as `CdPicker` in v3; generalized to back multiple
//! pickers (`cd`, `git`, future `npm`/`ssh`/…) that all render the same
//! anchored, fuzzy-filtered popup and paste a shell-quoted value on Enter.
//!
//! Reuses the command palette's paint + input plumbing (zindex 100,
//! `paint_modal` integration, keyevent dispatch), but does NOT import from
//! `palette.rs` — keeps our divergence isolated so upstream palette changes
//! don't ripple into here.
//!
//! Trigger + candidate gathering logic lives in each caller module (e.g.
//! `cd.rs`, `git.rs`); this file only owns render and per-keystroke state.

use crate::overlay::selector::{matcher_pattern, matcher_score};
use crate::termwindow::box_model::{
    BorderColor, BoxDimension, ComputedElement, Corners, DisplayType, Element, ElementColors,
    ElementContent, InheritableColor, LayoutContext, SizedPoly,
};
use crate::termwindow::modal::{Modal, ModalPressResult};
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::TermWindow;
use crate::utilsprites::RenderMetrics;
use config::{Dimension, DimensionContext};
use mux::pane::PaneId;
use rayon::prelude::*;
use std::cell::{Ref, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent};
use window::color::LinearRgba;

/// Monotonic counter for per-activation picker IDs. Used so a background
/// gather task can check whether the currently-installed modal is the one
/// it was spawned for — guards against a stale result landing in a newer
/// picker (e.g. user opens cd, Escapes, opens cd again before the first
/// gather returns).
static NEXT_PICKER_ID: AtomicU64 = AtomicU64::new(0);

/// One candidate row in the picker.
#[derive(Debug, Clone)]
pub struct PickerEntry {
    /// Shown in the list (e.g. a `strip_prefix`'d relative path).
    pub label: String,
    /// Pasted into the shell on select (e.g. the absolute path).
    pub value: String,
}

struct MatchResults {
    selection: String,
    /// Indices into `entries`, sorted by descending match score.
    matches: Vec<usize>,
}

pub struct PopupPicker {
    /// Per-activation identity. Background loaders compare this against the
    /// current modal's id before applying results, so stale loads from a
    /// dismissed-and-reopened picker can't bleed into a fresh one.
    id: u64,
    /// Candidate list. `RefCell` so async pickers (e.g. cd) can swap in a
    /// freshly-gathered list via `set_entries_loaded` after the modal is
    /// already on screen. Mutation only happens on the main thread.
    entries: RefCell<Vec<PickerEntry>>,
    /// True while an async gather is in flight. When true with an empty
    /// entry list, the popup renders a "Loading…" placeholder instead of
    /// "(no matches)" so the user sees that results are still coming.
    loading: RefCell<bool>,
    source_pane_id: PaneId,
    /// Viewport column of the shell cursor at trigger time (used to anchor
    /// the popup just below/left-aligned to the typed `cd ` insertion point).
    cursor_col: usize,
    /// Viewport row of the shell cursor at trigger time. Can be negative if
    /// the cursor is above the visible area, though in practice the picker
    /// only fires at a live prompt where the cursor is on-screen.
    cursor_viewport_row: isize,
    // Interior mutability: `Modal` methods take `&self`.
    /// Filter text. Append-only — no in-filter cursor. The "real" cursor
    /// stays in the shell behind the popup; users edit via Backspace
    /// (removes last char) or Ctrl-U (clears).
    selection: RefCell<String>,
    selected_row: RefCell<usize>,
    top_row: RefCell<usize>,
    matches: RefCell<Option<MatchResults>>,
    element: RefCell<Option<Vec<ComputedElement>>>,
    max_rows_on_screen: RefCell<usize>,
}

impl PopupPicker {
    /// Construct in a "loading" state with no entries yet. The caller is
    /// expected to call `set_entries_loaded` on the main thread once the
    /// candidates finish gathering (typically via a `TermWindowNotif::Apply`
    /// posted from a background thread). This lets the modal appear
    /// instantly while the expensive gather runs off the main thread.
    pub fn new_loading(
        source_pane_id: PaneId,
        cursor_col: usize,
        cursor_viewport_row: isize,
    ) -> Self {
        Self {
            id: NEXT_PICKER_ID.fetch_add(1, Ordering::Relaxed),
            entries: RefCell::new(Vec::new()),
            loading: RefCell::new(true),
            source_pane_id,
            cursor_col,
            cursor_viewport_row,
            selection: RefCell::new(String::new()),
            selected_row: RefCell::new(0),
            top_row: RefCell::new(0),
            matches: RefCell::new(None),
            element: RefCell::new(None),
            max_rows_on_screen: RefCell::new(0),
        }
    }

    /// Swap in a freshly-loaded entry list and mark the picker as no longer
    /// loading. Invalidates cached match list + element so the next render
    /// re-fuzzies against the current filter (which the user may have typed
    /// into while the gather was running).
    pub fn set_entries_loaded(&self, entries: Vec<PickerEntry>) {
        *self.entries.borrow_mut() = entries;
        *self.loading.borrow_mut() = false;
        self.matches.borrow_mut().take();
        self.element.borrow_mut().take();
    }

    /// Per-activation identity. Callers spawning async work capture this
    /// when the picker is constructed and use it on completion to verify
    /// they're still talking to the same picker instance.
    pub fn id(&self) -> u64 {
        self.id
    }

    fn updated_input(&self) {
        *self.selected_row.borrow_mut() = 0;
        *self.top_row.borrow_mut() = 0;
    }

    fn move_up(&self) {
        let mut row = self.selected_row.borrow_mut();
        *row = row.saturating_sub(1);
        let mut top_row = self.top_row.borrow_mut();
        if *row < *top_row {
            *top_row = *row;
        }
    }

    fn move_down(&self) {
        let max_rows_on_screen = *self.max_rows_on_screen.borrow();
        let limit = self
            .matches
            .borrow()
            .as_ref()
            .map(|m| m.matches.len())
            .unwrap_or_else(|| self.entries.borrow().len())
            .saturating_sub(1);
        let mut row = self.selected_row.borrow_mut();
        *row = row.saturating_add(1).min(limit);
        let mut top_row = self.top_row.borrow_mut();
        if max_rows_on_screen > 0 && *row > *top_row + max_rows_on_screen - 1 {
            *top_row = row.saturating_sub(max_rows_on_screen - 1);
        }
    }

    fn filter_append(&self, c: char) {
        self.selection.borrow_mut().push(c);
    }

    /// Pop the last character off the filter. Uses char boundaries so
    /// multi-byte characters (e.g. `é` = 2 bytes) come off in one step.
    fn filter_pop(&self) {
        self.selection.borrow_mut().pop();
    }

    fn filter_clear(&self) {
        self.selection.borrow_mut().clear();
    }

    /// Forward a single key to the source pane's PTY. Used to echo filter
    /// typing into the shell so the command line stays in sync with the
    /// popup's internal filter state.
    fn forward_to_pane(&self, key: KeyCode, mods: KeyModifiers) {
        if let Some(pane) = mux::Mux::get().get_pane(self.source_pane_id) {
            let _ = pane.key_down(key, mods);
        }
    }

    /// Send `n` Backspace keys to the source pane's PTY. Used when the user
    /// clears the filter (Ctrl-U) or selects an entry (Enter) — both need to
    /// rub out the filter chars already echoed into the shell before any
    /// replacement text is pasted.
    fn send_backspaces(&self, n: usize) {
        if let Some(pane) = mux::Mux::get().get_pane(self.source_pane_id) {
            for _ in 0..n {
                let _ = pane.key_down(KeyCode::Backspace, KeyModifiers::NONE);
            }
        }
    }

    /// Returns the absolute path to paste on Enter, or None if there's no
    /// valid selection (empty match list).
    fn selected_value(&self) -> Option<String> {
        let selected_idx = *self.selected_row.borrow();
        let matches = self.matches.borrow();
        let results = matches.as_ref()?;
        let entry_idx = *results.matches.get(selected_idx)?;
        self.entries.borrow().get(entry_idx).map(|e| e.value.clone())
    }

    fn compute(
        &self,
        term_window: &mut TermWindow,
        matches: &MatchResults,
        max_rows_on_screen: usize,
    ) -> anyhow::Result<Vec<ComputedElement>> {
        let entries_ref = self.entries.borrow();
        let entries = entries_ref.as_slice();
        let is_loading = *self.loading.borrow();
        let selected_row = *self.selected_row.borrow();
        let top_row = *self.top_row.borrow();
        let font = term_window
            .fonts
            .command_palette_font()
            .expect("to resolve command palette font");
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());

        let top_bar_height = if term_window.show_tab_bar && !term_window.config.tab_bar_at_bottom {
            term_window.tab_bar_pixel_height().unwrap()
        } else {
            0.
        };
        let (padding_left, padding_top) = term_window.padding_left_top();
        let os_border = term_window.get_os_border();
        let grid_origin_y = top_bar_height + padding_top + os_border.top.get() as f32;

        let solid_bg_color: InheritableColor = term_window
            .config
            .command_palette_bg_color
            .to_linear()
            .into();
        let solid_fg_color: InheritableColor = term_window
            .config
            .command_palette_fg_color
            .to_linear()
            .into();

        // No header row: the filter text is echoed into the shell itself
        // (see `forward_to_pane` in `key_down`), so the user reads it from
        // the prompt. The popup shows filtered candidates only.
        let mut rows: Vec<Element> = Vec::new();

        if matches.matches.is_empty() {
            // Show "Loading…" while an async gather is still in flight so the
            // instant-open popup communicates "results are coming" instead of
            // the false-negative "(no matches)".
            let placeholder = if is_loading {
                "  Loading…"
            } else {
                "  (no matches)"
            };
            rows.push(
                Element::new(&font, ElementContent::Text(placeholder.to_string()))
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: LinearRgba::TRANSPARENT.into(),
                        text: solid_fg_color.clone(),
                    })
                    .display(DisplayType::Block),
            );
        } else {
            for (display_idx, entry) in matches
                .matches
                .iter()
                .map(|&idx| &entries[idx])
                .enumerate()
                .skip(top_row)
                .take(max_rows_on_screen)
            {
                let (bg, text) = if display_idx == selected_row {
                    (solid_fg_color.clone(), solid_bg_color.clone())
                } else {
                    (LinearRgba::TRANSPARENT.into(), solid_fg_color.clone())
                };

                rows.push(
                    Element::new(&font, ElementContent::Text(format!("  {}", entry.label)))
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg,
                            text,
                        })
                        .padding(BoxDimension {
                            left: Dimension::Cells(0.25),
                            right: Dimension::Cells(0.25),
                            top: Dimension::Cells(0.),
                            bottom: Dimension::Cells(0.),
                        })
                        .min_width(Some(Dimension::Percent(1.)))
                        .display(DisplayType::Block),
                );
            }
        }

        let dimensions = term_window.dimensions;
        let size = term_window.terminal_size();
        let cell_w = term_window.render_metrics.cell_size.width as f32;
        let cell_h = term_window.render_metrics.cell_size.height as f32;
        let avail_pixel_width = size.cols as f32 * cell_w;

        // Sizing: narrow popup anchored near the cursor — a dropdown, not a
        // palette. 50 cols reads comfortably for most paths without dominating
        // the pane. Clamped to available width on narrow windows.
        let desired_width_cells = 50_usize.min(size.cols.max(1));
        let desired_pixel_width = desired_width_cells as f32 * cell_w;

        // Anchor X: left-aligned to the cursor column, but clamped so the
        // popup doesn't spill off the right edge.
        let cursor_x_px = padding_left + self.cursor_col as f32 * cell_w;
        let max_x_px = padding_left + avail_pixel_width - desired_pixel_width;
        let anchor_x = cursor_x_px.min(max_x_px).max(padding_left);

        // Anchor Y: one row below the cursor row by default. Negative
        // viewport_row means the cursor has scrolled above the visible
        // area — rare, but clamp to the top of the grid so the popup
        // stays visible.
        let cursor_row = self.cursor_viewport_row.max(0) as f32;
        let below_anchor_y = grid_origin_y + (cursor_row + 1.) * cell_h;
        let anchor_y = below_anchor_y;

        let element = Element::new(&font, ElementContent::Children(rows))
            .colors(ElementColors {
                border: BorderColor::new(
                    term_window.config.command_palette_fg_color.to_linear(),
                ),
                bg: solid_bg_color,
                text: solid_fg_color,
            })
            .padding(BoxDimension {
                left: Dimension::Cells(0.5),
                right: Dimension::Cells(0.5),
                top: Dimension::Cells(0.25),
                bottom: Dimension::Cells(0.25),
            })
            .border(BoxDimension::new(Dimension::Pixels(1.)))
            .border_corners(Some(Corners {
                top_left: SizedPoly {
                    width: Dimension::Cells(0.5),
                    height: Dimension::Cells(0.5),
                    poly: TOP_LEFT_ROUNDED_CORNER,
                },
                top_right: SizedPoly {
                    width: Dimension::Cells(0.5),
                    height: Dimension::Cells(0.5),
                    poly: TOP_RIGHT_ROUNDED_CORNER,
                },
                bottom_left: SizedPoly {
                    width: Dimension::Cells(0.5),
                    height: Dimension::Cells(0.5),
                    poly: BOTTOM_LEFT_ROUNDED_CORNER,
                },
                bottom_right: SizedPoly {
                    width: Dimension::Cells(0.5),
                    height: Dimension::Cells(0.5),
                    poly: BOTTOM_RIGHT_ROUNDED_CORNER,
                },
            }))
            .min_width(Some(Dimension::Pixels(desired_pixel_width)))
            .max_width(Some(Dimension::Pixels(desired_pixel_width)));

        let mut computed = term_window.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_height as f32,
                    pixel_cell: cell_h,
                },
                width: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_width as f32,
                    pixel_cell: cell_w,
                },
                bounds: euclid::rect(
                    anchor_x,
                    anchor_y,
                    desired_pixel_width,
                    size.rows as f32 * cell_h,
                ),
                metrics: &metrics,
                gl_state: term_window.render_state.as_ref().unwrap(),
                zindex: 100,
            },
            &element,
        )?;

        // Flip above the cursor when the below-anchored popup would
        // overflow the grid bottom. Only flips if the above position
        // actually has more vertical room — otherwise the popup stays
        // below (bottom-cut is usually better than top-cut because the
        // top rows are the higher-ranked matches under fuzzy ordering).
        let grid_bottom_y = grid_origin_y + size.rows as f32 * cell_h;
        let popup_height = computed.bounds.height();
        if computed.bounds.max_y() > grid_bottom_y {
            let cursor_top_y = grid_origin_y + cursor_row * cell_h;
            let above_anchor_y = cursor_top_y - popup_height;
            let room_above = cursor_top_y - grid_origin_y;
            let room_below = grid_bottom_y - below_anchor_y;
            if room_above > room_below {
                let delta = above_anchor_y - below_anchor_y;
                computed.translate(euclid::vec2(0., delta));
            }
        }

        Ok(vec![computed])
    }

    /// Hit-test the most recently computed popup bounds against a
    /// pixel point. Returns `None` if the popup hasn't painted yet
    /// (the cached element hasn't been built) — callers should treat
    /// an unpainted popup as "no defined bounds".
    fn hit_test(&self, point: (f32, f32)) -> Option<bool> {
        let element = self.element.borrow();
        let first = element.as_ref()?.first()?;
        let pt = euclid::point2(point.0, point.1);
        Some(first.bounds.contains(pt))
    }
}

fn compute_matches(selection: &str, entries: &[PickerEntry]) -> Vec<usize> {
    if selection.is_empty() {
        (0..entries.len()).collect()
    } else {
        let pattern = matcher_pattern(selection);
        let mut scored: Vec<(usize, u32)> = entries
            .par_iter()
            .enumerate()
            .filter_map(|(idx, e)| matcher_score(&pattern, &e.label).map(|s| (idx, s)))
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.into_iter().map(|(idx, _)| idx).collect()
    }
}

impl Modal for PopupPicker {
    fn mouse_event(&self, _event: MouseEvent, _term_window: &mut TermWindow) -> anyhow::Result<()> {
        Ok(())
    }

    /// Click-away dismiss. A press outside the popup bounds cancels the
    /// modal and is consumed so the shell underneath doesn't also react.
    /// A press inside the popup is left untouched (future work: row
    /// click-to-select). If the popup hasn't painted yet we can't
    /// hit-test — in that rare case, pass through.
    fn mouse_press_pixel(&self, point: (f32, f32), term_window: &TermWindow) -> ModalPressResult {
        match self.hit_test(point) {
            Some(false) => {
                term_window.cancel_modal();
                ModalPressResult::Consume
            }
            _ => ModalPressResult::Passthrough,
        }
    }

    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<bool> {
        match (key, mods) {
            (KeyCode::Escape, KeyModifiers::NONE)
            | (KeyCode::Char('g'), KeyModifiers::CTRL)
            | (KeyCode::Char('c'), KeyModifiers::CTRL) => {
                term_window.cancel_modal();
                return Ok(true);
            }
            (KeyCode::UpArrow, KeyModifiers::NONE)
            | (KeyCode::Char('p'), KeyModifiers::CTRL)
            | (KeyCode::Char('k'), KeyModifiers::CTRL) => {
                self.move_up();
            }
            (KeyCode::DownArrow, KeyModifiers::NONE)
            | (KeyCode::Char('n'), KeyModifiers::CTRL)
            | (KeyCode::Char('j'), KeyModifiers::CTRL) => {
                self.move_down();
            }
            (KeyCode::PageUp, KeyModifiers::NONE) => {
                let step = *self.max_rows_on_screen.borrow();
                for _ in 0..step {
                    self.move_up();
                }
            }
            (KeyCode::PageDown, KeyModifiers::NONE) => {
                let step = *self.max_rows_on_screen.borrow();
                for _ in 0..step {
                    self.move_down();
                }
            }
            // Positional edits (Left/Right/Home/End and their Ctrl-B/F/A/E
            // equivalents) happen in the shell under the PTY-echo model —
            // the popup has no internal cursor. Forward them so the shell
            // caret moves; the popup filter stays append-only.
            (KeyCode::LeftArrow, KeyModifiers::NONE)
            | (KeyCode::RightArrow, KeyModifiers::NONE)
            | (KeyCode::Home, KeyModifiers::NONE)
            | (KeyCode::End, KeyModifiers::NONE)
            | (KeyCode::Char('b'), KeyModifiers::CTRL)
            | (KeyCode::Char('f'), KeyModifiers::CTRL)
            | (KeyCode::Char('a'), KeyModifiers::CTRL)
            | (KeyCode::Char('e'), KeyModifiers::CTRL) => {
                self.forward_to_pane(key, mods);
            }
            (KeyCode::Backspace, KeyModifiers::NONE) | (KeyCode::Delete, KeyModifiers::NONE) => {
                // Always forward to the shell so the echoed character gets
                // rubbed out of the command line. If the filter was already
                // empty, close the popup — the user is backing out past the
                // trigger (e.g. erasing the space after `cd`).
                let was_empty = self.selection.borrow().is_empty();
                if !was_empty {
                    self.filter_pop();
                    self.updated_input();
                }
                if was_empty {
                    term_window.cancel_modal();
                }
                self.forward_to_pane(key, mods);
                if was_empty {
                    return Ok(true);
                }
            }
            (KeyCode::Char('u'), KeyModifiers::CTRL) => {
                // Rub out every echoed filter char from the shell, then clear
                // the popup's filter state.
                let n = self.selection.borrow().chars().count();
                self.send_backspaces(n);
                self.filter_clear();
                self.updated_input();
            }
            (KeyCode::Char(c), KeyModifiers::NONE) | (KeyCode::Char(c), KeyModifiers::SHIFT) => {
                self.filter_append(c);
                self.updated_input();
                // Echo the typed char into the shell so the command line
                // stays in sync with the filter text.
                self.forward_to_pane(key, mods);
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if let Some(value) = self.selected_value() {
                    // Erase any filter text we echoed into the shell, then
                    // paste the chosen value. The trigger Space was already
                    // re-sent at activation time, so no leading space here.
                    let n = self.selection.borrow().chars().count();
                    self.send_backspaces(n);
                    if let Some(pane) = mux::Mux::get().get_pane(self.source_pane_id) {
                        let _ = pane.send_paste(&super::shell_quote(&value));
                    }
                }
                term_window.cancel_modal();
                return Ok(true);
            }
            _ => return Ok(false),
        }
        term_window.invalidate_modal();
        Ok(true)
    }

    fn computed_element(
        &self,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<Ref<'_, [ComputedElement]>> {
        let selection = self.selection.borrow().clone();

        let font = term_window
            .fonts
            .command_palette_font()
            .expect("to resolve command palette font");
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());

        // 60% of window height, minus the header and some breathing room.
        let max_rows_on_screen = ((term_window.dimensions.pixel_height * 6 / 10)
            / metrics.cell_size.height as usize)
            .saturating_sub(3)
            .max(1);
        *self.max_rows_on_screen.borrow_mut() = max_rows_on_screen;

        {
            let mut results = self.matches.borrow_mut();
            let rebuild = results
                .as_ref()
                .map(|m| m.selection != selection)
                .unwrap_or(true);
            if rebuild {
                let entries = self.entries.borrow();
                results.replace(MatchResults {
                    selection: selection.clone(),
                    matches: compute_matches(&selection, &entries),
                });
                // Keep selected_row in bounds after re-score.
                let new_len = results.as_ref().map(|m| m.matches.len()).unwrap_or(0);
                let mut row = self.selected_row.borrow_mut();
                if *row >= new_len {
                    *row = new_len.saturating_sub(1).min(new_len);
                }
            }
        }

        if self.element.borrow().is_none() {
            let matches_ref = self.matches.borrow();
            let matches = matches_ref.as_ref().unwrap();
            let element = self.compute(term_window, matches, max_rows_on_screen)?;
            drop(matches_ref);
            self.element.borrow_mut().replace(element);
        }

        Ok(Ref::map(self.element.borrow(), |v| {
            v.as_ref().unwrap().as_slice()
        }))
    }

    fn reconfigure(&self, _term_window: &mut TermWindow) {
        self.element.borrow_mut().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_picker(filter: &str) -> PopupPicker {
        let picker = PopupPicker::new_loading(0, 0, 0);
        *picker.selection.borrow_mut() = filter.to_string();
        picker
    }

    #[test]
    fn append_adds_to_end() {
        let p = mk_picker("foo");
        p.filter_append('X');
        assert_eq!(*p.selection.borrow(), "fooX");
    }

    #[test]
    fn backspace_pops_last_char() {
        let p = mk_picker("hello");
        p.filter_pop();
        assert_eq!(*p.selection.borrow(), "hell");
        p.filter_pop();
        assert_eq!(*p.selection.borrow(), "hel");
    }

    #[test]
    fn pop_on_empty_is_noop() {
        let p = mk_picker("");
        p.filter_pop();
        assert_eq!(*p.selection.borrow(), "");
    }

    #[test]
    fn ctrl_u_clears_filter() {
        let p = mk_picker("abcdef");
        p.filter_clear();
        assert_eq!(*p.selection.borrow(), "");
    }

    /// Multi-byte chars: `é` is 2 bytes. `String::pop` is char-aware so a
    /// single backspace removes the whole `é`, not a stray byte.
    #[test]
    fn pop_handles_multibyte_chars() {
        let p = mk_picker("café");
        assert_eq!(p.selection.borrow().len(), 5);
        p.filter_pop();
        assert_eq!(*p.selection.borrow(), "caf");
    }

    #[test]
    fn enter_resolves_selected_entry() {
        let entries = vec![
            PickerEntry {
                label: "a".into(),
                value: "/a".into(),
            },
            PickerEntry {
                label: "b".into(),
                value: "/b".into(),
            },
            PickerEntry {
                label: "c".into(),
                value: "/c".into(),
            },
        ];
        let picker = PopupPicker::new_loading(0, 0, 0);
        picker.set_entries_loaded(entries);
        // Seed an all-pass match list (what `compute_matches("", …)` returns
        // for an empty filter). `set_entries_loaded` clears any cached
        // matches, so we assign afterwards.
        *picker.matches.borrow_mut() = Some(MatchResults {
            selection: String::new(),
            matches: vec![0, 1, 2],
        });
        assert_eq!(picker.selected_value().as_deref(), Some("/a"));
        picker.move_down();
        assert_eq!(picker.selected_value().as_deref(), Some("/b"));
        picker.move_down();
        assert_eq!(picker.selected_value().as_deref(), Some("/c"));
        // Down at the last row saturates.
        picker.move_down();
        assert_eq!(picker.selected_value().as_deref(), Some("/c"));
    }

    #[test]
    fn selected_value_is_none_when_no_matches() {
        let picker = PopupPicker::new_loading(0, 0, 0);
        *picker.matches.borrow_mut() = Some(MatchResults {
            selection: String::new(),
            matches: vec![],
        });
        assert!(picker.selected_value().is_none());
    }
}
