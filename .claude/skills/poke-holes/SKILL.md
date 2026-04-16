---
name: poke-holes
description: Principal engineer review of recent changes — validate approach, find holes, catch missed edge cases. Use after implementing a feature or before merging.
user_invocable: true
---

## Instructions

Review the most recent changes from the perspective of a principal engineer specializing in systems programming and terminal emulator internals. Your job is to stress-test the approach, not rubber-stamp it.

### Steps

1. **Identify what changed** — Use `git diff main...HEAD` (or `git diff HEAD~1` if already on main) and read the feature spec if one exists. Understand the full scope: new files, modified files, tests.

2. **Read every changed file in full** — Don't skim. Read the actual implementation, not just the diff. Understand how it integrates with surrounding code.

3. **Review as a principal engineer** — Evaluate across these dimensions:

   **Correctness**
   - Does the implementation actually do what the spec/plan says?
   - Are there logic errors, off-by-one issues, or incorrect assumptions?
   - Are race conditions possible (concurrent PTY reads, async rendering, event ordering)?

   **Edge cases & failure modes**
   - What happens at boundaries? (huge scrollback, zero-size terminal, rapid resize, unicode edge cases)
   - What happens when things go wrong? (PTY EOF, GPU context lost, font missing, permission denied)
   - Are there states the terminal can get into that weren't considered?

   **Architecture & integration**
   - Does this fit the existing WezTerm patterns, or does it introduce unnecessary divergence?
   - Are there existing utilities or abstractions that should have been reused?
   - Does it create coupling that will cause pain later?
   - Does this work on both macOS and Linux?

   **Platform & rendering**
   - Are there platform-specific assumptions that break cross-platform?
   - Could this cause rendering glitches, dropped frames, or GPU memory leaks?
   - Are escape sequences handled correctly per the xterm spec?

   **Security**
   - Any new attack surface? (escape sequence injection, PTY command injection, malicious OSC payloads)
   - Is user input validated at the boundary?

   **Performance**
   - Will this cause frame drops during fast output (e.g., `cat` on a large file)?
   - Are there unnecessary allocations in hot paths?
   - Could this degrade with large scrollback buffers or many open tabs?

   **Testing gaps**
   - What isn't tested that should be?
   - Are the tests actually testing meaningful behavior, or just confirming the code compiles?

   **What's missing entirely**
   - Are there requirements from the spec/plan that weren't implemented?
   - Is anything left in an inconsistent state?

4. **Be specific and actionable** — Don't say "consider error handling." Say exactly what error case is unhandled and what the consequence is. Reference specific files and line numbers.

### Output Format

```
## Principal Engineer Review: <feature/change name>

### Verdict: <APPROVE / CONCERNS / BLOCK>

### What's solid
- Brief bullets on what's done well (keep short)

### Issues

#### Critical (blocks merge)
- **[File:line]** Description of the issue, why it matters, and what to do about it

#### Significant (should fix)
- **[File:line]** Description and recommendation

#### Minor (nice to have)
- **[File:line]** Description

### Missing
- Things that weren't implemented but should have been
- Edge cases with no coverage
- Tests that should exist

### Questions for the author
- Anything where intent is unclear and the answer affects the review
```

Use the **Critical / Significant / Minor** tiers honestly:
- **Critical** = will cause bugs, crashes, security issues, or breaks existing behavior. Must fix before merge.
- **Significant** = won't break things immediately but creates real risk, tech debt, or poor UX. Should fix now.
- **Minor** = style, naming, small improvements. Fine to defer.

If the changes look solid, say so clearly and keep the review short. Don't manufacture issues to seem thorough. An honest "this looks good, ship it" with 1-2 minor notes is more valuable than a padded review.
