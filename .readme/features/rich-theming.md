# Rich Theming

**Status:** Done for now — milestones 1 (semantic-zone tinting), 2 (error-zone tinting), 4a (zone-only theme builder), 4b (palette editor + interactive color picker), and 5 (live preview pane) shipped. Other future milestones under Scope remain deferred.

## Overview

ben-term extends WezTerm's existing TOML color-scheme file format into a structured theme format. A theme file already defines a palette under `[colors]`; this work adds an optional `[semantic_zones]` section that tints cell backgrounds by the cell's `SemanticType` (input / output / prompt, sourced from OSC 133 shell integration). The same file format is the contract a future interactive theme builder will read and write.

Everything hot-reloads via WezTerm's existing `notify` watcher. One tiny adjustment was required: the watcher's coverage had to extend to `color_scheme_dirs` (see Implementation notes below).

## Scope

**Milestone 1 (this work)**
- Extend `ColorSchemeFile` TOML with `[semantic_zones]`.
- New `Theme { palette, semantic_zones }` wrapper; `Config::color_schemes` becomes `HashMap<String, Theme>`.
- `term::ColorPalette` gains a `semantic_zones` field so renderers get zone colors via existing `Pane::palette()` plumbing.
- Render hook in `screen_line.rs` that tints `bg_color` by `SemanticType` when the cell's background is the default palette bg.
- Hot-reload via the existing `config_was_reloaded()` path.

**Future milestones (not this work)**
- Claude Code-aware theming — detect foreground-process identity and swap zone colors / palette overrides.
- Per-project auto-switching — CWD-driven theme selection.
- Fast-path palette-swap reload that skips full Lua re-evaluation (defer unless latency becomes noticeable).

## Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Theme file format | Extend existing `ColorSchemeFile` TOML with an optional `[semantic_zones]` section | Already the idiomatic theme-file in WezTerm (`~/.config/wezterm/colors/*.toml`); zero new DSL or loader |
| Theme data model | New `Theme { palette, semantic_zones }` wrapper; `Config::color_schemes: HashMap<String, Theme>` | Theme builder will serialize a scheme as one atomic unit. Stapling zones onto `Palette` pollutes a struct reused as the inline `colors` block |
| Where `SemanticZoneColors` lives | `term/src/color.rs` next to `ColorPalette` | `config` depends on `wezterm-term`, not the other way — placing it in `term` avoids a circular dep. `config` keeps a mirror struct with `FromDynamic`/`ToDynamic` derives for TOML/Lua |
| How zones reach the renderer | Extend `term::ColorPalette` with a `semantic_zones` field | Every `Pane::palette()` funnels through `config::TermConfig::color_palette()` — one change propagates everywhere |
| Tinting strategy | Replace cell bg only when `bg_is_default` | Respects explicit bg from shell output (TUI colors, CSI 48 / SGR background runs). Users dial weight via the zone color's alpha channel |
| User opt-in | Enabled iff `[semantic_zones]` fields are set | `SemanticZoneColors::default()` is all `None` — absent section is inert. Zero ceremony |
| Per-window cache | Reuse `TermWindow.palette: Option<ColorPalette>` | Already invalidated by `self.palette.take()` in `config_was_reloaded()`. Zones ride for free |
| Serde pattern | `FromDynamic` / `ToDynamic` + `impl_lua_conversion_dynamic!` | Codebase-wide convention; `Palette` itself uses it. Avoid `serde::{Serialize, Deserialize}` on these types |

## Architecture

### Files modified

| File | Change |
|------|--------|
| `term/src/color.rs` | Add `SemanticZoneColors` struct; extend `ColorPalette` with `semantic_zones` field |
| `config/src/color.rs` | Add mirror `SemanticZoneColors` (dynamic derives); add `Theme` wrapper; extend `ColorSchemeFile`; add `From<Theme> for ColorPalette`; add `From<Palette> for Theme`; add `ColorSchemeFile::into_theme()` |
| `config/src/config.rs` | Switch `color_schemes` to `HashMap<String, Theme>`; `resolve_color_scheme()` now returns `Option<Theme>` (cloned), with a `resolve_palette()` companion for palette-only callers; make `compute_color_scheme_dirs()` pub so the reload path can watch them |
| `config/src/lib.rs` | `ConfigInner::reload()` adds every `compute_color_scheme_dirs()` dir (that exists) to the notify watch list, so `*.toml` edits hot-reload. `COLOR_SCHEMES` built-ins stay typed as `Palette`; they're wrapped in `Theme` at resolve time — less invasive than re-typing the 1000+ generated entries |
| `config/src/terminal.rs` | `TermConfig::color_palette()` resolves the full `Theme` and populates `ColorPalette.semantic_zones` |
| `wezterm-gui/src/termwindow/render/screen_line.rs` | Tint `bg_color` by `SemanticType` when `bg_is_default`; two call sites (cluster loop + pixel-positioning path) |

