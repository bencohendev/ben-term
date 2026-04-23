//! Interactive theme builder Modal.
//!
//! Edits the currently-active color scheme's `[semantic_zones]` section.
//! File on disk is the source of truth — every accepted edit writes the
//! TOML, and the existing hot-reload watcher (see `config/src/lib.rs`
//! `ConfigInner::reload`) re-renders the terminal within ~200 ms. No
//! in-memory dirty state, no explicit "save" step.
//!
//! Structurally parallel to `PopupPicker` (shared Modal trait, zindex 100
//! paint integration) but deliberately without shared code, so this module
//! can diverge as the builder grows (4b: full palette) without risking
//! regressions in the shell-command pickers.
//!
//! Milestone 4a scope:
//!  - 3 editable rows (`input_bg`, `output_bg`, `prompt_bg`)
//!  - Hex text input per row
//!  - Built-in schemes get an "Export to file" row instead — once exported,
//!    the modal reopens against the new file and becomes editable.

use super::file_ops::{
    export_target_path, export_theme_to_file, resolve_scheme_file, write_zone_color, ZoneField,
};
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
use config::{Dimension, DimensionContext, RgbaColor, SemanticZoneColors, Theme};
use std::cell::{Ref, RefCell};
use std::convert::TryFrom;
use std::ops::Deref;
use std::path::PathBuf;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent};
use window::color::LinearRgba;

/// One editable row in the builder. For 4a this is always a zone row, but
/// the enum leaves room for 4b additions (foreground/background/ansi/etc.)
/// without reshaping the selection loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Zone(ZoneField),
    /// Only present for built-in schemes that don't yet have a file on disk.
    /// Selecting it writes the palette to disk and flips the modal into
    /// normal editing mode.
    Export,
}

pub struct ThemeBuilder {
    scheme_name: String,
    /// `None` for a built-in scheme. Flips to `Some` after Export.
    file_path: RefCell<Option<PathBuf>>,
    /// Current zone colors. Kept in sync with the file after every write
    /// so the render shows the just-saved values without waiting for the
    /// config reload notification to round-trip.
    zones: RefCell<SemanticZoneColors>,
    /// The palette the modal was opened with, used as the source of truth
    /// when a built-in scheme exports to disk. Zone colors at that moment
    /// also come from `zones`.
    palette_snapshot: config::Palette,
    /// Which color-scheme dirs to search — captured at construction so the
    /// modal has a stable view even if config reloads mid-edit.
    scheme_dirs: Vec<PathBuf>,

    selected_row: RefCell<usize>,
    /// `Some(buf)` when the user is typing a hex value into the selected
    /// row; `None` in browse mode.
    editing: RefCell<Option<String>>,
    /// Transient error message shown below the rows (e.g. "invalid hex").
    /// Cleared on the next keystroke in edit mode.
    error: RefCell<Option<String>>,

    element: RefCell<Option<Vec<ComputedElement>>>,
}

impl ThemeBuilder {
    /// Construct from the live `TermWindow`. Resolves the active scheme,
    /// finds its on-disk file (if any), and seeds the in-memory zone
    /// state.
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

