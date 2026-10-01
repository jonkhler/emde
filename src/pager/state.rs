//! The pager's state: what [`super::update()`] changes and
//! [`super::view()`] shows.
//!
//! Nothing here does I/O. Documents come in as [`PagerDoc`]s (read by the
//! shell), layouts as values the shell computed; the state keeps the
//! current document ([`Page`]), its layout and everything the reader did:
//! the scroll position, the search, the focused link, the open overlay,
//! the history.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::config::{PagerOptions, SearchCase};
use crate::ir::{Document, HeadingId, LinkId, SrcPos};
use crate::layout::Layout;
use crate::options::{FrontMatterMode, RenderOptions};
use crate::source::{Origin, Source};

use super::PagerDoc;
use super::search::{Corpus, Search};

/// Documents kept in memory for back and forward.
pub(crate) const MAX_PAGES: usize = 8;
/// Longest back or forward list.
const MAX_VISITS: usize = 256;

/// Which document a page is, for the history.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DocKey {
    /// A file (its canonical path).
    Path(PathBuf),
    /// Standard input or text without a file (never reloaded).
    Unnamed(u32),
}

impl DocKey {
    /// The file, if there is one.
    pub fn path(&self) -> Option<&Path> {
        match self {
            DocKey::Path(p) => Some(p),
            DocKey::Unnamed(_) => None,
        }
    }
}

/// A document open in the pager.
#[derive(Debug)]
pub(crate) struct Page {
    pub(crate) key: DocKey,
    /// Shown in the status bar (the file name, or `stdin`).
    pub(crate) name: String,
    pub(crate) source: Source,
    pub(crate) doc: Document,
    /// Added to link numbers in OSC 8 ids, so links of different
    /// documents never share an id.
    pub(crate) link_base: u32,
    front_matter: FrontMatterMode,
    corpus: OnceCell<Corpus>,
}

impl Page {
    pub(crate) fn new(
        pd: PagerDoc,
        key: DocKey,
        link_base: u32,
        front_matter: FrontMatterMode,
    ) -> Page {
        let name = display_name(&pd.source.origin);
        Page {
            key,
            name,
            source: pd.source,
            doc: pd.doc,
            link_base,
            front_matter,
            corpus: OnceCell::new(),
        }
    }

    /// The search corpus, built on first use.
    pub(crate) fn corpus(&self) -> &Corpus {
        self.corpus
            .get_or_init(|| Corpus::new(&self.doc, self.front_matter))
    }

    /// The file behind the page, if it can be reloaded.
    pub(crate) fn path(&self) -> Option<&Path> {
        match &self.source.origin {
            Origin::File(p) => Some(p),
            Origin::Stdin | Origin::Memory => None,
        }
    }

    /// Where relative links go from.
    pub(crate) fn base_dir(&self) -> PathBuf {
        self.doc
            .base_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// The status bar name of a source: its file name, or `stdin`.
fn display_name(origin: &Origin) -> String {
    let raw = match origin {
        Origin::File(p) => p.file_name().map_or_else(
            || p.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        Origin::Stdin => "stdin".to_owned(),
        Origin::Memory => "text".to_owned(),
    };
    crate::text::sanitize(&raw).replace(['\n', '\t'], " ")
}

/// One occurrence of a link on screen: the hits of one link that follow
/// each other (a link wrapped over lines has one hit per line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Occurrence {
    /// Index of the first hit in [`Layout::link_hits`].
    pub(crate) first: u32,
    /// Number of hits.
    pub(crate) hits: u32,
    /// Line of the first hit.
    pub(crate) line: u32,
    pub(crate) link: LinkId,
    /// A footnote back-link.
    pub(crate) back: bool,
}

/// Indexes of one layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Derived {
    /// Link occurrences, top to bottom.
    pub(crate) links: Vec<Occurrence>,
    /// Shown headings with their first line, top to bottom.
    pub(crate) headings: Vec<(u32, HeadingId)>,
}

impl Derived {
    pub(crate) fn new(layout: &Layout) -> Derived {
        let mut links: Vec<Occurrence> = Vec::new();
        for (i, hit) in layout.link_hits.iter().enumerate() {
            let i = u32::try_from(i).unwrap_or(u32::MAX);
            if let Some(last) = links.last_mut() {
                let last_line = last.line.saturating_add(last.hits - 1);
                if last.link == hit.link && last.back == hit.back && hit.line == last_line + 1 {
                    last.hits += 1;
                    continue;
                }
            }
            links.push(Occurrence {
                first: i,
                hits: 1,
                line: hit.line,
                link: hit.link,
                back: hit.back,
            });
        }
        let headings = layout
            .heading_line
            .iter()
            .enumerate()
            .filter(|&(_, &line)| line != u32::MAX)
            .map(|(i, &line)| (line, HeadingId(u32::try_from(i).unwrap_or(u32::MAX))))
            .collect();
        Derived { links, headings }
    }