### TOML example

```toml
[metadata]
name = "my-theme"

[colors]
foreground = "#e0e0e0"
background = "#1a1a1a"
ansi    = ["#000","#c44","#5a5","#cb4","#46b","#a4a","#3bb","#ccc"]
brights = ["#555","#f77","#7d7","#ed7","#79d","#d7d","#7dd","#fff"]

[semantic_zones]
# Any field omitted / commented out = no tint for that zone.
input_bg  = "#2a1f1f"   # tints what the user is typing
prompt_bg = "#1f2a1f"   # tints the prompt line
# output_bg intentionally unset — command output keeps the base bg
```

Select it the normal WezTerm way in `wezterm.lua`:
```lua
config.color_scheme = "my-theme"
```

### Dependency on OSC 133

Semantic zones rely on shell integration emitting OSC 133 prompt markers (`\e]133;A\a` start of prompt, `\e]133;B\a` end of prompt / start of input, etc.). WezTerm's parser already attaches `SemanticType::{Input, Output, Prompt}` to each cell when these markers are present; ben-term only adds the rendering.

Shells without OSC 133 default every cell to `SemanticType::Output`. With `output_bg` unset, the result is an untinted screen — graceful fallback, zero user action required.

## Open questions

- **Cluster homogeneity** — the renderer assumes shaped clusters don't span `SemanticType` boundaries. `wezterm-surface/src/line/line.rs:448-451` breaks zone ranges on `semantic_type` change at the surface level; confirm during implementation that cluster shaping preserves that boundary. Fallback if not: per-cell tint lookup inside the cluster paint (O(1) per cell).
- **Cursor contrast** — on very dark zone tints with dim cursor themes, the caret may be hard to see. Cursor is drawn above the bg-paint pass, so contrast is preserved, but note as a polish item.
<!-- Input-zone-cells-persist-after-backspace was resolved during milestone 1
     by trimming trailing blank cells from the Input tint rect — see
     Implementation notes below. -->
- **Leading or interior spaces in Input** — spaces inside typed input are still tinted (only the contiguous trailing run of blanks is trimmed), so `"  foo"` tints the leading spaces. Acceptable: typed leading spaces are invisible anyway, and tinting them matches "what the user typed."

## Implementation notes (milestone 1, as shipped)

- **Hot-reload required a one-line watcher extension.** The spec claimed reload would ride `config_was_reloaded()` "for free", but the existing file watcher only watched `wezterm.lua` and its parent dir (non-recursive), not the color-scheme search path. `ConfigInner::reload()` now also watches every dir returned by `Config::compute_color_scheme_dirs()`, so editing a `*.toml` palette or `[semantic_zones]` block updates the live window within ~200 ms.
- **`screen_line.rs` has two tint sites**, not one: the cluster loop *and* the pixel-positioning path. Both must apply the tint or font-shaping variants (e.g. ligature-aware positioning) leave cells un-tinted.
- **Input tint trims trailing blanks.** Shells implement backspace by overwriting the deleted character with a space that stays inside the 133;B region, so the cell remains `SemanticType::Input`. Without a trim the input tint would grow leftward-only and never visually shrink. The paint-site in `screen_line.rs` counts the contiguous trailing run of U+0020 in each Input cluster's text and narrows the tint rect by that many cells. Trim is scoped to Input; Prompt and Output zones paint full-cluster.

## Milestone 4a — Interactive theme builder (semantic zones)

Inline Modal UI to edit the active scheme's `[semantic_zones]` section. File-on-disk is the source of truth: every accepted edit writes the TOML, and the existing hot-reload path (milestone 1) paints the change within ~200 ms. No "Save" button, no in-memory dirty state.

### Scope

- **Entry point:** new `ActivateThemeBuilder` key assignment. Unbound by default — users bind it in `wezterm.lua`.
- **Fields edited (4a):** `input_bg`, `output_bg`, `prompt_bg` only. The rest of the palette is 4b.
- **Target file:** the on-disk TOML that defines the currently-active scheme. Built-in schemes (no file) show an "Export to make editable" row that dumps the resolved palette to `$XDG_CONFIG_HOME/wezterm/colors/<scheme>.toml` and then re-opens the builder against that new file.
- **Non-goals for 4a:** RGB sliders, color-cube picker, creating fresh (non-exported) themes, renaming schemes, editing the 16-color ANSI palette.

### Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| UI primitive | `Modal` trait (parallel to `PopupPicker`, no shared code) | Same upstream paint/input plumbing the palette + pickers use; zero risk of bleed-back into shared picker infra |
| Source of truth | File on disk | Hot-reload already propagates file edits. Any in-memory mirror would drift; saving every edit keeps the contract trivial |
| TOML writer | `toml_edit` | Preserves comments + key ordering. The vanilla `toml` crate round-trips lossily, which would silently eat the user's `# output_bg intentionally unset` kind of comments |
| File-path resolution | Scan `compute_color_scheme_dirs()` twice: first for a `<name>.toml` filename match, then parse remaining TOMLs for `[metadata].name == <name>` | Mirrors how `load_color_schemes` accepts both conventions. Parse step only runs on name mismatch, so cost is bounded |
| Built-in scheme case | Detect-via-no-path; offer export rather than silently creating a file | Surprise-write to the config dir would violate user expectation of "my settings are under my control" |
| Input UX | Append-only hex text input (e.g. `#1a2b3c`), Enter commits, Escape cancels the field edit | Matches PopupPicker's PTY-echo append-only model so users get one input idiom across all overlays. Full cursor-editor behavior can come in 4b if needed |
| Validation | Accept `#rgb` / `#rrggbb` (case-insensitive), reject otherwise | `ColorSchemeFile` TOML already accepts these; we round-trip to the same parser |
| Unset a field | Submit empty string → remove the key from the TOML | Lets users blank a zone (graceful-fallback untinted behavior). Explicit "remove" clearer than a sentinel value |
| Live preview | Rely on file-watcher hot-reload | Already works. Builder only writes; painting is someone else's job |
| Swatch render | Small colored polygon inside each row | Users want to *see* the color next to the hex, not just read it |
| Anchor | Center of the window (like the palette) | Not contextual to a cursor — this is a "settings" UI, not a command-completion popup |

### Files to modify

| File | Change |
|------|--------|
| `config/src/keyassignment.rs` | Add `ActivateThemeBuilder` variant |
| `wezterm-gui/src/commands.rs` | Wire assignment in `derive_command_from_key_assignment`; no default binding |
| `wezterm-gui/src/termwindow/mod.rs` | Dispatch `ActivateThemeBuilder` in `perform_key_assignment` → `theme_builder::try_activate` |
| `wezterm-gui/src/overlay/mod.rs` | `pub mod theme_builder;` |
| `wezterm-gui/src/overlay/theme_builder/mod.rs` | **New.** `try_activate` entry + helpers |
| `wezterm-gui/src/overlay/theme_builder/builder.rs` | **New.** `ThemeBuilder` struct + `Modal` impl (render, key_down, mouse handlers) |
| `wezterm-gui/src/overlay/theme_builder/file_ops.rs` | **New.** `resolve_active_scheme_path`, `write_zone_color`, `export_builtin` |
| `config/Cargo.toml` or `wezterm-gui/Cargo.toml` | Add `toml_edit` dependency (put in whichever crate actually calls it — probably the gui crate for 4a, since only the builder writes) |

### Verification

- [x] `cargo build --release --bin wezterm-gui` passes
- [x] `cargo build --bin wezterm-gui` passes
- [x] `cargo test --bin wezterm-gui theme_builder` — 14/14 passing (row state, edit flow, TOML round-trip incl. comment preservation + empty-table cleanup)
- [x] `cargo clippy --no-deps -p wezterm-gui` clean on the new module (pre-existing upstream lint debt in `build.rs` and several upstream crates unchanged — same carve-out already documented for the cd/git pickers)

Left for the user to confirm on their setup:

- [ ] Bind `ActivateThemeBuilder` to a key (e.g. `{ key = "t", mods = "CMD|SHIFT", action = wezterm.action.ActivateThemeBuilder }`)
- [ ] Open against `/tmp/ben-term-theming-test/colors/zones-test.toml` → modal lists three zones with current values + swatches
- [ ] Edit `input_bg` → TOML updated on disk, comments preserved, tint changes live within ~200 ms
- [ ] Submit empty value for `output_bg` → key removed from TOML; if it was the last key, `[semantic_zones]` removed too
- [ ] Invalid hex `#zzz` → rejected with inline error, field stays in edit mode
- [ ] Against a built-in scheme (e.g. `config.color_scheme = "Builtin Solarized Dark"`) → "Export" row appears; pick it → file created under config colors dir, modal flips into editable mode
- [ ] Escape from editing a field cancels the edit (keeps modal open); Escape from row list closes the modal
- [ ] Click outside the modal → dismisses (click-away inherits same contract as PopupPicker)

### Implementation notes (as shipped)

- **Parallel to `PopupPicker`, not shared code.** This mirrors the cd/git picker decision: isolating divergent overlays prevents shared-helper creep that would hurt upstream rebases. The two modules both implement `Modal` and both use the `command_palette_*` color/font config, but they don't share a base type.
- **`toml_edit` for write-back, `toml` for export.** Edits to an existing file use `toml_edit::DocumentMut` to preserve comments and ordering. The export path (built-in → disk) reuses the existing `ColorSchemeFile::save_to_file` which emits a fresh, formatted file with no user comments to preserve — simpler and already tested.
- **`[semantic_zones]` table is removed when emptied** so a user who unsets all three fields doesn't see a lingering empty section.
- **`std::convert::TryFrom` import is required** in `builder.rs` because `wezterm-gui` is edition 2018. Future edition bump would make this redundant.

