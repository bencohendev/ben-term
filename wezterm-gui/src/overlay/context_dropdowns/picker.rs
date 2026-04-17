//! Minimal fuzzy picker for context dropdowns.
//!
//! `selector.rs` is tied to Lua `EmitEvent` callbacks; this variant runs a
//! similar render/input loop but returns a plain `Option<PickerEntry>` so
//! Rust code can consume the selection directly.
//!
//! Reuses the nucleo matcher and `truncate_right` helpers from upstream.

use crate::overlay::selector::{matcher_pattern, matcher_score};
use mux::termwiztermtab::TermWizTerminal;
use rayon::prelude::*;
use termwiz::cell::{AttributeChange, CellAttributes};
use termwiz::color::ColorAttribute;
use termwiz::input::{InputEvent, KeyCode, KeyEvent, Modifiers};
use termwiz::surface::{Change, Position};
use termwiz::terminal::Terminal;
use termwiz_funcs::truncate_right;

const ROW_OVERHEAD: usize = 3;

#[derive(Debug, Clone)]
pub struct PickerEntry {
    /// What's shown in the list.
    pub label: String,
    /// What's returned to the caller on select.
    pub value: String,
}

pub struct PickerParams {
    pub title: String,
    pub description: String,
    pub entries: Vec<PickerEntry>,
    /// Pre-filled filter text (empty = show everything).
    pub initial_filter: String,
}

struct State {
    entries: Vec<PickerEntry>,
    filtered: Vec<usize>,
    filter: String,
    /// Byte offset into `filter` where text insertion / deletion happens.
    /// Anchored at `filter.len()` on startup; `Left`/`Right`/`Home`/`End`
    /// and `Backspace`/`Delete` move or consume around this point.
    filter_cursor: usize,
    active: usize,
    top: usize,
    max_items: usize,
    title: String,
    description: String,
}

