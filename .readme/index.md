# Ben-Term — Documentation Index

## Overview

A custom terminal emulator forked from WezTerm, with first-party platform features: fuzzy directory navigation, intelligent autocomplete, and integrated AI — built as native overlay UI within the terminal renderer.

## Tech Stack

| Layer | Choice |
|-------|--------|
| Language | Rust |
| Base | WezTerm fork |
| GPU Rendering | Metal (macOS), OpenGL (Linux) |
| VT Parser | vte / wezterm-escape-parser |
| Font Shaping | harfbuzz (vendored) |
| Shell Integration | zsh / bash hooks |
| Platforms | macOS, Linux |

## Crate Structure

```
ben-term/
├── wezterm/              # CLI entry point
├── wezterm-gui/          # GUI application, window management, rendering
├── wezterm-surface/      # Terminal surface (cell grid, state machine)
├── wezterm-escape-parser/# VT100/xterm escape sequence parser
├── wezterm-cell/         # Cell representation (character + attributes)
├── wezterm-mux-server/   # Multiplexer server (tabs, panes, domains)
├── wezterm-ssh/          # SSH client integration
├── bidi/                 # Bidirectional text support
├── strip-ansi-escapes/   # ANSI escape stripping utility
└── deps/                 # Vendored dependencies (freetype, harfbuzz, zlib)
```

## Implemented Features

| Feature | Spec | Description |
|---------|------|-------------|
| WezTerm core | (upstream) | Full terminal emulator: PTY, VT parsing, GPU rendering, tabs, panes, fonts, config |
| Git picker | [features/git-picker.md](features/git-picker.md) | `git <sub>` + Space dropdown. Phases 1–5 shipped (refs, stash, files, remotes, commits) |
| Rich theming | [features/rich-theming.md](features/rich-theming.md) | TOML `[semantic_zones]` section tints cell backgrounds by OSC 133 zone (input/output/prompt/error). Milestones 1 + 2 + 4a + 4b shipped; milestone 5 (live preview pane) active |

## Active Features

| Feature | Spec | Description |
|---------|------|-------------|
| Context dropdowns | [features/context-dropdowns.md](features/context-dropdowns.md) | Shared infra (popup shell, trigger helpers) + `cd` picker. v1/v2/v3 shipped (incl. click-away, flip-above) |

## Planned Features

| Feature | Description |
|---------|-------------|
| Rich search history | Directory-aware filtering, exit code tracking, timestamps + duration, output preview snippets, frecent ranking |
| Workspaces & layouts | Named workspaces with pre-configured tab/split layouts, layout persistence across restarts, project-aware tab grouping |
| Command blocks | Visually group each command + output as a discrete unit — collapsible, copyable, re-runnable |
| Clickable paths & URLs | Detect file paths and URLs in output, cmd-click to open — expanded detection for stack traces, compiler errors, test failures |
| Smart notifications | System notification when a long-running command finishes in a background tab |
| Inline images | Polish existing iTerm2/Kitty image protocol support for matplotlib, image previews, etc. |
| Shell integration hooks | ZLE widgets and precmd/preexec hooks for structured shell-to-terminal communication |
| Platform keyboard parity | System-convention keybindings across the board (e.g. macOS: cmd+delete → delete to line start, option+delete → delete word, cmd+arrow word-jumps, etc.) |
| Recent-commands popup | Up-arrow opens a popup listing the most recent shell commands for quick re-run; can ride on the context-dropdowns PopupPicker infra with a history-based gather |

## Bugs

| Resource | Location |
|----------|----------|
| Bug Log | [bugs/log.md](bugs/log.md) |
