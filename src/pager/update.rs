//! `update(&mut State, Action) -> Vec<Effect>`: everything the reader does,
//! as a pure function.
//!
//! Input ([`Action::Key`], [`Action::Mouse`]) goes through the keymap to a
//! [`Command`]; commands change the state and return [`Effect`]s for the
//! shell, which does the I/O (loading and laying out documents, opening
//! URLs, the clipboard, terminal modes) and reports back with more actions
//! ([`Action::Layout`], [`Action::Opened`], …).

use std::mem;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::ir::{Anchor, Document, HeadingId, Target};
use crate::layout::{Layout, LineKind};

use super::PagerDoc;
use super::gallery::{self, Location};
use super::hints::{self, Act};
use super::keymap::{Command, Context};
use super::links::{self, Follow};
use super::picker::{self, PickerEvent, PickerJob};
use super::search::{Corpus, Search, current_cols};
use super::state::{
    Derived, DocKey, Focus, Goto, HintKind, ImagePage, Mode, Outline, Page, Place, Prompt, State,
    Viewer, Visit,
};
use super::term::{Button, Key, KeyCode, Mouse, MouseKind};
use super::{sidebar, toc, visual};

/// Largest count prefix.
const MAX_COUNT: u32 = 1_000_000;

/// Columns `zh`, `zl` and a sideways wheel step shift wide blocks by.
const HSCROLL_STEP: u16 = 8;

/// Columns kept beside a match (or link) shifted into view.
const HSCROLL_MARGIN: u16 = 2;

/// Something that happened, for [`update`].
#[derive(Clone, Debug)]
pub enum Action {
    /// A key press.
    Key(Key),
    /// No key followed an unfinished key sequence for
    /// [`super::keymap::SEQUENCE_TIMEOUT`]: what it is bound to runs.
    KeyTimeout,
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
    /// The shell switched images to the mode named `name`; with `relayout`,
    /// figure sizes change (images on or off), so the document is laid out
    /// again at the same place.
    ImageMode { name: String, relayout: bool },
    /// What the image view shows, after [`Effect::ShowImage`]: a document
    /// of one figure, laid out to fill the screen (no figure placement when
    /// the image cannot be shown); the image's size in pixels when it was
    /// read, and why it cannot be shown.
    ImageShown {
        doc: Box<Document>,
        layout: Layout,
        pixels: Option<(u32, u32)>,
        problem: Option<String>,
    },
    /// A link led to this local file (after [`Effect::ShowFile`]), and its
    /// first bytes are an image's: show it in the image view.
    ViewFile(PathBuf),
    /// What the shell found for the file picker or the `:` prompt.
    Picker(PickerEvent),
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
    /// Put text on the clipboard; `what` is the message about it.
    Copy { text: String, what: String },
    /// Open the file in the editor at `line` (1-based), then read it again.
    Edit { path: PathBuf, line: usize },
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
    /// Switch to the next image mode (the shell answers with
    /// [`Action::ImageMode`]).
    CycleImages,
    /// Lay out the image the image view shows (the shell answers with
    /// [`Action::ImageShown`]).
    ShowImage,
    /// Open a local file (an image) with the system's opener (or copy its
    /// path where opening is not possible).
    OpenFile(PathBuf),
    /// Open any path in the pager (the file picker, `:e`): a Markdown file
    /// as a document, a directory through its README or index (else listed
    /// in the picker), an image in the image view.
    OpenPath(PathBuf),
    /// Work for the file picker or the `:` prompt (searching, listing a
    /// directory, a preview).
    Picker(PickerJob),
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
    let effects = apply(state, action);
    if state.image.is_some() && !matches!(state.mode, Mode::Image(_)) {
        // The image view closed: the text is painted again.
        state.image = None;
        state.generation = state.generation.wrapping_add(1);
    }
    sidebar::follow(state);
    effects
}

