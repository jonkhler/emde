//! The outline sidebar (`T`): the headings on the left of the text, the
//! current section highlighted.
//!
//! # Showing it
//!
//! `[pager] toc_sidebar` decides ([`cols`]): `auto` shows the sidebar when
//! the terminal has room for it beside the text column at its full width
//! (`toc_width + max_width + 2·margin + 4` columns, [`auto_cols`]), `always`
//! whenever the text keeps [`MIN_TEXT_COLS`] columns beside it, `never`
//! only when `T` asks for it. `T` overrides the setting for the rest of the
//! session; a resize re-evaluates `auto`. A document without headings gets
//! no sidebar. The document is laid out for the columns that are left
//! ([`super::State::doc_cols`]), so centring and wrapping happen in them.
//!
//! # Scroll-spy
//!
//! The current section ([`current`]) is the last heading at or above the
//! top line, as in the status bar's breadcrumb (at the very bottom of the
//! document: at or above the top third of the screen, so the last short
//! sections can be current too). A jump to a heading puts it on the top
//! line, so the entry clicked is the entry marked. Headings inside
//! collapsed `<details>` are listed dimmed (going to one opens them), but
//! never current. The sidebar scrolls itself to keep it in view
//! ([`follow`], after every update) unless the reader scrolled the sidebar
//! with the wheel since the current section last changed; with the keys in
//! the sidebar (`T` a second time) the selected entry is kept in view
//! instead.

use crate::ir::{Document, HeadingId};
use crate::options::When;

use super::state::{Mode, SidebarSettings, State};

/// Columns the text keeps beside the sidebar (it is hidden on narrower
/// terminals).
pub(crate) const MIN_TEXT_COLS: u16 = 40;
/// Columns to spare beside the text column (both sides together) before
/// `auto` shows the sidebar.
const AUTO_SPARE: u16 = 4;
/// The measure `auto` assumes when `max_width` is 0 (no cap).
const NOMINAL_MEASURE: u16 = 100;
/// Most columns an entry is indented by its level.
const MAX_INDENT: usize = 8;
/// Entries kept in view around the current (or selected) one.
const CONTEXT: usize = 2;

/// The terminal width from which `auto` shows a sidebar `width` columns
/// wide beside a text column of `max_width` (0: no cap) with `margin`
/// columns on each side.
pub(crate) fn auto_cols(width: u16, max_width: u16, margin: u16) -> u16 {
    let measure = if max_width == 0 {
        NOMINAL_MEASURE
    } else {
        max_width
    };
    width
        .saturating_add(measure)
        .saturating_add(margin.saturating_mul(2))
        .saturating_add(AUTO_SPARE)
}

/// Columns the sidebar takes on a terminal `cols` wide (0: not shown), for
/// `doc` with the settings `s` and what `T` asked for (`toggled`).
pub(crate) fn cols(s: &SidebarSettings, toggled: Option<bool>, cols: u16, doc: &Document) -> u16 {
    if doc.headings.is_empty() || !fits(s, cols) {
        return 0;
    }
    let wanted = toggled.unwrap_or(match s.when {
        When::Always => true,
        When::Never => false,
        When::Auto => cols >= s.auto_cols,
    });
    if wanted { s.width } else { 0 }
}

/// Whether the sidebar fits on a terminal `cols` wide at all.
pub(crate) fn fits(s: &SidebarSettings, cols: u16) -> bool {
    cols >= s.width.saturating_add(MIN_TEXT_COLS)
}

/// What the sidebar remembers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sidebar {
    /// Shown (`Some(true)`) or hidden by `T`, whatever `toc_sidebar` says.
    pub(crate) toggled: Option<bool>,
    /// The first entry shown.
    pub(crate) scroll: usize,
    /// The reader scrolled the sidebar (the wheel) since the current entry
    /// last changed: it is left where it is.
    pub(crate) manual: bool,
    /// The current entry when it was last followed.
    pub(crate) spied: Option<usize>,
    /// The heading last jumped to: current at the bottom of the document
    /// while it is on screen (a jump there cannot put it on the top line).
    pub(crate) jumped: Option<HeadingId>,
}

/// The current section's entry: the last heading at or above the top line,
/// as the status bar's breadcrumb has it (`None` above the first heading).
/// At the bottom of the document, where the last sections can never reach
/// the top line: the heading last jumped to while it is on screen, else the
/// last heading at or above the top third of the screen.
pub(crate) fn current(state: &State) -> Option<usize> {
    if state.top < state.max_top() || state.top == 0 {
        return state.derived.entry_at(state.top);
    }
    let rows = state.view_rows();
    let visible = |line: u32| (state.top..state.top + rows).contains(&(line as usize));
    let headings = &state.derived.headings;
    let jumped = state.sidebar.jumped.and_then(|h| {
        (0..headings.len())
            .find(|&i| headings[i].1 == h && visible(headings[i].0) && !state.derived.is_folded(i))
    });
    jumped.or_else(|| state.derived.entry_at(state.top + rows / 3))
}