    /// The heading whose section contains `line`.
    pub(crate) fn section_at(&self, line: usize) -> Option<HeadingId> {
        let line = u32::try_from(line).unwrap_or(u32::MAX);
        let i = self.headings.partition_point(|&(l, _)| l <= line);
        i.checked_sub(1)
            .and_then(|i| self.headings.get(i))
            .map(|&(_, h)| h)
    }
}

/// The focused link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Focus {
    /// Index into [`Derived::links`].
    pub(crate) occ: usize,
    pub(crate) link: LinkId,
    pub(crate) back: bool,
    /// Where it is, to find it again after a new layout.
    pub(crate) pos: SrcPos,
}

/// The search prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prompt {
    pub(crate) backward: bool,
    pub(crate) input: String,
    /// Where the screen was when the prompt opened (incremental search
    /// starts there, cancelling returns there).
    pub(crate) origin: Place,
    /// The search before the prompt opened.
    pub(crate) saved: Option<Search>,
}

/// The outline overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outline {
    pub(crate) filter: String,
    /// Headings that pass the filter.
    pub(crate) items: Vec<HeadingId>,
    /// Index into `items`.
    pub(crate) selected: usize,
    /// First item shown.
    pub(crate) scroll: usize,
}

/// Link hints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hints {
    /// Label and link occurrence.
    pub(crate) labels: Vec<(String, usize)>,
    /// What was typed so far.
    pub(crate) typed: String,
}

/// What has the keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    Prompt(Prompt),
    Outline(Outline),
    Help {
        scroll: usize,
    },
    Hints(Hints),
    /// The `:` prompt, with what was typed after the colon.
    Command(String),
}

/// A message in the status bar, shown until the next key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Message {
    pub(crate) text: String,
    pub(crate) error: bool,
}

/// A place in a document that survives a new layout: a position, and
/// which of the lines that share it (a heading's rule and the blank line
/// after it have the heading's position).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Place {
    pub pos: SrcPos,
    /// Lines after the first line with `pos`.
    pub extra: u32,
}

impl Place {
    /// The place of line `line`.
    pub fn of(layout: &Layout, line: usize) -> Place {
        let Some(pos) = layout.lines.get(line).map(|l| l.pos) else {
            return Place::default();
        };
        let first = layout.line_at(pos);
        Place {
            pos,
            extra: u32::try_from(line.saturating_sub(first)).unwrap_or(u32::MAX),
        }
    }

    /// The line showing this place in `layout`.
    pub fn line(&self, layout: &Layout) -> usize {
        let first = layout.line_at(self.pos);
        let Some(pos) = layout.lines.get(first).map(|l| l.pos) else {
            return first;
        };
        let same = layout
            .lines
            .iter()
            .skip(first + 1)
            .take(self.extra as usize)
            .take_while(|l| l.pos == pos)
            .count();
        first + same
    }
}

/// A visited place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Visit {
    pub(crate) key: DocKey,
    pub(crate) place: Place,
}

/// Back and forward lists, and the documents kept in memory.
#[derive(Debug, Default)]
pub(crate) struct History {
    pub(crate) back: Vec<Visit>,
    pub(crate) forward: Vec<Visit>,
    /// Least recently used first; the current page is always here.
    pub(crate) pages: Vec<Rc<Page>>,
}

impl History {
    pub(crate) fn push_back(&mut self, v: Visit) {
        push_capped(&mut self.back, v);
    }

    pub(crate) fn push_forward(&mut self, v: Visit) {
        push_capped(&mut self.forward, v);
    }

    /// A kept page.
    pub(crate) fn page(&self, key: &DocKey) -> Option<Rc<Page>> {
        self.pages.iter().find(|p| &p.key == key).cloned()
    }

    /// Keep `page` as the most recently used one, dropping the least
    /// recently used file beyond [`MAX_PAGES`] (standard input, which
    /// cannot be read again, is kept).
    pub(crate) fn touch(&mut self, page: &Rc<Page>) {
        self.pages.retain(|p| p.key != page.key);
        self.pages.push(Rc::clone(page));
        while self.pages.len() > MAX_PAGES {
            let Some(i) = self.pages.iter().position(|p| p.path().is_some()) else {
                break;
            };
            if i + 1 == self.pages.len() {
                break;
            }
            self.pages.remove(i);
        }
    }
}

fn push_capped(list: &mut Vec<Visit>, v: Visit) {
    list.push(v);
    if list.len() > MAX_VISITS {
        list.remove(0);
    }
}

