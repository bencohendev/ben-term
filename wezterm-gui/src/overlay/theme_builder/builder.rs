//! Interactive theme builder Modal.
//!
//! Edits the currently-active color scheme's `[semantic_zones]` and
//! `[colors]` sections. File on disk is the source of truth — every
//! accepted edit writes the TOML, and the existing hot-reload watcher
//! (see `config/src/lib.rs` `ConfigInner::reload`) re-renders the
//! terminal within ~200 ms. No in-memory dirty state, no explicit
//! "save" step.
//!
//! Structurally parallel to `PopupPicker` (shared Modal trait, zindex 100
//! paint integration) but deliberately without shared code, so this module
//! can diverge as the builder grows (future: tab bar colors, etc.)
//! without risking regressions in the shell-command pickers.
//!
//! Milestone 4b scope:
//!  - Semantic-zone rows (4a) + Core palette (fg, bg, cursor, selection) +
//!    ANSI 8 + Brights 8
//!  - Hex text input per row; Space opens an HSV color picker (see
//!    `picker.rs`) for richer editing
//!  - Vertical scrolling when row count exceeds the window

use super::file_ops::{
    export_target_path, export_theme_to_file, resolve_scheme_file, write_color_field, ColorField,
    CoreField, ZoneField,
};
use super::picker::{render_picker, PickerState};
use super::preview;
use crate::termwindow::box_model::{
    BorderColor, BoxDimension, ComputedElement, Corners, DisplayType, Element, ElementColors,
    ElementContent, InheritableColor, LayoutContext, SizedPoly,
};
use crate::termwindow::modal::Modal;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::TermWindow;
use crate::utilsprites::RenderMetrics;
use config::{Dimension, DimensionContext, Palette, RgbaColor, SemanticZoneColors, Theme};
use std::cell::{Ref, RefCell};
use std::convert::TryFrom;
use std::ops::Deref;
use std::path::PathBuf;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent};
use window::color::LinearRgba;

/// One row in the builder. `Section` rows render as non-selectable
/// headers; the selection loop skips over them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Section(&'static str),
    Field(ColorField),
    /// Only present for built-in schemes that don't yet have a file on disk.
    /// Selecting it writes the palette to disk and flips the modal into
    /// normal editing mode.
    Export,
}

impl RowKind {
    fn is_selectable(self) -> bool {
        !matches!(self, RowKind::Section(_))
    }
}

pub struct ThemeBuilder {
    scheme_name: String,
    /// `None` for a built-in scheme. Flips to `Some` after Export.
    file_path: RefCell<Option<PathBuf>>,
    /// Full palette state — kept in sync with the file after every commit
    /// so renders show just-saved values immediately, and ANSI/brights
    /// writes have accurate seed values for the other 7 slots when the
    /// file is missing the array.
    palette: RefCell<Palette>,
    /// Current zone colors. Same sync model as `palette`.
    zones: RefCell<SemanticZoneColors>,
    /// Snapshot of the initial palette used by the built-in Export flow.
    /// Not kept in sync with edits — Export only fires before any writes.
    palette_snapshot: Palette,
    /// Which color-scheme dirs to search — captured at construction so the
    /// modal has a stable view even if config reloads mid-edit.
    scheme_dirs: Vec<PathBuf>,

    selected_row: RefCell<usize>,
    /// First row visible in the scroll window. Updated by move_up/down.
    top_row: RefCell<usize>,
    /// Number of rows that fit in the current modal viewport. Set during
    /// compute(); used by scroll math.
    max_rows_on_screen: RefCell<usize>,

    /// `Some(buf)` when the user is typing a hex value into the selected
    /// row; `None` in browse mode.
    editing: RefCell<Option<String>>,
    /// `Some(state)` while the HSV picker is open over an editing row.
    /// Mutually exclusive with not-editing: if this is `Some`, `editing`
    /// is also `Some` (the buffer is kept in sync with the picker's color).
    picker: RefCell<Option<PickerState>>,
    /// Transient error message shown below the rows (e.g. "invalid hex").
    /// Cleared on the next keystroke in edit mode.
    error: RefCell<Option<String>>,

    element: RefCell<Option<Vec<ComputedElement>>>,
}

impl ThemeBuilder {
    /// Construct from the live `TermWindow`. Resolves the active scheme,
    /// finds its on-disk file (if any), and seeds the in-memory state.
    pub fn new(term_window: &TermWindow) -> anyhow::Result<Self> {
        let scheme_name = term_window
            .config
            .color_scheme
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no active color_scheme configured"))?;

        let theme: Theme = term_window
            .config
            .resolve_color_scheme()
            .ok_or_else(|| anyhow::anyhow!("color_scheme `{}` is not defined", scheme_name))?;

        let scheme_dirs = term_window.config.compute_color_scheme_dirs();
        let file_path = resolve_scheme_file(&scheme_name, &scheme_dirs);

        let selected_row = initial_selected_row(file_path.is_some());

        Ok(Self {
            scheme_name,
            file_path: RefCell::new(file_path),
            palette: RefCell::new(theme.palette.clone()),
            zones: RefCell::new(theme.semantic_zones.clone()),
            palette_snapshot: theme.palette,
            scheme_dirs,
            selected_row: RefCell::new(selected_row),
            top_row: RefCell::new(0),
            max_rows_on_screen: RefCell::new(0),
            editing: RefCell::new(None),
            picker: RefCell::new(None),
            error: RefCell::new(None),
            element: RefCell::new(None),
        })
    }

    /// Full ordered row list for the current modal state. Sections are
    /// interleaved so they render as group headers; selection loop skips
    /// over them.
    fn rows(&self) -> Vec<RowKind> {
        if self.file_path.borrow().is_none() {
            return vec![RowKind::Export];
        }

        let mut rows = Vec::new();
        rows.push(RowKind::Section("Semantic zones"));
        for z in ZoneField::all() {
            rows.push(RowKind::Field(ColorField::Zone(z)));
        }
        rows.push(RowKind::Section("Core palette"));
        for c in CoreField::all() {
            rows.push(RowKind::Field(ColorField::Core(c)));
        }
        rows.push(RowKind::Section("ANSI"));
        for i in 0u8..8 {
            rows.push(RowKind::Field(ColorField::Ansi(i)));
        }
        rows.push(RowKind::Section("Bright"));
        for i in 0u8..8 {
            rows.push(RowKind::Field(ColorField::Bright(i)));
        }
        rows
    }