## Milestone 4b — Palette editor + interactive color picker

Extends the 4a modal beyond `[semantic_zones]` into the full `[colors]` palette (core + 16 ANSI) and replaces the hex-text-only input with an inline HSV color picker so users don't have to type hex for every tweak.

### Scope

- **Rows added (in order under the existing zone rows):**
  - Core: `foreground`, `background`, `cursor_bg`, `cursor_fg`, `selection_bg`, `selection_fg`
  - ANSI (8): black, red, green, yellow, blue, magenta, cyan, white
  - Brights (8): same names, bright variants
  - Static, non-selectable section headers between groups.
- **Scroll:** the expanded row list will overflow a small window. Clamp visible rows to available height and scroll the selected row into view.
- **Color picker:** Space on an editing row opens an inline HSV picker (saturation×value square + hue slider). Mouse click/drag and arrow keys update the color; the row's edit buffer live-updates to the current hex. Enter commits (writes TOML + exits picker + exits edit mode). Esc closes the picker, keeping the previous hex-text buffer.
- **Non-goals for 4b:** fresh-theme creation, renaming, indexed (256-color) palette, tab-bar colors, `cursor_border`, `compose_cursor`, copy-mode/quick-select sub-palettes.

### Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Row organization | Flat linear list with static section headers | 4a is linear; collapsible sections add state/UI for marginal density win |
| Scroll model | Clamp to available height, scroll selection into view | Mirrors existing PopupPicker viewport logic |
| Picker color model | HSV (sat×val square + hue slider) | Standard color-picker ergonomics; HSV↔RGB mapping is trivial |
| Picker render | Grid of solid-bg `Element` rects (24×16 SV + 24 hue cells) | Reuses the known-good box-model paint path; introducing a gradient primitive is real renderer work, defer unless the swatch grid looks bad |
| Picker entry key | `Space` on an editing row | Unambiguous, easy to reach. `Tab` considered but less discoverable |
| Picker commit | Enter → writes hex to file + closes picker + closes edit mode | Single commit path; Esc returns to hex-text without losing prior buffer |
| TOML writer | Generalize `write_zone_color` → `write_color_field`, dispatching by field kind (top-level, `[colors]` scalar, `colors.ansi`/`colors.brights` array slot). `write_zone_color` stays as a thin wrapper for the zone sub-table | Factor field routing once; keep 4a tests and semantics intact |
| Field identity | New `ColorField` enum covering `Zone(ZoneField)`, `Core(CoreField)`, `Ansi(u8)`, `Bright(u8)` — `RowKind` becomes `Field(ColorField)` + `Section(&'static str)` + `Export` | One enum means one write path, one edit path, no parallel branches to keep in sync |

### Files to modify

| File | Change |
|------|--------|
| `wezterm-gui/src/overlay/theme_builder/file_ops.rs` | Add `ColorField` enum + `CoreField` enum + `write_color_field`. `write_zone_color` becomes `write_color_field(ColorField::Zone(...))` wrapper. Round-trip tests for core scalars + `ansi[n]` + `brights[n]` |
| `wezterm-gui/src/overlay/theme_builder/builder.rs` | Reshape `RowKind` to include sections + core/ANSI fields; add `view_offset` for scrolling; add picker sub-state (`editing: Option<EditState { buffer, picker: Option<PickerState> }>`); route Space-to-open-picker |
| `wezterm-gui/src/overlay/theme_builder/picker.rs` | **New.** `ColorPicker` struct, HSV↔RGB math, render (swatch grid), keyboard + mouse handlers |
| `wezterm-gui/src/overlay/theme_builder/mod.rs` | `pub mod picker;` |

### Verification

- [x] `cargo build --bin wezterm-gui` passes
- [x] `cargo clippy --no-deps -p wezterm-gui` — new modules clean (same pre-existing 4a `ZoneField`/`Bg` lint carry-over; no new warnings introduced)
- [x] `cargo test --bin wezterm-gui theme_builder` — 34/34 passing (9 existing 4a + new: core-scalar / ansi-slot / bright-slot round-trips, section-skipping navigation, view-offset scroll math, HSV↔RGB conversions)

Left for the user to confirm on their setup:

- [ ] Open builder on a file-backed scheme → rows grouped Zones / Core / ANSI / Brights, headers render but skip on ↑/↓
- [ ] Edit `foreground` via hex → written to `[colors].foreground`, other keys + comments preserved
- [ ] Edit ANSI red (index 1) via hex → written to `ansi[1]`, other 7 slots untouched
- [ ] Edit an ANSI slot when `ansi` array is absent → array created from the builder's current palette seed, edited slot set
- [ ] Small window with 25+ rows → scroll indicator shows "↑/↓ more", selected row stays visible as you move
- [ ] Press Space mid-edit → picker opens inline below the row with 16×8 SV grid + 24-cell hue bar, cursor dots on current values
- [ ] Arrow nudges in picker → hex readout updates live, Enter writes to file and closes picker
- [ ] Shift+Left/Right in picker → hue wraps around 0°/360° without glitching
- [ ] Esc in picker → drops back to hex-text edit with the pre-picker buffer restored

