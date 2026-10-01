//! `view(&State, &Ctx) -> Frame`: what the screen shows, as a pure
//! function of the state.
//!
//! A [`Frame`] has one [`Row`] per screen row. Document rows name their
//! layout line and the style marks laid over it (search matches, the
//! focused link); the status bar and the overlays (outline, help, link
//! hints) are already bytes. Every row carries a 64-bit hash of what it
//! shows, so the painter ([`super::diff`]) only writes rows that changed —
//! and building a frame never touches the layout's text for rows that
//! merely scroll.

use std::borrow::Cow;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::render::Mark;
use crate::render::sgr::{self, Palette};
use crate::style::{Attrs, Color, Style, StylePatch, Underline};
use crate::term::{Caps, ColorDepth};
use crate::text::str_width;
use crate::text::width::{grapheme_width, next_grapheme_end};
use crate::theme::{Element, Theme};

use super::keymap::{self, HelpLine};
use super::links;
use super::search::{LineText, line_marks};
use super::state::{Hints, Mode, Outline, State};
use super::toc::{self, BoxGeom};

/// Width of the keys column in the help.
const HELP_KEYS: usize = 22;

/// A style as a patch: its colours and attributes replace those
/// underneath (default colours leave them alone).
fn patch_of(s: Style) -> StylePatch {
    let color = |c: Color| (c != Color::Default).then_some(c);
    StylePatch {
        fg: color(s.fg),
        bg: color(s.bg),
        underline_color: color(s.underline_color),
        set: s.attrs,
        clear: Attrs::empty(),
        underline: (s.underline != Underline::None).then_some(s.underline),
    }
}

/// `over` applied after `under`, as one patch.
fn compose(under: StylePatch, over: StylePatch) -> StylePatch {
    StylePatch {
        fg: over.fg.or(under.fg),
        bg: over.bg.or(under.bg),
        underline_color: over.underline_color.or(under.underline_color),
        set: (over.set | (under.set - under.clear)) - over.clear,
        clear: over.clear | (under.clear - over.set),
        underline: over.underline.or(under.underline),
    }
}

/// The pager's own styles, from the theme.
#[derive(Clone, Debug, PartialEq, Eq)]
struct UiStyles {
    status: Style,
    name: Style,
    message: Style,
    error: Style,
    prompt: Style,
    cursor: Style,
    search_match: StylePatch,
    search_current: StylePatch,
    link_focus: StylePatch,
    hint: Style,
    toc: Style,
    toc_current: Style,
    toc_selected: Style,
    border: Style,
    title: Style,
    key: Style,
    text: Style,
    muted: Style,
}

impl UiStyles {
    fn new(theme: &Theme) -> UiStyles {
        let status = theme.style(Element::Status);
        let toc = theme.style(Element::Toc);
        let error_fg = theme.style(Element::AlertCaution).fg;
        let error = Style {
            fg: if error_fg == Color::Default {
                theme.style(Element::StatusMsg).fg
            } else {
                error_fg
            },
            ..theme.style(Element::StatusMsg)
        }
        .with(Attrs::BOLD);
        UiStyles {
            name: status.with(Attrs::BOLD),
            message: theme.style(Element::StatusMsg),
            error,
            prompt: theme.style(Element::Prompt),
            cursor: theme.style(Element::Prompt).with(Attrs::REVERSE),
            search_match: patch_of(theme.style(Element::SearchMatch)),
            search_current: patch_of(theme.style(Element::SearchCurrent)),
            link_focus: patch_of(theme.style(Element::LinkFocus)),
            hint: theme.style(Element::Hint),
            toc_current: theme.style(Element::TocCurrent),
            toc_selected: toc.with(Attrs::REVERSE),
            toc,
            border: theme.style(Element::Muted),
            title: theme.style(Element::H3),
            key: theme.style(Element::Strong),
            text: theme.style(Element::Text),
            muted: theme.style(Element::Muted),
            status,
        }
    }
}