    fn current_value(&self, field: ColorField) -> Option<RgbaColor> {
        match field {
            ColorField::Zone(z) => {
                let zones = self.zones.borrow();
                match z {
                    ZoneField::Input => zones.input_bg,
                    ZoneField::Output => zones.output_bg,
                    ZoneField::Prompt => zones.prompt_bg,
                    ZoneField::Error => zones.error_bg,
                }
            }
            ColorField::Core(c) => {
                let p = self.palette.borrow();
                match c {
                    CoreField::Foreground => p.foreground,
                    CoreField::Background => p.background,
                    CoreField::CursorBg => p.cursor_bg,
                    CoreField::CursorFg => p.cursor_fg,
                    CoreField::SelectionBg => p.selection_bg,
                    CoreField::SelectionFg => p.selection_fg,
                }
            }
            ColorField::Ansi(i) => self
                .palette
                .borrow()
                .ansi
                .and_then(|a| a.get(i as usize).copied()),
            ColorField::Bright(i) => self
                .palette
                .borrow()
                .brights
                .and_then(|a| a.get(i as usize).copied()),
        }
    }

    fn set_value(&self, field: ColorField, color: Option<RgbaColor>) {
        match field {
            ColorField::Zone(z) => {
                let mut zones = self.zones.borrow_mut();
                match z {
                    ZoneField::Input => zones.input_bg = color,
                    ZoneField::Output => zones.output_bg = color,
                    ZoneField::Prompt => zones.prompt_bg = color,
                    ZoneField::Error => zones.error_bg = color,
                }
            }
            ColorField::Core(c) => {
                let mut p = self.palette.borrow_mut();
                match c {
                    CoreField::Foreground => p.foreground = color,
                    CoreField::Background => p.background = color,
                    CoreField::CursorBg => p.cursor_bg = color,
                    CoreField::CursorFg => p.cursor_fg = color,
                    CoreField::SelectionBg => p.selection_bg = color,
                    CoreField::SelectionFg => p.selection_fg = color,
                }
            }
            ColorField::Ansi(i) => {
                if let Some(c) = color {
                    let mut p = self.palette.borrow_mut();
                    let mut arr = p.ansi.unwrap_or([RgbaColor::default(); 8]);
                    if (i as usize) < arr.len() {
                        arr[i as usize] = c;
                    }
                    p.ansi = Some(arr);
                }
            }
            ColorField::Bright(i) => {
                if let Some(c) = color {
                    let mut p = self.palette.borrow_mut();
                    let mut arr = p.brights.unwrap_or([RgbaColor::default(); 8]);
                    if (i as usize) < arr.len() {
                        arr[i as usize] = c;
                    }
                    p.brights = Some(arr);
                }
            }
        }
    }

    /// Snapshot the current 8-slot ANSI/brights array as hex strings for
    /// use as `seed_array` when the file lacks the relevant array. Returns
    /// `None` if the palette has no array declared (caller passes None and
    /// the file writer falls back to `#000000` placeholders).
    fn array_seed(&self, field: ColorField) -> Option<[String; 8]> {
        let p = self.palette.borrow();
        let arr = match field {
            ColorField::Ansi(_) => p.ansi?,
            ColorField::Bright(_) => p.brights?,
            _ => return None,
        };
        let mut out: [String; 8] = Default::default();
        for (i, c) in arr.iter().enumerate() {
            out[i] = String::from(c);
        }
        Some(out)
    }

    /// Palette + zones the live preview should paint with: committed
    /// in-memory state with the current edit buffer overlaid onto exactly
    /// the field being edited, iff the buffer parses as a valid color.
    /// Partial / invalid buffers silently fall back to committed so the
    /// preview never flashes garbage mid-type.
    fn preview_colors(&self) -> (Palette, SemanticZoneColors) {
        let mut palette = self.palette.borrow().clone();
        let mut zones = self.zones.borrow().clone();

        let buf = match self.editing.borrow().clone() {
            Some(b) => b,
            None => return (palette, zones),
        };
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            return (palette, zones);
        }
        let color = match RgbaColor::try_from(trimmed.to_string()) {
            Ok(c) => c,
            Err(_) => return (palette, zones),
        };

