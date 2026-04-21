# Rich Theming

**Status:** Active — milestone 1 in flight (structured theme format + semantic-zone tinting).

## Overview

ben-term extends WezTerm's existing TOML color-scheme file format into a structured theme format. A theme file already defines a palette under `[colors]`; this work adds an optional `[semantic_zones]` section that tints cell backgrounds by the cell's `SemanticType` (input / output / prompt, sourced from OSC 133 shell integration). The same file format is the contract a future interactive theme builder will read and write.

Everything hot-reloads via WezTerm's existing `notify` watcher — no new config machinery.

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
- Error-zone tinting based on command exit codes.
- Interactive theme builder — native overlay UI that reads/writes these TOML files.
- Fast-path palette-swap reload that skips full Lua re-evaluation (defer unless latency becomes noticeable).

## Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Theme file format | Extend existing `ColorSchemeFile` TOML with an optional `[semantic_zones]` section | Already the idiomatic theme-file in WezTerm (`~/.config/wezterm/colors/*.toml`); zero new DSL or loader |
| Theme data model | New `Theme { palette, semantic_zones }` wrapper; `Config::color_schemes: HashMap<String, Theme>` | Theme builder will serialize a scheme as one atomic unit. Stapling zones onto `Palette` pollutes a struct reused as the inline `colors` block |
| Where `SemanticZoneColors` lives | `term/src/color.rs` next to `ColorPalette` | `config` depends on `wezterm-term`, not the other way — placing it in `term` avoids a circular dep. `config` keeps a mirror struct with `FromDynamic`/`ToDynamic` derives for TOML/Lua |
| How zones reach the renderer | Extend `term::ColorPalette` with a `semantic_zones` field | Every `Pane::palette()` funnels through `config::TermConfig::color_palette()` — one change propagates everywhere |
| Tinting strategy | Replace cell bg only when `bg_is_default` | Respects explicit bg from shell output (`ls --color`, TUI colors). Users dial weight via the zone color's alpha channel |
| User opt-in | Enabled iff `[semantic_zones]` fields are set | `SemanticZoneColors::default()` is all `None` — absent section is inert. Zero ceremony |
| Per-window cache | Reuse `TermWindow.palette: Option<ColorPalette>` | Already invalidated by `self.palette.take()` in `config_was_reloaded()`. Zones ride for free |
| Serde pattern | `FromDynamic` / `ToDynamic` + `impl_lua_conversion_dynamic!` | Codebase-wide convention; `Palette` itself uses it. Avoid `serde::{Serialize, Deserialize}` on these types |

## Architecture

### Files modified

| File | Change |
|------|--------|
| `term/src/color.rs` | Add `SemanticZoneColors` struct; extend `ColorPalette` with `semantic_zones` field |
| `config/src/color.rs` | Add mirror `SemanticZoneColors` (dynamic derives); add `Theme` wrapper; extend `ColorSchemeFile`; add `From<Theme> for ColorPalette`; add `From<Palette> for Theme`; add `ColorSchemeFile::into_theme()` |
| `config/src/config.rs` | Switch `color_schemes` to `HashMap<String, Theme>`; update `resolve_color_scheme()` to return `Option<&Theme>`; add `resolve_palette()` companion for palette-only callers |
| `config/src/lib.rs` | `COLOR_SCHEMES` built-in map typed as `HashMap<String, Theme>` (or a lookup wrapper) |
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

## Manual test recipe

1. Drop the TOML example above at `~/.config/wezterm/colors/zones-test.toml`.
2. `config.color_scheme = "zones-test"` in `wezterm.lua`.
3. Enable OSC 133 in the shell (zsh `precmd` / `preexec` hooks, or an existing oh-my-zsh plugin that emits them).
4. Launch ben-term. Observe: prompt line tinted green-ish, typed input tinted red-ish, command output unchanged.
5. Edit `zones-test.toml`, change `input_bg`, save — tint updates within ~200 ms.
6. `ls --color=always` — colorized filenames keep their explicit bg (no tint over them).
7. Launch with a shell lacking OSC 133 — screen is untinted (everything reads as `Output`, and `output_bg` is unset).