/// Settings that do not change while the pager runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Lines per wheel step.
    pub scroll_lines: u16,
    pub search_case: SearchCase,
    /// How front matter is shown (hidden front matter is not searched).
    pub front_matter: FrontMatterMode,
    /// ASCII decorations only (overlay borders).
    pub ascii: bool,
    /// East Asian Ambiguous characters are wide.
    pub ambiguous_wide: bool,
}

impl Settings {
    /// The settings for `pager` and `render` options.
    pub fn new(pager: &PagerOptions, render: &RenderOptions) -> Settings {
        Settings {
            scroll_lines: pager.scroll_lines.max(1),
            search_case: pager.search_case,
            front_matter: render.front_matter,
            ascii: render.ascii,
            ambiguous_wide: render.ambiguous_wide,
        }
    }
}

/// Where the view goes once the next layout is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Goto {
    /// The line showing this place.
    Place(Place),
    /// A heading or `<a id>` anchor (by name).
    Anchor(String),
    /// The top of the document.
    Top,
}

/// The pager's state; see the module docs.
#[derive(Debug)]
pub struct State {
    pub(crate) page: Rc<Page>,
    pub(crate) layout: Layout,
    pub(crate) derived: Derived,
    /// Bumped with every new layout (row hashes include it).
    pub(crate) generation: u64,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    /// First document line on screen.
    pub(crate) top: usize,
    /// Count typed before a command.
    pub(crate) count: Option<u32>,
    pub(crate) mode: Mode,
    pub(crate) search: Option<Search>,
    /// The last pattern searched and whether it went up (`?`), for an
    /// empty `/` and for `n` after the search was cleared.
    pub(crate) last_pattern: Option<(String, bool)>,
    pub(crate) focus: Option<Focus>,
    pub(crate) message: Option<Message>,
    pub(crate) history: History,
    pub(crate) pending: Option<Goto>,
    /// `max_width` switched off (`w`).
    pub(crate) wide: bool,
    pub(crate) mouse: bool,
    pub(crate) watch: bool,
    pub(crate) settings: Settings,
    /// The documents named on the command line, in order (`:n`, `:p`).
    pub(crate) files: Vec<DocKey>,
    /// Which of them was shown last.
    pub(crate) file: usize,
    /// The file `:n` or `:p` asked for, until it is shown.
    pub(crate) pending_file: Option<usize>,
    next_unnamed: u32,
    next_link_base: u32,
}

impl State {
    /// The state for a first document, its layout for a `cols` × `rows`
    /// terminal, and the pager options (mouse and watch start as they
    /// say).
    pub fn new(
        doc: PagerDoc,
        key: Option<DocKey>,
        layout: Layout,
        (cols, rows): (u16, u16),
        pager: &PagerOptions,
        settings: Settings,
    ) -> State {
        let key = key.unwrap_or(DocKey::Unnamed(0));
        let links = u32::try_from(doc.doc.links.len()).unwrap_or(u32::MAX);
        let page = Rc::new(Page::new(doc, key.clone(), 0, settings.front_matter));
        let mut history = History::default();
        history.touch(&page);
        // Whether files are watched; standard input never is (`watching`).
        let watch = pager.watch;
        let mut state = State {
            derived: Derived::new(&layout),
            page,
            layout,
            generation: 1,
            cols,
            rows,
            top: 0,
            count: None,
            mode: Mode::Normal,
            search: None,
            last_pattern: None,
            focus: None,
            message: None,
            history,
            pending: None,
            wide: false,
            mouse: pager.mouse,
            watch,
            settings,
            files: vec![key],
            file: 0,
            pending_file: None,
            next_unnamed: 1,
            next_link_base: links,
        };
        state.clamp_top();
        state
    }

    /// The other documents named on the command line, after the first one
    /// (`key`: their file, `None` for standard input): `:n` and `:p` go
    /// through them all. They are kept in memory like visited documents
    /// (as far as the history keeps documents; files it drops are read
    /// again when shown), the first document staying the current one.
    pub fn add_files(&mut self, docs: Vec<(PagerDoc, Option<DocKey>)>) {
        for (doc, key) in docs {
            let page = self.new_page(doc, key);
            self.files.push(page.key.clone());
            self.history.touch(&page);
        }
        let current = Rc::clone(&self.page);
        self.history.touch(&current);
    }

    /// A page for a newly read document.
    pub(crate) fn new_page(&mut self, doc: PagerDoc, key: Option<DocKey>) -> Rc<Page> {
        let key = key.unwrap_or_else(|| {
            let k = DocKey::Unnamed(self.next_unnamed);
            self.next_unnamed += 1;
            k
        });
        let base = self.link_base(doc.doc.links.len());
        Rc::new(Page::new(doc, key, base, self.settings.front_matter))
    }

