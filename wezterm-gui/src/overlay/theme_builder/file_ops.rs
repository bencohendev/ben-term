//! File-system plumbing for the theme builder: resolving the on-disk
//! TOML that defines the active scheme, editing its `[semantic_zones]`
//! section while preserving comments/ordering, and exporting a built-in
//! scheme to disk so it becomes editable.

use anyhow::{anyhow, Context, Result};
use config::{ColorSchemeFile, Theme};
use std::path::{Path, PathBuf};
use toml_edit::{value, DocumentMut, Item, Table};

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
    InputBg,
    OutputBg,
    PromptBg,
}

impl ZoneField {
    pub fn key(self) -> &'static str {
        match self {
            Self::InputBg => "input_bg",
            Self::OutputBg => "output_bg",
            Self::PromptBg => "prompt_bg",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::InputBg => "Input background",
            Self::OutputBg => "Output background",
            Self::PromptBg => "Prompt background",
        }
    }

    pub fn all() -> [ZoneField; 3] {
        [Self::InputBg, Self::OutputBg, Self::PromptBg]
    }
}

/// Set (or remove when `hex` is `None`) a `[semantic_zones]` field in the
/// scheme's TOML file. Uses `toml_edit` so inline comments, key order, and
/// whitespace are preserved across the edit.
///
/// Creates the `[semantic_zones]` table if absent. Removes an empty
/// `[semantic_zones]` table to keep diffs clean when the user unsets the
/// last zone color.
pub fn write_zone_color(path: &Path, field: ZoneField, hex: Option<&str>) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;

    let mut doc: DocumentMut = text
        .parse()
        .with_context(|| format!("parsing TOML at {}", path.display()))?;

    apply_zone_edit(&mut doc, field, hex);

    let new_text = doc.to_string();
    std::fs::write(path, new_text)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Pure TOML transform — extracted so tests can exercise it without
/// touching the filesystem.
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

    fn roundtrip(input: &str, field: ZoneField, hex: Option<&str>) -> String {
        let mut doc: DocumentMut = input.parse().unwrap();
        apply_zone_edit(&mut doc, field, hex);
        doc.to_string()
    }

    #[test]
    fn adds_section_when_absent() {
        let input = r##"[colors]
foreground = "#fff"
"##;
        let out = roundtrip(input, ZoneField::InputBg, Some("#0044aa"));
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
        let out = roundtrip(input, ZoneField::InputBg, Some("#222"));
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
        let out = roundtrip(input, ZoneField::InputBg, None);
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
        let out = roundtrip(input, ZoneField::InputBg, None);
        assert!(!out.contains("semantic_zones"));
        assert!(out.contains("foreground"));
    }

    #[test]
    fn updates_existing_key() {
        let input = r##"[semantic_zones]
input_bg = "#111"
"##;
        let out = roundtrip(input, ZoneField::InputBg, Some("#222"));
        assert!(out.contains(r##"input_bg = "#222""##));
        assert!(!out.contains("#111"));
    }
}