        let rows = self.rows();
        let idx = *self.selected_row.borrow();
        if let Some(RowKind::Field(field)) = rows.get(idx).copied() {
            apply_field_override(&mut palette, &mut zones, field, color);
        }
        (palette, zones)
    }

    fn move_up(&self) {
        let rows = self.rows();
        let cur = *self.selected_row.borrow();
        if let Some(next) = prev_selectable(&rows, cur) {
            *self.selected_row.borrow_mut() = next;
            self.scroll_into_view();
        }
    }

    fn move_down(&self) {
        let rows = self.rows();
        let cur = *self.selected_row.borrow();
        if let Some(next) = next_selectable(&rows, cur) {
            *self.selected_row.borrow_mut() = next;
            self.scroll_into_view();
        }
    }

    /// Keep the selected row visible within [top_row, top_row + max_rows).
    /// When moving forward, prefer to show the selected row's section
    /// header too so context is preserved.
    fn scroll_into_view(&self) {
        let max_rows = *self.max_rows_on_screen.borrow();
        if max_rows == 0 {
            return;
        }
        let sel = *self.selected_row.borrow();
        let rows = self.rows();
        let mut top = *self.top_row.borrow();

        let anchor = preceding_section_index(&rows, sel).unwrap_or(sel);

        if anchor < top {
            top = anchor;
        } else if sel >= top + max_rows {
            top = sel + 1 - max_rows;
        }

        *self.top_row.borrow_mut() = top;
    }

    fn invalidate(&self) {
        self.element.borrow_mut().take();
    }

    /// Enter edit mode for the currently selected row. Seeds the input
    /// buffer with the current hex value so the user can tweak rather than
    /// retype from scratch. No-op on Section/Export rows.
    fn begin_edit(&self) {
        let rows = self.rows();
        let idx = *self.selected_row.borrow();
        let field = match rows.get(idx).copied() {
            Some(RowKind::Field(f)) => f,
            _ => return,
        };
        let initial = self
            .current_value(field)
            .map(|c| String::from(&c))
            .unwrap_or_default();
        *self.editing.borrow_mut() = Some(initial);
        self.error.borrow_mut().take();
        self.invalidate();
    }

    fn cancel_edit(&self) {
        self.editing.borrow_mut().take();
        self.picker.borrow_mut().take();
        self.error.borrow_mut().take();
        self.invalidate();
    }

    /// Open the HSV picker over the row currently being edited. Seeds
    /// HSV from the edit buffer (if it parses) or the field's current
    /// value (if any). No-op if we're not already in edit mode.
    fn begin_picker(&self) {
        let buffer = match self.editing.borrow().clone() {
            Some(b) => b,
            None => return,
        };

        let rows = self.rows();
        let idx = *self.selected_row.borrow();
        let field = match rows.get(idx).copied() {
            Some(RowKind::Field(f)) => f,
            _ => return,
        };

        let fallback = self.current_value(field).map(|c| String::from(&c));
        let state = PickerState::from_buffer(&buffer, fallback.as_deref());

        // Seed the edit buffer with the picker's starting hex so the row
        // value mirrors the picker from the moment it opens.
        *self.editing.borrow_mut() = Some(state.to_hex());
        *self.picker.borrow_mut() = Some(state);
        self.error.borrow_mut().take();
        self.invalidate();
    }

    /// Esc from the picker: close it and restore the pre-picker buffer.
    /// Stays in edit mode so the user can continue typing hex.
    fn cancel_picker(&self) {
        let saved = {
            let mut picker = self.picker.borrow_mut();
            picker.take().map(|s| s.saved_buffer)
        };
        if let Some(buf) = saved {
            *self.editing.borrow_mut() = Some(buf);
        }
        self.error.borrow_mut().take();
        self.invalidate();
    }

    /// Sync the edit buffer with the picker's current color. Called after
    /// each keyboard nudge so the hex readout in the compact view
    /// reflects the live picker state.
    fn update_picker_buffer(&self) {
        if let Some(state) = self.picker.borrow().as_ref() {
            *self.editing.borrow_mut() = Some(state.to_hex());
        }
        self.invalidate();
    }

    /// Commit the current edit buffer: parse the hex, write it to the
    /// file, update in-memory state. On parse failure, show an error and
    /// leave edit mode active so the user can fix the value.
    fn commit_edit(&self) {
        let buf = match self.editing.borrow().clone() {
            Some(b) => b,
            None => return,
        };

        let rows = self.rows();
        let idx = *self.selected_row.borrow();
        let field = match rows.get(idx).copied() {
            Some(RowKind::Field(f)) => f,
            _ => return,
        };

        let trimmed = buf.trim();
        let (parsed, hex): (Option<RgbaColor>, Option<String>) = if trimmed.is_empty() {
            if !field.allows_unset() {
                *self.error.borrow_mut() =
                    Some("cannot unset this field; enter a hex color".into());
                self.invalidate();
                return;
            }
            (None, None)
        } else {
            match RgbaColor::try_from(trimmed.to_string()) {
                Ok(c) => {
                    let hex = String::from(&c);
                    (Some(c), Some(hex))
                }
                Err(_) => {
                    *self.error.borrow_mut() = Some(format!("invalid color: {}", trimmed));
                    self.invalidate();
                    return;
                }
            }
        };

        let path = match self.file_path.borrow().clone() {
            Some(p) => p,
            None => {
                *self.error.borrow_mut() = Some("no file to write — export first".into());
                self.invalidate();
                return;
            }
        };

        let seed = self.array_seed(field);
        if let Err(err) = write_color_field(&path, field, hex.as_deref(), seed.as_ref()) {
            *self.error.borrow_mut() = Some(format!("write failed: {}", err));
            self.invalidate();
            return;
        }

        self.set_value(field, parsed);
        self.editing.borrow_mut().take();
        self.picker.borrow_mut().take();
        self.error.borrow_mut().take();
        self.invalidate();
    }

    /// Built-in schemes: dump the palette + current zones to a file under
    /// the user's config `colors/` dir, then flip `file_path` so the modal
    /// transitions into normal editing mode on its next render.
    fn do_export(&self) {
        let theme = Theme {
            palette: self.palette_snapshot.clone(),
            semantic_zones: self.zones.borrow().clone(),
        };

        let path = match export_target_path(&self.scheme_name, &self.scheme_dirs) {
            Ok(p) => p,
            Err(err) => {
                *self.error.borrow_mut() = Some(format!("export failed: {}", err));
                self.invalidate();
                return;
            }
        };

        if path.exists() {
            *self.error.borrow_mut() = Some(format!(
                "refusing to overwrite existing {}",
                path.display()
            ));
            self.invalidate();
            return;
        }

        if let Err(err) = export_theme_to_file(&path, &theme, &self.scheme_name) {
            *self.error.borrow_mut() = Some(format!("export failed: {}", err));
            self.invalidate();
            return;
        }

        *self.file_path.borrow_mut() = Some(path);
        *self.selected_row.borrow_mut() = initial_selected_row(true);
        *self.top_row.borrow_mut() = 0;
        self.error.borrow_mut().take();
        self.invalidate();
    }
}