        Ok(Self {
            scheme_name,
            file_path: RefCell::new(file_path),
            zones: RefCell::new(theme.semantic_zones.clone()),
            palette_snapshot: theme.palette,
            scheme_dirs,
            selected_row: RefCell::new(0),
            editing: RefCell::new(None),
            error: RefCell::new(None),
            element: RefCell::new(None),
        })
    }

    fn rows(&self) -> Vec<RowKind> {
        if self.file_path.borrow().is_some() {
            ZoneField::all().iter().copied().map(RowKind::Zone).collect()
        } else {
            vec![RowKind::Export]
        }
    }

    fn current_zone_value(&self, field: ZoneField) -> Option<RgbaColor> {
        let zones = self.zones.borrow();
        match field {
            ZoneField::InputBg => zones.input_bg,
            ZoneField::OutputBg => zones.output_bg,
            ZoneField::PromptBg => zones.prompt_bg,
        }
    }

    fn set_zone_value(&self, field: ZoneField, color: Option<RgbaColor>) {
        let mut zones = self.zones.borrow_mut();
        match field {
            ZoneField::InputBg => zones.input_bg = color,
            ZoneField::OutputBg => zones.output_bg = color,
            ZoneField::PromptBg => zones.prompt_bg = color,
        }
    }

    fn move_up(&self) {
        let mut row = self.selected_row.borrow_mut();
        *row = row.saturating_sub(1);
    }

    fn move_down(&self) {
        let limit = self.rows().len().saturating_sub(1);
        let mut row = self.selected_row.borrow_mut();
        *row = row.saturating_add(1).min(limit);
    }

    fn invalidate(&self) {
        self.element.borrow_mut().take();
    }

    /// Enter edit mode for the currently selected zone row. Seeds the
    /// input buffer with the current hex value so the user can tweak it
    /// rather than retyping from scratch.
    fn begin_edit(&self) {
        let rows = self.rows();
        let idx = *self.selected_row.borrow();
        if let Some(RowKind::Zone(field)) = rows.get(idx).copied() {
            let initial = self
                .current_zone_value(field)
                .map(|c| String::from(&c))
                .unwrap_or_default();
            *self.editing.borrow_mut() = Some(initial);
            self.error.borrow_mut().take();
            self.invalidate();
        }
    }

    fn cancel_edit(&self) {
        self.editing.borrow_mut().take();
        self.error.borrow_mut().take();
        self.invalidate();
    }

    /// Commit the current edit buffer: parse the hex, write it to the
    /// file, update in-memory zone state. On parse failure, show an error
    /// and leave edit mode active so the user can fix the value.
    fn commit_edit(&self) {
        let buf = match self.editing.borrow().clone() {
            Some(b) => b,
            None => return,
        };

        let rows = self.rows();
        let idx = *self.selected_row.borrow();
        let field = match rows.get(idx).copied() {
            Some(RowKind::Zone(f)) => f,
            _ => return,
        };

        // Empty string = unset. Otherwise parse as RgbaColor.
        let trimmed = buf.trim();
        let parsed: Option<RgbaColor> = if trimmed.is_empty() {
            None
        } else {
            match RgbaColor::try_from(trimmed.to_string()) {
                Ok(c) => Some(c),
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

        let hex = parsed.map(|c| String::from(&c));
        if let Err(err) = write_zone_color(&path, field, hex.as_deref()) {
            *self.error.borrow_mut() = Some(format!("write failed: {}", err));
            self.invalidate();
            return;
        }

        self.set_zone_value(field, parsed);
        self.editing.borrow_mut().take();
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
        *self.selected_row.borrow_mut() = 0;
        self.error.borrow_mut().take();
        self.invalidate();
    }
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
                // text color doesn't matter — content is only spaces —
                // but set it to a predictable value anyway.
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

        let selected = *self.selected_row.borrow();
        let editing_buf = self.editing.borrow().clone();
        let rows_def = self.rows();

        let mut elements: Vec<Element> = Vec::new();

        // Title row.
        elements.push(
            Element::new(
                &font,
                ElementContent::Text(format!(
                    "Theme builder — {}",
                    self.scheme_name
                )),
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
            .display(DisplayType::Block),
        );

        // Zone / export rows.
        for (idx, row) in rows_def.iter().enumerate() {
            let is_selected = idx == selected;
            match *row {
                RowKind::Zone(field) => {
                    let is_editing = is_selected && editing_buf.is_some();
                    let current = self.current_zone_value(field);
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

                    elements.push(render_row(
                        &font,
                        field.label().into(),
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
                    elements.push(render_row(
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

        // Footer: editing hint OR error OR default hint.
        let footer_text = if let Some(err) = self.error.borrow().as_ref() {
            format!("! {}", err)
        } else if editing_buf.is_some() {
            "Type a hex color (#rrggbb), Enter to save, Esc to cancel, empty to unset".into()
        } else if self.file_path.borrow().is_some() {
            "↑/↓ move  ↵ edit  Esc close".into()
        } else {
            "↵ export  Esc close".into()
        };
        elements.push(
            Element::new(&font, ElementContent::Text(footer_text))
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
                .display(DisplayType::Block),
        );

        // Sizing: centered, fixed at 60 cols (clamped to available).
        let dimensions = term_window.dimensions;
        let size = term_window.terminal_size();
        let cell_w = term_window.render_metrics.cell_size.width as f32;
        let cell_h = term_window.render_metrics.cell_size.height as f32;
        let avail_pixel_width = size.cols as f32 * cell_w;
        let desired_width_cells = 60_usize.min(size.cols.max(1));
        let desired_pixel_width = desired_width_cells as f32 * cell_w;

        let element = Element::new(&font, ElementContent::Children(elements))
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

        // Center horizontally, pin just below the top chrome vertically.
        let anchor_x =
            padding_left + ((avail_pixel_width - desired_pixel_width) / 2.).max(0.);
        // A little breathing room below the tab bar / OS border.
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
                metrics: &metrics,
                gl_state: term_window.render_state.as_ref().unwrap(),
                zindex: 100,
            },
            &element,
        )?;

        Ok(vec![computed])
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

    /// Click-away dismiss — same contract as PopupPicker. A click inside
    /// the modal is left alone (row-click-to-select is a future polish).
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

        if editing {
            // Edit-mode keymap.
            match (key.clone(), mods) {
                (KeyCode::Escape, KeyModifiers::NONE)
                | (KeyCode::Char('g'), KeyModifiers::CTRL)
                | (KeyCode::Char('c'), KeyModifiers::CTRL) => {
                    self.cancel_edit();
                }
                (KeyCode::Enter, KeyModifiers::NONE) => {
                    self.commit_edit();
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

        // Browse-mode keymap.
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
                    Some(RowKind::Zone(_)) => self.begin_edit(),
                    Some(RowKind::Export) => self.do_export(),
                    None => {}
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
        ThemeBuilder {
            scheme_name: "test".into(),
            file_path: RefCell::new(file_path),
            zones: RefCell::new(SemanticZoneColors::default()),
            palette_snapshot: config::Palette::default(),
            scheme_dirs: vec![],
            selected_row: RefCell::new(0),
            editing: RefCell::new(None),
            error: RefCell::new(None),
            element: RefCell::new(None),
        }
    }

    #[test]
    fn rows_for_file_backed_scheme_are_three_zones() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        assert_eq!(b.rows().len(), 3);
        assert!(matches!(b.rows()[0], RowKind::Zone(ZoneField::InputBg)));
    }

    #[test]
    fn rows_for_builtin_scheme_is_export_only() {
        let b = builder_with_rows(None);
        assert_eq!(b.rows().len(), 1);
        assert!(matches!(b.rows()[0], RowKind::Export));
    }

    #[test]
    fn move_down_clamps_at_last_row() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        for _ in 0..10 {
            b.move_down();
        }
        assert_eq!(*b.selected_row.borrow(), 2);
    }

    #[test]
    fn move_up_clamps_at_zero() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        b.move_up();
        assert_eq!(*b.selected_row.borrow(), 0);
    }

    #[test]
    fn begin_edit_seeds_with_current_value() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        b.set_zone_value(
            ZoneField::InputBg,
            Some(RgbaColor::from((0x00u8, 0x44, 0xaa))),
        );
        b.begin_edit();
        assert_eq!(b.editing.borrow().as_deref(), Some("#0044aa"));
    }

    #[test]
    fn begin_edit_on_unset_zone_seeds_empty() {
        let b = builder_with_rows(Some("/tmp/x.toml".into()));
        b.begin_edit();
        assert_eq!(b.editing.borrow().as_deref(), Some(""));
    }

    #[test]
    fn commit_edit_parses_and_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.toml");
        std::fs::write(&path, "[colors]\nforeground = \"#fff\"\n").unwrap();

        let b = builder_with_rows(Some(path.clone()));
        b.begin_edit();
        *b.editing.borrow_mut() = Some("#112233".into());
        b.commit_edit();

        assert!(b.editing.borrow().is_none());
        assert_eq!(
            b.current_zone_value(ZoneField::InputBg)
                .map(|c| String::from(&c)),
            Some("#112233".into())
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("input_bg"));
        assert!(saved.contains("112233"));
    }

    #[test]
    fn commit_empty_buffer_unsets_zone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.toml");
        std::fs::write(
            &path,
            "[semantic_zones]\ninput_bg = \"#112233\"\n",
        )
        .unwrap();
        let b = builder_with_rows(Some(path.clone()));
        b.set_zone_value(
            ZoneField::InputBg,
            Some(RgbaColor::from((0x11u8, 0x22, 0x33))),
        );
        b.begin_edit();
        *b.editing.borrow_mut() = Some("".into());
        b.commit_edit();

        assert!(b.current_zone_value(ZoneField::InputBg).is_none());
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("input_bg"));
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
        assert!(b.current_zone_value(ZoneField::InputBg).is_none());
    }
}
