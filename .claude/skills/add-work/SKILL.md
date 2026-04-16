---
name: add-work
description: Triage a list of work items into features, bugs, and quick fixes, then route each to the right place in .readme/. Use when adding a batch of work items.
argument-hint: "[list of work items]"
user_invocable: true
---

# Add Work

Take a freeform list of work items, classify each one, and route it to the correct documentation location.

## Arguments

- `$ARGUMENTS` — a freeform list of work items (natural language, bullet points, comma-separated, etc.)

## Steps

### 1. Read current state

Read these files to understand what already exists:

- `.readme/index.md` — master index, implemented and planned tables
- `.readme/bugs/log.md` — bug log Open section

Also check for an existing quick-fixes spec with `Not started` or `Active` status:

```
grep -rl "quick-fixes" .readme/features/
```

If a quick-fixes spec exists and is `Done`, quick fixes will go into a **new** quick-fixes file (e.g. `quick-fixes-2.md`, incrementing the number). If one exists and is `Not started` or `Active`, append to it.

### 2. Classify each item

For each work item in the input, determine:

**Category** — one of:
- **Feature** — a new capability, significant enhancement, or standalone piece of work that would warrant its own spec or Planned table row
- **Bug** — something that's broken, behaving incorrectly, or regressed
- **Quick fix** — small polish, minor behavioral improvement, or nit too small for its own spec

### 3. Present for approval

Display the classification as a table for user review **before writing anything**:

```
## Proposed Classification

| # | Item | Category | Destination |
|---|------|----------|-------------|
| 1 | Item description | Feature | Planned table |
| 2 | Item description | Bug | bugs/log.md Open |
| 3 | Item description | Quick fix | quick-fixes-N.md |
```

Ask the user to confirm or request changes. Do not proceed until approved.

### 4. Write to destinations

After approval, make the edits:

#### Features → Planned table

Add a new row to the **Planned Features** table in `.readme/index.md`:

```markdown
| Feature Name | One-line description |
```

- No spec link (bare text) — these are ideas without specs
- Place at the end of the Planned table

#### Bugs → Bug log

Add a new row to the **Open** section of `.readme/bugs/log.md`:

```markdown
| {today's date YYYY-MM-DD} | Bug description | Notes if any |
```

- Use today's date
- Keep descriptions concise

#### Quick fixes → Quick fixes spec

If creating a **new** quick-fixes spec (because the existing one is `Done` or none exists):

1. Create `.readme/features/quick-fixes-{N}.md` where N increments from the last one (or omit the number if this is the first)
2. Use this format:

```markdown
**Status:** Not started

# Quick Fixes {N}

A batch of small polish and minor improvements too small to warrant individual specs.

---

## {Category heading}

- **Item name** — Brief description
```

3. Add to `.readme/index.md` Implemented table (since it's a trackable spec):

```markdown
| [Quick Fixes {N}](features/quick-fixes-{N}.md) | Batch of small polish and minor improvements |
```

If **appending** to an existing active quick-fixes spec:

1. Add new items under the most appropriate existing section heading, or create a new section if none fits
2. Items should follow the existing format: `- **Item name** — Brief description`

### 5. Summary

After writing, print a short summary of what was added and where:

```
Done. Added:
- N features to Planned table
- N bugs to bug log
- N quick fixes to {spec file}
```