fn prev_char_boundary(s: &str, pos: usize) -> usize {
    s[..pos]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn next_char_boundary(s: &str, pos: usize) -> usize {
    s[pos..]
        .chars()
        .next()
        .map(|c| pos + c.len_utf8())
        .unwrap_or(pos)
}

impl State {
    fn update_filter(&mut self) {
        if self.filter.is_empty() {
            self.filtered = (0..self.entries.len()).collect();
        } else {
            let pattern = matcher_pattern(&self.filter);
            let mut scored: Vec<(usize, u32)> = self
                .entries
                .par_iter()
                .enumerate()
                .filter_map(|(idx, e)| matcher_score(&pattern, &e.label).map(|s| (idx, s)))
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1));
            self.filtered = scored.into_iter().map(|(idx, _)| idx).collect();
        }
        self.active = 0;
        self.top = 0;
    }

    fn move_up(&mut self) {
        self.active = self.active.saturating_sub(1);
        if self.active < self.top {
            self.top = self.active;
        }
    }

    fn move_down(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.active = (self.active + 1).min(self.filtered.len() - 1);
        if self.active >= self.top + self.max_items {
            self.top = self.active + 1 - self.max_items;
        }
    }

    fn filter_insert(&mut self, c: char) {
        self.filter.insert(self.filter_cursor, c);
        self.filter_cursor += c.len_utf8();
        self.update_filter();
    }

    fn filter_backspace(&mut self) {
        if self.filter_cursor == 0 {
            return;
        }
        let prev = prev_char_boundary(&self.filter, self.filter_cursor);
        self.filter.drain(prev..self.filter_cursor);
        self.filter_cursor = prev;
        self.update_filter();
    }

    fn filter_delete(&mut self) {
        if self.filter_cursor >= self.filter.len() {
            return;
        }
        let next = next_char_boundary(&self.filter, self.filter_cursor);
        self.filter.drain(self.filter_cursor..next);
        self.update_filter();
    }

    fn filter_left(&mut self) {
        if self.filter_cursor > 0 {
            self.filter_cursor = prev_char_boundary(&self.filter, self.filter_cursor);
        }
    }

    fn filter_right(&mut self) {
        if self.filter_cursor < self.filter.len() {
            self.filter_cursor = next_char_boundary(&self.filter, self.filter_cursor);
        }
    }

    fn filter_home(&mut self) {
        self.filter_cursor = 0;
    }

    fn filter_end(&mut self) {
        self.filter_cursor = self.filter.len();
    }

    fn filter_clear(&mut self) {
        self.filter.clear();
        self.filter_cursor = 0;
        self.update_filter();
    }

    #[allow(clippy::result_large_err)] // `termwiz::Result` is used by every overlay's render
    fn render(&mut self, term: &mut TermWizTerminal) -> termwiz::Result<()> {
        let size = term.get_screen_size()?;
        let max_width = size.cols.saturating_sub(4);
        self.max_items = size.rows.saturating_sub(ROW_OVERHEAD).max(1);

        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(0),
            },
            Change::Text(format!(
                "{} > {}\r\n",
                truncate_right(&self.description, max_width),
                truncate_right(&self.filter, max_width),
            )),
            Change::Text(format!(
                "{}\r\n",
                "─".repeat(max_width.min(size.cols))
            )),
            Change::AllAttributes(CellAttributes::default()),
        ];

        if self.filtered.is_empty() {
            changes.push(Change::Text("(no matches)\r\n".to_string()));
        } else {
            for (row_num, entry_idx) in self
                .filtered
                .iter()
                .enumerate()
                .skip(self.top)
                .take(self.max_items)
                .map(|(i, idx)| (i, *idx))
            {
                if row_num == self.active {
                    changes.push(AttributeChange::Reverse(true).into());
                }
                let label = &self.entries[entry_idx].label;
                changes.push(Change::Text(format!(
                    "  {}\r\n",
                    truncate_right(label, max_width.saturating_sub(2))
                )));
                if row_num == self.active {
                    changes.push(AttributeChange::Reverse(false).into());
                }
            }
        }

        // Position the terminal cursor on the filter line at the user's
        // edit position so Left/Right feel natural. The filter row layout
        // is: "<description> > <filter>" on row 0.
        let desc_truncated = truncate_right(&self.description, max_width);
        let desc_cols = wezterm_term::unicode_column_width(&desc_truncated, None);
        let filter_prefix = &self.filter[..self.filter_cursor];
        let cursor_col = desc_cols
            + 3 // " > "
            + wezterm_term::unicode_column_width(filter_prefix, None);
        changes.push(Change::CursorPosition {
            x: Position::Absolute(cursor_col.min(size.cols.saturating_sub(1))),
            y: Position::Absolute(0),
        });

        term.render(&changes)?;
        let _ = self.title; // kept for future Change::Title use
        Ok(())
    }
}