    /// A fresh range of OSC 8 link numbers for a document with `links`
    /// links.
    pub(crate) fn link_base(&mut self, links: usize) -> u32 {
        let base = self.next_link_base;
        let n = u32::try_from(links).unwrap_or(u32::MAX);
        self.next_link_base = base.saturating_add(n);
        base
    }

    /// Rows for the document (all but the status bar).
    pub(crate) fn view_rows(&self) -> usize {
        usize::from(self.rows.saturating_sub(1))
    }

    /// The last top line that still fills the screen.
    pub(crate) fn max_top(&self) -> usize {
        self.layout.len().saturating_sub(self.view_rows())
    }

    pub(crate) fn clamp_top(&mut self) {
        self.top = self.top.min(self.max_top());
    }

    /// Put `line` at the top of the screen (as far as the end allows).
    pub(crate) fn jump_to(&mut self, line: usize) {
        self.top = line;
        self.clamp_top();
    }

    /// Scroll so `line` is on screen: nothing when it already is,
    /// otherwise it goes a third of the way down.
    pub(crate) fn reveal(&mut self, line: usize) {
        let rows = self.view_rows();
        if line >= self.top && line < self.top + rows {
            return;
        }
        self.top = line.saturating_sub(rows.saturating_sub(1) / 3);
        self.clamp_top();
    }

    /// The position of the top line.
    pub(crate) fn top_pos(&self) -> SrcPos {
        self.layout
            .lines
            .get(self.top)
            .map(|l| l.pos)
            .unwrap_or_default()
    }

    /// The place of the top line (to re-anchor after a new layout).
    pub(crate) fn top_place(&self) -> Place {
        Place::of(&self.layout, self.top)
    }

    pub(crate) fn visit(&self) -> Visit {
        Visit {
            key: self.page.key.clone(),
            place: self.top_place(),
        }
    }

    /// Show a message until the next key.
    pub(crate) fn say(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: false,
        });
    }

    /// Show an error until the next key.
    pub(crate) fn complain(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: true,
        });
    }

    // Read-only views for tests and the shell.

    /// First document line on screen.
    pub fn top(&self) -> usize {
        self.top
    }

    /// The current layout.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The current document.
    pub fn document(&self) -> &Document {
        &self.page.doc
    }

    /// The current document's text.
    pub fn source(&self) -> &Source {
        &self.page.source
    }

    /// The current document's key.
    pub fn key(&self) -> &DocKey {
        &self.page.key
    }

    /// The terminal size the state is for: `(columns, rows)`.
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Whether the mouse is on.
    pub fn mouse(&self) -> bool {
        self.mouse
    }

    /// Whether the file is watched.
    pub fn watching(&self) -> bool {
        self.watch && self.page.path().is_some()
    }

    /// Whether `max_width` is switched off.
    pub fn wide(&self) -> bool {
        self.wide
    }

    /// The status message, if any.
    pub fn message(&self) -> Option<&str> {
        self.message.as_ref().map(|m| m.text.as_str())
    }

    /// Number of search matches (`None` without a search).
    pub fn match_count(&self) -> Option<usize> {
        self.search.as_ref().map(|s| s.matches.len())
    }

    /// The current match index.
    pub fn current_match(&self) -> Option<usize> {
        self.search.as_ref().and_then(|s| s.current)
    }

    /// The focused link.
    pub fn focused_link(&self) -> Option<LinkId> {
        self.focus.map(|f| f.link)
    }

    /// Whether an overlay or prompt has the keys.
    pub fn mode_name(&self) -> &'static str {
        match self.mode {
            Mode::Normal => "normal",
            Mode::Prompt(_) => "prompt",
            Mode::Outline(_) => "outline",
            Mode::Help { .. } => "help",
            Mode::Hints(_) => "hints",
            Mode::Command(_) => "command",
        }
    }

    /// The position of the current document among the files named on the
    /// command line, and how many there are: `Some((0, 2))` for the first
    /// of two; `None` when the document is not one of them.
    pub fn file_position(&self) -> Option<(usize, usize)> {
        (self.files.get(self.file) == Some(&self.page.key)).then_some((self.file, self.files.len()))
    }

    /// Whether a document with this key is in memory.
    pub fn has_page(&self, key: &DocKey) -> bool {
        self.history.page(key).is_some()
    }

    /// Back list length.
    pub fn back_len(&self) -> usize {
        self.history.back.len()
    }

    /// Forward list length.
    pub fn forward_len(&self) -> usize {
        self.history.forward.len()
    }

    /// Number of documents in memory.
    pub fn pages_kept(&self) -> usize {
        self.history.pages.len()
    }
}
