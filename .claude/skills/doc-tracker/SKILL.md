---
name: doc-tracker
description: Format and structure guidelines for feature specs and bug docs in .readme/. Use when creating or updating documentation.
---

## Documentation Structure

All documentation lives in `.readme/`:
- **`index.md`** — master index with implemented and planned feature tables
- **`features/`** — one file per feature (planned or complete)
- **`bugs/`** — `log.md` for quick fixes, individual files for complex bugs

## Feature Spec Format

Feature specs are stored in `.readme/features/` as separate per-feature markdown documents.

### File Naming

- Use kebab-case slugs (e.g., `fuzzy-cd-picker.md`, `gpu-renderer.md`)
- Add new files to the appropriate table (Implemented or Planned) in `index.md`

### Document Structure

Each spec should include:

1. **Title + Status** — feature name as H1, status line (e.g., `**Status: Done**`)
2. **Overview** — 1-2 sentence summary of what the feature does
3. **Decisions** — table of key design decisions and why
4. **Architecture** — key crate paths, modules, and what they do
5. **Verification** — checklist of expected behaviors (checked when confirmed working)

Not every section is required — use what's relevant to the feature.

### Feature Status Values

- **Done** — fully implemented, no open work
- **Active** — implemented but has new planned scope (document new work in the spec)
- **Not started** — planned but not yet built

When new work is planned for a Done feature, set status to Active and add the new scope to the spec. When it ships, set it back to Done. The index doesn't change — the spec self-reports its status.

To find features with open work: `grep -l "Status.*Active" .readme/features/`

## Bug Documentation Format

### Quick Fixes — `bugs/log.md`

Add a row to the table for simple bugs that don't need investigation docs:

```markdown
| 2026-04-16 | Cursor blink rate ignores config | Renderer | Fixed default timer | Closed |
```

### Complex Bugs — Individual Files

For bugs requiring root-cause analysis, create a file in `.readme/bugs/` (e.g., `gpu-crash-on-resize.md`):

1. **Title** — bug name as H1
2. **Status / Area / Severity** — metadata block
3. **Description** — what happens and when
4. **Root cause** — why it happens
5. **Fix** — what was changed
6. **Key files** — affected source files

Link to the individual file from the table in `bugs/log.md`.

## Guidelines

- Always read the existing doc before modifying it
- Make surgical edits rather than rewriting entire sections
- Add notes about why decisions changed when context is useful
- If a change affects multiple features, update all affected specs
- Verification items should describe user-facing behavior, not implementation details
