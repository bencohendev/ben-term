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
    active: usize,
    top: usize,
    max_items: usize,
    title: String,
    description: String,
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

        term.render(&changes)?;
        let _ = self.title; // kept for future Change::Title use
        Ok(())
    }
}

/// Runs the picker loop. Returns `Some(entry)` on select, `None` on cancel.
pub fn run(mut term: TermWizTerminal, params: PickerParams) -> anyhow::Result<Option<PickerEntry>> {
    let mut state = State {
        entries: params.entries,
        filtered: vec![],
        filter: params.initial_filter,
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
                if state.filter.pop().is_some() {
                    state.update_filter();
                }
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char(c),
                modifiers,
            }) if modifiers == Modifiers::NONE || modifiers == Modifiers::SHIFT => {
                state.filter.push(c);
                state.update_filter();
            }
            _ => {}
        }
        state.render(&mut term)?;
    }

    Ok(picked)
}
