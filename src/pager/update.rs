//! `update(&mut State, Action) -> Vec<Effect>`: everything the reader does,
//! as a pure function.
//!
//! Input ([`Action::Key`], [`Action::Mouse`]) goes through the keymap to a
//! [`Command`]; commands change the state and return [`Effect`]s for the
//! shell, which does the I/O (loading and laying out documents, opening
//! URLs, the clipboard, terminal modes) and reports back with more actions
//! ([`Action::Layout`], [`Action::Opened`], …).

use std::mem;
use std::path::PathBuf;
use std::rc::Rc;

use crate::layout::Layout;

use super::PagerDoc;
use super::keymap::{self, Command, Context};
use super::links::{self, Follow};
use super::search::Search;
use super::state::{
    Derived, DocKey, Focus, Goto, Hints, Mode, Outline, Page, Place, Prompt, State, Visit,
};
use super::term::{Button, Key, KeyCode, Mouse, MouseKind};
use super::toc;

/// Largest count prefix.
const MAX_COUNT: u32 = 1_000_000;

/// Something that happened, for [`update`].
#[derive(Clone, Debug)]
pub enum Action {
    /// A key press.
    Key(Key),
    /// A mouse event.
    Mouse(Mouse),
    /// Pasted text (typed into a prompt or filter).
    Paste(String),
    /// A command, as if its key had been pressed.
    Command(Command),
    /// The terminal has a new size (after debouncing).
    Resize { cols: u16, rows: u16 },
    /// The layout the shell made after [`Effect::Relayout`].
    Layout(Layout),
    /// Show an anchor without recording history (the first document's
    /// `FILE#anchor`).
    Anchor(String),
    /// A document was read for [`Effect::Load`]. `key` identifies it
    /// (`None`: no file behind it).
    Opened {
        doc: PagerDoc,
        key: Option<DocKey>,
        request: LoadRequest,
    },
    /// [`Effect::Load`] asked for a document that is already in memory.
    Switch { key: DocKey, request: LoadRequest },
    /// [`Effect::Load`] failed: say why (and drop a back or forward
    /// entry that leads nowhere now).
    LoadFailed { request: LoadRequest, error: String },
    /// The current file was read again and changed.
    Reloaded(PagerDoc),
    /// Show a message.
    Message(String),
    /// Show an error.
    Error(String),
}

/// Something for the shell to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Leave the pager.
    Quit,
    /// Open an external URL (or copy it where opening is not possible).
    Open(String),
    /// Read another document.
    Load(LoadRequest),
    /// A local file that is not Markdown: show its path (a directory is
    /// loaded through its README).
    ShowFile(PathBuf),
    /// Put text on the clipboard.
    Copy(String),
    /// Read the current file again.
    Reload,
    /// Turn mouse reporting on or off.
    SetMouse(bool),
    /// Stop the process (Ctrl-Z).
    Suspend,
    /// Lay the current document out again and send [`Action::Layout`].
    Relayout,
    /// Repaint the whole screen.
    Redraw,
    /// Switch to the next image mode (a [`Effect::Relayout`] follows).
    CycleImages,
}

/// How a load moves through the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    /// Following a link: a new entry.
    Push,
    /// Going back.
    Back,
    /// Going forward.
    Forward,
}

/// A document to read, and where to go in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadRequest {
    /// The file or directory (relative paths are relative to the current
    /// document's directory already).
    pub path: PathBuf,
    /// A heading or anchor to show.
    pub anchor: Option<String>,
    /// A place to show (back and forward).
    pub restore: Option<Place>,
    pub nav: Nav,
}

