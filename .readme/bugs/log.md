# Bug Log

All bugs are tracked here. If a bug needs root-cause analysis or detailed investigation notes, create a separate file in this directory and link to it from the table.

## Open

| Date | Bug | Notes |
| 2026-04-17 | Context-dropdown picker: keystrokes batch intermittently — user types a char, nothing appears; next keystroke causes both chars to appear at once | Pre-existing since v1 (not a v2 regression). Originally attributed to the overlay-pane input/render plumbing (TermWizTerminal ↔ GUI repaint signalling). **Likely obsolete:** v3 migrated the picker off the overlay-pane onto the `Modal` path, and the shared PopupPicker now echoes input through the PTY directly — both cited code paths (`picker.rs` / TermWizTerminal) have been deleted. Needs re-observation on the new stack before being closed. |

## Closed

(none yet)