### Implementation notes (as shipped)

- **Keyboard-only picker for 4b.** The scope-time plan included mouse click/drag. In practice, the cost of plumbing pixel-level bounds for the SV / hue sub-rects through the `Modal` trait significantly outweighed the value versus arrow-key + Shift-arrow nudges. Deferred as "mouse polish" — can be added without reshaping `PickerState` or the render path.
- **`write_zone_color` removed, not wrapped.** The scope said the 4a entry point would stay as a thin wrapper for source compatibility; the callers all migrated to `write_color_field` cleanly, so the wrapper would have been dead code. Tests route through `apply_color_edit` directly.
- **Picker replaces the row list while open** rather than rendering beneath it. Keeps the modal a reasonable height — the full 25-row list + picker would overflow most windows.
- **ANSI/brights array seeding.** When the TOML is missing the `ansi` or `brights` array and the user edits a slot, the writer creates the array using the builder's in-memory palette values as seeds. The builder sources those from the resolved `Theme` it was constructed with, so "missing array" gets a sensible default-palette rendering rather than seven `#000000` placeholders + one edited slot.

## Milestone 2 — Error-zone tinting

Extends semantic-zone tinting so Output cells that belong to a command which exited non-zero (surfaced via OSC 133;D) paint with a configurable `error_bg` instead of the base `output_bg`. Rides the existing paint path (milestone 1) and the existing theme-builder plumbing (4a/4b).

### Scope

- New `error_bg` field on `SemanticZoneColors` (term mirror + config mirror + TOML `[semantic_zones]`).
- OSC 133;D currently parsed + discarded. Add tracking: remember the visible row where OSC 133;C (start-of-output) fired; on 133;D with non-zero status, mark every `Line` from the command's start row through the current row as "failed command."
- `screen_line.rs`: when the current line is marked failed and the cluster's semantic type is `Output`, prefer `error_bg` over `output_bg`. Success commands and zero-status Output keep painting `output_bg`.
- Theme builder: add a fourth zone row (`Error output (error_bg)`), keyed `"error_bg"` in the TOML. Reuses 4a/4b file-ops + row rendering.

**Non-goals**
- Rendering the exit code itself (text, gutter marker). Separate feature.
- Tinting Input/Prompt cells of failed commands — only Output.
- Per-command collapse / command-block UI.
- Styling based on signal termination, time-to-run, or any state beyond "exit code was non-zero."

### Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Where failed-command state lives | A new `IS_COMMAND_FAILED` flag on `LineBits` | `LineBits` has room (9 of 16 bits used); a bool flag is zero marginal memory, vs. ~8 bytes per line for `Option<i32>`. Tinting only needs yes/no. Exit-code display would be a separate future feature with its own storage |
| Start-of-command marker | `command_start_stable_row: Option<StableRowIndex>` on `TerminalState`, set in the OSC 133;C handler | Stable row index is scroll-safe. Scrollback may push the row out of the visible range before 133;D arrives; stable index survives that |
| When to stamp | On OSC 133;D with non-zero status, walk stable rows `[start..=current]`, convert to phys, set the flag on each `Line` | Exit code arrives after output is already emitted. Walk is bounded by one command's output (typically a screen or two). Skip when no `command_start_stable_row` is set (shell without 133;C) |
| Zero status | Do nothing — leave flag clear | Successful commands are the overwhelming default; tinting all of them would be noise |
| Paint | `screen_line.rs` existing cluster + pixel-positioning tint sites | Both sites get the same guard: if cluster is `Output` and `params.line.is_command_failed()`, return `zones.error_bg.or(zones.output_bg)` |
| Fallback when `error_bg` unset | Same as existing zone behavior: no tint change | Respects user intent — users who don't set `error_bg` stay on `output_bg` |
| Theme builder row placement | Immediately after `Output (output_bg)` in the Zones section | Keeps related fields adjacent; no effect on other rows |

### Files to modify