/// Apply an action; see the module docs.
pub fn update(state: &mut State, action: Action) -> Vec<Effect> {
    match action {
        Action::Key(key) => {
            state.message = None;
            on_key(state, key)
        }
        Action::Mouse(m) => on_mouse(state, m),
        Action::Paste(text) => {
            type_text(state, &text);
            Vec::new()
        }
        Action::Command(cmd) => {
            state.message = None;
            command(state, cmd)
        }
        Action::Resize { cols, rows } => resize(state, cols, rows),
        Action::Layout(layout) => {
            install(state, layout);
            Vec::new()
        }
        Action::Anchor(name) => {
            go(state, Goto::Anchor(name));
            Vec::new()
        }
        Action::Opened { doc, key, request } => {
            let page = state.new_page(doc, key);
            switch(state, page, request)
        }
        Action::Switch { key, request } => match state.history.page(&key) {
            Some(page) => switch(state, page, request),
            None => {
                state.complain("that document is no longer in memory");
                Vec::new()
            }
        },
        Action::LoadFailed { request, error } => {
            match request.nav {
                Nav::Back => {
                    state.history.back.pop();
                }
                Nav::Forward => {
                    state.history.forward.pop();
                }
                Nav::Push => {}
            }
            state.complain(error);
            Vec::new()
        }
        Action::Reloaded(doc) => reloaded(state, doc),
        Action::Message(text) => {
            state.say(text);
            Vec::new()
        }
        Action::Error(text) => {
            state.complain(text);
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

fn context(mode: &Mode) -> Context {
    match mode {
        Mode::Normal => Context::Normal,
        Mode::Prompt(_) => Context::Prompt,
        Mode::Outline(_) => Context::Outline,
        Mode::Help { .. } => Context::Help,
        Mode::Hints(_) => Context::Hints,
    }
}

fn on_key(state: &mut State, key: Key) -> Vec<Effect> {
    let cmd = keymap::lookup(context(&state.mode), key);
    match (&state.mode, cmd) {
        (Mode::Normal, Some(Command::Count)) => {
            if let KeyCode::Char(c) = key.code
                && let Some(d) = c.to_digit(10)
            {
                let n = state
                    .count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(d);
                state.count = Some(n.min(MAX_COUNT));
            }
            Vec::new()
        }
        (_, Some(cmd)) => command(state, cmd),
        (Mode::Normal, None) => {
            state.count = None;
            Vec::new()
        }
        (Mode::Prompt(_) | Mode::Outline(_) | Mode::Hints(_), None) => match key.text() {
            Some(c) => {
                let mut buf = [0u8; 4];
                type_text(state, c.encode_utf8(&mut buf))
            }
            None if matches!(state.mode, Mode::Hints(_)) => {
                state.mode = Mode::Normal;
                Vec::new()
            }
            None => Vec::new(),
        },
        (Mode::Help { .. }, None) => Vec::new(),
    }
}

/// Text typed (or pasted) into the prompt, the outline filter or hints.
fn type_text(state: &mut State, text: &str) -> Vec<Effect> {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    if clean.is_empty() {
        return Vec::new();
    }
    match &mut state.mode {
        Mode::Prompt(p) => {
            p.input.push_str(&clean);
            incremental(state);
            Vec::new()
        }
        Mode::Outline(o) => {
            o.filter.push_str(&clean);
            refilter(state);
            Vec::new()
        }
        Mode::Hints(h) => {
            h.typed.push_str(&clean);
            hint_typed(state)
        }
        Mode::Normal | Mode::Help { .. } => Vec::new(),
    }
}

/// `n` lines down (or up, negative), saturating.
fn lines(n: usize, down: bool) -> isize {
    let n = isize::try_from(n).unwrap_or(isize::MAX);
    if down { n } else { -n }
}

/// Run a command (taking any count typed before it).
fn command(state: &mut State, cmd: Command) -> Vec<Effect> {
    let count = state.count.take().map(|c| c as usize);
    let n = count.unwrap_or(1).max(1);
    let page = state.view_rows().max(1).saturating_mul(n);
    let half = (state.view_rows() / 2).max(1).saturating_mul(n);
    match cmd {
        Command::Count => {}
        Command::LineDown => scroll(state, lines(n, true)),
        Command::LineUp => scroll(state, lines(n, false)),
        Command::PageDown => scroll(state, lines(page, true)),
        Command::PageUp => scroll(state, lines(page, false)),
        Command::HalfDown => scroll(state, lines(half, true)),
        Command::HalfUp => scroll(state, lines(half, false)),
        Command::Top => state.jump_to(count.map_or(0, |c| c.saturating_sub(1))),
        Command::Bottom => match count {
            Some(c) => state.jump_to(c.saturating_sub(1)),
            None => state.top = state.max_top(),
        },
        Command::Percent => {
            let pct = count.unwrap_or(0).min(100);
            state.jump_to(state.layout.len().saturating_mul(pct) / 100);
        }
        Command::NextHeading => heading_jump(state, n, true, false),
        Command::PrevHeading => heading_jump(state, n, false, false),
        Command::NextSection => heading_jump(state, n, true, true),
        Command::PrevSection => heading_jump(state, n, false, true),
        Command::Outline => open_outline(state),
        Command::SearchForward => open_prompt(state, false),
        Command::SearchBackward => open_prompt(state, true),
        Command::NextMatch => next_match(state, n, false),
        Command::PrevMatch => next_match(state, n, true),
        Command::ClearSearch => {
            if state.search.take().is_none() {
                state.focus = None;
            }
        }
        Command::FocusNext => focus_step(state, n, true),
        Command::FocusPrev => focus_step(state, n, false),
        Command::Follow => return follow_focus(state),
        Command::Hints => open_hints(state),
        Command::Back => return go_history(state, Nav::Back),
        Command::Forward => return go_history(state, Nav::Forward),
        Command::CopyUrl => return copy_focus(state),
        Command::Reload => {
            if state.page.path().is_some() {
                return vec![Effect::Reload];
            }
            state.complain("standard input cannot be reloaded");
        }
        Command::ToggleWatch => {
            if state.page.path().is_none() {
                state.complain("standard input cannot be watched");
            } else {
                state.watch = !state.watch;
                state.say(if state.watch {
                    "watching the file for changes"
                } else {
                    "not watching the file"
                });
            }
        }
        Command::CycleImages => {
            state.pending = Some(Goto::Place(state.top_place()));
            return vec![Effect::CycleImages, Effect::Relayout];
        }
        Command::ToggleWidth => {
            state.wide = !state.wide;
            state.pending = Some(Goto::Place(state.top_place()));
            state.say(if state.wide {
                "full width"
            } else {
                "limited width"
            });
            return vec![Effect::Relayout];
        }
        Command::ToggleMouse => {
            state.mouse = !state.mouse;
            state.say(if state.mouse {
                "mouse on"
            } else {
                "mouse off: the terminal selects text"
            });
            return vec![Effect::SetMouse(state.mouse)];
        }
        Command::Help => state.mode = Mode::Help { scroll: 0 },
        Command::Redraw => return vec![Effect::Redraw],
        Command::Suspend => return vec![Effect::Suspend],
        Command::Quit => return vec![Effect::Quit],
        Command::PromptAccept => accept_prompt(state),
        Command::PromptCancel => cancel_prompt(state),
        Command::PromptErase => {
            if let Mode::Prompt(p) = &mut state.mode {
                if p.input.pop().is_none() {
                    cancel_prompt(state);
                } else {
                    incremental(state);
                }
            }
        }
        Command::PromptClear => {
            if let Mode::Prompt(p) = &mut state.mode {
                p.input.clear();
                incremental(state);
            }
        }
        Command::OutlineDown => outline_move(state, 1),
        Command::OutlineUp => outline_move(state, -1),
        Command::OutlinePageDown => outline_move(state, lines(outline_page(state), true)),
        Command::OutlinePageUp => outline_move(state, lines(outline_page(state), false)),
        Command::OutlineJump => outline_jump(state),
        Command::OutlineClose => state.mode = Mode::Normal,
        Command::OutlineErase => {
            if let Mode::Outline(o) = &mut state.mode
                && o.filter.pop().is_some()
            {
                refilter(state);
            }
        }
        Command::HintsCancel => state.mode = Mode::Normal,
        Command::HintsErase => {
            if let Mode::Hints(h) = &mut state.mode
                && h.typed.pop().is_none()
            {
                state.mode = Mode::Normal;
            }
        }
        Command::HelpDown => help_scroll(state, 1),
        Command::HelpUp => help_scroll(state, -1),
        Command::HelpPageDown => help_scroll(state, lines(help_page(state), true)),
        Command::HelpPageUp => help_scroll(state, lines(help_page(state), false)),
        Command::HelpClose => state.mode = Mode::Normal,
    }
    Vec::new()
}

/// Scroll by `delta` lines (clamped).
fn scroll(state: &mut State, delta: isize) {
    let top = if delta >= 0 {
        state.top.saturating_add(delta.unsigned_abs())
    } else {
        state.top.saturating_sub(delta.unsigned_abs())
    };
    state.top = top;
    state.clamp_top();
}

// ---------------------------------------------------------------------------
// Headings
// ---------------------------------------------------------------------------

/// `]` `[` `}` `{`: `n` headings down or up; `same_level` only stops at
/// headings of the current section's level or higher.
fn heading_jump(state: &mut State, n: usize, down: bool, same_level: bool) {
    let doc = &state.page.doc;
    let level = if same_level {
        state
            .derived
            .section_at(state.top)
            .and_then(|h| doc.heading(h))
            .map_or(6, |h| h.level)
    } else {
        6
    };
    let eligible: Vec<usize> = state
        .derived
        .headings
        .iter()
        .filter(|&&(_, h)| doc.heading(h).is_some_and(|x| x.level <= level))
        .map(|&(line, _)| line as usize)
        .collect();
    let mut target = None;
    let mut from = state.top;
    for _ in 0..n {
        let next = if down {
            eligible.iter().copied().find(|&l| l > from)
        } else {
            eligible.iter().rev().copied().find(|&l| l < from)
        };
        match next {
            Some(l) => {
                target = Some(l);
                from = l;
            }
            None => break,
        }
    }
    let before = state.top;
    if let Some(line) = target {
        state.jump_to(line);
    }
    if state.top == before {
        state.say(if down {
            "no heading below"
        } else {
            "no heading above"
        });
    }
}

// ---------------------------------------------------------------------------
// Outline
// ---------------------------------------------------------------------------

fn open_outline(state: &mut State) {
    let items = toc::filtered(&state.page.doc, &state.derived, "");
    if items.is_empty() {
        state.say("no headings");
        return;
    }
    let current = state.derived.section_at(state.top);
    let selected = current
        .and_then(|h| items.iter().position(|&x| x == h))
        .unwrap_or(0);
    state.mode = Mode::Outline(Outline {
        filter: String::new(),
        items,
        selected,
        scroll: 0,
    });
    fix_outline_scroll(state);
}

/// Entries one page of the outline shows.
fn outline_page(state: &State) -> usize {
    match &state.mode {
        Mode::Outline(o) => {
            let g = toc::outline_box(state.cols, state.rows, o.items.len());
            toc::outline_rows(&g).max(1)
        }
        _ => 1,
    }
}

fn fix_outline_scroll(state: &mut State) {
    let (cols, rows) = (state.cols, state.rows);
    if let Mode::Outline(o) = &mut state.mode {
        let g = toc::outline_box(cols, rows, o.items.len());
        o.scroll = toc::scroll_to(o.scroll, o.selected, toc::outline_rows(&g));
    }
}

fn outline_move(state: &mut State, delta: isize) {
    if let Mode::Outline(o) = &mut state.mode {
        let last = o.items.len().saturating_sub(1);
        o.selected = if delta >= 0 {
            o.selected.saturating_add(delta.unsigned_abs()).min(last)
        } else {
            o.selected.saturating_sub(delta.unsigned_abs())
        };
    }
    fix_outline_scroll(state);
}

/// Filter the outline again, keeping the selected heading if it passes.
fn refilter(state: &mut State) {
    let doc = &state.page.doc;
    if let Mode::Outline(o) = &mut state.mode {
        let was = o.items.get(o.selected).copied();
        o.items = toc::filtered(doc, &state.derived, &o.filter);
        o.selected = was
            .and_then(|h| o.items.iter().position(|&x| x == h))
            .unwrap_or(0);
        o.scroll = 0;
    }
    fix_outline_scroll(state);
}

fn outline_jump(state: &mut State) {
    let Mode::Outline(o) = mem::replace(&mut state.mode, Mode::Normal) else {
        return;
    };
    let Some(&h) = o.items.get(o.selected) else {
        return;
    };
    if let Some(&line) = state.layout.heading_line.get(h.index())
        && line != u32::MAX
    {
        jump_recorded(state, line as usize);
    }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

fn open_prompt(state: &mut State, backward: bool) {
    let saved = state.search.take();
    state.mode = Mode::Prompt(Prompt {
        backward,
        input: String::new(),
        origin: state.top_place(),
        saved,
    });
}

/// Search for `pattern` from `origin` (where the prompt opened) and show
/// the nearest match. Returns whether anything was found.
fn search_from(state: &mut State, pattern: &str, backward: bool, origin: Place) -> bool {
    let origin = origin.line(&state.layout);
    let mut s = Search::run(
        state.page.corpus(),
        pattern,
        backward,
        state.settings.search_case,
    );
    let from = state
        .layout
        .lines
        .get(origin)
        .map(|l| l.pos)
        .unwrap_or_default();
    s.current = s.nearest(from, backward);
    let pos = s.current.and_then(|i| s.matches.get(i)).map(|m| m.pos());
    let found = !s.matches.is_empty();
    state.search = Some(s);
    state.top = origin;
    state.clamp_top();
    if let Some(pos) = pos {
        let line = state.layout.line_at(pos);
        state.reveal(line);
    }
    found
}

/// Search as the pattern is typed, from where the prompt was opened.
fn incremental(state: &mut State) {
    let Mode::Prompt(p) = &state.mode else {
        return;
    };
    let (origin, backward, input) = (p.origin, p.backward, p.input.clone());
    if input.is_empty() {
        state.search = None;
        state.top = origin.line(&state.layout);
        state.clamp_top();
        return;
    }
    search_from(state, &input, backward, origin);
}

fn accept_prompt(state: &mut State) {
    let Mode::Prompt(p) = mem::replace(&mut state.mode, Mode::Normal) else {
        return;
    };
    let pattern = if p.input.is_empty() {
        match state.last_pattern.clone() {
            Some(last) => last,
            None => {
                state.search = p.saved;
                return;
            }
        }
    } else {
        p.input
    };
    state.last_pattern = Some(pattern.clone());
    if !search_from(state, &pattern, p.backward, p.origin) {
        state.search = None;
        state.complain(format!("not found: {pattern}"));
    }
}

fn cancel_prompt(state: &mut State) {
    let Mode::Prompt(p) = mem::replace(&mut state.mode, Mode::Normal) else {
        return;
    };
    state.search = p.saved;
    state.top = p.origin.line(&state.layout);
    state.clamp_top();
}

/// `n` (or `N` with `reverse`), `count` times.
fn next_match(state: &mut State, count: usize, reverse: bool) {
    if state.search.is_none() {
        let Some(pattern) = state.last_pattern.clone() else {
            state.say("no search yet: / searches");
            return;
        };
        state.search = Some(Search::run(
            state.page.corpus(),
            &pattern,
            false,
            state.settings.search_case,
        ));
    }
    let rows = state.view_rows();
    let top = state.top;
    let top_pos = state.top_pos();
    let layout = &state.layout;
    let Some(s) = state.search.as_mut() else {
        return;
    };
    let n = s.matches.len();
    if n == 0 {
        let pattern = s.pattern.clone();
        state.complain(format!("not found: {pattern}"));
        return;
    }
    let up = s.backward != reverse;
    // Start from the current match when it is on screen, else from the
    // top of the screen.
    let on_screen = s.current.filter(|&i| {
        s.matches.get(i).is_some_and(|m| {
            let line = layout.line_at(m.pos());
            line >= top && line < top + rows
        })
    });
    let mut wrapped = false;
    let mut cur = on_screen;
    for _ in 0..count {
        let next = match cur {
            Some(i) if up => {
                if i == 0 {
                    wrapped = true;
                    n - 1
                } else {
                    i - 1
                }
            }
            Some(i) => {
                if i + 1 >= n {
                    wrapped = true;
                    0
                } else {
                    i + 1
                }
            }
            None => {
                let i = s.nearest(top_pos, up).unwrap_or(0);
                let first = s.matches.get(i).map(|m| m.pos());
                wrapped = match first {
                    Some(p) if up => p >= top_pos,
                    Some(p) => p < top_pos,
                    None => false,
                };
                i
            }
        };
        cur = Some(next);
    }
    s.current = cur;
    let pos = cur.and_then(|i| s.matches.get(i)).map(|m| m.pos());
    if let Some(pos) = pos {
        let line = state.layout.line_at(pos);
        state.reveal(line);
    }
    if wrapped {
        state.say(if up {
            "search wrapped to the bottom"
        } else {
            "search wrapped to the top"
        });
    }
}

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

fn set_focus(state: &mut State, occ: usize) {
    let Some(o) = state.derived.links.get(occ).copied() else {
        return;
    };
    let pos = state
        .layout
        .lines
        .get(o.line as usize)
        .map(|l| l.pos)
        .unwrap_or_default();
    state.focus = Some(Focus {
        occ,
        link: o.link,
        back: o.back,
        pos,
    });
}

/// Tab / S-Tab, `n` times.
fn focus_step(state: &mut State, n: usize, forward: bool) {
    let total = state.derived.links.len();
    if total == 0 {
        state.say("no links in this document");
        return;
    }
    let (top, rows) = (state.top, state.view_rows());
    let visible_focus = state.focus.map(|f| f.occ).filter(|&i| {
        state
            .derived
            .links
            .get(i)
            .is_some_and(|o| links::visible(o, top, rows))
    });
    let mut cur = visible_focus;
    for _ in 0..n {
        cur = Some(match cur {
            Some(i) if forward => (i + 1) % total,
            Some(i) => (i + total - 1) % total,
            None if forward => state
                .derived
                .links
                .iter()
                .position(|o| o.line as usize + o.hits as usize > top)
                .unwrap_or(0),
            None => state
                .derived
                .links
                .iter()
                .rposition(|o| (o.line as usize) < top + rows)
                .unwrap_or(total - 1),
        });
    }
    if let Some(i) = cur {
        set_focus(state, i);
        if let Some(o) = state.derived.links.get(i).copied() {
            state.reveal(o.line as usize);
        }
    }
}

fn follow_focus(state: &mut State) -> Vec<Effect> {
    match state.focus {
        Some(f) => follow(state, f.occ),
        None => {
            state.say("no link focused: Tab or o picks one");
            Vec::new()
        }
    }
}

/// Follow link occurrence `occ`.
fn follow(state: &mut State, occ: usize) -> Vec<Effect> {
    let Some(o) = state.derived.links.get(occ).copied() else {
        return Vec::new();
    };
    let base = state.page.base_dir();
    match links::target(&state.page.doc, &base, &state.layout, &o) {
        Follow::Line(line) => {
            jump_recorded(state, line);
            Vec::new()
        }
        Follow::Load { path, anchor } => vec![Effect::Load(LoadRequest {
            path,
            anchor,
            restore: None,
            nav: Nav::Push,
        })],
        Follow::File(path) => vec![Effect::ShowFile(path)],
        Follow::Open(url) => vec![Effect::Open(url)],
        Follow::Nowhere(what) => {
            state.complain(format!("cannot follow: no {what}"));
            Vec::new()
        }
    }
}

fn copy_focus(state: &mut State) -> Vec<Effect> {
    let Some(f) = state.focus else {
        state.say("no link focused: Tab or o picks one");
        return Vec::new();
    };
    let base = state.page.base_dir();
    let text = state
        .derived
        .links
        .get(f.occ)
        .and_then(|o| links::copy_text(&state.page.doc, &base, o));
    match text {
        Some(t) => vec![Effect::Copy(t)],
        None => {
            state.say("nothing to copy for this link");
            Vec::new()
        }
    }
}

fn open_hints(state: &mut State) {
    let (top, rows) = (state.top, state.view_rows());
    let shown: Vec<usize> = state
        .derived
        .links
        .iter()
        .enumerate()
        .filter(|(_, o)| links::visible(o, top, rows))
        .map(|(i, _)| i)
        .collect();
    if shown.is_empty() {
        state.say("no links on screen");
        return;
    }
    let labels = links::hint_labels(shown.len())
        .into_iter()
        .zip(shown)
        .collect();
    state.mode = Mode::Hints(Hints {
        labels,
        typed: String::new(),
    });
}

fn hint_typed(state: &mut State) -> Vec<Effect> {
    let Mode::Hints(h) = &state.mode else {
        return Vec::new();
    };
    if let Some(&(_, occ)) = h.labels.iter().find(|(l, _)| *l == h.typed) {
        state.mode = Mode::Normal;
        set_focus(state, occ);
        return follow(state, occ);
    }
    if !h
        .labels
        .iter()
        .any(|(l, _)| l.starts_with(h.typed.as_str()))
    {
        let typed = h.typed.clone();
        state.mode = Mode::Normal;
        state.complain(format!("no link labelled {typed}"));
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// History and documents
// ---------------------------------------------------------------------------

/// Jump to `line` of this document, recording where the reader was.
fn jump_recorded(state: &mut State, line: usize) {
    let here = state.visit();
    state.history.push_back(here);
    state.history.forward.clear();
    state.jump_to(line);
}

/// Back or forward.
fn go_history(state: &mut State, nav: Nav) -> Vec<Effect> {
    let list = match nav {
        Nav::Back => &state.history.back,
        Nav::Forward | Nav::Push => &state.history.forward,
    };
    let Some(visit) = list.last().cloned() else {
        state.say(if nav == Nav::Back {
            "nothing to go back to"
        } else {
            "nothing to go forward to"
        });
        return Vec::new();
    };
    if let Some(page) = state.history.page(&visit.key) {
        return switch(state, page, restore(&visit, nav));
    }
    match visit.key.path() {
        Some(path) => vec![Effect::Load(LoadRequest {
            path: path.to_path_buf(),
            ..restore(&visit, nav)
        })],
        None => {
            // Standard input cannot be read again: forget the entry.
            match nav {
                Nav::Back => state.history.back.pop(),
                Nav::Forward | Nav::Push => state.history.forward.pop(),
            };
            state.complain("that document is gone (standard input cannot be read again)");
            Vec::new()
        }
    }
}

fn restore(visit: &Visit, nav: Nav) -> LoadRequest {
    LoadRequest {
        path: visit
            .key
            .path()
            .map(|p| p.to_path_buf())
            .unwrap_or_default(),
        anchor: None,
        restore: Some(visit.place),
        nav,
    }
}

/// Show `page` (possibly the current one) as `req` says.
fn switch(state: &mut State, page: Rc<Page>, req: LoadRequest) -> Vec<Effect> {
    let here = state.visit();
    match req.nav {
        Nav::Push => {
            state.history.push_back(here);
            state.history.forward.clear();
        }
        Nav::Back => {
            state.history.back.pop();
            state.history.push_forward(here);
        }
        Nav::Forward => {
            state.history.forward.pop();
            state.history.push_back(here);
        }
    }
    state.history.touch(&page);
    let goto = match (req.restore, req.anchor) {
        (Some(place), _) => Goto::Place(place),
        (None, Some(anchor)) => Goto::Anchor(anchor),
        (None, None) => Goto::Top,
    };
    state.mode = Mode::Normal;
    state.count = None;
    if page.key == state.page.key {
        // A link into the document itself: no new layout needed.
        go(state, goto);
        return Vec::new();
    }
    state.page = page;
    state.search = None;
    state.focus = None;
    state.pending = Some(goto);
    vec![Effect::Relayout]
}

/// Move the view as `goto` says, in the current layout.
fn go(state: &mut State, goto: Goto) {
    match goto {
        Goto::Place(place) => state.top = place.line(&state.layout),
        Goto::Anchor(name) => match links::anchor_line(&state.page.doc, &state.layout, &name) {
            Some(line) => state.top = line,
            None => {
                state.top = 0;
                let bare = name.strip_prefix('#').unwrap_or(&name);
                state.complain(format!("no anchor #{bare} in {}", state.page.name));
            }
        },
        Goto::Top => state.top = 0,
    }
    state.clamp_top();
}

/// The current file changed: take the new text and lay it out again at
/// the same place.
fn reloaded(state: &mut State, doc: PagerDoc) -> Vec<Effect> {
    let key = state.page.key.clone();
    let page = Rc::new(Page::new(
        doc,
        key,
        state.page.link_base,
        state.settings.front_matter,
    ));
    state.history.touch(&page);
    state.page = page;
    state.pending = Some(Goto::Place(state.top_place()));
    if let Some(s) = state.search.take() {
        let mut again = Search::run(
            state.page.corpus(),
            &s.pattern,
            s.backward,
            state.settings.search_case,
        );
        again.current = s.current.filter(|&i| i < again.matches.len());
        state.search = Some(again);
    }
    if matches!(state.mode, Mode::Hints(_) | Mode::Prompt(_)) {
        state.mode = Mode::Normal;
    }
    // Link ids are not stable across versions of a document.
    state.focus = None;
    state.say("reloaded");
    vec![Effect::Relayout]
}

/// A new layout from the shell.
fn install(state: &mut State, layout: Layout) {
    state.layout = layout;
    state.derived = Derived::new(&state.layout);
    state.generation = state.generation.wrapping_add(1);
    if let Some(goto) = state.pending.take() {
        go(state, goto);
    }
    state.clamp_top();
    // The focused link, found again near where it was.
    if let Some(f) = state.focus {
        let near = state.layout.line_at(f.pos);
        let best = state
            .derived
            .links
            .iter()
            .enumerate()
            .filter(|(_, o)| o.link == f.link && o.back == f.back)
            .min_by_key(|(_, o)| (o.line as usize).abs_diff(near))
            .map(|(i, _)| i);
        state.focus = None;
        if let Some(i) = best {
            set_focus(state, i);
        }
    }
    match &state.mode {
        Mode::Hints(_) => state.mode = Mode::Normal,
        Mode::Outline(_) => refilter(state),
        _ => {}
    }
}

fn resize(state: &mut State, cols: u16, rows: u16) -> Vec<Effect> {
    if (cols, rows) == (state.cols, state.rows) {
        return Vec::new();
    }
    let width_changed = cols != state.cols;
    if width_changed && state.pending.is_none() {
        state.pending = Some(Goto::Place(state.top_place()));
    }
    state.cols = cols;
    state.rows = rows;
    if matches!(state.mode, Mode::Hints(_)) {
        state.mode = Mode::Normal;
    }
    fix_outline_scroll(state);
    if width_changed {
        return vec![Effect::Relayout];
    }
    state.clamp_top();
    Vec::new()
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

fn help_page(state: &State) -> usize {
    let lines = keymap::help_lines().len();
    toc::help_box(state.cols, state.rows, lines)
        .inner_rows()
        .max(1)
}

fn help_scroll(state: &mut State, delta: isize) {
    let lines = keymap::help_lines().len();
    let shown = toc::help_box(state.cols, state.rows, lines).inner_rows();
    let max = lines.saturating_sub(shown);
    if let Mode::Help { scroll } = &mut state.mode {
        *scroll = if delta >= 0 {
            scroll.saturating_add(delta.unsigned_abs()).min(max)
        } else {
            scroll.saturating_sub(delta.unsigned_abs())
        };
    }
}

// ---------------------------------------------------------------------------
// Mouse
// ---------------------------------------------------------------------------

fn on_mouse(state: &mut State, m: Mouse) -> Vec<Effect> {
    let step = lines(usize::from(state.settings.scroll_lines).max(1), true);
    match m.kind {
        MouseKind::WheelDown | MouseKind::WheelUp => {
            let delta = if m.kind == MouseKind::WheelDown {
                step
            } else {
                -step
            };
            match state.mode {
                Mode::Outline(_) => outline_move(state, delta.signum()),
                Mode::Help { .. } => help_scroll(state, delta),
                _ => scroll(state, delta),
            }
            Vec::new()
        }
        MouseKind::Press(Button::Left) => {
            state.message = None;
            click(state, m.col, m.row)
        }
        _ => Vec::new(),
    }
}

fn click(state: &mut State, col: u16, row: u16) -> Vec<Effect> {
    match &state.mode {
        Mode::Outline(o) => {
            let g = toc::outline_box(state.cols, state.rows, o.items.len());
            if !g.contains(col, row) {
                state.mode = Mode::Normal;
                return Vec::new();
            }
            // Entries start below the top border and the filter row.
            let first = usize::from(g.y) + 2;
            let row = usize::from(row);
            if row >= first && row < first + toc::outline_rows(&g) {
                let i = o.scroll + (row - first);
                if i < o.items.len() {
                    if let Mode::Outline(o) = &mut state.mode {
                        o.selected = i;
                    }
                    outline_jump(state);
                }
            }
            Vec::new()
        }
        Mode::Help { .. } => {
            state.mode = Mode::Normal;
            Vec::new()
        }
        Mode::Hints(_) | Mode::Prompt(_) | Mode::Normal => {
            if matches!(state.mode, Mode::Hints(_)) {
                state.mode = Mode::Normal;
            }
            if matches!(state.mode, Mode::Prompt(_)) {
                return Vec::new();
            }
            let row = usize::from(row);
            if row >= state.view_rows() {
                return Vec::new();
            }
            let line = state.top + row;
            let Some(col) = col.checked_sub(state.layout.indent) else {
                return Vec::new();
            };
            match links::occurrence_at(&state.layout, &state.derived, line, col) {
                Some(occ) => {
                    set_focus(state, occ);
                    follow(state, occ)
                }
                None => Vec::new(),
            }
        }
    }
}