/// Apply a single-color edit on top of a palette+zones snapshot. Mirrors
/// the arms in `ThemeBuilder::set_value` but operates on owned state so
/// the preview can overlay without mutating committed buffers.
fn apply_field_override(
    palette: &mut Palette,
    zones: &mut SemanticZoneColors,
    field: ColorField,
    color: RgbaColor,
) {
    match field {
        ColorField::Zone(z) => match z {
            ZoneField::Input => zones.input_bg = Some(color),
            ZoneField::Output => zones.output_bg = Some(color),
            ZoneField::Prompt => zones.prompt_bg = Some(color),
            ZoneField::Error => zones.error_bg = Some(color),
        },
        ColorField::Core(c) => match c {
            CoreField::Foreground => palette.foreground = Some(color),
            CoreField::Background => palette.background = Some(color),
            CoreField::CursorBg => palette.cursor_bg = Some(color),
            CoreField::CursorFg => palette.cursor_fg = Some(color),
            CoreField::SelectionBg => palette.selection_bg = Some(color),
            CoreField::SelectionFg => palette.selection_fg = Some(color),
        },
        ColorField::Ansi(i) => {
            let mut arr = palette.ansi.unwrap_or([RgbaColor::default(); 8]);
            if (i as usize) < arr.len() {
                arr[i as usize] = color;
            }
            palette.ansi = Some(arr);
        }
        ColorField::Bright(i) => {
            let mut arr = palette.brights.unwrap_or([RgbaColor::default(); 8]);
            if (i as usize) < arr.len() {
                arr[i as usize] = color;
            }
            palette.brights = Some(arr);
        }
    }
}

fn initial_selected_row(file_backed: bool) -> usize {
    if file_backed {
        // Row 0 is "Semantic zones" section header; first field is row 1.
        1
    } else {
        0
    }
}

fn next_selectable(rows: &[RowKind], start: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, r)| r.is_selectable())
        .map(|(i, _)| i)
}

fn prev_selectable(rows: &[RowKind], start: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .take(start)
        .rev()
        .find(|(_, r)| r.is_selectable())
        .map(|(i, _)| i)
}

fn preceding_section_index(rows: &[RowKind], idx: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .take(idx)
        .rev()
        .find(|(_, r)| matches!(r, RowKind::Section(_)))
        .map(|(i, _)| i)
}

fn render_row(
    font: &std::rc::Rc<wezterm_font::LoadedFont>,
    label: String,
    value: String,
    swatch: Option<LinearRgba>,
    selected: bool,
    solid_bg_color: &InheritableColor,
    solid_fg_color: &InheritableColor,
) -> Element {
    let (bg, text) = if selected {
        (solid_fg_color.clone(), solid_bg_color.clone())
    } else {
        (LinearRgba::TRANSPARENT.into(), solid_fg_color.clone())
    };

    let left = Element::new(
        font,
        ElementContent::Text(format!("  {:<22}{:<12}", label, value)),
    )
    .colors(ElementColors {
        border: BorderColor::default(),
        bg: LinearRgba::TRANSPARENT.into(),
        text: text.clone(),
    });

    let mut children = vec![left];

    if let Some(color) = swatch {
        children.push(
            Element::new(font, ElementContent::Text("      ".to_string())).colors(ElementColors {
                border: BorderColor::default(),
                bg: color.into(),
                text: text.clone(),
            }),
        );
    }

    Element::new(font, ElementContent::Children(children))
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
        .display(DisplayType::Block)
}

fn render_section(
    font: &std::rc::Rc<wezterm_font::LoadedFont>,
    label: &str,
    solid_fg_color: &InheritableColor,
) -> Element {
    Element::new(font, ElementContent::Text(format!("— {} —", label)))
        .colors(ElementColors {
            border: BorderColor::default(),
            bg: LinearRgba::TRANSPARENT.into(),
            text: solid_fg_color.clone(),
        })
        .padding(BoxDimension {
            left: Dimension::Cells(0.25),
            right: Dimension::Cells(0.25),
            top: Dimension::Cells(0.3),
            bottom: Dimension::Cells(0.1),
        })
        .display(DisplayType::Block)
}

