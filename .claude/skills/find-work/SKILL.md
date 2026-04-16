---
name: find-work
description: Scan .readme/ for open work items — active features, not-started features, planned items without specs, and open bugs. Use when looking for something to work on.
user_invocable: true
---

## Instructions

Scan the project's `.readme/` documentation to find all available work, then present a concise summary.

### Steps

1. **Active features** — grep for `\*\*Status:\*\*.*Active` recursively in `.readme/`. For each match, read the file to get a one-line description of what work remains.

2. **Not-started features** — grep for `\*\*Status:\*\*.*Not started` recursively in `.readme/`. For each match, read the file to get its overview.

3. **Planned features without specs** — read `.readme/index.md`. Find rows in "Planned" tables that do NOT link to a feature file (no `[...](...)` markdown link).

4. **Open bugs** — read `.readme/bugs/log.md`. List only non-struck-through rows (skip `~~...~~` entries — those are fixed).

### Output Format

Print a summary grouped by category. **Every item must be its own bullet point — never use comma-separated inline lists.**

```
## Active Features
- **Feature Name** — what's left to do

## Not Started (specced but not built)
- **Feature Name** — overview

## Ideas (planned, no spec yet)
- **Item name** — description

## Open Bugs
- **Bug** — description
```

Rules:
- Every item gets its own `- **Name** — description` line — no exceptions
- Omit any section that has zero items
- If everything is empty, say "Nothing open — all features are done and no bugs are tracked."