fn apply(state: &mut State, action: Action) -> Vec<Effect> {
    match action {
        Action::Key(key) => {
            state.message = None;
            state.completions = None;
            on_key(state, key)
        }
        Action::KeyTimeout => {
            let keys = mem::take(&mut state.keys);
            match state
                .settings
                .keymap
                .lookup(context(&state.mode), &keys)
                .exact
            {
                Some(cmd) if !keys.is_empty() => command(state, cmd),
                _ => Vec::new(),
            }
        }
        Action::Mouse(m) => on_mouse(state, m),
        Action::Paste(text) => type_text(state, &text),
        Action::Command(cmd) => {
            state.message = None;
            command(state, cmd)
        }
        Action::Resize { cols, rows } => resize(state, cols, rows),
        Action::Layout(layout) => install(state, layout),
        Action::Anchor(name) => match hidden_heading(state, &name) {
            Some((h, folds)) => unfold(state, &folds, Goto::Heading(h)),
            None => {
                go(state, Goto::Anchor(name));
                Vec::new()
            }
        },
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
            state.pending_file = None;
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
        Action::ImageMode { name, relayout } => {
            state.say(format!("images: {name}"));
            let mut out = Vec::new();
            if relayout {
                state.pending = Some(Goto::Place(state.top_place()));
                out.push(Effect::Relayout);
            }
            if matches!(state.mode, Mode::Image(_)) {
                out.push(Effect::ShowImage);
            }
            out
        }
        Action::ImageShown {
            doc,
            layout,
            pixels,
            problem,
        } => {
            if matches!(state.mode, Mode::Image(_)) {
                state.image = Some(ImagePage {
                    doc: *doc,
                    layout,
                    pixels,
                    problem,
                });
                state.generation = state.generation.wrapping_add(1);
            }
            Vec::new()
        }
        Action::ViewFile(path) => view_image(state, Some(gallery::file(&path)), Some(&path)),
        Action::Picker(PickerEvent(e)) => picker::event(state, e),
    }
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

fn context(mode: &Mode) -> Context {
    match mode {
        Mode::Normal => Context::Normal,
        Mode::Visual(_) => Context::Visual,
        Mode::Prompt(_) => Context::Prompt,
        Mode::Outline(_) => Context::Outline,
        Mode::Help { .. } => Context::Help,
        Mode::Hints(_) => Context::Hints,
        Mode::Command(_) => Context::Command,
        Mode::Sidebar(_) => Context::Sidebar,
        Mode::Image(_) => Context::Image,
        Mode::Picker(_) => Context::Picker,
    }
}

fn on_key(state: &mut State, key: Key) -> Vec<Effect> {
    if let Some(cmd) = state.awaiting.take() {
        return argument(state, cmd, key);
    }
    let ctx = context(&state.mode);
    if ctx.types_text() {
        // Text input: single keys, the rest types.
        return match state.settings.keymap.command(ctx, key) {
            Some(cmd) => command(state, cmd),
            None => match key.text() {
                Some(c) => {
                    let mut buf = [0u8; 4];
                    type_text(state, c.encode_utf8(&mut buf))
                }
                None if ctx == Context::Hints => {
                    state.mode = Mode::Normal;
                    Vec::new()
                }
                None => Vec::new(),
            },
        };
    }
    if key == Key::plain(KeyCode::Esc) && !state.keys.is_empty() {
        // Esc drops an unfinished sequence (and its count).
        state.keys.clear();
        state.count = None;
        return Vec::new();
    }
    let mut seq = mem::take(&mut state.keys);
    seq.push(key);
    let found = state.settings.keymap.lookup(ctx, &seq);
    if found.longer {
        // More keys may follow (`g` of `gg`): wait for them.
        state.keys = seq;
        return Vec::new();
    }
    if let Some(cmd) = found.exact {
        return key_command(state, cmd, key);
    }
    if let Some((_, before)) = seq.split_last()
        && !before.is_empty()
    {
        // The keys before did not continue: what they mean, then this key
        // on its own.
        let mut out = match state.settings.keymap.lookup(ctx, before).exact {
            Some(cmd) => command(state, cmd),
            None => {
                state.count = None;
                Vec::new()
            }
        };
        out.extend(on_key(state, key));
        return out;
    }
    if ctx != Context::Help {
        state.count = None;
    }
    Vec::new()
}

/// A bound key: a digit of the count, or a command.
fn key_command(state: &mut State, cmd: Command, key: Key) -> Vec<Effect> {
    if cmd == Command::Count {
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
        return Vec::new();
    }
    command(state, cmd)
}

/// The key after `m` or `'`: the mark's name.
fn argument(state: &mut State, cmd: Command, key: Key) -> Vec<Effect> {
    state.count = None;
    let Some(name) = key.text() else {
        return Vec::new();
    };
    let jump = cmd == Command::JumpMark;
    if !(name.is_ascii_lowercase() || jump && name == '\'') {
        state.complain(format!(
            "marks are a to z, not {}",
            crate::text::sanitize(&name.to_string())
        ));
        return Vec::new();
    }
    if !jump {
        state.set_mark(name, state.top_place());
        state.say(format!("mark {name} set"));
        return Vec::new();
    }
    match state.mark(name) {
        Some(place) => {
            state.record_jump();
            state.top = place.line(&state.layout);
            state.clamp_top();
        }
        None if name == '\'' => state.say("no jump yet"),
        None => state.complain(format!("no mark {name}")),
    }
    Vec::new()
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
        Mode::Command(input) => {
            input.push_str(&clean);
            Vec::new()
        }
        Mode::Picker(_) => picker::typed(state, &clean),
        Mode::Normal | Mode::Help { .. } | Mode::Visual(_) | Mode::Sidebar(_) | Mode::Image(_) => {
            Vec::new()
        }
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
    if matches!(state.mode, Mode::Visual(_)) && visual::motion(state, cmd, n, count) {
        return Vec::new();
    }
    // The ends of the sidebar and of the gallery.
    let end = match cmd {
        Command::Top => Some(isize::MIN),
        Command::Bottom => Some(isize::MAX),
        _ => None,
    };
    match (&state.mode, end) {
        (Mode::Sidebar(_), Some(delta)) => {
            sidebar_move(state, delta);
            return Vec::new();
        }
        (Mode::Image(_), Some(delta)) => return image_step(state, delta),
        _ => {}
    }
    match cmd {
        Command::Count => {}
        Command::LineDown => scroll(state, lines(n, true)),
        Command::LineUp => scroll(state, lines(n, false)),
        Command::PageDown => scroll(state, lines(page, true)),
        Command::PageUp => scroll(state, lines(page, false)),
        Command::HalfDown => scroll(state, lines(half, true)),
        Command::HalfUp => scroll(state, lines(half, false)),
        Command::Top => {
            state.record_jump();
            state.jump_to(count.map_or(0, |c| c.saturating_sub(1)));
        }
        Command::Bottom => {
            state.record_jump();
            match count {
                Some(c) => state.jump_to(c.saturating_sub(1)),
                None => state.top = state.max_top(),
            }
        }
        Command::Percent => {
            state.record_jump();
            let pct = count.unwrap_or(0).min(100);
            state.jump_to(state.layout.len().saturating_mul(pct) / 100);
        }
        Command::ScrollCenter | Command::ScrollTop | Command::ScrollBottom => {
            let line = current_line(state);
            place_line(state, line, cmd);
        }
        Command::ScrollLeft | Command::ScrollRight => {
            let cols = usize::from(HSCROLL_STEP).saturating_mul(n);
            shift(state, Shift::By(lines(cols, cmd == Command::ScrollRight)));
        }
        Command::HalfLeft | Command::HalfRight => {
            let cols = usize::from((state.layout.measure / 2).max(1)).saturating_mul(n);
            shift(state, Shift::By(lines(cols, cmd == Command::HalfRight)));
        }
        Command::ScrollStart => shift(state, Shift::Start),
        Command::ScrollEnd => shift(state, Shift::End),
        Command::ToggleDetails => return toggle_near_top(state),
        Command::NextBlock | Command::PrevBlock => {}
        Command::SetMark | Command::JumpMark => state.awaiting = Some(cmd),
        Command::NextHeading => heading_jump(state, n, true, false),
        Command::PrevHeading => heading_jump(state, n, false, false),
        Command::NextSection => heading_jump(state, n, true, true),
        Command::PrevSection => heading_jump(state, n, false, true),
        Command::Outline => open_outline(state),
        Command::Sidebar => return toggle_sidebar(state),
        Command::SidebarDown => sidebar_move(state, 1),
        Command::SidebarUp => sidebar_move(state, -1),
        Command::SidebarPageDown => sidebar_move(state, lines(state.view_rows().max(1), true)),
        Command::SidebarPageUp => sidebar_move(state, lines(state.view_rows().max(1), false)),
        Command::SidebarJump => {
            if let Mode::Sidebar(i) = state.mode {
                state.mode = Mode::Normal;
                return sidebar_jump(state, i);
            }
        }
        Command::SidebarLeave => state.mode = Mode::Normal,
        Command::SidebarHide => {
            state.mode = Mode::Normal;
            return set_sidebar(state, false);
        }
        Command::SearchForward => open_prompt(state, false),
        Command::SearchBackward => open_prompt(state, true),
        Command::NextMatch => return next_match(state, n, false),
        Command::PrevMatch => return next_match(state, n, true),
        Command::ClearSearch => {
            if state.search.take().is_none() {
                state.focus = None;
                state.summary_focus = None;
            }
        }
        Command::FocusNext => focus_step(state, n, true),
        Command::FocusPrev => focus_step(state, n, false),
        Command::Follow => return follow_focus(state),
        Command::LinkHints => hints::open(state, HintKind::Links),
        Command::FollowHints => hints::open(state, HintKind::Follow),
        Command::YankHints => {
            if visible_focus(state).is_some() {
                return copy_focus(state);
            }
            hints::open(state, HintKind::Yank);
        }
        Command::VisualHints => hints::open(state, HintKind::Visual),
        Command::VisualLine => visual::enter(state),
        Command::VisualSwap => visual::swap(state),
        Command::VisualYank => return visual::yank(state),
        Command::VisualYankText => return visual::yank_text(state),
        Command::VisualExit => state.mode = Mode::Normal,
        Command::Edit => return edit(state),
        Command::Back => return go_history(state, Nav::Back),
        Command::Forward => return go_history(state, Nav::Forward),
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
        Command::CycleImages => return vec![Effect::CycleImages],
        Command::Gallery => return view_image(state, None, None),
        Command::ImageNext => return image_step(state, lines(n, true)),
        Command::ImagePrev => return image_step(state, lines(n, false)),
        Command::ImageClose => state.mode = Mode::Normal,
        Command::ImageYank => {
            if let Some(p) = state.viewed() {
                return hints::Copied::shown(p.at.text()).effect();
            }
        }
        Command::ImageOpen => return image_open(state),
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
        Command::CommandLine => state.mode = Mode::Command(String::new()),
        Command::CommandRun => return run_command(state),
        Command::CommandComplete => return picker::command_tab(state),
        Command::Picker => return picker::open(state, String::new()),
        Command::PickerDown => return picker::key(state, picker::Key::Move(1)),
        Command::PickerUp => return picker::key(state, picker::Key::Move(-1)),
        Command::PickerPageDown => return picker::key(state, picker::Key::Page(true)),
        Command::PickerPageUp => return picker::key(state, picker::Key::Page(false)),
        Command::PickerOpen => return picker::key(state, picker::Key::Open),
        Command::PickerClose => return picker::key(state, picker::Key::Close),
        Command::PickerErase => return picker::key(state, picker::Key::Erase),
        Command::PickerEraseWord => return picker::key(state, picker::Key::EraseWord),
        Command::PickerClear => return picker::key(state, picker::Key::Clear),
        Command::PickerComplete => return picker::key(state, picker::Key::Complete),
        Command::PickerToggleAll => return picker::key(state, picker::Key::ToggleAll),
        Command::CommandCancel => state.mode = Mode::Normal,
        Command::CommandErase => {
            if let Mode::Command(input) = &mut state.mode
                && input.pop().is_none()
            {
                state.mode = Mode::Normal;
            }
        }
        Command::CommandClear => {
            if let Mode::Command(input) = &mut state.mode {
                input.clear();
            }
        }
        Command::Help => state.mode = Mode::Help { scroll: 0 },
        Command::Redraw => return vec![Effect::Redraw],
        Command::Suspend => return vec![Effect::Suspend],
        Command::Quit => return vec![Effect::Quit],
        Command::PromptAccept => return accept_prompt(state),
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
        Command::OutlineJump => return outline_jump(state),
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

/// The line `zz`, `zt` and `zb` move: the current match, else the
/// focused link if they are on screen, else the middle line.
fn current_line(state: &State) -> usize {
    let (top, rows) = (state.top, state.view_rows());
    let shown = |l: &usize| (top..top + rows).contains(l);
    let found = state
        .search
        .as_ref()
        .and_then(|s| s.current.and_then(|i| s.matches.get(i)))
        .map(|m| state.layout.line_at(m.pos()))
        .filter(shown);
    let focused = visible_focus(state)
        .and_then(|i| state.derived.links.get(i))
        .map(|o| (o.line as usize).max(top));
    found
        .or(focused)
        .unwrap_or_else(|| top + rows.saturating_sub(1) / 2)
}

/// `zz`, `zt`, `zb`: put `line` in the middle, at the top or at the
/// bottom of the screen (as far as the ends allow).
pub(crate) fn place_line(state: &mut State, line: usize, how: Command) {
    let rows = state.view_rows().max(1);
    state.top = match how {
        Command::ScrollTop => line,
        Command::ScrollBottom => (line + 1).saturating_sub(rows),
        _ => line.saturating_sub(rows / 2),
    };
    state.clamp_top();
}

/// `e`: the editor at the source line of the selection (Visual mode) or
/// of the block at the top.
fn edit(state: &mut State) -> Vec<Effect> {
    let Some(path) = state.page.path().map(PathBuf::from) else {
        state.complain("standard input cannot be edited");
        return Vec::new();
    };
    let at = state.selection().map_or(state.top, |(lo, _)| lo);
    let line = visual::edit_line(state, at);
    if matches!(state.mode, Mode::Visual(_)) {
        state.mode = Mode::Normal;
    }
    vec![Effect::Edit { path, line }]
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
        .enumerate()
        .filter(|&(i, &(_, h))| {
            !state.derived.is_folded(i) && doc.heading(h).is_some_and(|x| x.level <= level)
        })
        .map(|(_, &(line, _))| line as usize)
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
    match target {
        Some(line) => {
            let before = state.top;
            state.record_jump();
            state.jump_to(line);
            if state.top == before {
                // The heading is on screen already, below the last top line.
                state.say("end of the document");
            }
        }
        None => state.say(if down {
            "no heading below"
        } else {
            "no heading above"
        }),
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

fn outline_jump(state: &mut State) -> Vec<Effect> {
    let Mode::Outline(o) = mem::replace(&mut state.mode, Mode::Normal) else {
        return Vec::new();
    };
    match o.items.get(o.selected) {
        Some(&h) => jump_heading(state, h),
        None => Vec::new(),
    }
}

/// Go to heading `h`, opening the `<details>` that hide it.
fn jump_heading(state: &mut State, h: HeadingId) -> Vec<Effect> {
    state.sidebar.jumped = Some(h);
    let folds = state.layout.heading_folds(h);
    if folds.is_empty() {
        if let Some(&line) = state.layout.heading_line.get(h.index())
            && line != u32::MAX
        {
            jump_recorded(state, line as usize);
        }
        return Vec::new();
    }
    unfold_recorded(state, &folds, Goto::Heading(h))
}

/// A jump into collapsed `<details>` `folds`: recorded in the history
/// (as [`jump_recorded`] does), they open and the view goes to `goto`.
fn unfold_recorded(state: &mut State, folds: &[u32], goto: Goto) -> Vec<Effect> {
    let here = state.visit();
    state.history.push_back(here);
    state.history.forward.clear();
    state.record_jump();
    unfold(state, folds, goto)
}

// ---------------------------------------------------------------------------
// The outline sidebar
// ---------------------------------------------------------------------------

/// `T`: show the sidebar; when it is shown, move the keys into it.
fn toggle_sidebar(state: &mut State) -> Vec<Effect> {
    if state.sidebar_cols() > 0 {
        let selected = sidebar::current(state).unwrap_or(0);
        state.mode = Mode::Sidebar(selected);
        return Vec::new();
    }
    if state.page.doc.headings.is_empty() {
        state.say("no headings");
        return Vec::new();
    }
    if !sidebar::fits(&state.settings.sidebar, state.cols) {
        let need = state
            .settings
            .sidebar
            .width
            .saturating_add(sidebar::MIN_TEXT_COLS);
        state.complain(format!("the outline sidebar needs {need} columns"));
        return Vec::new();
    }
    set_sidebar(state, true)
}

/// Show or hide the sidebar for the rest of the session, laying the
/// document out again at the same place.
fn set_sidebar(state: &mut State, shown: bool) -> Vec<Effect> {
    let before = state.sidebar_cols();
    state.sidebar.toggled = Some(shown);
    if state.sidebar_cols() == before {
        return Vec::new();
    }
    state.pending = Some(Goto::Place(state.top_place()));
    vec![Effect::Relayout]
}

/// Move the sidebar's selection by `delta` entries.
fn sidebar_move(state: &mut State, delta: isize) {
    let last = state.derived.headings.len().saturating_sub(1);
    if let Mode::Sidebar(i) = &mut state.mode {
        *i = if delta >= 0 {
            i.saturating_add(delta.unsigned_abs()).min(last)
        } else {
            i.saturating_sub(delta.unsigned_abs())
        };
    }
}

/// Go to the heading of sidebar entry `i`.
fn sidebar_jump(state: &mut State, i: usize) -> Vec<Effect> {
    match state.derived.headings.get(i) {
        Some(&(_, h)) => jump_heading(state, h),
        None => Vec::new(),
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
        hscroll: state.hscroll(),
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
        reveal_match_cols(state);
    }
    found
}

/// Search as the pattern is typed, from where the prompt was opened.
fn incremental(state: &mut State) {
    let Mode::Prompt(p) = &state.mode else {
        return;
    };
    let (origin, backward, input, hscroll) = (p.origin, p.backward, p.input.clone(), p.hscroll);
    if input.is_empty() {
        state.search = None;
        state.top = origin.line(&state.layout);
        state.clamp_top();
        state.doc_view_mut().hscroll = hscroll;
        return;
    }
    search_from(state, &input, backward, origin);
}

fn accept_prompt(state: &mut State) -> Vec<Effect> {
    let Mode::Prompt(p) = mem::replace(&mut state.mode, Mode::Normal) else {
        return Vec::new();
    };
    let pattern = if p.input.is_empty() {
        match state.last_pattern.clone() {
            Some((last, _)) => last,
            None => {
                state.search = p.saved;
                return Vec::new();
            }
        }
    } else {
        p.input
    };
    state.last_pattern = Some((pattern.clone(), p.backward));
    state.set_mark('\'', p.origin);
    if !search_from(state, &pattern, p.backward, p.origin) {
        state.search = None;
        state.complain(format!("not found: {pattern}"));
        return Vec::new();
    }
    show_match(state)
}

/// Show the current match: open the `<details>` hiding it (laying out
/// again), else scroll to it, sideways too.
fn show_match(state: &mut State) -> Vec<Effect> {
    let Some(pos) = state
        .search
        .as_ref()
        .and_then(|s| s.current.and_then(|i| s.matches.get(i)))
        .map(|m| m.pos())
    else {
        return Vec::new();
    };
    let folds = state.layout.folded_at(pos);
    if !folds.is_empty() {
        return unfold(state, &folds, Goto::Reveal(pos));
    }
    let line = state.layout.line_at(pos);
    state.reveal(line);
    reveal_match_cols(state);
    Vec::new()
}

fn cancel_prompt(state: &mut State) {
    let Mode::Prompt(p) = mem::replace(&mut state.mode, Mode::Normal) else {
        return;
    };
    state.search = p.saved;
    state.top = p.origin.line(&state.layout);
    state.clamp_top();
    state.doc_view_mut().hscroll = p.hscroll;
}

/// `n` (or `N` with `reverse`), `count` times.
fn next_match(state: &mut State, count: usize, reverse: bool) -> Vec<Effect> {
    if state.search.is_none() {
        let Some((pattern, backward)) = state.last_pattern.clone() else {
            state.say("no search yet: / searches");
            return Vec::new();
        };
        state.search = Some(Search::run(
            state.page.corpus(),
            &pattern,
            backward,
            state.settings.search_case,
        ));
    }
    let rows = state.view_rows();
    let top = state.top;
    let top_pos = state.top_pos();
    let layout = &state.layout;
    let Some(s) = state.search.as_mut() else {
        return Vec::new();
    };
    let n = s.matches.len();
    if n == 0 {
        let pattern = s.pattern.clone();
        state.complain(format!("not found: {pattern}"));
        return Vec::new();
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
    if wrapped {
        state.say(if up {
            "search wrapped to the bottom"
        } else {
            "search wrapped to the top"
        });
    }
    if cur.is_none() {
        return Vec::new();
    }
    state.record_jump();
    show_match(state)
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

/// What Tab stops at: a link occurrence or a `<details>` summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Link(usize),
    Summary(u32),
}

/// Every Tab stop with its first and last line and its column, top to
/// bottom and left to right.
fn stops(state: &State) -> Vec<(usize, usize, u16, Stop)> {
    let layout = &state.layout;
    let mut links = state.derived.links.iter().enumerate().map(|(i, o)| {
        let col = layout
            .link_hits
            .get(o.first as usize)
            .map_or(0, |h| h.cols.start);
        let line = o.line as usize;
        let last = line + o.hits.saturating_sub(1) as usize;
        (line, last, col, Stop::Link(i))
    });
    // Shown summaries are in order too.
    let mut summaries = layout
        .details
        .iter()
        .enumerate()
        .filter(|(_, d)| d.line != u32::MAX)
        .map(|(id, d)| {
            let line = d.line as usize;
            let last = line + d.rows.saturating_sub(1) as usize;
            let id = u32::try_from(id).unwrap_or(u32::MAX);
            (line, last, d.col, Stop::Summary(id))
        });
    let mut out = Vec::new();
    let (mut a, mut b) = (links.next(), summaries.next());
    loop {
        let first = match (a, b) {
            (Some(x), Some(y)) => (x.0, x.2) <= (y.0, y.2),
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if first {
            out.extend(a);
            a = links.next();
        } else {
            out.extend(b);
            b = summaries.next();
        }
    }
    out
}

/// Tab / S-Tab, `n` times: links and `<details>` summaries.
fn focus_step(state: &mut State, n: usize, forward: bool) {
    let stops = stops(state);
    let total = stops.len();
    if total == 0 {
        state.say("no links in this document");
        return;
    }
    let (top, rows) = (state.top, state.view_rows());
    let focused = match (visible_summary(state), visible_focus(state)) {
        (Some(id), _) => Some(Stop::Summary(id)),
        (None, Some(i)) => Some(Stop::Link(i)),
        (None, None) => None,
    };
    let mut cur = focused.and_then(|f| stops.iter().position(|s| s.3 == f));
    for _ in 0..n {
        cur = Some(match cur {
            Some(i) if forward => (i + 1) % total,
            Some(i) => (i + total - 1) % total,
            None if forward => stops.iter().position(|s| s.1 >= top).unwrap_or(0),
            None => stops
                .iter()
                .rposition(|s| s.0 < top + rows)
                .unwrap_or(total - 1),
        });
    }
    let Some(&(line, _, col, stop)) = cur.and_then(|i| stops.get(i)) else {
        return;
    };
    match stop {
        Stop::Link(i) => {
            state.summary_focus = None;
            set_focus(state, i);
        }
        Stop::Summary(id) => {
            state.focus = None;
            state.summary_focus = Some(id);
        }
    }
    state.reveal(line);
    // A link shows whole where it can (its first hit, on `line`).
    let cols = match stop {
        Stop::Link(i) => state
            .derived
            .links
            .get(i)
            .and_then(|o| state.layout.link_hits.get(o.first as usize))
            .filter(|h| h.line as usize == line)
            .map(|h| h.cols.clone()),
        Stop::Summary(_) => None,
    };
    reveal_cols(state, line, cols.unwrap_or(col..col.saturating_add(1)));
}

/// The focused `<details>` summary, if it is on screen.
fn visible_summary(state: &State) -> Option<u32> {
    let (top, rows) = (state.top, state.view_rows());
    state.summary_focus.filter(|&id| {
        state.layout.details.get(id as usize).is_some_and(|d| {
            let line = d.line as usize;
            d.line != u32::MAX && line < top + rows && line + d.rows as usize > top
        })
    })
}

/// The focused link occurrence, if it is on screen.
fn visible_focus(state: &State) -> Option<usize> {
    let (top, rows) = (state.top, state.view_rows());
    state.focus.map(|f| f.occ).filter(|&i| {
        state
            .derived
            .links
            .get(i)
            .is_some_and(|o| links::visible(o, top, rows))
    })
}

fn follow_focus(state: &mut State) -> Vec<Effect> {
    if let Some(id) = visible_summary(state) {
        return toggle(state, id);
    }
    match visible_focus(state) {
        Some(occ) => follow(state, occ),
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
    if let Some(at) = gallery::link_location(state, &o) {
        return view_image(state, Some(at), None);
    }
    // A heading inside a collapsed `<details>`: open it on the way.
    let doc = &state.page.doc;
    if let Some(Target::Anchor(name)) = doc.link(o.link).map(|l| &l.target)
        && let Some(Anchor::Heading(h)) = doc.resolve_anchor(name)
        && !state.layout.heading_folds(h).is_empty()
    {
        return jump_heading(state, h);
    }
    // A footnote back-link to a reference inside a collapsed `<details>`
    // (shown, the reference has a hit to go to).
    if o.back
        && !state
            .layout
            .link_hits
            .iter()
            .any(|h| h.link == o.link && !h.back)
        && let Some(pos) = Corpus::link_pos(doc, o.link)
    {
        let folds = state.layout.folded_at(pos);
        if !folds.is_empty() {
            return unfold_recorded(state, &folds, Goto::Place(Place { pos, extra: 0 }));
        }
    }
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
    let Some(occ) = visible_focus(state) else {
        state.say("no link focused: Tab or o picks one");
        return Vec::new();
    };
    let base = state.page.base_dir();
    let text = state
        .derived
        .links
        .get(occ)
        .and_then(|o| links::copy_text(&state.page.doc, &base, o));
    match text {
        Some(t) => hints::Copied::shown(t).effect(),
        None => {
            state.say("nothing to copy for this link");
            Vec::new()
        }
    }
}

/// A label typed in full does what it is on; a prefix of none ends the
/// hints.
fn hint_typed(state: &mut State) -> Vec<Effect> {
    let Mode::Hints(h) = &state.mode else {
        return Vec::new();
    };
    if let Some((_, target)) = h.labels.iter().find(|(l, _)| *l == h.typed) {
        let act = target.act.clone();
        state.mode = Mode::Normal;
        return act_on(state, act);
    }
    if !h
        .labels
        .iter()
        .any(|(l, _)| l.starts_with(h.typed.as_str()))
    {
        let typed = crate::text::sanitize(&h.typed).into_owned();
        state.mode = Mode::Normal;
        state.complain(format!("no label {typed}"));
    }
    Vec::new()
}

fn act_on(state: &mut State, act: Act) -> Vec<Effect> {
    match act {
        Act::Link(occ) => {
            set_focus(state, occ);
            follow(state, occ)
        }
        Act::Jump(line) => {
            jump_recorded(state, line);
            Vec::new()
        }
        Act::View(image) => view_figure(state, image),
        Act::Copy(c) => c.effect(),
        Act::Select(lines) => {
            visual::start(state, lines);
            Vec::new()
        }
        Act::Toggle(id) => toggle(state, id),
    }
}

// ---------------------------------------------------------------------------
// Wide blocks
// ---------------------------------------------------------------------------

/// How to shift wide blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shift {
    /// By so many columns (negative: back towards their start).
    By(isize),
    Start,
    End,
}

/// The most the wide blocks on screen can be shifted (`None`: none is on
/// screen).
fn wide_on_screen(state: &State) -> Option<u16> {
    let layout = &state.layout;
    let top = u32::try_from(state.top).unwrap_or(u32::MAX);
    let rows = u32::try_from(state.view_rows()).unwrap_or(u32::MAX);
    let bottom = top.saturating_add(rows);
    layout
        .wide
        .iter()
        .filter(|w| w.start < bottom && w.end > top)
        .map(|w| w.max_shift(layout.measure))
        .max()
}

/// `zh`, `zl`, …: shift the wide blocks of the document, as far as those
/// on screen go (normal text never moves).
fn shift(state: &mut State, how: Shift) {
    let Some(max) = wide_on_screen(state) else {
        state.say("no wide code or table on screen");
        return;
    };
    let cur = state.hscroll().min(max);
    let to = match how {
        Shift::By(d) if d < 0 => {
            cur.saturating_sub(u16::try_from(d.unsigned_abs()).unwrap_or(u16::MAX))
        }
        Shift::By(d) => cur.saturating_add(u16::try_from(d.unsigned_abs()).unwrap_or(u16::MAX)),
        Shift::Start => 0,
        Shift::End => max,
    };
    state.doc_view_mut().hscroll = to.min(max);
}

/// Shift the wide block of line `line` (if it is in one) so its columns
/// `cols` (relative to the text column) are in view.
fn reveal_cols(state: &mut State, line: usize, cols: std::ops::Range<u16>) {
    let layout = &state.layout;
    let Some(w) = layout.wide_at(line) else {
        return;
    };
    let max = w.max_shift(layout.measure);
    let view = layout.measure.saturating_sub(w.x0);
    let cur = layout.shift_at(line, state.hscroll());
    // In columns of the block; the edges hold the `‹` and `›`.
    let lo = cols.start.saturating_sub(w.x0);
    let hi = cols
        .end
        .max(cols.start.saturating_add(1))
        .saturating_sub(w.x0);
    // Columns wider than the view show from their start.
    let hi = hi.min(lo.saturating_add(view.saturating_sub(2 * HSCROLL_MARGIN + 1).max(1)));
    let to = if cur > 0 && lo <= cur {
        lo.saturating_sub(HSCROLL_MARGIN)
    } else if hi.saturating_add(1) > cur.saturating_add(view) {
        hi.saturating_add(HSCROLL_MARGIN).saturating_sub(view)
    } else {
        return;
    };
    state.doc_view_mut().hscroll = to.min(max);
}

/// Shift the current search match into view sideways.
fn reveal_match_cols(state: &mut State) {
    let Some(s) = state.search.as_ref() else {
        return;
    };
    let Some(pos) = s.current.and_then(|i| s.matches.get(i)).map(|m| m.pos()) else {
        return;
    };
    // The match is on one of the lines starting at its position: the first
    // can be a code block's padding or a table's top border.
    let layout = &state.layout;
    let first = layout.line_at(pos);
    let Some(at) = layout.lines.get(first).map(|l| l.pos) else {
        return;
    };
    let shown = (first..layout.len())
        .take_while(|&i| layout.lines.get(i).is_some_and(|l| l.pos == at))
        .take(64)
        .filter(|&i| layout.wide_at(i).is_some())
        .find_map(|i| Some((i, current_cols(state.page.corpus(), layout, i, s)?)));
    if let Some((line, cols)) = shown {
        reveal_cols(state, line, cols);
    }
}

// ---------------------------------------------------------------------------
// Collapsible details
// ---------------------------------------------------------------------------

/// Open or close `<details>` `id` (by its toggle).
fn flip(state: &mut State, id: u32) {
    let toggled = &mut state.doc_view_mut().toggled;
    match toggled.binary_search(&id) {
        Ok(i) => {
            toggled.remove(i);
        }
        Err(i) => toggled.insert(i, id),
    }
}

/// Open or close `<details>` `id`, keeping what is at the top of the
/// screen where it is.
pub(crate) fn toggle(state: &mut State, id: u32) -> Vec<Effect> {
    let Some(d) = state.layout.details.get(id as usize).copied() else {
        return Vec::new();
    };
    // Closing the block the top line is in: its summary goes to the top.
    let place = if d.open && d.line != u32::MAX && d.holds(state.top_pos()) {
        Place::of(&state.layout, d.line as usize)
    } else {
        state.top_place()
    };
    flip(state, id);
    state.pending = Some(Goto::Place(place));
    vec![Effect::Relayout]
}

/// Open the collapsed `<details>` `ids`, then go where `goto` says.
fn unfold(state: &mut State, ids: &[u32], goto: Goto) -> Vec<Effect> {
    for &id in ids {
        if state
            .layout
            .details
            .get(id as usize)
            .is_some_and(|d| !d.open)
        {
            flip(state, id);
        }
    }
    state.pending = Some(goto);
    vec![Effect::Relayout]
}

/// `za`: open or close the focused `<details>`, else the first one whose
/// summary is in the top third of the screen, else close the one the top
/// line is in.
fn toggle_near_top(state: &mut State) -> Vec<Effect> {
    if let Some(id) = visible_summary(state) {
        return toggle(state, id);
    }
    let top = state.top;
    let near = top..top + (state.view_rows() / 3).max(1);
    let layout = &state.layout;
    let found = layout
        .details
        .iter()
        .position(|d| d.line != u32::MAX && near.contains(&(d.line as usize)))
        .or_else(|| {
            let pos = state.top_pos();
            layout.details.iter().rposition(|d| {
                d.open && d.line != u32::MAX && (d.line as usize) < top && d.holds(pos)
            })
        });
    match found.and_then(|i| u32::try_from(i).ok()) {
        Some(id) => toggle(state, id),
        None => {
            state.say("no details block at the top");
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// The image view
// ---------------------------------------------------------------------------

/// Open the image view on the document's gallery at the image at `at` (the
/// first without), `extra` being a file to count as an image although its
/// name does not say so.
fn view_image(state: &mut State, at: Option<Location>, extra: Option<&Path>) -> Vec<Effect> {
    let gallery = gallery::build(state, extra);
    let index = match &at {
        Some(at) => gallery.iter().position(|p| p.at == *at),
        None => (!gallery.is_empty()).then_some(0),
    };
    let Some(index) = index else {
        state.say(if gallery.is_empty() {
            "no images in this document"
        } else {
            "that image is not in the gallery"
        });
        return Vec::new();
    };
    state.mode = Mode::Image(Viewer {
        gallery: gallery.into(),
        index,
    });
    vec![Effect::ShowImage]
}

/// The image view on figure `image` of the current document.
fn view_figure(state: &mut State, image: crate::ir::ImageId) -> Vec<Effect> {
    match gallery::figure_location(state, image) {
        Some(at) => view_image(state, Some(at), None),
        None => {
            state.complain("that image has no source to show");
            Vec::new()
        }
    }
}

/// `delta` images on in the gallery (as far as its ends).
fn image_step(state: &mut State, delta: isize) -> Vec<Effect> {
    let Mode::Image(v) = &mut state.mode else {
        return Vec::new();
    };
    let last = v.gallery.len().saturating_sub(1);
    let to = if delta >= 0 {
        v.index.saturating_add(delta.unsigned_abs()).min(last)
    } else {
        v.index.saturating_sub(delta.unsigned_abs())
    };
    if to == v.index {
        let only = last == 0;
        state.say(match (only, delta >= 0) {
            (true, _) => "the only image",
            (false, true) => "the last image",
            (false, false) => "the first image",
        });
        return Vec::new();
    }
    v.index = to;
    vec![Effect::ShowImage]
}

/// `o` in the image view: the image with the system's opener.
fn image_open(state: &mut State) -> Vec<Effect> {
    match state.viewed().map(|p| p.at.clone()) {
        Some(Location::File(path)) => vec![Effect::OpenFile(path)],
        Some(Location::Url(url)) => vec![Effect::Open(url)],
        Some(Location::Data(_)) => {
            state.say("an image in a data: URI has no file to open (y copies it)");
            Vec::new()
        }
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The `:` prompt and the files named on the command line
// ---------------------------------------------------------------------------

/// Run what was typed at the `:` prompt.
fn run_command(state: &mut State) -> Vec<Effect> {
    let Mode::Command(input) = mem::replace(&mut state.mode, Mode::Normal) else {
        return Vec::new();
    };
    match input.trim() {
        "" => Vec::new(),
        "n" | "next" => step_file(state, true),
        "p" | "N" | "prev" | "previous" => step_file(state, false),
        "x" | "first" => go_file(state, 0),
        "q" | "quit" => vec![Effect::Quit],
        other => {
            if let Some(arg) = picker::edit_arg(other) {
                return picker::edit(state, arg);
            }
            let shown = crate::text::sanitize(other).into_owned();
            state.complain(format!("unknown command :{shown} (:e FILE, :n, :p, :x, :q)"));
            Vec::new()
        }
    }
}

/// `:n` and `:p`: the next or previous file named on the command line.
fn step_file(state: &mut State, forward: bool) -> Vec<Effect> {
    let n = state.files.len();
    if n < 2 {
        state.say("there is only one file");
        return Vec::new();
    }
    let target = if forward {
        state.file.checked_add(1).filter(|&i| i < n)
    } else {
        state.file.checked_sub(1)
    };
    match target {
        Some(i) => go_file(state, i),
        None => {
            state.say(if forward {
                "this is the last file"
            } else {
                "this is the first file"
            });
            Vec::new()
        }
    }
}

/// Show file `i` of the command line (from where it was read, if the
/// history no longer keeps it).
fn go_file(state: &mut State, i: usize) -> Vec<Effect> {
    let Some(key) = state.files.get(i).cloned() else {
        return Vec::new();
    };
    // Back where the reader last was in another file, as less does.
    let restore = if key == state.page.key {
        None
    } else {
        state
            .history
            .back
            .iter()
            .rev()
            .find(|v| v.key == key)
            .map(|v| v.place)
    };
    let request = LoadRequest {
        path: key.path().map(PathBuf::from).unwrap_or_default(),
        anchor: None,
        restore,
        nav: Nav::Push,
    };
    state.pending_file = Some(i);
    if let Some(page) = state.history.page(&key) {
        return switch(state, page, request);
    }
    if key.path().is_some() {
        return vec![Effect::Load(request)];
    }
    state.pending_file = None;
    state.complain("that document is gone (standard input cannot be read again)");
    Vec::new()
}

// ---------------------------------------------------------------------------
// History and documents
// ---------------------------------------------------------------------------

/// Jump to `line` of this document, recording where the reader was (in
/// the history and as mark `'`).
fn jump_recorded(state: &mut State, line: usize) {
    let here = state.visit();
    state.history.push_back(here);
    state.history.forward.clear();
    state.record_jump();
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
    // Which of the files named on the command line this is now.
    let asked = state.pending_file.take();
    if let Some(i) = asked.filter(|&i| state.files.get(i) == Some(&page.key)) {
        state.file = i;
    } else if state.files.get(state.file) != Some(&page.key)
        && let Some(i) = state.files.iter().position(|k| *k == page.key)
    {
        state.file = i;
    }
    let goto = match (req.restore, req.anchor) {
        (Some(place), _) => Goto::Place(place),
        (None, Some(anchor)) => Goto::Anchor(anchor),
        (None, None) => Goto::Top,
    };
    state.mode = Mode::Normal;
    state.count = None;
    state.keys.clear();
    state.awaiting = None;
    if page.key == state.page.key {
        // A link into the document itself: no new layout needed.
        go(state, goto);
        return Vec::new();
    }
    state.page = page;
    state.search = None;
    state.focus = None;
    state.summary_focus = None;
    state.sidebar.jumped = None;
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
        Goto::Heading(h) => {
            if let Some(&line) = state.layout.heading_line.get(h.index())
                && line != u32::MAX
            {
                state.top = line as usize;
            }
        }
        Goto::Reveal(pos) => {
            state.clamp_top();
            let line = state.layout.line_at(pos);
            state.reveal(line);
            reveal_match_cols(state);
        }
    }
    state.clamp_top();
}

/// The current file changed: take the new text and lay it out again at
/// the same place.
fn reloaded(state: &mut State, doc: PagerDoc) -> Vec<Effect> {
    let key = state.page.key.clone();
    // A fresh range of link ids: the new version may have more links.
    let base = state.link_base(doc.doc.links.len());
    let page = Rc::new(Page::new(doc, key, base, state.settings.front_matter));
    state.history.touch(&page);
    state.page = page;
    state.sidebar.jumped = None;
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
    if matches!(
        state.mode,
        Mode::Hints(_) | Mode::Prompt(_) | Mode::Visual(_)
    ) {
        state.mode = Mode::Normal;
    }
    // Link ids are not stable across versions of a document (the image
    // view keeps its gallery: it holds the images themselves).
    state.focus = None;
    state.summary_focus = None;
    state.say("reloaded");
    vec![Effect::Relayout]
}

/// A new layout from the shell.
/// The heading anchor `name` names, when collapsed `<details>` hide it:
/// with those blocks.
fn hidden_heading(state: &State, name: &str) -> Option<(HeadingId, Vec<u32>)> {
    let bare = name.strip_prefix('#').unwrap_or(name);
    let Some(Anchor::Heading(h)) = state.page.doc.resolve_anchor(bare) else {
        return None;
    };
    let folds = state.layout.heading_folds(h);
    (!folds.is_empty()).then_some((h, folds))
}

fn install(state: &mut State, layout: Layout) -> Vec<Effect> {
    state.spare = Some(mem::replace(&mut state.layout, layout));
    state.derived = Derived::new(&state.layout);
    state.generation = state.generation.wrapping_add(1);
    // An anchor inside collapsed `<details>`: they open, and the layout
    // after that goes there.
    let hidden = match &state.pending {
        Some(Goto::Anchor(name)) => hidden_heading(state, name),
        _ => None,
    };
    if hidden.is_some() {
        state.pending = None;
    }
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
    // A focused summary stays focused while it is shown.
    if let Some(id) = state.summary_focus
        && state
            .layout
            .details
            .get(id as usize)
            .is_none_or(|d| d.line == u32::MAX)
    {
        state.summary_focus = None;
    }
    match &state.mode {
        Mode::Hints(_) => state.mode = Mode::Normal,
        Mode::Outline(_) => refilter(state),
        _ => {}
    }
    match hidden {
        Some((h, folds)) => unfold(state, &folds, Goto::Heading(h)),
        None => Vec::new(),
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
    help_scroll(state, 0);
    let mut out = Vec::new();
    if width_changed {
        out.push(Effect::Relayout);
    } else {
        state.clamp_top();
    }
    if matches!(state.mode, Mode::Image(_)) {
        // The image fills the new screen.
        out.push(Effect::ShowImage);
    }
    if matches!(state.mode, Mode::Picker(_)) {
        out.extend(picker::resized(state));
    }
    out
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

fn help_page(state: &State) -> usize {
    let lines = state.settings.keymap.help_lines().len();
    toc::help_box(state.cols, state.rows, lines)
        .inner_rows()
        .max(1)
}

fn help_scroll(state: &mut State, delta: isize) {
    let lines = state.settings.keymap.help_lines().len();
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
                Mode::Image(_) => {}
                Mode::Picker(_) => {
                    let k = picker::Key::Move(delta.signum());
                    return picker::key(state, k);
                }
                Mode::Outline(_) => outline_move(state, delta.signum()),
                Mode::Help { .. } => help_scroll(state, delta),
                Mode::Sidebar(_) if m.col < state.left() => sidebar_move(state, delta.signum()),
                _ if m.col < state.left() => sidebar::scroll(state, delta),
                _ => scroll(state, delta),
            }
            Vec::new()
        }
        MouseKind::WheelLeft | MouseKind::WheelRight => {
            if !matches!(
                state.mode,
                Mode::Image(_) | Mode::Outline(_) | Mode::Help { .. } | Mode::Picker(_)
            ) && m.col >= state.left()
                && wide_on_screen(state).is_some()
            {
                let right = m.kind == MouseKind::WheelRight;
                shift(state, Shift::By(lines(usize::from(HSCROLL_STEP), right)));
            }
            Vec::new()
        }
        MouseKind::Press(Button::Left) => {
            state.message = None;
            // A click ends a key sequence or a mark being typed.
            state.keys.clear();
            state.awaiting = None;
            state.count = None;
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
                    return outline_jump(state);
                }
            }
            Vec::new()
        }
        Mode::Help { .. } => {
            state.mode = Mode::Normal;
            Vec::new()
        }
        Mode::Picker(_) => picker::click(state, col, row),
        Mode::Image(_) => Vec::new(),
        Mode::Hints(_)
        | Mode::Prompt(_)
        | Mode::Command(_)
        | Mode::Normal
        | Mode::Visual(_)
        | Mode::Sidebar(_) => {
            if matches!(
                state.mode,
                Mode::Hints(_) | Mode::Visual(_) | Mode::Sidebar(_)
            ) {
                state.mode = Mode::Normal;
            }
            if matches!(state.mode, Mode::Prompt(_) | Mode::Command(_)) {
                return Vec::new();
            }
            let row = usize::from(row);
            if row >= state.view_rows() {
                return Vec::new();
            }
            let left = state.left();
            if col < left {
                // The sidebar: its entries go to their headings.
                return match sidebar::entry_at(state, row) {
                    Some(i) => sidebar_jump(state, i),
                    None => Vec::new(),
                };
            }
            let line = state.top + row;
            let Some(col) = col.checked_sub(left.saturating_add(state.layout.indent)) else {
                return Vec::new();
            };
            // The column of the line under the pointer (a wide block may be
            // shifted); under a `‹` or `›`, none.
            let shift = state.layout.shift_at(line, state.hscroll());
            let at = state.layout.layout_col(line, col, shift);
            let col = match state.layout.shown_col(line, at, shift) {
                Some(_) => at,
                None => u16::MAX,
            };
            if let Some(occ) = links::occurrence_at(&state.layout, &state.derived, line, col) {
                state.summary_focus = None;
                set_focus(state, occ);
                return follow(state, occ);
            }
            if let Some(id) = state.layout.summary_at(line) {
                return toggle(state, id);
            }
            // A figure shown as an image: the image view.
            let figure = match state.layout.lines.get(line).map(|l| l.kind) {
                Some(LineKind::Image { placement, .. }) => state
                    .layout
                    .images
                    .get(placement as usize)
                    .filter(|p| (p.col..p.col.saturating_add(p.cols)).contains(&col))
                    .map(|p| p.image),
                _ => None,
            };
            match figure {
                Some(image) => view_figure(state, image),
                None => Vec::new(),
            }
        }
    }
}