/// What [`view`] needs besides the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ctx {
    styles: UiStyles,
    depth: ColorDepth,
    styled_underline: bool,
}

impl Ctx {
    /// The view context for a theme and a terminal.
    pub fn new(theme: &Theme, caps: &Caps) -> Ctx {
        Ctx {
            styles: UiStyles::new(theme),
            depth: caps.color,
            styled_underline: caps.styled_underline,
        }
    }
}

/// What a row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// Layout line `index`, with marks over its text.
    Line { index: usize, marks: Vec<Mark> },
    /// Nothing (past the end of the document).
    Blank,
    /// Ready bytes, the whole row wide (the status bar).
    Text(Vec<u8>),
}

/// Bytes drawn over a row from column `col` (overlays).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub col: u16,
    pub bytes: Vec<u8>,
}

/// One screen row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Hash of everything the row shows.
    pub hash: u64,
    pub body: Body,
    /// Drawn after the body, in order.
    pub overlays: Vec<Segment>,
}

/// A whole screen; see the module docs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    /// Document line on the first row.
    pub top: usize,
    /// The layout generation the document rows refer to.
    pub generation: u64,
    /// One entry per screen row; the last one is the status bar.
    pub lines: Vec<Row>,
    /// Whether an overlay is drawn (no scroll fast path then).
    pub overlay: bool,
    /// Whether an image drawn by a pixel protocol that does not scroll
    /// with the text (iTerm2, sixel, kitty classic) is on screen. Nothing
    /// draws such images yet, so this is always `false`.
    pub images: bool,
}

/// A row being built.
struct Building {
    body: Body,
    overlays: Vec<Segment>,
}

/// Styled bytes: SGR transitions between downsampled styles.
struct Styled {
    palette: Palette,
    pen: Style,
    out: Vec<u8>,
    cols: usize,
}

impl Styled {
    fn new(ctx: &Ctx) -> Styled {
        Styled {
            palette: Palette::new(ctx.depth, ctx.styled_underline),
            pen: Style::PLAIN,
            out: Vec::with_capacity(256),
            cols: 0,
        }
    }

    /// Text in `style`, measured with `amb`.
    fn put(&mut self, style: &Style, text: &str, amb: bool) {
        if text.is_empty() {
            return;
        }
        let s = self.palette.style(style);
        sgr::write_transition(&self.pen, &s, &mut self.out);
        self.pen = s;
        self.out.extend_from_slice(text.as_bytes());
        self.cols += str_width(text, amb);
    }

    fn spaces(&mut self, style: &Style, n: usize) {
        if n > 0 {
            self.put(style, &" ".repeat(n), false);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.pen != Style::PLAIN {
            self.out.extend_from_slice(sgr::RESET);
        }
        self.out
    }
}

/// Text safe to show: control characters as pictures, one line.
fn clean(s: &str) -> Cow<'_, str> {
    let s = crate::text::sanitize(s);
    if s.contains(['\n', '\t']) {
        Cow::Owned(s.replace(['\n', '\t'], " "))
    } else {
        s
    }
}

/// The longest prefix of `s` at most `cols` wide (whole graphemes), and
/// its width.
fn fit(s: &str, cols: usize, amb: bool) -> (&str, usize) {
    if s.is_ascii() {
        let n = s.len().min(cols);
        return (s.get(..n).unwrap_or(""), n);
    }
    let mut pos = 0;
    let mut used = 0;
    while pos < s.len() {
        let end = next_grapheme_end(s, pos);
        let w = grapheme_width(s.get(pos..end).unwrap_or(""), amb);
        if used + w > cols {
            break;
        }
        used += w;
        pos = end;
    }
    (s.get(..pos).unwrap_or(""), used)
}

/// `s` in at most `cols` columns, ending in `…` when cut.
fn ellipsize(s: &str, cols: usize, amb: bool) -> Cow<'_, str> {
    if str_width(s, amb) <= cols {
        return Cow::Borrowed(s);
    }
    if cols == 0 {
        return Cow::Borrowed("");
    }
    let (head, _) = fit(s, cols - 1, amb);
    Cow::Owned(format!("{head}…"))
}