/// Runs the picker loop. Returns `Some(entry)` on select, `None` on cancel.
pub fn run(mut term: TermWizTerminal, params: PickerParams) -> anyhow::Result<Option<PickerEntry>> {
    let filter_cursor = params.initial_filter.len();
    let mut state = State {
        entries: params.entries,
        filtered: vec![],
        filter: params.initial_filter,
        filter_cursor,
        active: 0,
        top: 0,
        max_items: 1,
        title: params.title.clone(),
        description: params.description,
    };

    term.set_raw_mode()?;
    term.render(&[Change::Title(params.title)])?;
    state.update_filter();
    state.render(&mut term)?;

    let mut picked: Option<PickerEntry> = None;

    while let Ok(Some(event)) = term.poll_input(None) {
        match event {
            InputEvent::Key(KeyEvent {
                key: KeyCode::Escape,
                ..
            })
            | InputEvent::Key(KeyEvent {
                key: KeyCode::Char('G' | 'C'),
                modifiers: Modifiers::CTRL,
            }) => {
                break;
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Enter,
                ..
            }) => {
                if let Some(&idx) = state.filtered.get(state.active) {
                    picked = state.entries.get(idx).cloned();
                }
                break;
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::UpArrow,
                ..
            })
            | InputEvent::Key(KeyEvent {
                key: KeyCode::Char('P' | 'K'),
                modifiers: Modifiers::CTRL,
            }) => {
                state.move_up();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::DownArrow,
                ..
            })
            | InputEvent::Key(KeyEvent {
                key: KeyCode::Char('N' | 'J'),
                modifiers: Modifiers::CTRL,
            }) => {
                state.move_down();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Backspace,
                ..
            }) => {
                state.filter_backspace();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Delete,
                ..
            }) => {
                state.filter_delete();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::LeftArrow,
                ..
            })
            | InputEvent::Key(KeyEvent {
                key: KeyCode::Char('B'),
                modifiers: Modifiers::CTRL,
            }) => {
                state.filter_left();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::RightArrow,
                ..
            })
            | InputEvent::Key(KeyEvent {
                key: KeyCode::Char('F'),
                modifiers: Modifiers::CTRL,
            }) => {
                state.filter_right();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Home,
                ..
            })
            | InputEvent::Key(KeyEvent {
                key: KeyCode::Char('A'),
                modifiers: Modifiers::CTRL,
            }) => {
                state.filter_home();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::End,
                ..
            })
            | InputEvent::Key(KeyEvent {
                key: KeyCode::Char('E'),
                modifiers: Modifiers::CTRL,
            }) => {
                state.filter_end();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('U'),
                modifiers: Modifiers::CTRL,
            }) => {
                state.filter_clear();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char(c),
                modifiers,
            }) if !modifiers.contains(Modifiers::CTRL)
                && !modifiers.contains(Modifiers::ALT)
                && !modifiers.contains(Modifiers::SUPER) =>
            {
                state.filter_insert(c);
            }
            _ => {}
        }
        state.render(&mut term)?;
    }

    Ok(picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_state(filter: &str) -> State {
        State {
            entries: vec![],
            filtered: vec![],
            filter: filter.to_string(),
            filter_cursor: filter.len(),
            active: 0,
            top: 0,
            max_items: 1,
            title: String::new(),
            description: String::new(),
        }
    }

    #[test]
    fn insert_at_cursor_inserts_midway() {
        let mut s = mk_state("foo");
        s.filter_cursor = 1; // between 'f' and 'o'
        s.filter_insert('X');
        assert_eq!(s.filter, "fXoo");
        assert_eq!(s.filter_cursor, 2);
    }

    #[test]
    fn left_right_respect_bounds() {
        let mut s = mk_state("ab");
        assert_eq!(s.filter_cursor, 2);
        s.filter_right();
        assert_eq!(s.filter_cursor, 2, "right at end is a no-op");
        s.filter_left();
        s.filter_left();
        s.filter_left();
        assert_eq!(s.filter_cursor, 0, "left at start saturates to 0");
    }

    #[test]
    fn backspace_removes_before_cursor() {
        let mut s = mk_state("hello");
        s.filter_cursor = 3; // between 'l' and 'l'
        s.filter_backspace();
        assert_eq!(s.filter, "helo");
        assert_eq!(s.filter_cursor, 2);
    }

    #[test]
    fn delete_removes_at_cursor() {
        let mut s = mk_state("hello");
        s.filter_cursor = 2;
        s.filter_delete();
        assert_eq!(s.filter, "helo");
        assert_eq!(s.filter_cursor, 2, "delete leaves cursor in place");
    }

    #[test]
    fn home_end_jump_to_boundaries() {
        let mut s = mk_state("abcdef");
        s.filter_home();
        assert_eq!(s.filter_cursor, 0);
        s.filter_end();
        assert_eq!(s.filter_cursor, 6);
    }

    #[test]
    fn ctrl_u_clears_and_resets_cursor() {
        let mut s = mk_state("abcdef");
        s.filter_clear();
        assert_eq!(s.filter, "");
        assert_eq!(s.filter_cursor, 0);
    }

    /// Multi-byte chars: `é` is 2 bytes. Cursor moves must land on char
    /// boundaries or filter.insert / drain panics at runtime.
    #[test]
    fn handles_multibyte_chars() {
        let mut s = mk_state("café");
        assert_eq!(s.filter.len(), 5);
        s.filter_left();
        // é starts at byte 3 in "café".
        assert_eq!(s.filter_cursor, 3);
        s.filter_backspace();
        assert_eq!(s.filter, "caé");
        assert_eq!(s.filter_cursor, 2);
    }
}