#[allow(clippy::too_many_arguments)]
fn finalize_modal(
    term_window: &mut TermWindow,
    font: &std::rc::Rc<wezterm_font::LoadedFont>,
    metrics: &RenderMetrics,
    elements: Vec<Element>,
    solid_bg_color: InheritableColor,
    solid_fg_color: InheritableColor,
    padding_left: f32,
    grid_origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    size: wezterm_term::TerminalSize,
    dimensions: crate::Dimensions,
    desired_width_cells: usize,
) -> anyhow::Result<Vec<ComputedElement>> {
    let avail_pixel_width = size.cols as f32 * cell_w;
    let desired_width_cells = desired_width_cells.min(size.cols.max(1));
    let desired_pixel_width = desired_width_cells as f32 * cell_w;

    let element = Element::new(font, ElementContent::Children(elements))
        .colors(ElementColors {
            border: BorderColor::new(term_window.config.command_palette_fg_color.to_linear()),
            bg: solid_bg_color,
            text: solid_fg_color,
        })
        .padding(BoxDimension {
            left: Dimension::Cells(0.5),
            right: Dimension::Cells(0.5),
            top: Dimension::Cells(0.5),
            bottom: Dimension::Cells(0.5),
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

    let anchor_x = padding_left + ((avail_pixel_width - desired_pixel_width) / 2.).max(0.);
    let anchor_y = grid_origin_y + cell_h * 2.;

    let computed = term_window.compute_element(
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
            metrics,
            gl_state: term_window.render_state.as_ref().unwrap(),
            zindex: 100,
        },
        &element,
    )?;

    Ok(vec![computed])
}

fn render_scroll_indicator(
    font: &std::rc::Rc<wezterm_font::LoadedFont>,
    label: &str,
    solid_fg_color: &InheritableColor,
) -> Element {
    Element::new(font, ElementContent::Text(format!("  {}", label)))
        .colors(ElementColors {
            border: BorderColor::default(),
            bg: LinearRgba::TRANSPARENT.into(),
            text: solid_fg_color.clone(),
        })
        .padding(BoxDimension {
            left: Dimension::Cells(0.25),
            right: Dimension::Cells(0.25),
            top: Dimension::Cells(0.),
            bottom: Dimension::Cells(0.),
        })
        .display(DisplayType::Block)
}

impl ThemeBuilder {
    fn compute(&self, term_window: &mut TermWindow) -> anyhow::Result<Vec<ComputedElement>> {
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

        // Compute viewport capacity first so move_up/down can scroll
        // correctly between keystrokes. Reserve a few rows for title,
        // footer, and surrounding padding.
        let dimensions = term_window.dimensions;
        let size = term_window.terminal_size();
        let cell_w = term_window.render_metrics.cell_size.width as f32;
        let cell_h = term_window.render_metrics.cell_size.height as f32;
        let avail_pixel_height = (dimensions.pixel_height as f32 * 0.8).max(cell_h * 8.);
        let row_height = cell_h;
        let reserved_rows = 4; // title + footer + padding
        let max_rows = (((avail_pixel_height / row_height) as i32) - reserved_rows).max(4) as usize;
        *self.max_rows_on_screen.borrow_mut() = max_rows;
        self.scroll_into_view();

        let selected = *self.selected_row.borrow();
        let top_row = *self.top_row.borrow();
        let editing_buf = self.editing.borrow().clone();
        let picker_state = self.picker.borrow().clone();
        let rows_def = self.rows();

        // Title.
        let title_element = Element::new(
            &font,
            ElementContent::Text(format!("Theme builder — {}", self.scheme_name)),
        )
        .colors(ElementColors {
            border: BorderColor::default(),
            bg: LinearRgba::TRANSPARENT.into(),
            text: solid_fg_color.clone(),
        })
        .padding(BoxDimension {
            left: Dimension::Cells(0.25),
            right: Dimension::Cells(0.25),
            top: Dimension::Cells(0.),
            bottom: Dimension::Cells(0.5),
        })
        .display(DisplayType::Block);

        // Body: picker mode shows only the editing row + picker; normal
        // mode shows the scrolled row list with scroll indicators.
        let mut body_elements: Vec<Element> = Vec::new();
        if let Some(state) = picker_state.as_ref() {
            if let Some(RowKind::Field(field)) = rows_def.get(selected).copied() {
                let current = self.current_value(field);
                let swatch = current.map(|c| c.deref().to_linear());
                let value_text = format!("{}_", editing_buf.as_deref().unwrap_or(""));
                body_elements.push(render_row(
                    &font,
                    field.label(),
                    value_text,
                    swatch,
                    true,
                    &solid_bg_color,
                    &solid_fg_color,
                ));
            }
            body_elements.push(render_picker(state, &font, &solid_fg_color));
        } else {
            if top_row > 0 {
                body_elements.push(render_scroll_indicator(
                    &font,
                    "↑ more above",
                    &solid_fg_color,
                ));
            }
            for (idx, row) in rows_def.iter().enumerate().skip(top_row).take(max_rows) {
                let is_selected = idx == selected;
                match *row {
                    RowKind::Section(label) => {
                        body_elements.push(render_section(&font, label, &solid_fg_color));
                    }
                    RowKind::Field(field) => {
                        let is_editing = is_selected && editing_buf.is_some();
                        let current = self.current_value(field);
                        let swatch = current.map(|c| c.deref().to_linear());

                        let value_text = if is_editing {
                            let buf = editing_buf.as_deref().unwrap_or("");
                            format!("{}_", buf)
                        } else {
                            match current {
                                Some(c) => String::from(&c),
                                None => "—".into(),
                            }
                        };

                        body_elements.push(render_row(
                            &font,
                            field.label(),
                            value_text,
                            swatch,
                            is_selected,
                            &solid_bg_color,
                            &solid_fg_color,
                        ));
                    }
                    RowKind::Export => {
                        let label = format!(
                            "Export `{}` to ~/.config/wezterm/colors/{}.toml",
                            self.scheme_name, self.scheme_name
                        );
                        body_elements.push(render_row(
                            &font,
                            label,
                            "↵".into(),
                            None,
                            is_selected,
                            &solid_bg_color,
                            &solid_fg_color,
                        ));
                    }
                }
            }
            if top_row + max_rows < rows_def.len() {
                body_elements.push(render_scroll_indicator(
                    &font,
                    "↓ more below",
                    &solid_fg_color,
                ));
            }
        }

        // Footer: editing hint OR error OR default hint.
        let footer_text = if let Some(err) = self.error.borrow().as_ref() {
            format!("! {}", err)
        } else if picker_state.is_some() {
            "↑↓←→ pick  Shift+←/→ hue  ↵ save  Esc back".into()
        } else if editing_buf.is_some() {
            "Type a hex color (#rrggbb), Enter to save, Esc to cancel, empty to unset".into()
        } else if self.file_path.borrow().is_some() {
            "↑/↓ move  ↵ edit  Esc close".into()
        } else {
            "↵ export  Esc close".into()
        };
        let footer_element = Element::new(&font, ElementContent::Text(footer_text))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: LinearRgba::TRANSPARENT.into(),
                text: solid_fg_color.clone(),
            })
            .padding(BoxDimension {
                left: Dimension::Cells(0.25),
                right: Dimension::Cells(0.25),
                top: Dimension::Cells(0.5),
                bottom: Dimension::Cells(0.),
            })
            .display(DisplayType::Block);

        // Decide whether the preview column fits. Needs body + preview +
        // modal chrome (~6 cells of padding/border) in the visible terminal.
        const BODY_COL_CELLS: usize = 46;
        const CHROME_CELLS: usize = 6;
        let want_preview_width =
            BODY_COL_CELLS + preview::PREVIEW_WIDTH_CELLS + CHROME_CELLS;
        let show_preview = size.cols >= want_preview_width;

        let (elements, desired_width_cells) = if show_preview {
            let (prev_palette, prev_zones) = self.preview_colors();
            let body_col = Element::new(&font, ElementContent::Children(body_elements))
                .display(DisplayType::Inline)
                .min_width(Some(Dimension::Cells(BODY_COL_CELLS as f32)))
                .max_width(Some(Dimension::Cells(BODY_COL_CELLS as f32)));
            let preview_col =
                preview::render_preview(&prev_palette, &prev_zones, &font, &solid_fg_color);
            let two_col = Element::new(
                &font,
                ElementContent::Children(vec![body_col, preview_col]),
            )
            .display(DisplayType::Block);
            (
                vec![title_element, two_col, footer_element],
                want_preview_width,
            )
        } else {
            let mut out = Vec::with_capacity(2 + body_elements.len());
            out.push(title_element);
            out.extend(body_elements);
            out.push(footer_element);
            (out, 60_usize)
        };

        finalize_modal(
            term_window,
            &font,
            &metrics,
            elements,
            solid_bg_color,
            solid_fg_color,
            padding_left,
            grid_origin_y,
            cell_w,
            cell_h,
            size,
            dimensions,
            desired_width_cells,
        )
    }

    fn hit_test(&self, point: (f32, f32)) -> Option<bool> {
        let element = self.element.borrow();
        let first = element.as_ref()?.first()?;
        let pt = euclid::point2(point.0, point.1);
        Some(first.bounds.contains(pt))
    }
}

