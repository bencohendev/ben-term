# Bug Log

All bugs are tracked here. If a bug needs root-cause analysis or detailed investigation notes, create a separate file in this directory and link to it from the table.

## Open

| Date | Bug | Notes |
| 2026-04-17 | Context-dropdown picker: keystrokes batch intermittently — user types a char, nothing appears; next keystroke causes both chars to appear at once | Pre-existing since v1 (not a v2 regression). Likely in the overlay-pane input/render plumbing (TermWizTerminal ↔ GUI repaint signalling), not in `picker.rs` itself — `selector.rs` uses the same `poll_input` + `state.render` loop. Next step: instrument key-event arrival time vs. GUI paint tick to see which is the lagging step. |

## Closed

(none yet)
