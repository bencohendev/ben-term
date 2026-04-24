//! File-system plumbing for the theme builder: resolving the on-disk
//! TOML that defines the active scheme, editing its `[semantic_zones]`
//! and `[colors]` sections while preserving comments/ordering, and
//! exporting a built-in scheme to disk so it becomes editable.

use anyhow::{anyhow, Context, Result};
use config::{ColorSchemeFile, Theme};
use std::path::{Path, PathBuf};
use toml_edit::{value, Array, DocumentMut, Item, Table};

/// Map a scheme name to the on-disk TOML file that defines it, if any.
///
/// Mirrors the load-time discovery in `Config::load_color_schemes`:
///
/// 1. Prefer a direct filename match: `<name>.toml` inside any entry of
///    `color_scheme_dirs`. This is the common case.
/// 2. Fall back to scanning every `*.toml` in those dirs and parsing the
///    `[metadata].name` field — schemes can declare their display name
///    independently of the filename.
///
/// Returns `None` for built-in schemes (no file on disk). Callers use that
/// to decide whether to offer an "Export" flow instead of editing in place.
pub fn resolve_scheme_file(scheme_name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    // First pass: direct filename match.
    for dir in dirs {
        let candidate = dir.join(format!("{}.toml", scheme_name));
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // Second pass: parse each TOML and compare its `[metadata].name`.
    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_toml = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("toml"))
                .unwrap_or(false);
            if !is_toml {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(scheme) = ColorSchemeFile::from_toml_str(&text) {
                    if scheme.metadata.name.as_deref() == Some(scheme_name) {
                        return Some(path);
                    }
                }
            }
        }
    }

    None
}

/// Field identifier for a zone-color edit. Keeps call sites stringly-free
/// so a typo can't silently target the wrong key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneField {
    Input,
    Output,
    Prompt,
    Error,
}

impl ZoneField {
    pub fn key(self) -> &'static str {
        match self {
            Self::Input => "input_bg",
            Self::Output => "output_bg",
            Self::Prompt => "prompt_bg",
            Self::Error => "error_bg",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Input => "Input background",
            Self::Output => "Output background",
            Self::Prompt => "Prompt background",
            Self::Error => "Error output background",
        }
    }

    pub fn all() -> [ZoneField; 4] {
        [Self::Input, Self::Output, Self::Prompt, Self::Error]
    }
}

/// Scalar entries inside the `[colors]` table (everything except the
/// `ansi`/`brights` arrays and `indexed` 256-color map, which are 4b+
/// scope via other variants of `ColorField`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreField {
    Foreground,
    Background,
    CursorBg,
    CursorFg,
    SelectionBg,
    SelectionFg,
}

impl CoreField {
    pub fn key(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
            Self::CursorBg => "cursor_bg",
            Self::CursorFg => "cursor_fg",
            Self::SelectionBg => "selection_bg",
            Self::SelectionFg => "selection_fg",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Foreground => "Foreground",
            Self::Background => "Background",
            Self::CursorBg => "Cursor background",
            Self::CursorFg => "Cursor foreground",
            Self::SelectionBg => "Selection background",
            Self::SelectionFg => "Selection foreground",
        }
    }

    pub fn all() -> [CoreField; 6] {
        [
            Self::Foreground,
            Self::Background,
            Self::CursorBg,
            Self::CursorFg,
            Self::SelectionBg,
            Self::SelectionFg,
        ]
    }
}

/// Which color the builder is editing. One enum for zones, core palette,
/// and indexed ANSI/brights slots so the edit/write path is unified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorField {
    Zone(ZoneField),
    Core(CoreField),
    /// Index 0..8 into `[colors].ansi`.
    Ansi(u8),
    /// Index 0..8 into `[colors].brights`.
    Bright(u8),
}

/// Standard 8-color ANSI names. Used for row labels and nothing else.
const ANSI_NAMES: [&str; 8] = [
    "Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White",
];

impl ColorField {
    pub fn label(self) -> String {
        match self {
            Self::Zone(z) => z.label().to_string(),
            Self::Core(c) => c.label().to_string(),
            Self::Ansi(i) => format!("ANSI {} ({})", i, ANSI_NAMES[i as usize % 8]),
            Self::Bright(i) => format!("Bright {} ({})", i, ANSI_NAMES[i as usize % 8]),
        }
    }