impl Modal for ThemeBuilder {
    fn mouse_event(&self, _event: MouseEvent, _term_window: &mut TermWindow) -> anyhow::Result<()> {
        Ok(())
    }

    fn mouse_press_pixel(&self, point: (f32, f32), term_window: &TermWindow) -> bool {
        match self.hit_test(point) {
            Some(false) => {
                term_window.cancel_modal();
                true
            }
            _ => false,
        }
    }

    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<bool> {
        let editing = self.editing.borrow().is_some();
        let picking = self.picker.borrow().is_some();

        if picking {
            // Picker-mode keymap: arrows nudge SV, Shift+arrows nudge hue,
            // Enter commits, Esc closes (restores pre-picker buffer).
            match (key, mods) {
                (KeyCode::Escape, KeyModifiers::NONE)
                | (KeyCode::Char('g'), KeyModifiers::CTRL)
                | (KeyCode::Char('c'), KeyModifiers::CTRL) => {
                    self.cancel_picker();
                }
                (KeyCode::Enter, KeyModifiers::NONE) => {
                    self.commit_edit();
                }
                (KeyCode::LeftArrow, KeyModifiers::NONE) => {
                    if let Some(s) = self.picker.borrow_mut().as_mut() {
                        s.nudge_sat(-PickerState::sat_step());
                    }
                    self.update_picker_buffer();
                }
                (KeyCode::RightArrow, KeyModifiers::NONE) => {
                    if let Some(s) = self.picker.borrow_mut().as_mut() {
                        s.nudge_sat(PickerState::sat_step());
                    }
                    self.update_picker_buffer();
                }
                (KeyCode::UpArrow, KeyModifiers::NONE) => {
                    if let Some(s) = self.picker.borrow_mut().as_mut() {
                        s.nudge_val(PickerState::val_step());
                    }
                    self.update_picker_buffer();
                }
                (KeyCode::DownArrow, KeyModifiers::NONE) => {
                    if let Some(s) = self.picker.borrow_mut().as_mut() {
                        s.nudge_val(-PickerState::val_step());
                    }
                    self.update_picker_buffer();
                }
                (KeyCode::LeftArrow, KeyModifiers::SHIFT) => {
                    if let Some(s) = self.picker.borrow_mut().as_mut() {
                        s.nudge_hue(-PickerState::hue_step());
                    }
                    self.update_picker_buffer();
                }
                (KeyCode::RightArrow, KeyModifiers::SHIFT) => {
                    if let Some(s) = self.picker.borrow_mut().as_mut() {
                        s.nudge_hue(PickerState::hue_step());
                    }
                    self.update_picker_buffer();
                }
                _ => return Ok(false),
            }
            term_window.invalidate_modal();
            return Ok(true);
        }

        if editing {
            match (key, mods) {
                (KeyCode::Escape, KeyModifiers::NONE)
                | (KeyCode::Char('g'), KeyModifiers::CTRL)
                | (KeyCode::Char('c'), KeyModifiers::CTRL) => {
                    self.cancel_edit();
                }
                (KeyCode::Enter, KeyModifiers::NONE) => {
                    self.commit_edit();
                }
                // Space opens the HSV picker. Hex never contains spaces,
                // so stealing Space costs nothing.
                (KeyCode::Char(' '), KeyModifiers::NONE) => {
                    self.begin_picker();
                }
                (KeyCode::Backspace, KeyModifiers::NONE) => {
                    if let Some(buf) = self.editing.borrow_mut().as_mut() {
                        buf.pop();
                    }
                    self.error.borrow_mut().take();
                    self.invalidate();
                }
                (KeyCode::Char('u'), KeyModifiers::CTRL) => {
                    if let Some(buf) = self.editing.borrow_mut().as_mut() {
                        buf.clear();
                    }
                    self.error.borrow_mut().take();
                    self.invalidate();
                }
                (KeyCode::Char(c), KeyModifiers::NONE)
                | (KeyCode::Char(c), KeyModifiers::SHIFT) => {
                    if let Some(buf) = self.editing.borrow_mut().as_mut() {
                        buf.push(c);
                    }
                    self.error.borrow_mut().take();
                    self.invalidate();
                }
                _ => return Ok(false),
            }
            term_window.invalidate_modal();
            return Ok(true);
        }

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
            (KeyCode::Enter, KeyModifiers::NONE) => {
                let idx = *self.selected_row.borrow();
                match self.rows().get(idx).copied() {
                    Some(RowKind::Field(_)) => self.begin_edit(),
                    Some(RowKind::Export) => self.do_export(),
                    _ => {}
                }
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
        if self.element.borrow().is_none() {
            let element = self.compute(term_window)?;
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
    use std::path::PathBuf;

    fn builder_with_rows(file_path: Option<PathBuf>) -> ThemeBuilder {
        let selected = initial_selected_row(file_path.is_some());
        ThemeBuilder {
            scheme_name: "test".into(),
            file_path: RefCell::new(file_path),
            palette: RefCell::new(Palette::default()),
            zones: RefCell::new(SemanticZoneColors::default()),
            palette_snapshot: Palette::default(),
            scheme_dirs: vec![],
            selected_row: RefCell::new(selected),
            top_row: RefCell::new(0),
            max_rows_on_screen: RefCell::new(10),
            editing: RefCell::new(None),
            picker: RefCell::new(None),
            error: RefCell::new(None),
            element: RefCell::new(None),
        }
    }

    #[test]
    fn rows_for_file_backed_scheme_contain_all_sections() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        let rows = b.rows();
        // 4 sections + 4 zones + 6 core + 8 ansi + 8 bright = 30 rows
        assert_eq!(rows.len(), 30);
        assert!(matches!(rows[0], RowKind::Section("Semantic zones")));
        assert!(matches!(
            rows[1],
            RowKind::Field(ColorField::Zone(ZoneField::Input))
        ));
    }

    #[test]
    fn rows_for_builtin_scheme_is_export_only() {
        let b = builder_with_rows(None);
        assert_eq!(b.rows().len(), 1);
        assert!(matches!(b.rows()[0], RowKind::Export));
    }

    #[test]
    fn initial_selection_skips_section_header() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        assert_eq!(*b.selected_row.borrow(), 1);
    }

    #[test]
    fn move_down_skips_section_headers() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        // Zones: Input (starts here), Output, Prompt, Error — then section
        // header "Core palette" should be skipped onto Foreground.
        b.move_down();
        b.move_down();
        b.move_down();
        b.move_down();
        let idx = *b.selected_row.borrow();
        let rows = b.rows();
        assert!(matches!(
            rows[idx],
            RowKind::Field(ColorField::Core(CoreField::Foreground))
        ));
    }