| File | Change |
|------|--------|
| `wezterm-surface/src/line/linebits.rs` | Add `IS_COMMAND_FAILED = 1<<9` |
| `wezterm-surface/src/line/line.rs` | Add `is_command_failed()` getter + `set_command_failed(bool)` setter on `Line` |
| `term/src/terminalstate.rs` (or wherever `TerminalState` is declared) | Add `command_start_stable_row: Option<StableRowIndex>` field + default |
| `term/src/terminalstate/performer.rs` | Set start row in the `MarkEndOfInputAndStartOfOutput` (OSC 133;C) arm; stamp failed flag on the row range in the `CommandStatus` (OSC 133;D) arm |
| `term/src/color.rs` | Add `error_bg: Option<SrgbaTuple>` to `SemanticZoneColors` |
| `config/src/color.rs` | Add mirror `error_bg: Option<RgbaColor>` + `From` conversions + round-trip test |
| `wezterm-gui/src/termwindow/render/screen_line.rs` | At both tint sites, if cluster is `Output` and `params.line.is_command_failed()` and `error_bg.is_some()`, use `error_bg` |
| `wezterm-gui/src/overlay/theme_builder/file_ops.rs` | Add `ZoneField::Error` variant (key `"error_bg"`, label text) |
| `wezterm-gui/src/overlay/theme_builder/builder.rs` | Include Error row in the zone section |

### Verification

- [x] `cargo build -p wezterm-gui` passes
- [x] `cargo clippy --no-deps -p wezterm-gui` — no new warnings in milestone-2 code (pre-existing upstream lint debt in `inputmap.rs`, `mouse.rs`, etc. unchanged; same carve-out as 4a/4b)
- [x] `cargo test --bin wezterm-gui theme_builder` — 36/36 passing (adds `writes_error_bg_alongside_output_bg`, `removes_error_bg_keeps_other_zone_keys`, plus the row-count bump)
- [x] `cargo test -p config` — 13/13 passing (adds `test_semantic_zones_error_bg`)
- [x] `cargo test -p wezterm-term` — 49/49 passing (no new tests; confirms existing semantic-zone flow still works)

Confirmed on the user's setup:

- [x] Set `error_bg` in `zones-test.toml` (e.g. `error_bg = "#3a1f1f"`)
- [x] Run a successful command (`true` / `ls`) → output untinted or `output_bg` only
- [x] Run a failing command (`false` / `ls /does-not-exist`) → the command's output cells paint `error_bg`
- [x] Scrollback preserves the tint — scroll back up to a prior failed command, still red
- [x] Theme builder shows the new `Error output` row; editing it writes `error_bg` to TOML and re-paints live
- [x] Shell without OSC 133;D (or with broken integration) — no tinting regression on Output; `output_bg` still works

### Implementation notes (as shipped)

- **Line-level flag beats per-cell exit code.** `LineBits` had 7 free bits of 16; adding `IS_COMMAND_FAILED = 1<<9` was zero marginal memory. A per-line `Option<i32>` would have cost ~8 bytes per cached line purely to carry an exit code that the tint path only reads as "zero or not zero." When/if we build exit-code display (gutter marker, command-block UI), it gets its own storage.
- **`command_start_stable_row` is tracked on `TerminalState`, not `Screen`.** The screen is swapped when entering/leaving alt-screen and reset on resize; carrying the marker on `TerminalState` keeps it alive across those transitions. Shells that miss OSC 133;C (start-of-output marker) leave it as `None`, in which case 133;D is a no-op — the graceful fallback specced out.
- **Walk range is inclusive on both ends and clamped via `stable_row_to_phys`.** If the start row has already scrolled out of the phys-row buffer, `stable_row_to_phys` returns `None` and the walk silently skips that row rather than stamping a bogus index — the same pattern the zone iterator uses elsewhere.
- **Paint site is guarded by `error_bg.is_some()` before the fallthrough.** This matters: if a user only sets `error_bg` (no `output_bg`), failed-command cells should tint red while successful cells stay untinted. The `Output if ... && zones.error_bg.is_some() => zones.error_bg` pattern preserves the "unset zone = no tint" contract from milestone 1.
- **One builder test needed the row-count bump.** `rows_for_file_backed_scheme_contain_all_sections` hard-codes the total row count (was 29, now 30) and `move_down_skips_section_headers` needed one more `move_down()` to cross the new Error field to reach Foreground. Both are mechanical updates; no behavior change.

## Milestone 5 — Live preview pane

Adds a miniature "sample terminal" pane alongside the theme builder's row list and HSV picker. Every themed surface (prompt / input / output / error, selection, cursor, foreground/background, ANSI + brights) is rendered inside the pane using the builder's **in-memory** palette + zones, with the currently-edited field overridden by the picker's live value. Users drag the hue slider or SV cursor and see the sample repaint instantly — no file write, no hot-reload round-trip.

### Motivation

Today's edit loop is: open picker → nudge color → press Enter → TOML writes → hot-reload fires ~200 ms later → the real terminal repaints. During the nudge itself the user sees only the hex readout change, not the color effect. For ANSI slot edits or zone edits this makes picking by eye almost impossible — you commit a guess, observe, re-open the picker, nudge again.

The preview pane collapses the loop to zero latency for a scripted scene that exercises every slot.

### Scope

