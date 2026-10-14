//! Live preview pane for the theme builder.
//!
//! Renders a scripted terminal transcript using the builder's candidate
//! palette + zones. Every themed surface (prompt_bg, input_bg, output_bg,
//! error_bg, cursor, selection, foreground/background, ANSI + brights) is
//! exercised so the user can judge a color's effect on every slot at
//! once.
//!
//! The scene is static — no PTY, no mirror of the user's real scrollback.
//! Each line is a Block row containing Inline text spans so segments can
//! carry independent backgrounds (e.g. prompt_bg + input_bg on one row).

use crate::termwindow::box_model::{
    BorderColor, BoxDimension, DisplayType, Element, ElementColors, ElementContent,
    InheritableColor,
};
use config::{Dimension, Palette, RgbaColor, SemanticZoneColors};
use std::ops::Deref;
use std::rc::Rc;
use wezterm_font::LoadedFont;
use window::color::LinearRgba;

/// Width of the preview column. Picked to fit the longest scripted line
/// (`cat: missing.txt: No such file` = 30 chars) plus the 16-cell ANSI
/// strip (32 chars with 2-char cells), with a little padding.
pub const PREVIEW_WIDTH_CELLS: usize = 40;

/// Fallback ANSI palette used when the theme hasn't declared one. These
/// match WezTerm's built-in defaults closely enough to make the strip
/// look like a real terminal and are never written back to disk.
const DEFAULT_ANSI: [(u8, u8, u8); 8] = [
    (0x00, 0x00, 0x00),
    (0xcc, 0x44, 0x44),
    (0x55, 0xaa, 0x55),
    (0xcc, 0xbb, 0x44),
    (0x44, 0x66, 0xbb),
    (0xaa, 0x44, 0xaa),
    (0x33, 0xbb, 0xbb),
    (0xcc, 0xcc, 0xcc),
];

const DEFAULT_BRIGHTS: [(u8, u8, u8); 8] = [
    (0x55, 0x55, 0x55),
    (0xff, 0x77, 0x77),
    (0x77, 0xdd, 0x77),
    (0xee, 0xdd, 0x77),
    (0x77, 0x99, 0xdd),
    (0xdd, 0x77, 0xdd),
    (0x77, 0xdd, 0xdd),
    (0xff, 0xff, 0xff),
];

const DEFAULT_FG: (u8, u8, u8) = (0xe0, 0xe0, 0xe0);
const DEFAULT_BG: (u8, u8, u8) = (0x1a, 0x1a, 0x1a);
const DEFAULT_SELECTION_BG: (u8, u8, u8) = (0x46, 0x46, 0x64);

struct Span {
    text: String,
    fg: LinearRgba,
    bg: LinearRgba,
}

fn linear(color: Option<RgbaColor>) -> Option<LinearRgba> {
    color.map(|c| c.deref().to_linear())
}

fn hex_linear(rgb: (u8, u8, u8)) -> LinearRgba {
    RgbaColor::from(rgb).deref().to_linear()
}

fn span_element(font: &Rc<LoadedFont>, span: &Span) -> Element {
    Element::new(font, ElementContent::Text(span.text.clone())).colors(ElementColors {
        border: BorderColor::default(),
        bg: span.bg.into(),
        text: span.fg.into(),
    })
}

fn preview_line(font: &Rc<LoadedFont>, spans: Vec<Span>) -> Element {
    let children: Vec<Element> = spans.iter().map(|s| span_element(font, s)).collect();
    Element::new(font, ElementContent::Children(children)).display(DisplayType::Block)
}