    /// Whether an empty-string commit should remove the value. Arrays can't
    /// have individual slots unset, so only scalar fields qualify.
    pub fn allows_unset(self) -> bool {
        matches!(self, Self::Zone(_) | Self::Core(_))
    }
}

/// Generalized TOML writer: set (or remove, when applicable) any
/// `ColorField` in the on-disk scheme file. Uses `toml_edit` so inline
/// comments, key order, and whitespace are preserved across the edit.
///
/// Behavior per field kind:
/// - `Zone`: add/remove key in `[semantic_zones]`; delete the table if
///   emptied.
/// - `Core`: add/remove key in `[colors]`; leave the (possibly empty)
///   `[colors]` table in place since it's the canonical palette container.
/// - `Ansi(i)` / `Bright(i)`: replace slot `i` in `[colors].ansi` or
///   `[colors].brights`. If the array is absent, it is created using
///   `seed_array` (or `#000000` for missing entries as a last resort) so
///   the resulting file remains a valid 8-color palette.
///
/// `seed_array` is only consulted when creating a missing `ansi`/`brights`
/// array; callers typically pass the current in-memory palette values so
/// other slots retain their previously-effective (default) colors.
pub fn write_color_field(
    path: &Path,
    field: ColorField,
    hex: Option<&str>,
    seed_array: Option<&[String; 8]>,
) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;

    let mut doc: DocumentMut = text
        .parse()
        .with_context(|| format!("parsing TOML at {}", path.display()))?;

    apply_color_edit(&mut doc, field, hex, seed_array);

    let new_text = doc.to_string();
    std::fs::write(path, new_text)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Pure TOML transform — extracted so tests can exercise it without
/// touching the filesystem.
fn apply_color_edit(
    doc: &mut DocumentMut,
    field: ColorField,
    hex: Option<&str>,
    seed_array: Option<&[String; 8]>,
) {
    match field {
        ColorField::Zone(z) => apply_zone_edit(doc, z, hex),
        ColorField::Core(c) => apply_core_edit(doc, c, hex),
        ColorField::Ansi(idx) => apply_array_edit(doc, "ansi", idx, hex, seed_array),
        ColorField::Bright(idx) => apply_array_edit(doc, "brights", idx, hex, seed_array),
    }
}

fn apply_zone_edit(doc: &mut DocumentMut, field: ZoneField, hex: Option<&str>) {
    let key = field.key();

    // Ensure [semantic_zones] exists when we're setting a value; if we're
    // unsetting, only touch the table if it's already there.
    let ensure_table = hex.is_some();
    if ensure_table && !doc.contains_key("semantic_zones") {
        let mut t = Table::new();
        t.set_implicit(false);
        doc["semantic_zones"] = Item::Table(t);
    }

    if let Some(table) = doc.get_mut("semantic_zones").and_then(Item::as_table_mut) {
        match hex {
            Some(h) => {
                table[key] = value(h);
            }
            None => {
                table.remove(key);
            }
        }

        // Clean up an emptied table to keep written files tidy.
        if table.iter().next().is_none() {
            doc.remove("semantic_zones");
        }
    }
}

fn apply_core_edit(doc: &mut DocumentMut, field: CoreField, hex: Option<&str>) {
    let key = field.key();

    let ensure_table = hex.is_some();
    if ensure_table && !doc.contains_key("colors") {
        let mut t = Table::new();
        t.set_implicit(false);
        doc["colors"] = Item::Table(t);
    }

    if let Some(table) = doc.get_mut("colors").and_then(Item::as_table_mut) {
        match hex {
            Some(h) => {
                table[key] = value(h);
            }
            None => {
                table.remove(key);
            }
        }
    }
}

fn apply_array_edit(
    doc: &mut DocumentMut,
    array_key: &str,
    idx: u8,
    hex: Option<&str>,
    seed_array: Option<&[String; 8]>,
) {
    // Individual slot-unset isn't meaningful for a fixed 8-element array.
    let hex = match hex {
        Some(h) => h,
        None => return,
    };

    if !doc.contains_key("colors") {
        let mut t = Table::new();
        t.set_implicit(false);
        doc["colors"] = Item::Table(t);
    }

    let colors = match doc.get_mut("colors").and_then(Item::as_table_mut) {
        Some(t) => t,
        None => return,
    };

    if !colors.contains_key(array_key) {
        let mut arr = Array::new();
        for slot in 0..8 {
            let seed = seed_array
                .and_then(|s| s.get(slot).map(String::as_str))
                .unwrap_or("#000000");
            arr.push(seed);
        }
        colors[array_key] = value(arr);
    }

    if let Some(arr) = colors.get_mut(array_key).and_then(Item::as_array_mut) {
        if (idx as usize) < arr.len() {
            arr.replace(idx as usize, hex);
        }
    }
}