/// Draw `(style, text)` pieces into exactly `cols` columns: cut where they
/// run out, padded with `pad` after.
fn draw_pieces(
    w: &mut Styled,
    pieces: &[(Style, Cow<'_, str>)],
    cols: usize,
    pad: &Style,
    amb: bool,
) {
    let start = w.cols;
    for (style, text) in pieces {
        let room = cols.saturating_sub(w.cols - start);
        if room == 0 {
            break;
        }
        let (shown, _) = fit(text, room, amb);
        w.put(style, shown, amb);
    }
    let used = w.cols - start;
    w.spaces(pad, cols.saturating_sub(used));
}

/// The screen for `state`; see the module docs.
pub fn view(state: &State, ctx: &Ctx) -> Frame {
    let rows = usize::from(state.rows);
    let mut building: Vec<Building> = Vec::with_capacity(rows);
    if rows > 0 && state.cols > 0 {
        for r in 0..state.view_rows() {
            let index = state.top + r;
            let body = if index < state.layout.len() {
                Body::Line {
                    index,
                    marks: marks(state, ctx, index),
                }
            } else {
                Body::Blank
            };
            building.push(Building {
                body,
                overlays: Vec::new(),
            });
        }
        match &state.mode {
            Mode::Outline(o) => outline(state, ctx, o, &mut building),
            Mode::Help { scroll } => help(state, ctx, *scroll, &mut building),
            Mode::Hints(h) => hints(state, ctx, h, &mut building),
            Mode::Normal | Mode::Prompt(_) => {}
        }
        building.push(Building {
            body: Body::Text(status_bar(state, ctx)),
            overlays: Vec::new(),
        });
    }
    let overlay = building.iter().any(|b| !b.overlays.is_empty());
    let lines = building
        .into_iter()
        .map(|b| Row {
            hash: hash_row(state.generation, &b.body, &b.overlays),
            body: b.body,
            overlays: b.overlays,
        })
        .collect();
    Frame {
        cols: state.cols,
        rows: state.rows,
        top: state.top,
        generation: state.generation,
        lines,
        overlay,
        images: false,
    }
}

fn hash_row(generation: u64, body: &Body, overlays: &[Segment]) -> u64 {
    let mut h = DefaultHasher::new();
    match body {
        Body::Line { index, marks } => {
            0u8.hash(&mut h);
            generation.hash(&mut h);
            index.hash(&mut h);
            for m in marks {
                m.range.start.hash(&mut h);
                m.range.end.hash(&mut h);
                m.patch.hash(&mut h);
            }
        }
        Body::Blank => 1u8.hash(&mut h),
        Body::Text(bytes) => {
            2u8.hash(&mut h);
            bytes.hash(&mut h);
        }
    }
    for s in overlays {
        s.col.hash(&mut h);
        s.bytes.hash(&mut h);
    }
    h.finish()
}

// ---------------------------------------------------------------------------
// Marks
// ---------------------------------------------------------------------------

/// The marks of document line `index`: the focused link, with search
/// matches over it.
fn marks(state: &State, ctx: &Ctx, index: usize) -> Vec<Mark> {
    let focus = focus_marks(state, ctx, index);
    let found = match &state.search {
        Some(s) if !s.matches.is_empty() => line_marks(
            state.page.corpus(),
            &state.layout,
            index,
            s,
            ctx.styles.search_match,
            ctx.styles.search_current,
        ),
        _ => Vec::new(),
    };
    merge(focus, found)
}

/// Marks over the spans of the focused link on line `index`.
fn focus_marks(state: &State, ctx: &Ctx, index: usize) -> Vec<Mark> {
    let mut out = Vec::new();
    let Some(f) = state.focus else {
        return out;
    };
    let Some(o) = state.derived.links.get(f.occ) else {
        return out;
    };
    let first = o.first as usize;
    let hits = state
        .layout
        .link_hits
        .get(first..first.saturating_add(o.hits as usize))
        .unwrap_or(&[]);
    if !hits.iter().any(|h| h.line as usize == index) {
        return out;
    }
    let text = LineText::new(&state.layout, index);
    for hit in hits.iter().filter(|h| h.line as usize == index) {
        // Columns to bytes of the line's text.
        let mut col = 0u16;
        let mut start = None;
        let mut end = 0usize;
        let mut byte = 0usize;
        for span in state.layout.line_spans(index) {
            let len = span.len as usize;
            if col >= hit.cols.start && col < hit.cols.end {
                start.get_or_insert(byte);
                end = byte + len;
            }
            col = col.saturating_add(span.cols);
            byte += len;
        }
        if let Some(s) = start {
            text.marks(s..end, ctx.styles.link_focus, &mut out);
        }
    }
    out
}

/// `over` laid on `under`: where both cover the text, the patches combine.
fn merge(under: Vec<Mark>, over: Vec<Mark>) -> Vec<Mark> {
    if over.is_empty() {
        return under;
    }
    if under.is_empty() {
        return over;
    }
    let mut cuts: Vec<u32> = under
        .iter()
        .chain(&over)
        .flat_map(|m| [m.range.start, m.range.end])
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let covering = |marks: &[Mark], a: u32, b: u32| {
        marks
            .iter()
            .find(|m| m.range.start <= a && b <= m.range.end)
            .map(|m| m.patch)
    };
    let mut out: Vec<Mark> = Vec::new();
    for w in cuts.windows(2) {
        let &[a, b] = w else {
            continue;
        };
        let patch = match (covering(&under, a, b), covering(&over, a, b)) {
            (Some(u), Some(o)) => compose(u, o),
            (Some(p), None) | (None, Some(p)) => p,
            (None, None) => continue,
        };
        match out.last_mut() {
            Some(last) if last.range.end == a && last.patch == patch => last.range.end = b,
            _ => out.push(Mark { range: a..b, patch }),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

/// Search counter and position: `3/12  L 120/1480 13%`.
fn status_right(state: &State) -> String {
    let mut parts = Vec::new();
    if let Some(s) = &state.search {
        let plus = if s.capped { "+" } else { "" };
        let n = s.matches.len();
        parts.push(match (n, s.current) {
            (0, _) => "no match".to_owned(),
            (_, Some(i)) if !matches!(state.mode, Mode::Prompt(_)) => {
                format!("{}/{n}{plus}", i + 1)
            }
            (1, _) => "1 match".to_owned(),
            _ => format!("{n}{plus} matches"),
        });
    }
    let total = state.layout.len();
    if total == 0 {
        parts.push("L 0/0".to_owned());
    } else {
        let seen = (state.top + state.view_rows()).min(total);
        parts.push(format!(
            "L {}/{total} {}%",
            state.top + 1,
            seen.saturating_mul(100) / total
        ));
    }
    parts.join("  ")
}

/// The left part: the prompt, a message, or name and breadcrumb.
fn status_left<'a>(state: &'a State, ctx: &Ctx, room: usize) -> Vec<(Style, Cow<'a, str>)> {
    let st = &ctx.styles;
    let amb = state.settings.ambiguous_wide;
    if let Mode::Prompt(p) = &state.mode {
        let sigil = if p.backward { "?" } else { "/" };
        // Keep the end of a long pattern in view.
        let input = clean(&p.input);
        let width = str_width(&input, amb);
        let input = if width + 2 > room {
            let skip = width + 2 - room;
            let mut pos = 0;
            let mut dropped = 0;
            while dropped < skip && pos < input.len() {
                let end = next_grapheme_end(&input, pos);
                dropped += grapheme_width(input.get(pos..end).unwrap_or(""), amb);
                pos = end;
            }
            Cow::Owned(input.get(pos..).unwrap_or("").to_owned())
        } else {
            input
        };
        return vec![
            (st.prompt, Cow::Borrowed(sigil)),
            (st.prompt, input),
            (st.cursor, Cow::Borrowed(" ")),
        ];
    }
    if let Some(m) = &state.message {
        let style = if m.error { st.error } else { st.message };
        return vec![(style, clean(&m.text))];
    }
    let name = clean(&state.page.name);
    let mut crumbs: Vec<Cow<'_, str>> = state
        .derived
        .section_at(state.top)
        .map(|h| toc::breadcrumb(&state.page.doc, h))
        .unwrap_or_default()
        .into_iter()
        .map(clean)
        .collect();
    let width = |crumbs: &[Cow<'_, str>]| {
        str_width(&name, amb) + crumbs.iter().map(|c| 3 + str_width(c, amb)).sum::<usize>()
    };
    // Too long: the outer sections give way first, the innermost stays.
    while width(&crumbs) > room && crumbs.len() > 1 {
        if let Some(first) = crumbs.first_mut().filter(|c| *c != "…") {
            *first = Cow::Borrowed("…");
        } else if crumbs.len() > 2 {
            crumbs.remove(1);
        } else {
            break;
        }
    }
    let mut out = vec![(st.name, name)];
    for c in crumbs {
        out.push((st.status, Cow::Borrowed(" › ")));
        out.push((st.status, c));
    }
    out
}

fn status_bar(state: &State, ctx: &Ctx) -> Vec<u8> {
    let cols = usize::from(state.cols);
    let amb = state.settings.ambiguous_wide;
    let st = &ctx.styles;
    let right = status_right(state);
    let right_w = str_width(&right, amb);
    let mut w = Styled::new(ctx);
    if right_w + 2 >= cols {
        // Only room for the position.
        let (shown, _) = fit(&right, cols, amb);
        draw_pieces(
            &mut w,
            &[(st.status, Cow::Borrowed(shown))],
            cols,
            &st.status,
            amb,
        );
        return w.finish();
    }
    let room = cols - right_w - 3;
    let left = status_left(state, ctx, room);
    w.spaces(&st.status, 1);
    draw_pieces(&mut w, &left, room, &st.status, amb);
    w.spaces(&st.status, 1);
    w.put(&st.status, &right, amb);
    w.spaces(&st.status, 1);
    w.finish()
}

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

/// Box-drawing glyphs: `[top-left, top-right, bottom-left, bottom-right,
/// horizontal, vertical]`.
fn box_glyphs(ascii: bool) -> [&'static str; 6] {
    if ascii {
        ["+", "+", "+", "+", "-", "|"]
    } else {
        ["╭", "╮", "╰", "╯", "─", "│"]
    }
}

/// A horizontal border with an optional label after the corner.
fn border(ctx: &Ctx, g: &BoxGeom, top: bool, label: &str, ascii: bool, amb: bool) -> Vec<u8> {
    let [tl, tr, bl, br, h, _] = box_glyphs(ascii);
    let st = &ctx.styles;
    let mut w = Styled::new(ctx);
    let inner = g.inner_cols();
    w.put(&st.border, if top { tl } else { bl }, amb);
    let label = ellipsize(label, inner.saturating_sub(3), amb);
    let mut used = 0;
    if !label.is_empty() && inner >= 4 {
        w.put(&st.border, h, amb);
        w.put(&st.title, &format!(" {label} "), amb);
        used = str_width(&label, amb) + 3;
    }
    w.put(&st.border, &h.repeat(inner.saturating_sub(used)), amb);
    if g.w >= 2 {
        w.put(&st.border, if top { tr } else { br }, amb);
    }
    w.finish()
}

/// One inner row of a box: the borders around `pieces`.
fn boxed(
    ctx: &Ctx,
    g: &BoxGeom,
    pieces: &[(Style, Cow<'_, str>)],
    fill: &Style,
    ascii: bool,
    amb: bool,
) -> Vec<u8> {
    let [.., v] = box_glyphs(ascii);
    let st = &ctx.styles;
    let mut w = Styled::new(ctx);
    w.put(&st.border, v, amb);
    draw_pieces(&mut w, pieces, g.inner_cols(), fill, amb);
    if g.w >= 2 {
        w.put(&st.border, v, amb);
    }
    w.finish()
}

fn put_segment(rows: &mut [Building], row: usize, col: u16, bytes: Vec<u8>) {
    if let Some(r) = rows.get_mut(row) {
        r.overlays.push(Segment { col, bytes });
    }
}

fn outline(state: &State, ctx: &Ctx, o: &Outline, rows: &mut [Building]) {
    let g = toc::outline_box(state.cols, state.rows, o.items.len());
    if g.h < 3 || g.w < 4 {
        return;
    }
    let st = &ctx.styles;
    let amb = state.settings.ambiguous_wide;
    let ascii = state.settings.ascii;
    let doc = &state.page.doc;
    let y = usize::from(g.y);
    let count = if o.items.is_empty() {
        "no match".to_owned()
    } else {
        format!("{}/{}", o.selected + 1, o.items.len())
    };
    put_segment(rows, y, g.x, border(ctx, &g, true, "Outline", ascii, amb));
    let filter: Vec<(Style, Cow<'_, str>)> = if o.filter.is_empty() {
        vec![(st.muted, Cow::Borrowed(" type to filter"))]
    } else {
        vec![
            (st.prompt, Cow::Borrowed(" › ")),
            (st.text, clean(&o.filter)),
            (st.cursor, Cow::Borrowed(" ")),
        ]
    };
    put_segment(
        rows,
        y + 1,
        g.x,
        boxed(ctx, &g, &filter, &st.toc, ascii, amb),
    );
    let current = state.derived.section_at(state.top);
    let base = toc::base_level(doc, &o.items);
    let (marker, plain) = if ascii { ("> ", "  ") } else { ("▸ ", "  ") };
    let list = toc::outline_rows(&g);
    for i in 0..list {
        let item = o.items.get(o.scroll + i).copied();
        let pieces: Vec<(Style, Cow<'_, str>)> =
            match item.and_then(|h| doc.heading(h).map(|x| (h, x))) {
                Some((h, heading)) => {
                    let selected = o.scroll + i == o.selected;
                    let is_current = Some(h) == current;
                    let style = match (selected, is_current) {
                        (true, _) => st.toc_selected,
                        (false, true) => st.toc_current,
                        (false, false) => st.toc,
                    };
                    let indent = usize::from(heading.level.saturating_sub(base)) * 2;
                    vec![
                        (
                            style,
                            Cow::Borrowed(if is_current { marker } else { plain }),
                        ),
                        (style, Cow::Owned(" ".repeat(indent))),
                        (style, clean(&heading.title)),
                    ]
                }
                None => Vec::new(),
            };
        let selected = item.is_some() && o.scroll + i == o.selected;
        let fill = if selected { st.toc_selected } else { st.toc };
        put_segment(
            rows,
            y + 2 + i,
            g.x,
            boxed(ctx, &g, &pieces, &fill, ascii, amb),
        );
    }
    let bottom = y + usize::from(g.h) - 1;
    put_segment(
        rows,
        bottom,
        g.x,
        border(ctx, &g, false, &count, ascii, amb),
    );
}

fn help(state: &State, ctx: &Ctx, scroll: usize, rows: &mut [Building]) {
    let lines = keymap::help_lines();
    let g = toc::help_box(state.cols, state.rows, lines.len());
    if g.h < 3 || g.w < 4 {
        return;
    }
    let st = &ctx.styles;
    let amb = state.settings.ambiguous_wide;
    let ascii = state.settings.ascii;
    let y = usize::from(g.y);
    let shown = g.inner_rows();
    let scroll = scroll.min(lines.len().saturating_sub(shown));
    put_segment(rows, y, g.x, border(ctx, &g, true, "Keys", ascii, amb));
    let keys_w = HELP_KEYS.min(g.inner_cols() / 2);
    for i in 0..shown {
        let pieces: Vec<(Style, Cow<'_, str>)> = match lines.get(scroll + i) {
            Some(HelpLine::Title(t)) => vec![(st.title, Cow::Owned(format!(" {t}")))],
            Some(HelpLine::Entry { keys, help }) => {
                let k = ellipsize(keys, keys_w.saturating_sub(3), amb);
                let pad = keys_w.saturating_sub(str_width(&k, amb) + 2);
                vec![
                    (st.text, Cow::Borrowed("  ")),
                    (st.key, Cow::Owned(k.into_owned())),
                    (st.text, Cow::Owned(" ".repeat(pad))),
                    (st.text, Cow::Borrowed(*help)),
                ]
            }
            Some(HelpLine::Blank) | None => Vec::new(),
        };
        put_segment(
            rows,
            y + 1 + i,
            g.x,
            boxed(ctx, &g, &pieces, &st.text, ascii, amb),
        );
    }
    let more = if scroll + shown < lines.len() {
        "more: j, Space"
    } else {
        "q closes"
    };
    let bottom = y + usize::from(g.h) - 1;
    put_segment(rows, bottom, g.x, border(ctx, &g, false, more, ascii, amb));
}

fn hints(state: &State, ctx: &Ctx, h: &Hints, rows: &mut [Building]) {
    let amb = state.settings.ambiguous_wide;
    let top = state.top;
    let shown = state.view_rows();
    for (label, occ) in &h.labels {
        if !label.starts_with(h.typed.as_str()) {
            continue;
        }
        let Some(o) = state.derived.links.get(*occ) else {
            continue;
        };
        let Some((line, col)) = links::first_visible_hit(&state.layout, o, top, shown) else {
            continue;
        };
        let col = col.saturating_add(state.layout.indent);
        let room = usize::from(state.cols.saturating_sub(col));
        let (text, _) = fit(label, room, amb);
        if text.is_empty() {
            continue;
        }
        let mut w = Styled::new(ctx);
        w.put(&ctx.styles.hint, text, amb);
        put_segment(rows, line - top, col, w.finish());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(set: Attrs, clear: Attrs) -> StylePatch {
        StylePatch {
            set,
            clear,
            ..StylePatch::default()
        }
    }

    #[test]
    fn composed_patches_match_applying_both() {
        let bases = [
            Style::PLAIN,
            Style::PLAIN.with(Attrs::BOLD | Attrs::ITALIC),
            Style::fg(Color::Ansi(1)).with(Attrs::REVERSE),
        ];
        let patches = [
            p(Attrs::REVERSE, Attrs::empty()),
            p(Attrs::BOLD, Attrs::ITALIC),
            p(Attrs::empty(), Attrs::BOLD | Attrs::REVERSE),
            StylePatch {
                fg: Some(Color::Ansi(3)),
                bg: Some(Color::Ansi(4)),
                ..StylePatch::default()
            },
        ];
        for base in bases {
            for a in patches {
                for b in patches {
                    assert_eq!(
                        base.patch(&compose(a, b)),
                        base.patch(&a).patch(&b),
                        "{base:?} {a:?} {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn merging_splits_overlaps() {
        let a = p(Attrs::REVERSE, Attrs::empty());
        let b = p(Attrs::BOLD, Attrs::empty());
        let merged = merge(
            vec![Mark {
                range: 0..10,
                patch: a,
            }],
            vec![Mark {
                range: 5..15,
                patch: b,
            }],
        );
        let ranges: Vec<_> = merged.iter().map(|m| m.range.clone()).collect();
        assert_eq!(ranges, [0..5, 5..10, 10..15]);
        assert_eq!(merged[1].patch, compose(a, b));
        assert_eq!(merged[2].patch, b);
    }

    #[test]
    fn fitting_text() {
        assert_eq!(fit("hello", 3, false), ("hel", 3));
        assert_eq!(fit("日本語", 5, false), ("日本", 4));
        assert_eq!(ellipsize("hello world", 8, false), "hello w…");
        assert_eq!(ellipsize("short", 8, false), "short");
        assert_eq!(ellipsize("x", 0, false), "");
        assert_eq!(clean("a\u{1b}b\tc"), "a␛b c");
    }
}