/// Build the preview pane as one inline-positioned column Element. Caller
/// places it to the right of the rows/picker column inside a two-column
/// container.
pub fn render_preview(
    palette: &Palette,
    zones: &SemanticZoneColors,
    font: &Rc<LoadedFont>,
    label_fg: &InheritableColor,
) -> Element {
    let fg = linear(palette.foreground).unwrap_or_else(|| hex_linear(DEFAULT_FG));
    let bg = linear(palette.background).unwrap_or_else(|| hex_linear(DEFAULT_BG));

    let prompt_bg = linear(zones.prompt_bg).unwrap_or(bg);
    let input_bg = linear(zones.input_bg).unwrap_or(bg);
    let output_bg = linear(zones.output_bg).unwrap_or(bg);
    // Error falls back to output first so a theme that sets output_bg but
    // not error_bg still looks consistent on the failed-command line,
    // matching the runtime tint precedence in screen_line.rs.
    let error_bg = linear(zones.error_bg).unwrap_or(output_bg);

    // Cursor inversion default: if the theme hasn't set cursor colors,
    // fall back to the fg-on-bg swap a bare terminal would use.
    let cursor_bg = linear(palette.cursor_bg).unwrap_or(fg);
    let cursor_fg = linear(palette.cursor_fg).unwrap_or(bg);
    let selection_bg =
        linear(palette.selection_bg).unwrap_or_else(|| hex_linear(DEFAULT_SELECTION_BG));
    let selection_fg = linear(palette.selection_fg).unwrap_or(fg);

    let ansi_colors: [LinearRgba; 8] = match palette.ansi {
        Some(arr) => {
            let mut out = [fg; 8];
            for (i, c) in arr.iter().enumerate() {
                out[i] = c.deref().to_linear();
            }
            out
        }
        None => DEFAULT_ANSI.map(hex_linear),
    };
    let brights_colors: [LinearRgba; 8] = match palette.brights {
        Some(arr) => {
            let mut out = [fg; 8];
            for (i, c) in arr.iter().enumerate() {
                out[i] = c.deref().to_linear();
            }
            out
        }
        None => DEFAULT_BRIGHTS.map(hex_linear),
    };

    let mut lines: Vec<Element> = Vec::new();

    // "Preview" label — outside the fake terminal, uses the modal's own
    // text color so it reads consistently even against dark palettes.
    lines.push(
        Element::new(font, ElementContent::Text("  Preview".into()))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: LinearRgba::TRANSPARENT.into(),
                text: label_fg.clone(),
            })
            .display(DisplayType::Block)
            .padding(BoxDimension {
                left: Dimension::Cells(0.25),
                right: Dimension::Cells(0.25),
                top: Dimension::Cells(0.),
                bottom: Dimension::Cells(0.25),
            }),
    );

    let prompt = "user ~ % ";
    let prompt_len = prompt.len();

    // Line: simple prompt + successful command.
    let input1 = "ls";
    lines.push(preview_line(
        font,
        vec![
            Span {
                text: prompt.into(),
                fg,
                bg: prompt_bg,
            },
            Span {
                text: pad_to(input1, PREVIEW_WIDTH_CELLS - prompt_len),
                fg,
                bg: input_bg,
            },
        ],
    ));

    // Line: command output.
    let out2 = "README.md  src/  target/";
    lines.push(preview_line(
        font,
        vec![Span {
            text: pad_to(out2, PREVIEW_WIDTH_CELLS),
            fg,
            bg: output_bg,
        }],
    ));

    // Line: prompt + in-progress typing + cursor.
    let input3 = "cat missing.txt";
    let cursor_cell = " ";
    let tail_input3 = PREVIEW_WIDTH_CELLS - prompt_len - input3.len() - cursor_cell.len();
    lines.push(preview_line(
        font,
        vec![
            Span {
                text: prompt.into(),
                fg,
                bg: prompt_bg,
            },
            Span {
                text: input3.into(),
                fg,
                bg: input_bg,
            },
            Span {
                text: cursor_cell.into(),
                fg: cursor_fg,
                bg: cursor_bg,
            },
            Span {
                text: " ".repeat(tail_input3),
                fg,
                bg: input_bg,
            },
        ],
    ));

    // Line: error output (failed-command tint).
    let err4 = "cat: missing.txt: No such file";
    lines.push(preview_line(
        font,
        vec![Span {
            text: pad_to(err4, PREVIEW_WIDTH_CELLS),
            fg,
            bg: error_bg,
        }],
    ));

    // Line: successful output with a selected range in the middle.
    let pre5 = "found ";
    let sel5 = "match";
    let post5 = " in 2 places";
    let tail5 = PREVIEW_WIDTH_CELLS - pre5.len() - sel5.len() - post5.len();
    lines.push(preview_line(
        font,
        vec![
            Span {
                text: pre5.into(),
                fg,
                bg: output_bg,
            },
            Span {
                text: sel5.into(),
                fg: selection_fg,
                bg: selection_bg,
            },
            Span {
                text: format!("{}{}", post5, " ".repeat(tail5)),
                fg,
                bg: output_bg,
            },
        ],
    ));

    // Spacer row on the base background.
    lines.push(preview_line(
        font,
        vec![Span {
            text: " ".repeat(PREVIEW_WIDTH_CELLS),
            fg,
            bg,
        }],
    ));

    // ANSI + brights strip: two chars per swatch, leading/trailing pad on
    // base bg so the strip aligns with the indented rows above.
    const INDENT: usize = 2;
    const SWATCH_CHARS: usize = 2;
    let strip_content = INDENT + (ansi_colors.len() + brights_colors.len()) * SWATCH_CHARS;
    let mut ansi_cells: Vec<Element> = Vec::new();
    ansi_cells.push(span_element(
        font,
        &Span {
            text: " ".repeat(INDENT),
            fg,
            bg,
        },
    ));
    for color in ansi_colors.iter().chain(brights_colors.iter()) {
        ansi_cells.push(
            Element::new(font, ElementContent::Text(" ".repeat(SWATCH_CHARS))).colors(
                ElementColors {
                    border: BorderColor::default(),
                    bg: (*color).into(),
                    text: fg.into(),
                },
            ),
        );
    }
    if PREVIEW_WIDTH_CELLS > strip_content {
        ansi_cells.push(span_element(
            font,
            &Span {
                text: " ".repeat(PREVIEW_WIDTH_CELLS - strip_content),
                fg,
                bg,
            },
        ));
    }
    lines.push(Element::new(font, ElementContent::Children(ansi_cells)).display(DisplayType::Block));

    Element::new(font, ElementContent::Children(lines))
        .display(DisplayType::Inline)
        .colors(ElementColors {
            border: BorderColor::default(),
            bg: bg.into(),
            text: fg.into(),
        })
        .padding(BoxDimension {
            left: Dimension::Cells(0.5),
            right: Dimension::Cells(0.),
            top: Dimension::Cells(0.),
            bottom: Dimension::Cells(0.),
        })
        .min_width(Some(Dimension::Cells(PREVIEW_WIDTH_CELLS as f32)))
        .max_width(Some(Dimension::Cells(PREVIEW_WIDTH_CELLS as f32)))
}

/// Truncate or right-pad with spaces to exactly `width` chars (ASCII).
fn pad_to(s: &str, width: usize) -> String {
    if s.len() >= width {
        s.chars().take(width).collect()
    } else {
        let mut out = String::with_capacity(width);
        out.push_str(s);
        for _ in 0..width - s.len() {
            out.push(' ');
        }
        out
    }
}