/// Determine the config dir we'd export into (`$XDG_CONFIG_HOME/wezterm/colors`
/// or the first entry in `color_scheme_dirs` that exists/is-writeable).
/// Picked conservatively — the user can always move the file afterwards.
pub fn export_target_path(scheme_name: &str, dirs: &[PathBuf]) -> Result<PathBuf> {
    let dir = dirs
        .iter()
        .find(|d| d.is_dir())
        .cloned()
        .or_else(|| dirs.first().cloned())
        .ok_or_else(|| anyhow!("no color_scheme dir configured"))?;

    // Create on demand — config dirs routinely don't have a `colors/`
    // subdir until the user adds their first theme file.
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;

    Ok(dir.join(format!("{}.toml", scheme_name)))
}

/// Dump a resolved `Theme` to `path` as a `ColorSchemeFile` TOML. Used by
/// the "Export built-in" flow so the user can then edit zones in place.
pub fn export_theme_to_file(path: &Path, theme: &Theme, scheme_name: &str) -> Result<()> {
    let mut file = ColorSchemeFile {
        colors: theme.palette.clone(),
        semantic_zones: theme.semantic_zones.clone(),
        ..Default::default()
    };
    // Preserve the name in metadata so the file is discoverable by
    // `[metadata].name` match even if the filename drifts.
    file.metadata.name = Some(scheme_name.to_string());
    file.save_to_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone_roundtrip(input: &str, field: ZoneField, hex: Option<&str>) -> String {
        let mut doc: DocumentMut = input.parse().unwrap();
        apply_color_edit(&mut doc, ColorField::Zone(field), hex, None);
        doc.to_string()
    }

    fn field_roundtrip(
        input: &str,
        field: ColorField,
        hex: Option<&str>,
        seed: Option<&[String; 8]>,
    ) -> String {
        let mut doc: DocumentMut = input.parse().unwrap();
        apply_color_edit(&mut doc, field, hex, seed);
        doc.to_string()
    }

    #[test]
    fn adds_section_when_absent() {
        let input = r##"[colors]
foreground = "#fff"
"##;
        let out = zone_roundtrip(input, ZoneField::Input, Some("#0044aa"));
        assert!(out.contains("[semantic_zones]"));
        assert!(out.contains("input_bg"));
        assert!(out.contains("#0044aa"));
    }

    #[test]
    fn preserves_comments() {
        let input = r##"[colors]
foreground = "#fff" # main fg

# Zones section
[semantic_zones]
input_bg = "#111" # typed input
# output_bg intentionally unset
"##;
        let out = zone_roundtrip(input, ZoneField::Input, Some("#222"));
        assert!(out.contains("# main fg"));
        assert!(out.contains("# Zones section"));
        assert!(out.contains("# typed input") || out.contains("# output_bg intentionally unset"));
    }

    #[test]
    fn removes_key_on_none() {
        let input = r##"[semantic_zones]
input_bg = "#111"
output_bg = "#222"
"##;
        let out = zone_roundtrip(input, ZoneField::Input, None);
        assert!(!out.contains("input_bg"));
        assert!(out.contains("output_bg"));
    }

    #[test]
    fn removes_emptied_table() {
        let input = r##"[colors]
foreground = "#fff"

[semantic_zones]
input_bg = "#111"
"##;
        let out = zone_roundtrip(input, ZoneField::Input, None);
        assert!(!out.contains("semantic_zones"));
        assert!(out.contains("foreground"));
    }

    #[test]
    fn updates_existing_key() {
        let input = r##"[semantic_zones]
input_bg = "#111"
"##;
        let out = zone_roundtrip(input, ZoneField::Input, Some("#222"));
        assert!(out.contains(r##"input_bg = "#222""##));
        assert!(!out.contains("#111"));
    }

    #[test]
    fn writes_error_bg_alongside_output_bg() {
        let input = r##"[semantic_zones]
output_bg = "#1a1a1a"
"##;
        let out = zone_roundtrip(input, ZoneField::Error, Some("#3a1f1f"));
        assert!(out.contains(r##"output_bg = "#1a1a1a""##));
        assert!(out.contains(r##"error_bg = "#3a1f1f""##));
    }

    #[test]
    fn removes_error_bg_keeps_other_zone_keys() {
        let input = r##"[semantic_zones]
output_bg = "#1a1a1a"
error_bg = "#3a1f1f"
"##;
        let out = zone_roundtrip(input, ZoneField::Error, None);
        assert!(!out.contains("error_bg"));
        assert!(out.contains(r##"output_bg = "#1a1a1a""##));
    }

    #[test]
    fn core_edit_creates_colors_and_sets_key() {
        let input = "";
        let out = field_roundtrip(
            input,
            ColorField::Core(CoreField::Foreground),
            Some("#abcdef"),
            None,
        );
        assert!(out.contains("[colors]"));
        assert!(out.contains(r##"foreground = "#abcdef""##));
    }

    #[test]
    fn core_edit_removes_key_but_keeps_table() {
        let input = r##"[colors]
foreground = "#abcdef"
background = "#111111"
"##;
        let out = field_roundtrip(input, ColorField::Core(CoreField::Foreground), None, None);
        assert!(!out.contains("foreground"));
        assert!(out.contains("[colors]"));
        assert!(out.contains("background"));
    }

    #[test]
    fn ansi_edit_creates_array_when_absent_with_seed() {
        let input = r##"[colors]
foreground = "#fff"
"##;
        let seed: [String; 8] = [
            "#010101".into(),
            "#020202".into(),
            "#030303".into(),
            "#040404".into(),
            "#050505".into(),
            "#060606".into(),
            "#070707".into(),
            "#080808".into(),
        ];
        let out = field_roundtrip(input, ColorField::Ansi(1), Some("#ff0000"), Some(&seed));
        // Red (index 1) takes the commit value; other slots keep their seed.
        assert!(out.contains("#ff0000"));
        assert!(out.contains("#010101"));
        assert!(out.contains("#080808"));
        assert!(out.contains("ansi"));
    }

    #[test]
    fn ansi_edit_replaces_existing_slot_only() {
        let input = r##"[colors]
ansi = ["#000000","#111111","#222222","#333333","#444444","#555555","#666666","#777777"]
"##;
        let out = field_roundtrip(input, ColorField::Ansi(2), Some("#abcdef"), None);
        assert!(out.contains("#abcdef"));
        assert!(!out.contains("#222222"));
        // Surrounding slots untouched.
        assert!(out.contains("#111111"));
        assert!(out.contains("#333333"));
    }

    #[test]
    fn bright_edit_targets_brights_array() {
        let input = r##"[colors]
ansi =    ["#000000","#111111","#222222","#333333","#444444","#555555","#666666","#777777"]
brights = ["#888888","#999999","#aaaaaa","#bbbbbb","#cccccc","#dddddd","#eeeeee","#ffffff"]
"##;
        let out = field_roundtrip(input, ColorField::Bright(3), Some("#deadbe"), None);
        assert!(out.contains("#deadbe"));
        // The ansi slot at index 3 was "#333333" — still present.
        assert!(out.contains("#333333"));
        // The original brights slot at index 3 was "#bbbbbb" — removed.
        assert!(!out.contains("#bbbbbb"));
    }

    #[test]
    fn ansi_edit_ignores_none_hex() {
        let input = r##"[colors]
ansi = ["#000000","#111111","#222222","#333333","#444444","#555555","#666666","#777777"]
"##;
        let out = field_roundtrip(input, ColorField::Ansi(0), None, None);
        // No-op: all slots intact.
        assert!(out.contains("#000000"));
        assert!(out.contains("#777777"));
    }

    #[test]
    fn allows_unset_matches_field_kind() {
        assert!(ColorField::Zone(ZoneField::Input).allows_unset());
        assert!(ColorField::Core(CoreField::Foreground).allows_unset());
        assert!(!ColorField::Ansi(0).allows_unset());
        assert!(!ColorField::Bright(0).allows_unset());
    }
}