- **Preview pane** rendered inside the `ThemeBuilder` modal, right of the row list (or right of the picker while the picker is open).
- **Sample content** — a static scripted transcript, not a live PTY. Exercises:
  - Prompt line (tints `prompt_bg`, uses `foreground` on `background`)
  - Typed input on the prompt (tints `input_bg`)
  - Successful command output (tints `output_bg`)
  - Failed command output (tints `error_bg`)
  - Selected text range (uses `selection_bg` / `selection_fg`)
  - Cursor cell on the input line (uses `cursor_bg` / `cursor_fg`)
  - ANSI + brights swatch strip (16 cells, labeled 0–15)
- **Live override** — while a field is being edited (text buffer or picker open), the pane renders with the candidate hex applied *in place of* the committed value for that field only. Every other field reads from the committed in-memory `palette` / `zones`.
- **Applies to every field kind** — zones, core, ANSI, brights. Picker nudges on any row update the pane on the next frame.

**Non-goals**
- Applying the mid-edit candidate to the **real** terminal window (i.e. live-tinting the user's actual scrollback before Enter). Doable but larger — requires a palette-override path through `TermConfig::color_palette()` and cache invalidation on every nudge. Deferred as "milestone 5 stretch" unless the scripted preview proves insufficient.
- Mirroring the user's real scrollback through the candidate palette in the preview pane (as opposed to a scripted scene). Same blocker as above plus cell-copying; the scripted scene gives full coverage with far less plumbing.
- Theming the preview pane itself beyond the candidate colors (no chrome color customization, no multi-scene switcher).
- Animating transitions between colors.

### Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Preview source | Scripted sample rendered via the existing modal `Element` box model | Reuses the box/cell paint the modal already uses for rows + picker swatches. Zero new renderer code. Guarantees every themed slot is exercised regardless of the user's actual session |
| Live-candidate plumbing | `ThemeBuilder::preview_colors() -> (Palette, SemanticZoneColors)` returning committed state with the in-edit field overridden from `editing`/`picker` buffers | Single read path the preview call site hits on every `compute()`. No extra state, no cache to invalidate |
| Invalid in-edit hex | Preview falls back to the committed color for that field | "Typing" should not flash arbitrary garbage — only valid hex (once a full `#rrggbb` parses) flows through. Matches how the picker itself treats intermediate states |
| Layout — picker closed | Row list on the left, preview on the right, fixed-width preview (~32 cells) | Keeps rows prominent (the primary interaction) while showing ambient effect |
| Layout — picker open | Picker on the left (replaces rows as 4b already does), preview on the right | Preview stays visible exactly when it matters most — during picker manipulation |
| Small-window behavior | If modal width < threshold (roughly "preview doesn't fit"), hide the preview and keep the existing single-column layout | 4b already scrolls rows in a small window; reintroducing overflow from a side pane would churn the row scroll math |
| ANSI / brights exercise | Single horizontal strip of 16 labeled cells at the bottom of the preview | Compact, deterministic; covers both arrays without needing a `ls --color` fake |
| Failed-command sample | A bash-style `$ cat missing.txt` + `cat: missing.txt: No such file` on the next line, with the output line tinted via `error_bg` | Makes the error tint visible even when OSC 133;D is never fired — the preview fakes the failed-flag directly |
| Cursor sample | Render a block cursor over a single input cell using `cursor_bg` / `cursor_fg` | `cursor_border` and blink modes are out of scope; a static block covers the common case |
| Selection sample | Render a 6-cell selected range inside the output line, using `selection_bg` / `selection_fg` | One obvious visual; avoids a full selection-mode simulation |
| Repaint trigger | Existing modal `compute()` already rebuilds the element tree on every key / mouse event. Preview rides that — no extra invalidation needed | Picker nudges already invalidate the tree |

### Files to modify

| File | Change |
|------|--------|
| `wezterm-gui/src/overlay/theme_builder/builder.rs` | Add `preview_colors()` helper returning `(Palette, SemanticZoneColors)` with the current edit overlay applied. Adjust `compute()` to emit a two-column outer box: left = existing row/picker column, right = preview column. Threshold check hides the preview column on narrow modal widths |
| `wezterm-gui/src/overlay/theme_builder/preview.rs` | **New.** `render_preview(palette, zones, metrics, width_cells) -> Element` that builds the scripted scene (prompt / input / output / error / selection / cursor / ANSI strip) as nested `Element` rows |
| `wezterm-gui/src/overlay/theme_builder/mod.rs` | `pub mod preview;` |
| `wezterm-gui/src/overlay/theme_builder/picker.rs` | Minor: picker's `compute()` unchanged, but the *caller* in `builder.rs` now places the picker in the left column instead of full-width |

No changes to `file_ops.rs`, no changes outside `theme_builder/`.

### Verification

- [x] `cargo build --bin wezterm-gui` passes
- [x] `cargo clippy --no-deps -p wezterm-gui` — preview module clean (same pre-existing 4a/4b upstream lint carve-out; our diff drops warning count by 1)
- [x] `cargo test --bin wezterm-gui theme_builder` — 44/44 passing (36 pre-existing + 8 new `preview_colors` tests: not-editing pass-through, valid hex overlay, partial/empty buffer fallback, core/ansi/bright field coverage, single-field isolation)

Left for the user to confirm on their setup:

- [x] Open builder on a file-backed scheme → preview pane appears right of the row list showing prompt / input / output / error / selection / cursor / ANSI strip painted with the active scheme's colors
- [x] Nudge hue in the picker → preview repaints every arrow keystroke (no file write, no ~200 ms lag)
- [ ] Select an ANSI slot (e.g. `ansi.red`, index 1) → picker opens, nudging repaints the slot's swatch in the ANSI strip instantly
- [ ] Select the `error_bg` zone row → preview's failed-command line repaints on each picker nudge
- [ ] Select `selection_bg` → the preview's selected range updates as you nudge
- [ ] Type a partial hex (e.g. `#1a`) into a row (no picker) → preview keeps the committed color for that field (no flicker); on a full valid `#1a2b3c` the preview updates
- [ ] Press Enter to commit → TOML written, preview re-seeds from the new committed state (no visible jump since it already matched the candidate)
- [ ] Press Esc from picker → preview reverts to committed color for that field
- [ ] Shrink the window width until the preview pane hides → single-column layout matches the pre-milestone-5 behavior; grow it back → preview returns
- [ ] Against a built-in scheme (pre-Export) → preview renders with the resolved built-in palette; after Export, preview continues seamlessly

### Implementation notes (as shipped)

- **Preview column width is 40 cells, not 32.** Spec called 32 a guess; in practice the longest scripted line (`cat: missing.txt: No such file` = 30 chars) plus desired right-side padding, and the 16-cell ANSI+brights strip at 2 chars per swatch (32 chars of color + indent), both pressured 32 into wrapping. 40 fits everything with a little slack.
- **Modal width grows when the preview is shown.** The pre-milestone modal was fixed at 60 cells; compute() now picks `BODY_COL_CELLS (46) + PREVIEW_WIDTH_CELLS (40) + CHROME_CELLS (6) = 92` when the terminal has ≥92 columns, otherwise it stays at 60 with the preview hidden. Threshold and fallback match the spec; the exact numbers landed in `builder.rs::compute`.
- **`apply_field_override` is a free helper, not a refactor of `set_value`.** The two paths differ slightly (`set_value` takes `Option<RgbaColor>` for the unset case, preview-overlay always has `Some`), so a shared implementation would have been a lossy generalization. Cheaper to mirror the four match arms in a small helper next to `initial_selected_row`.
- **Preview reads `editing` only, not `picker`.** The picker's `update_picker_buffer()` keeps `editing` in sync with each HSV nudge, so the preview overlay has one input instead of two. If a future feature introduces a picker-without-edit-buffer mode, the overlay will need to read both.
- **Footer hint for the picker moved into the shared footer decision.** The early-return path for picker mode used to push its own footer element inside the picker branch; the compute() refactor unifies footers so the `↑↓←→ pick  Shift+←/→ hue  ↵ save  Esc back` hint shows only when `picker_state.is_some()`. Behavior unchanged, but the branch now lives in one place.

### Open questions (resolved)

- **Preview width (~32 cells)** — resolved: 40 cells. See implementation notes.
- **Cursor on the prompt line** — in the current scene the cursor sits on the *input* segment (after the `%`), not the prompt segment. `cursor_bg` paints cleanly over `input_bg` with no stacking ambiguity. Noting here in case the scene later moves the cursor onto the prompt row.
- **Stretch: live-tint the real window** — still deferred. Scripted pane is expected to cover the common case; revisit only if user feedback surfaces a specific need.

## Manual test recipe

1. Drop the TOML example above at `~/.config/wezterm/colors/zones-test.toml`.
2. `config.color_scheme = "zones-test"` in `wezterm.lua`.
3. Enable OSC 133 in the shell (zsh `precmd` / `preexec` hooks, or an existing oh-my-zsh plugin that emits them).
4. Launch ben-term. Observe: prompt line tinted green-ish, typed input tinted red-ish, command output unchanged.
5. Edit `zones-test.toml`, change `input_bg`, save — tint updates within ~200 ms.
6. Explicit-bg respected: `printf '\033[41mRED BG\033[0m\n'` (portable; `ls --color=always` is GNU-only and won't run on macOS BSD `ls`). The `RED BG` cells keep their red background — no zone tint overlaid.
7. Backspace doesn't leave tint trails: type `abcdef`, then backspace 3× — the tint shrinks to cover `abc`, not the full 6-cell region.
8. Launch with a shell lacking OSC 133 — everything classifies as `Output`. If `output_bg` is unset the screen is untinted; if set, the whole screen is uniformly tinted (graceful — no two-tone artifact).
