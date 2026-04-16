---
name: update-docs
description: Audit and update all documentation after a feature is completed or changed. Use after finishing implementation work.
user_invocable: true
---

## Instructions

Audit all documentation layers for the most recently completed work, identify what's stale or missing, and make the updates.

### Step 1: Identify the work

Determine what was recently completed. Use these signals in priority order:

1. **User context** — if the user says what feature/fix to document, use that
2. **Git history** — run `git log --oneline -20` and `git diff main~5...main --stat` to find recently changed source files
3. **Active specs** — grep for `^\*\*Status:\*\*.*Active` in `.readme/` to find in-progress features

Read the relevant source code to understand what was built. Build a mental model of the feature before touching any docs.

### Step 2: Audit all documentation layers

Check each layer below. Read every file before editing. For each layer, determine: does it exist? Is it accurate? Is it complete?

#### A. Feature spec (`.readme/features/`)

- **If no spec exists:** create one following the format in the `doc-tracker` skill (title + status, overview, key decisions, architecture with file/crate paths, verification)
- **If spec exists:** update it to match the current implementation — correct file paths, accurate architecture description, check off verification items
- **Set status** to `Done` (or `Active` if there's remaining scope)

#### B. Master index (`.readme/index.md`)

- Ensure the feature is in the **Implemented** table with a link to the spec
- If it was in the **Planned** table, remove it from there
- Check that the description is accurate

#### C. Cross-references

Search for stale references to the feature across all docs:
- Grep `.readme/` for the feature name, related module names, and key terms
- Check that any references in other specs or bug logs are still accurate
- If the feature was mentioned as "planned" or "future work" somewhere, update that reference

### Step 3: Make the updates

- Enter a worktree before making any edits
- Make surgical edits — don't rewrite sections that are already correct
- Read each file before editing it
- Follow existing formatting conventions in each file

### Step 4: Present the summary

After all updates are made, print a summary:

```
## Documentation Updated

### Feature: <name>

| Layer | Action |
|-------|--------|
| Feature spec | Created / Updated / Already current |
| Master index | Updated / Already current |
| Cross-references | Fixed N stale references / None found |

### Files changed
- path/to/file — brief description of change
```
