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

## Active Features

| Feature | Spec | Description |
|---------|------|-------------|
| Context dropdowns | [features/context-dropdowns.md](features/context-dropdowns.md) | Native overlay pickers for cd, git checkout/stash, npm run/cargo, ssh, docker, kill, env — triggered by command + space. v1 in progress: `cd` only |

## Planned Features

| Feature | Description |
|---------|-------------|
| Rich theming | Semantic zones (input/output/error tinting), Claude Code-aware themes, per-project auto-switching, hot-reloadable CSS-like config layer |
| Rich search history | Directory-aware filtering, exit code tracking, timestamps + duration, output preview snippets, frecent ranking |
| Workspaces & layouts | Named workspaces with pre-configured tab/split layouts, layout persistence across restarts, project-aware tab grouping |
| Command blocks | Visually group each command + output as a discrete unit — collapsible, copyable, re-runnable |
| Clickable paths & URLs | Detect file paths and URLs in output, cmd-click to open — expanded detection for stack traces, compiler errors, test failures |
| Smart notifications | System notification when a long-running command finishes in a background tab |
| Inline images | Polish existing iTerm2/Kitty image protocol support for matplotlib, image previews, etc. |
| Shell integration hooks | ZLE widgets and precmd/preexec hooks for structured shell-to-terminal communication |

## Bugs

| Resource | Location |
|----------|----------|
| Bug Log | [bugs/log.md](bugs/log.md) |
