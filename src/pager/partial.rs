//! How much of a partly laid-out document an action needs.
//!
//! A long document is laid out a few blocks at a time ([`LayoutJob`]):
//! the first screen before the first frame, the rest in idle slices. The
//! shell asks [`action_need`] before every action and [`relayout_need`]
//! before every new layout, and lays out at least that much first. Moving
//! around near the top (lines, pages, sideways, `za`) only needs the lines
//! around the screen; anything that looks at the whole document (`G`,
//! `%`, search, the outline and the sidebar, links, marks, hints, Visual
//! mode, counts) finishes the layout first. A partial layout is the start
//! of the whole one, line for line, so what is on screen never changes
//! when the rest comes in.
//!
//! [`LayoutJob`]: crate::layout::LayoutJob

use crate::ir::SrcPos;
use crate::layout::Layout;

use super::keymap::{Command, Context};
use super::state::{Goto, Mode, State};
use super::term::{KeyCode, MouseKind};
use super::update::Action;

/// Screens laid out beyond the top line before an action that moves by at
/// most a screen runs (a page down, and a screen to show after it).
const SCREENS_AHEAD: usize = 3;

/// How much of the layout is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Need {
    /// At least this many lines (fewer if the document is shorter).
    Lines(usize),
    /// Every line showing positions up to `pos`, and then `ahead` more
    /// lines: a place in the document is found again in a new layout.
    Place { pos: SrcPos, ahead: usize },
    /// The whole layout.
    Whole,
}

impl Need {
    /// Whether `layout` holds what is needed; a predicate that only looks
    /// at the lines (see [`crate::layout::LayoutJob::advance`]). For
    /// [`Need::Place`], `seen` remembers where the place was passed.
    pub(crate) fn met(self, layout: &Layout, seen: &mut Option<usize>) -> bool {
        let n = layout.lines.len();
        match self {
            Need::Lines(lines) => n >= lines,
            Need::Place { pos, ahead } => {
                if seen.is_none() && layout.lines.last().is_some_and(|l| l.pos > pos) {
                    *seen = Some(n);
                }
                seen.is_some_and(|at| n >= at.saturating_add(ahead))
            }
            Need::Whole => false,
        }
    }
}

/// What the first layout of a document needs: a few screens of `rows`.
pub(crate) fn first_screens(rows: u16) -> Need {
    Need::Lines(SCREENS_AHEAD * usize::from(rows.max(1)))
}

/// Lines around the screen: from the top, [`SCREENS_AHEAD`] screens of
/// `rows` (the larger of the current and the new height).
fn around_screen(state: &State, rows: u16) -> Need {
    let rows = usize::from(rows.max(state.rows)).max(1);
    Need::Lines(state.top.saturating_add(SCREENS_AHEAD * rows))
}

/// Whether nothing in the state looks at the whole layout: the plain
/// reading mode with no search, link focus, typed count or sidebar.
fn local(state: &State) -> bool {
    matches!(state.mode, Mode::Normal)
        && state.search.is_none()
        && state.focus.is_none()
        && state.summary_focus.is_none()
        && state.count.is_none()
        && state.awaiting.is_none()
        && state.sidebar_cols() == 0
}

/// What `cmd` needs (in the plain reading mode, with no count).
fn command_need(state: &State, cmd: Command) -> Need {
    use Command as C;
    match cmd {
        C::Count
        | C::LineDown
        | C::LineUp
        | C::PageDown
        | C::PageUp
        | C::HalfDown
        | C::HalfUp
        | C::ScrollLeft
        | C::ScrollRight
        | C::HalfLeft
        | C::HalfRight
        | C::ScrollStart
        | C::ScrollEnd
        | C::ToggleDetails
        | C::ToggleWidth
        | C::CycleImages
        | C::ToggleMouse
        | C::Picker
        | C::Redraw
        | C::Suspend
        | C::Quit => around_screen(state, state.rows),
        C::Top => Need::Lines(SCREENS_AHEAD * usize::from(state.rows.max(1))),
        _ => Need::Whole,
    }
}

/// What `action` needs of the current layout before it runs.
pub(crate) fn action_need(state: &State, action: &Action) -> Need {
    if matches!(state.mode, Mode::Picker(_)) {
        // The picker never looks at the document; what it opens is laid
        // out anew.
        return around_screen(state, state.rows);
    }
    if !local(state) {
        return Need::Whole;
    }
    match action {
        Action::Key(key) => {
            if key.code == KeyCode::Esc && !state.keys.is_empty() {
                // Drops the unfinished sequence.
                return around_screen(state, state.rows);
            }
            let mut seq = state.keys.clone();
            seq.push(*key);
            let found = state.settings.keymap.lookup(Context::Normal, &seq);
            match found.exact {
                _ if found.longer => around_screen(state, state.rows),
                Some(cmd) => command_need(state, cmd),
                // A key bound to nothing does nothing; after a sequence
                // that did not continue, the sequence runs too.
                None if seq.len() == 1 => around_screen(state, state.rows),
                None => Need::Whole,
            }
        }
        Action::KeyTimeout => {
            match state
                .settings
                .keymap
                .lookup(Context::Normal, &state.keys)
                .exact
            {
                Some(cmd) => command_need(state, cmd),
                None => around_screen(state, state.rows),
            }
        }
        Action::Mouse(m) => match m.kind {
            MouseKind::WheelUp
            | MouseKind::WheelDown
            | MouseKind::WheelLeft
            | MouseKind::WheelRight
            | MouseKind::Moved => around_screen(state, state.rows),
            _ => Need::Whole,
        },
        Action::Command(cmd) => command_need(state, *cmd),
        Action::Resize { rows, .. } => around_screen(state, *rows),
        // Another document is shown, or the same one again: laid out anew
        // ([`relayout_need`]).
        Action::Opened { .. } | Action::Switch { .. } | Action::Reloaded(_) => {
            around_screen(state, state.rows)
        }
        Action::Message(_) | Action::Error(_) | Action::ImageMode { .. } => {
            around_screen(state, state.rows)
        }
        _ => Need::Whole,
    }
}

/// What a new layout of the current document needs before it is
/// installed: the place it goes back to, or the lines from the top it
/// keeps; all of it when the reader is doing more than reading on.
pub(crate) fn relayout_need(state: &State) -> Need {
    if !local(state) {
        return Need::Whole;
    }
    let ahead = SCREENS_AHEAD * usize::from(state.rows.max(1));
    match &state.pending {
        Some(Goto::Place(place)) => Need::Place {
            pos: place.pos,
            ahead: ahead.saturating_add(place.extra as usize),
        },
        Some(Goto::Top) => Need::Lines(ahead),
        None => Need::Lines(state.top.saturating_add(ahead)),
        Some(_) => Need::Whole,
    }
}