    #[test]
    fn move_down_clamps_at_last_selectable() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        for _ in 0..100 {
            b.move_down();
        }
        let rows = b.rows();
        let idx = *b.selected_row.borrow();
        assert!(matches!(rows[idx], RowKind::Field(ColorField::Bright(7))));
    }

    #[test]
    fn move_up_stays_on_first_selectable() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        b.move_up();
        assert_eq!(*b.selected_row.borrow(), 1);
    }

    #[test]
    fn begin_edit_seeds_with_current_zone_value() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        b.set_value(
            ColorField::Zone(ZoneField::Input),
            Some(RgbaColor::from((0x00u8, 0x44, 0xaa))),
        );
        b.begin_edit();
        assert_eq!(b.editing.borrow().as_deref(), Some("#0044aa"));
    }

    #[test]
    fn begin_edit_on_core_row_seeds_current_value() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        let rows = b.rows();
        let fg_idx = rows
            .iter()
            .position(|r| {
                matches!(
                    r,
                    RowKind::Field(ColorField::Core(CoreField::Foreground))
                )
            })
            .unwrap();
        *b.selected_row.borrow_mut() = fg_idx;
        b.set_value(
            ColorField::Core(CoreField::Foreground),
            Some(RgbaColor::from((0xff, 0xee, 0xdd))),
        );
        b.begin_edit();
        assert_eq!(b.editing.borrow().as_deref(), Some("#ffeedd"));
    }

    #[test]
    fn commit_edit_writes_core_field_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.toml");
        std::fs::write(&path, "[colors]\nforeground = \"#000000\"\n").unwrap();

        let b = builder_with_rows(Some(path.clone()));
        let rows = b.rows();
        let fg_idx = rows
            .iter()
            .position(|r| {
                matches!(
                    r,
                    RowKind::Field(ColorField::Core(CoreField::Foreground))
                )
            })
            .unwrap();
        *b.selected_row.borrow_mut() = fg_idx;
        b.begin_edit();
        *b.editing.borrow_mut() = Some("#abcdef".into());
        b.commit_edit();

        assert!(b.editing.borrow().is_none());
        assert_eq!(
            b.current_value(ColorField::Core(CoreField::Foreground))
                .map(|c| String::from(&c)),
            Some("#abcdef".into())
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("#abcdef"));
        assert!(!saved.contains("#000000"));
    }

    #[test]
    fn commit_empty_buffer_unsets_scalar_zone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.toml");
        std::fs::write(&path, "[semantic_zones]\ninput_bg = \"#112233\"\n").unwrap();
        let b = builder_with_rows(Some(path.clone()));
        b.set_value(
            ColorField::Zone(ZoneField::Input),
            Some(RgbaColor::from((0x11u8, 0x22, 0x33))),
        );
        b.begin_edit();
        *b.editing.borrow_mut() = Some("".into());
        b.commit_edit();

        assert!(b
            .current_value(ColorField::Zone(ZoneField::Input))
            .is_none());
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("input_bg"));
    }

    #[test]
    fn commit_empty_buffer_rejects_ansi_unset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.toml");
        std::fs::write(
            &path,
            "[colors]\nansi = [\"#000000\",\"#111111\",\"#222222\",\"#333333\",\"#444444\",\"#555555\",\"#666666\",\"#777777\"]\n",
        ).unwrap();
        let b = builder_with_rows(Some(path.clone()));
        let rows = b.rows();
        let ansi0_idx = rows
            .iter()
            .position(|r| matches!(r, RowKind::Field(ColorField::Ansi(0))))
            .unwrap();
        *b.selected_row.borrow_mut() = ansi0_idx;
        b.begin_edit();
        *b.editing.borrow_mut() = Some("".into());
        b.commit_edit();

        assert!(b.editing.borrow().is_some());
        assert!(b.error.borrow().is_some());
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("#000000"));
    }

    #[test]
    fn commit_invalid_hex_reports_error_and_stays_editing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.toml");
        std::fs::write(&path, "[colors]\nforeground = \"#fff\"\n").unwrap();

        let b = builder_with_rows(Some(path));
        b.begin_edit();
        *b.editing.borrow_mut() = Some("#zzzz".into());
        b.commit_edit();

        assert!(b.editing.borrow().is_some());
        assert!(b.error.borrow().is_some());
        assert!(b
            .current_value(ColorField::Zone(ZoneField::Input))
            .is_none());
    }

    #[test]
    fn commit_edit_updates_ansi_slot_in_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.toml");
        std::fs::write(
            &path,
            "[colors]\nansi = [\"#000000\",\"#111111\",\"#222222\",\"#333333\",\"#444444\",\"#555555\",\"#666666\",\"#777777\"]\n",
        ).unwrap();
        let b = builder_with_rows(Some(path.clone()));
        b.palette.borrow_mut().ansi = Some([
            RgbaColor::from((0x00, 0x00, 0x00)),
            RgbaColor::from((0x11, 0x11, 0x11)),
            RgbaColor::from((0x22, 0x22, 0x22)),
            RgbaColor::from((0x33, 0x33, 0x33)),
            RgbaColor::from((0x44, 0x44, 0x44)),
            RgbaColor::from((0x55, 0x55, 0x55)),
            RgbaColor::from((0x66, 0x66, 0x66)),
            RgbaColor::from((0x77, 0x77, 0x77)),
        ]);
        let rows = b.rows();
        let ansi1_idx = rows
            .iter()
            .position(|r| matches!(r, RowKind::Field(ColorField::Ansi(1))))
            .unwrap();
        *b.selected_row.borrow_mut() = ansi1_idx;
        b.begin_edit();
        *b.editing.borrow_mut() = Some("#ff0000".into());
        b.commit_edit();

        assert!(b.editing.borrow().is_none());
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("#ff0000"));
        assert!(saved.contains("#000000"));
        assert!(saved.contains("#777777"));
    }

    #[test]
    fn scroll_into_view_brings_selection_into_window() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        *b.max_rows_on_screen.borrow_mut() = 5;
        *b.selected_row.borrow_mut() = 15;
        b.scroll_into_view();
        let top = *b.top_row.borrow();
        assert!(top <= 15);
        assert!(15 < top + 5);
    }

    fn select_field(b: &ThemeBuilder, field: ColorField) {
        let rows = b.rows();
        let idx = rows
            .iter()
            .position(|r| matches!(r, RowKind::Field(f) if *f == field))
            .expect("field present");
        *b.selected_row.borrow_mut() = idx;
    }

    #[test]
    fn preview_colors_returns_committed_when_not_editing() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        let committed = RgbaColor::from((0x10u8, 0x20, 0x30));
        b.set_value(ColorField::Zone(ZoneField::Input), Some(committed));
        let (_palette, zones) = b.preview_colors();
        assert_eq!(zones.input_bg, Some(committed));
    }

    #[test]
    fn preview_colors_overlays_valid_hex_on_edited_field() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        b.set_value(
            ColorField::Zone(ZoneField::Input),
            Some(RgbaColor::from((0x10u8, 0x20, 0x30))),
        );
        select_field(&b, ColorField::Zone(ZoneField::Input));
        *b.editing.borrow_mut() = Some("#aabbcc".into());
        let (_palette, zones) = b.preview_colors();
        assert_eq!(zones.input_bg, Some(RgbaColor::from((0xaau8, 0xbb, 0xcc))));
    }

    #[test]
    fn preview_colors_falls_back_to_committed_on_partial_hex() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        let committed = RgbaColor::from((0x10u8, 0x20, 0x30));
        b.set_value(ColorField::Zone(ZoneField::Input), Some(committed));
        select_field(&b, ColorField::Zone(ZoneField::Input));
        *b.editing.borrow_mut() = Some("#aa".into());
        let (_palette, zones) = b.preview_colors();
        assert_eq!(zones.input_bg, Some(committed));
    }

    #[test]
    fn preview_colors_falls_back_to_committed_on_empty_buffer() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        let committed = RgbaColor::from((0x10u8, 0x20, 0x30));
        b.set_value(ColorField::Zone(ZoneField::Input), Some(committed));
        select_field(&b, ColorField::Zone(ZoneField::Input));
        *b.editing.borrow_mut() = Some("".into());
        let (_palette, zones) = b.preview_colors();
        assert_eq!(zones.input_bg, Some(committed));
    }

    #[test]
    fn preview_colors_overlays_core_field() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        select_field(&b, ColorField::Core(CoreField::Foreground));
        *b.editing.borrow_mut() = Some("#112233".into());
        let (palette, _zones) = b.preview_colors();
        assert_eq!(
            palette.foreground,
            Some(RgbaColor::from((0x11u8, 0x22, 0x33)))
        );
    }

    #[test]
    fn preview_colors_overlays_ansi_slot() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        select_field(&b, ColorField::Ansi(1));
        *b.editing.borrow_mut() = Some("#ff0000".into());
        let (palette, _zones) = b.preview_colors();
        let ansi = palette.ansi.expect("ansi array populated by overlay");
        assert_eq!(ansi[1], RgbaColor::from((0xffu8, 0x00, 0x00)));
    }

    #[test]
    fn preview_colors_overlays_bright_slot() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        select_field(&b, ColorField::Bright(3));
        *b.editing.borrow_mut() = Some("#00ff00".into());
        let (palette, _zones) = b.preview_colors();
        let brights = palette.brights.expect("brights array populated by overlay");
        assert_eq!(brights[3], RgbaColor::from((0x00u8, 0xff, 0x00)));
    }

    #[test]
    fn preview_colors_overlays_only_selected_field() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        // Commit a baseline for Output so we can confirm it survives.
        let committed_output = RgbaColor::from((0x40u8, 0x40, 0x40));
        b.set_value(ColorField::Zone(ZoneField::Output), Some(committed_output));
        // Edit Input, not Output.
        select_field(&b, ColorField::Zone(ZoneField::Input));
        *b.editing.borrow_mut() = Some("#abcdef".into());
        let (_palette, zones) = b.preview_colors();
        assert_eq!(
            zones.input_bg,
            Some(RgbaColor::from((0xabu8, 0xcd, 0xef)))
        );
        assert_eq!(zones.output_bg, Some(committed_output));
    }
}