/// Keep the right entry in view after an update: the selected one with the
/// keys in the sidebar, else the current one (unless the reader scrolled
/// the sidebar since it last changed). Leaves the keys' sidebar mode when
/// the sidebar is gone.
pub(crate) fn follow(state: &mut State) {
    if state.sidebar_cols() == 0 {
        if matches!(state.mode, Mode::Sidebar(_)) {
            state.mode = Mode::Normal;
        }
        return;
    }
    let n = state.derived.headings.len();
    let rows = state.view_rows().max(1);
    if let Mode::Sidebar(i) = &mut state.mode {
        // A new version of the document may have fewer headings.
        *i = (*i).min(n.saturating_sub(1));
    }
    let cur = current(state);
    let sb = &mut state.sidebar;
    if cur != sb.spied {
        sb.spied = cur;
        sb.manual = false;
    }
    let keep = match state.mode {
        Mode::Sidebar(selected) => Some(selected),
        _ if !sb.manual => cur,
        _ => None,
    };
    if let Some(i) = keep {
        sb.scroll = scroll_near(sb.scroll, i, rows);
    }
    sb.scroll = sb.scroll.min(n.saturating_sub(rows));
}

/// The first entry to show so that entry `i` is in view with up to
/// [`CONTEXT`] entries around it (as little scrolling as that takes).
fn scroll_near(scroll: usize, i: usize, rows: usize) -> usize {
    let margin = CONTEXT.min(rows.saturating_sub(1) / 2);
    if i < scroll.saturating_add(margin) {
        i.saturating_sub(margin)
    } else if i.saturating_add(margin) >= scroll.saturating_add(rows) {
        (i + margin + 1).saturating_sub(rows)
    } else {
        scroll
    }
}

/// Scroll the sidebar by `delta` entries (the wheel).
pub(crate) fn scroll(state: &mut State, delta: isize) {
    let n = state.derived.headings.len();
    let rows = state.view_rows().max(1);
    let sb = &mut state.sidebar;
    sb.scroll = if delta >= 0 {
        sb.scroll.saturating_add(delta.unsigned_abs())
    } else {
        sb.scroll.saturating_sub(delta.unsigned_abs())
    }
    .min(n.saturating_sub(rows));
    sb.manual = true;
}

/// The entry on screen row `row` of the sidebar, if there is one.
pub(crate) fn entry_at(state: &State, row: usize) -> Option<usize> {
    let i = state.sidebar.scroll.checked_add(row)?;
    (row < state.view_rows() && i < state.derived.headings.len()).then_some(i)
}

/// How far entry `level` is indented below the outermost level `base`.
pub(crate) fn indent(level: u8, base: u8) -> usize {
    (usize::from(level.saturating_sub(base)) * 2).min(MAX_INDENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{ParseOptions, parse};

    fn settings(when: When) -> SidebarSettings {
        SidebarSettings {
            when,
            width: 32,
            auto_cols: auto_cols(32, 100, 2),
        }
    }

    #[test]
    fn when_the_sidebar_shows() {
        assert_eq!(auto_cols(32, 100, 2), 140);
        assert_eq!(auto_cols(32, 0, 2), 140, "no cap counts as 100");
        assert_eq!(auto_cols(60, u16::MAX, 9), u16::MAX);
        let doc = parse("# A\n\ntext\n", &ParseOptions::default());
        let none = parse("just text\n", &ParseOptions::default());
        let auto = settings(When::Auto);
        assert_eq!(cols(&auto, None, 140, &doc), 32);
        assert_eq!(cols(&auto, None, 139, &doc), 0);
        assert_eq!(cols(&auto, None, 200, &none), 0, "no headings");
        // `T` wins where it fits.
        assert_eq!(cols(&auto, Some(true), 80, &doc), 32);
        assert_eq!(cols(&auto, Some(true), 71, &doc), 0, "too narrow");
        assert_eq!(cols(&auto, Some(false), 200, &doc), 0);
        assert_eq!(cols(&settings(When::Always), None, 72, &doc), 32);
        assert_eq!(cols(&settings(When::Always), None, 0, &doc), 0);
        assert_eq!(cols(&settings(When::Never), None, 300, &doc), 0);
        assert_eq!(cols(&settings(When::Never), Some(true), 300, &doc), 32);
    }

    #[test]
    fn scrolling_keeps_context() {
        assert_eq!(scroll_near(0, 0, 10), 0);
        assert_eq!(scroll_near(0, 7, 10), 0);
        assert_eq!(scroll_near(0, 8, 10), 1);
        assert_eq!(scroll_near(5, 6, 10), 4);
        assert_eq!(scroll_near(5, 1, 10), 0);
        // Too few rows for context: just in view.
        assert_eq!(scroll_near(0, 3, 1), 3);
        assert_eq!(scroll_near(0, 3, 2), 2);
        assert_eq!(scroll_near(9, 3, 2), 3);
    }

    #[test]
    fn indentation_is_capped() {
        assert_eq!(indent(1, 1), 0);
        assert_eq!(indent(3, 2), 2);
        assert_eq!(indent(6, 1), MAX_INDENT);
        assert_eq!(indent(1, 3), 0);
    }
}
