//! Block elements: the dispatcher, headings, paragraphs, lists, quotes and
//! alerts, rules, definition lists, `<details>`, alignment, front matter,
//! raw HTML and the footnote section.
//!
//! Code blocks, tables, figures and display math, which have geometry of
//! their own, live in sibling modules.

use crate::color::mix_oklab;
use crate::ir::{
    Alert, Block, CodeBlock, DefItem, FrontMatter, FrontMatterFormat, HAlign, HeadingId, Inlines,
    List,
};
use crate::options::{FrontMatterMode, H1Style, H2Style, When};
use crate::style::{Attrs, Color, Rgb, Style, StyleId};
use crate::term::ColorDepth;
use crate::text::str_width;
use crate::theme::Element;

use super::build::{Builder, Fade, Seg};
use super::inline::{Composed, Look, Tail};
use super::scripts::superscript;
use super::style::Ctx;
use super::{Fill, LineKind, SpanFlags};

/// Share of the alert colour in an alert's background tint.
pub(crate) const ALERT_TINT: f32 = 0.10;

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// How an `h1` bar is painted at the current depth.
enum Bar {
    /// A background gradient (truecolor).
    Gradient(Rgb, Rgb),
    /// One background colour.
    Solid(Style),
    /// Reverse video (16 colours and attributes-only output).
    Reverse,
}

impl Builder<'_> {
    /// Lay out one block.
    pub(super) fn block(&mut self, b: &Block) {
        match b {
            Block::Para(inl) => self.para(inl),
            Block::Heading { level, text, id } => self.heading(*level, text, *id),
            Block::Code(c) => self.code_block(c),
            Block::Math(m) => self.math_block(m),
            Block::Quote { alert, body } => self.quote(*alert, body),
            Block::List(list) => self.list(list),
            Block::Table(t) => self.table(t),
            Block::Rule => self.rule(),
            Block::Figure(f) => self.figure(f),
            Block::DefList(items) => self.deflist(items),
            Block::Details { summary, body } => self.details(summary, body),
            Block::Align { align, body } => self.aligned(*align, |b| b.blocks(body, false)),
            Block::FrontMatter(fm) => self.front_matter(fm),
            Block::Html(raw) => self.html_block(raw),
            Block::FootnoteSection => self.footnote_section(),
        }
    }

    /// Whether gradients (h1 bars, fading rules) are drawn.
    pub(super) fn gradients(&self) -> bool {
        match self.opts.gradients {
            When::Always => self.sty.depth() >= ColorDepth::Ansi16,
            When::Never => false,
            When::Auto => self.sty.depth() == ColorDepth::TrueColor,
        }
    }

    // ----- headings -----------------------------------------------------------

    fn heading(&mut self, level: u8, text: &Inlines, id: Option<HeadingId>) {
        let level = level.clamp(1, 6);
        if let Some(h) = id {
            self.mark_heading(h);
        }
        let off = self.off;
        let width = self.avail();
        let prefix = self.heading_prefix(level, id);
        match level {
            1 => self.h1(text, &prefix, width, off),
            2 => self.h2(text, &prefix, width, off),
            _ => {
                let base = self.sty.el(Self::heading_el(level));
                let c = self.compose(text, base);
                let hang = to_u16(str_width(&prefix, self.amb));
                let c = self.prefixed(&prefix, base, c);
                let look = Look { hang, ..Look::TEXT };
                if !text.is_empty() {
                    self.emit(&c, width, &look, off);
                }
            }
        }
        self.heading_done();
        self.off = off.saturating_add(to_u32(text.text.len()));
    }

    /// Heading numbers (`1.2 `) and the level's marker.
    fn heading_prefix(&mut self, level: u8, id: Option<HeadingId>) -> String {
        let mut prefix = String::new();
        let idx = usize::from(level - 1);
        if self.opts.heading.numbers && id.is_some() {
            if let Some(n) = self.numbers.get_mut(idx) {
                *n = n.saturating_add(1);
            }
            for n in self.numbers.iter_mut().skip(idx + 1) {
                *n = 0;
            }
            let parts: Vec<String> = self
                .numbers
                .iter()
                .take(idx + 1)
                .skip_while(|&&n| n == 0)
                .map(u32::to_string)
                .collect();
            if !parts.is_empty() {
                prefix.push_str(&parts.join("."));
                prefix.push(' ');
            }
        }
        if let Some(marker) = self.deco.markers.get(idx) {
            prefix.push_str(&marker.text);
        }
        prefix
    }

    /// How the `h1` bar is painted, if at all.
    fn h1_bar(&self) -> Option<Bar> {
        let depth = self.sty.depth();
        let style = self.sty.of(Element::H1);
        let gradient = self.theme.gradient(Element::H1);
        if depth == ColorDepth::None {
            return None;
        }
        if self.gradients()
            && let Some((Color::Rgb(from), Color::Rgb(to))) = gradient
        {
            return Some(Bar::Gradient(from, to));
        }
        let bg = match style.bg {
            Color::Default => gradient.map(|(from, _)| from),
            bg => Some(bg),
        };
        let reverse = style.attrs.contains(Attrs::REVERSE);
        match depth {
            ColorDepth::TrueColor | ColorDepth::Ansi256 => match bg {
                Some(bg) => Some(Bar::Solid(style.on(bg))),
                None if reverse => Some(Bar::Solid(style)),
                None => None,
            },
            _ => match bg {
                // A theme made for 16 colours keeps its bar colour.
                Some(bg @ Color::Ansi(_)) => Some(Bar::Solid(style.on(bg))),
                Some(_) => Some(Bar::Reverse),
                None if reverse => Some(Bar::Reverse),
                None => None,
            },
        }
    }

    fn h1(&mut self, text: &Inlines, prefix: &str, width: u16, off: u32) {
        let base = self.sty.el(Element::H1);
        let hang = to_u16(str_width(prefix, self.amb));
        let bar = match self.opts.heading.h1 {
            H1Style::Bar => self.h1_bar(),
            H1Style::Underline | H1Style::Plain => {
                let c = self.compose(text, base);
                let c = self.prefixed(prefix, base, c);
                let look = Look { hang, ..Look::TEXT };
                if !text.is_empty() {
                    self.emit(&c, width, &look, off);
                }
                return;
            }
        };
        let Some(bar) = bar else {
            // No bar: the styled text, over a double rule without escapes.
            let c = self.compose(text, base);
            let c = self.prefixed(prefix, base, c);
            let look = Look { hang, ..Look::TEXT };
            if !text.is_empty() {
                self.emit(&c, width, &look, off);
            }
            if self.sty.depth() == ColorDepth::None {
                let rule = self.deco.h1_rule;
                self.begin();
                self.repeat(rule, width, base);
                self.end(LineKind::Text, Fill::None, off);
            }
            return;
        };
        let x0 = self.right_edge().saturating_sub(width);
        let x1 = self.right_edge();
        let (text_style, ctx, fill) = match bar {
            Bar::Gradient(from, to) => {
                let mut s = self.sty.of(Element::H1);
                s.bg = Color::Default;
                (
                    self.sty.intern(s),
                    Ctx::default(),
                    Fill::Gradient { from, to, x0, x1 },
                )
            }
            Bar::Solid(s) => {
                let id = self.sty.intern(s);
                let ctx = Ctx {
                    bg: Some(s.bg),
                    attrs: Attrs::empty(),
                };
                (
                    id,
                    ctx,
                    Fill::Panel {
                        style: id,
                        to_col: x1,
                    },
                )
            }
            Bar::Reverse => {
                let s = Style::PLAIN.with(Attrs::BOLD | Attrs::REVERSE);
                let id = self.sty.intern(s);
                let ctx = Ctx {
                    bg: None,
                    attrs: Attrs::REVERSE,
                };
                (
                    id,
                    ctx,
                    Fill::Panel {
                        style: id,
                        to_col: x1,
                    },
                )
            }
        };
        self.push_ctx(ctx);
        let c = self.compose(text, text_style);
        let c = self.prefixed(prefix, text_style, c);
        // One column of bar on each side of the text, where there is room.
        let pad = if width >= 3 { 1 } else { 0 };
        let look = Look {
            kind: LineKind::Text,
            fill,
            pad,
            pad_style: text_style,
            hang,
        };
        self.emit(&c, width.saturating_sub(2 * pad).max(1), &look, off);
        self.pop_ctx();
    }

    fn h2(&mut self, text: &Inlines, prefix: &str, width: u16, off: u32) {
        let base = self.sty.el(Element::H2);
        let c = self.compose(text, base);
        let hang = to_u16(str_width(prefix, self.amb));
        let c = self.prefixed(prefix, base, c);
        let look = Look { hang, ..Look::TEXT };
        let widest = if text.is_empty() {
            0
        } else {
            self.emit(&c, width, &look, off)
        };
        if self.opts.heading.h2 == H2Style::Plain {
            return;
        }
        let rule = self.sty.el(Element::HeadingRule);
        let (heavy, light) = (self.deco.h2_heavy, self.deco.h2_light);
        self.begin();
        // The rule spans the whole width: container alignment has nothing
        // to move, the heavy part is placed below.
        self.placed();
        if self.sty.depth() < ColorDepth::Ansi256 {
            self.repeat(light, width, rule);
        } else {
            let under = widest.clamp(1, width);
            // In an aligned container (`<h2 align="center">`) the heavy
            // part stays under the text, light rule on both sides.
            let lead = match self.alignment() {
                HAlign::Left => 0,
                HAlign::Center => (width - under) / 2,
                HAlign::Right => width - under,
            };
            let tail = width - under - lead;
            let fading = match self.sty.of(Element::HeadingRule).fg {
                Color::Rgb(rgb) if self.gradients() => Some(rgb),
                _ => None,
            };
            match fading {
                Some(rgb) => self.fade(light, lead, rgb, Fade::In),
                None => self.repeat(light, lead, rule),
            }
            self.repeat(heavy, under, rule);
            match fading {
                Some(rgb) => self.fade(light, tail, rgb, Fade::Out),
                None => self.repeat(light, tail, rule),
            }
        }
        self.end(LineKind::Text, Fill::None, off);
    }

    /// `cols` columns of `glyph` fading between `rgb` and the background in
    /// the given shape: over the whole run in two-column steps ([`Fade::Out`]
    /// and its mirror [`Fade::In`]), or over the first and last few glyphs
    /// ([`Fade::Both`]). The runs of equal colour are computed once per
    /// glyph width, length, colour and shape.
    fn fade(&mut self, glyph: &str, cols: u16, rgb: Rgb, shape: Fade) {
        let w = to_u16(str_width(glyph, self.amb)).max(1);
        let n = cols / w;
        let key = (w, n, rgb, shape);
        let runs = match self.fades.get(&key) {
            Some(runs) => runs.clone(),
            None => {
                let runs = self.fade_runs(w, n, rgb, shape);
                self.fades.insert(key, runs.clone());
                runs
            }
        };
        let plain = self.sty.el(Element::Rule);
        // Any remainder of a wide glyph pads the faint end of the rule.
        let rest = cols - n * w;
        if shape == Fade::In {
            self.spaces(rest, plain);
        }
        for (style, count) in runs {
            self.put(&glyph.repeat(usize::from(count)), style, None);
        }
        if shape != Fade::In {
            self.spaces(rest, plain);
        }
    }

    /// The runs of [`Builder::fade`]: `(style, glyphs)`.
    fn fade_runs(&mut self, w: u16, n: u16, rgb: Rgb, shape: Fade) -> Vec<(StyleId, u16)> {
        let base = self.theme.base;
        let plain = self.sty.el(Element::Rule);
        let ramp = u32::from((n / 8).clamp(1, 10));
        let steps = (u32::from(n) * u32::from(w) / 2).max(1);
        let mut runs: Vec<(Rgb, u16)> = Vec::new();
        for i in 0..n {
            let t = if shape == Fade::Both {
                let edge = u32::from((i + 1).min(n - i));
                (edge as f32 / (ramp + 1) as f32).min(1.0)
            } else {
                let step = u32::from(i) * u32::from(w) / 2;
                1.0 - 0.85 * step as f32 / steps as f32
            };
            let c = mix_oklab(base, rgb, t);
            match runs.last_mut() {
                Some((last, count)) if *last == c => *count += 1,
                _ => runs.push((c, 1)),
            }
        }
        if shape == Fade::In {
            runs.reverse();
        }
        runs.into_iter()
            .map(|(c, count)| (self.sty.with_fg(plain, Color::Rgb(c)), count))
            .collect()
    }

    // ----- rules --------------------------------------------------------------

    fn rule(&mut self) {
        let width = self.avail();
        let style = self.sty.el(Element::Rule);
        let glyph = self.deco.rule.text.clone();
        self.begin();
        self.placed();
        match self.sty.of(Element::Rule).fg {
            Color::Rgb(rgb) if self.gradients() => self.fade(&glyph, width, rgb, Fade::Both),
            _ => self.repeat(&glyph, width, style),
        }
        self.end(LineKind::Text, Fill::None, self.off);
    }

    // ----- quotes and alerts --------------------------------------------------

    fn quote(&mut self, alert: Option<Alert>, body: &[Block]) {
        let saved = self.text_el;
        match alert {
            None => {
                let bar = self.sty.el(Element::QuoteBar);
                let glyph = self.deco.quote.text.clone();
                let plain = StyleId(0);
                let segs = vec![Seg::new(glyph, bar), Seg::new(" ", plain)];
                self.push_level(segs.clone(), segs);
                self.text_el = Element::Quote;
                self.blocks(body, false);
                self.pop_level();
            }
            Some(a) => self.alert(a, body),
        }
        self.text_el = saved;
    }

    fn alert(&mut self, a: Alert, body: &[Block]) {
        let (el, icon, title) = match a {
            Alert::Note => (Element::AlertNote, 0, "Note"),
            Alert::Tip => (Element::AlertTip, 1, "Tip"),
            Alert::Important => (Element::AlertImportant, 2, "Important"),
            Alert::Warning => (Element::AlertWarning, 3, "Warning"),
            Alert::Caution => (Element::AlertCaution, 4, "Caution"),
        };
        let colour = self.sty.of(el).fg;
        let tint = match colour {
            Color::Rgb(rgb) if self.sty.depth() == ColorDepth::TrueColor => {
                Some(Color::Rgb(mix_oklab(self.theme.base, rgb, ALERT_TINT)))
            }
            _ => None,
        };
        let plain = StyleId(0);
        let quote_bar = self.sty.el(Element::QuoteBar);
        let mut bar = self.sty.with_fg(quote_bar, colour);
        let mut space = plain;
        if let Some(t) = tint {
            bar = self.sty.with_bg(bar, t);
            space = self.sty.with_bg(plain, t);
        }
        let glyph = self.deco.quote.text.clone();
        let segs = vec![Seg::new(glyph.clone(), bar), Seg::new(" ", space)];
        let panel = tint.map(|_| (space, self.right_edge()));
        self.push_level_with_panel(segs.clone(), segs, panel);
        let ctx = Ctx {
            bg: tint,
            attrs: Attrs::empty(),
        };
        self.push_ctx(ctx);
        let title_style = self.sty.el(el);
        let icon = self.deco.icons.get(icon).copied().unwrap_or("!");
        self.begin();
        self.put(icon, title_style, None);
        self.put(" ", title_style, None);
        self.put(title, title_style, None);
        self.end(LineKind::Text, Fill::None, self.off);
        self.text_el = Element::Text;
        self.blocks(body, false);
        self.pop_ctx();
        self.pop_level();
    }

    // ----- lists --------------------------------------------------------------

    fn list(&mut self, list: &List) {
        let depth = self.list_depth;
        self.list_depth += 1;
        let marker_style = self.sty.el(Element::ListMarker);
        let done_style = self.sty.el(Element::TaskDone);
        let todo_style = self.sty.el(Element::TaskTodo);
        let plain = StyleId(0);
        let amb = self.amb;
        let numbers: Vec<String> = match list.start {
            Some(start) => (0..list.items.len())
                .map(|i| format!("{}.", start.saturating_add(i as u64)))
                .collect(),
            None => Vec::new(),
        };
        let number_width = numbers.iter().map(|n| str_width(n, amb)).max().unwrap_or(0);
        let bullet = self.deco.bullet(depth).clone();
        let (open, done) = (self.deco.task_open.clone(), self.deco.task_done.clone());
        let task_width = usize::from(open.cols.max(done.cols));
        let any_task = list.items.iter().any(|i| i.task.is_some());
        let base_width = if list.start.is_some() {
            number_width
        } else if any_task {
            task_width.max(usize::from(bullet.cols))
        } else {
            usize::from(bullet.cols)
        };
        let width = if list.start.is_some() && any_task {
            base_width + 1 + task_width + 1
        } else {
            base_width + 1
        };
        let mut produced = false;
        for (i, item) in list.items.iter().enumerate() {
            let before = self.out.lines.len();
            if produced && !list.tight {
                self.request_gap();
            }
            let mut first: Vec<Seg> = Vec::new();
            let mut used = 0usize;
            if let Some(n) = numbers.get(i) {
                let pad = number_width.saturating_sub(str_width(n, amb));
                first.push(Seg::new(format!("{}{n}", " ".repeat(pad)), marker_style));
                first.push(Seg::new(" ", plain));
                used = number_width + 1;
            }
            match item.task {
                Some(state) => {
                    let (glyph, style) = if state {
                        (&done, done_style)
                    } else {
                        (&open, todo_style)
                    };
                    first.push(Seg::new(glyph.text.clone(), style));
                    used += usize::from(glyph.cols);
                }
                None if list.start.is_none() => {
                    first.push(Seg::new(bullet.text.clone(), marker_style));
                    used += usize::from(bullet.cols);
                }
                None => {}
            }
            if used < width {
                first.push(Seg::new(" ".repeat(width - used), plain));
            }
            let rest = vec![Seg::new(" ".repeat(width), plain)];
            self.push_level(first, rest);
            let dim = item.task == Some(true);
            if dim {
                self.push_ctx(Ctx {
                    bg: None,
                    attrs: Attrs::DIM,
                });
            }
            self.blocks(&item.body, list.tight);
            if self.level_fresh() {
                // An empty item still shows its marker.
                self.begin();
                self.end(LineKind::Text, Fill::None, self.off);
            }
            if dim {
                self.pop_ctx();
            }
            self.pop_level();
            if self.out.lines.len() > before {
                produced = true;
            }
        }
        self.list_depth = depth;
    }

    // ----- definition lists, details --------------------------------------------

    fn deflist(&mut self, items: &[DefItem]) {
        let term = self.sty.el(Element::DefTerm);
        let mut produced = false;
        for item in items {
            let before = self.out.lines.len();
            if produced {
                self.request_gap();
            }
            if !item.term.is_empty() {
                self.para_styled(&item.term, term, &[]);
            }
            for def in &item.defs {
                self.push_indent(4);
                self.blocks(def, false);
                self.pop_level();
            }
            if self.out.lines.len() > before {
                produced = true;
            }
        }
    }

    fn details(&mut self, summary: &Inlines, body: &[Block]) {
        let marker_style = self.sty.el(Element::Muted);
        let strong = self.sty.el(Element::Strong);
        let marker = format!("{} ", self.deco.details);
        let hang = to_u16(str_width(&marker, self.amb));
        let off = self.off;
        let c = if summary.is_empty() {
            self.plain_composed("Details", strong)
        } else {
            self.compose(summary, strong)
        };
        let c = self.prefixed(&marker, marker_style, c);
        let width = self.avail();
        let look = Look { hang, ..Look::TEXT };
        self.emit(&c, width, &look, off);
        self.off = off.saturating_add(to_u32(summary.text.len()));
        self.push_indent(hang);
        self.blocks(body, false);
        self.pop_level();
    }

    // ----- front matter and raw HTML -----------------------------------------------

    fn front_matter(&mut self, fm: &FrontMatter) {
        let off = self.off;
        match (self.opts.front_matter, &fm.fields) {
            (FrontMatterMode::Hide, _) => {}
            (FrontMatterMode::Card, Some(fields)) if !fields.is_empty() => self.card(fields, off),
            _ => {
                let lang = match fm.format {
                    FrontMatterFormat::Yaml => "yaml",
                    FrontMatterFormat::Toml => "toml",
                };
                let code = CodeBlock {
                    lang: Some(lang.into()),
                    info: lang.into(),
                    title: None,
                    code: fm.raw.trim_end_matches('\n').to_string(),
                };
                self.code_block(&code);
            }
        }
        self.off = off.saturating_add(to_u32(fm.raw.len()));
    }

    /// A key/value card in a box (`key: value` lines when too narrow).
    fn card(&mut self, fields: &[(Box<str>, Box<str>)], off: u32) {
        let width = self.avail();
        if width < 16 {
            let key_style = self.sty.el(Element::FrontMatterKey);
            let value_style = self.sty.el(Element::FrontMatterValue);
            for (k, v) in fields {
                let key = self.plain_composed(&format!("{k}: "), key_style);
                let value = self.plain_composed(v, value_style);
                let line = Composed::concat(&[&key, &value]);
                let look = Look {
                    hang: 2,
                    ..Look::TEXT
                };
                self.emit(&line, width, &look, off);
            }
            return;
        }
        let border = self.sty.el(Element::TableBorder);
        let key_style = self.sty.el(Element::FrontMatterKey);
        let value_style = self.sty.el(Element::FrontMatterValue);
        let b = self.deco.frame;
        let amb = self.amb;
        let keys: Vec<Composed<'static>> = fields
            .iter()
            .map(|(k, _)| self.plain_composed(k, key_style))
            .collect();
        let values: Vec<Composed<'static>> = fields
            .iter()
            .map(|(_, v)| self.plain_composed(v, value_style))
            .collect();
        let key_w = keys
            .iter()
            .map(|k| k.natural_width(amb))
            .max()
            .unwrap_or(0)
            .min(width / 3)
            .max(1);
        let value_natural = values
            .iter()
            .map(|v| v.natural_width(amb))
            .max()
            .unwrap_or(0);
        // │ key  value │
        let room = width.saturating_sub(key_w + 6).max(1);
        let value_w = value_natural.clamp(1, room);
        let inner = key_w + 2 + value_w;
        let top = format!(
            "{}{}{}",
            b.top_left,
            b.horizontal.repeat(usize::from(inner + 2)),
            b.top_right
        );
        let bottom = format!(
            "{}{}{}",
            b.bottom_left,
            b.horizontal.repeat(usize::from(inner + 2)),
            b.bottom_right
        );
        self.text_line(&top, border, off);
        for (k, v) in keys.iter().zip(&values) {
            let mut kl = Vec::new();
            let mut kp = Vec::new();
            self.wrap_composed(k, key_w, key_w, &mut kl, &mut kp);
            let mut vl = Vec::new();
            let mut vp = Vec::new();
            self.wrap_composed(v, value_w, value_w, &mut vl, &mut vp);
            let rows = kl.len().max(vl.len());
            for r in 0..rows {
                self.begin();
                self.put_glyph(b.vertical, border);
                self.put(" ", StyleId(0), None);
                let start = self.cols();
                self.cell_line(k, &kl, &kp, r);
                let used = self.cols() - start;
                self.spaces(key_w.saturating_sub(used) + 2, StyleId(0));
                let start = self.cols();
                self.cell_line(v, &vl, &vp, r);
                let used = self.cols() - start;
                self.spaces(value_w.saturating_sub(used) + 1, StyleId(0));
                self.put_glyph(b.vertical, border);
                self.end(LineKind::Text, Fill::None, off);
            }
        }
        self.text_line(&bottom, border, off);
    }

    /// Line `r` of wrapped composed text (nothing if there is no such line).
    pub(super) fn cell_line(
        &mut self,
        c: &Composed<'_>,
        lines: &[crate::text::Line],
        pieces: &[crate::text::Piece],
        r: usize,
    ) {
        let Some(line) = lines.get(r) else {
            return;
        };
        let start = pieces.partition_point(|p| (p.line as usize) < r);
        let end = pieces.partition_point(|p| (p.line as usize) <= r);
        self.put_pieces(c, line, pieces.get(start..end).unwrap_or(&[]), None);
    }

    /// Raw HTML (`html = "raw"`), shown as dim text.
    fn html_block(&mut self, raw: &str) {
        let style = self.sty.el(Element::Html);
        let width = self.avail();
        let off = self.off;
        for line in raw.split('\n') {
            let c = self.plain_composed(line.trim_end(), style);
            self.emit(&c, width, &Look::TEXT, off);
        }
        self.off = off.saturating_add(to_u32(raw.len()));
    }

    // ----- footnotes ----------------------------------------------------------------

    fn footnote_section(&mut self) {
        let doc = self.doc;
        if doc.footnotes.is_empty() {
            return;
        }
        let width = self.avail();
        let rule = self.sty.el(Element::Rule);
        let label = self.sty.el(Element::Footnote);
        let light = self.deco.h2_light;
        let off = self.off;
        self.begin();
        self.repeat(light, 2, rule);
        self.put(" ", rule, None);
        self.put("Footnotes", label, None);
        self.put(" ", rule, None);
        let used = self.content_cols();
        self.repeat(light, width.saturating_sub(used), rule);
        self.end(LineKind::Text, Fill::None, off);
        self.request_gap();

        let saved = self.text_el;
        self.text_el = Element::Footnote;
        let n = doc.footnotes.len();
        let marker_w = n.to_string().len() + 1;
        let marker_style = self.sty.el(Element::ListMarker);
        let back_style = self.sty.el(Element::FootnoteRef);
        let loose = doc.footnotes.iter().any(|f| f.body.len() > 1);
        let mut produced = false;
        for (i, f) in doc.footnotes.iter().enumerate() {
            let before = self.out.lines.len();
            if produced && loose {
                self.request_gap();
            }
            let number = format!("{}.", i + 1);
            let pad = marker_w.saturating_sub(number.len());
            let first = vec![
                Seg::new(format!("{}{number}", " ".repeat(pad)), marker_style),
                Seg::new(" ", StyleId(0)),
            ];
            let rest = vec![Seg::new(" ".repeat(marker_w + 1), StyleId(0))];
            self.push_level(first, rest);
            let tail: Vec<Tail> = f
                .refs
                .iter()
                .enumerate()
                .map(|(k, &link)| {
                    let mut text = format!(" {}", self.deco.backref);
                    if k > 0 {
                        let n = (k + 1).to_string();
                        match superscript(&n) {
                            Some(s) if !self.deco.ascii => text.push_str(&s),
                            _ => text.push_str(&n),
                        }
                    }
                    Tail {
                        text,
                        style: back_style,
                        link: Some(link),
                        flags: SpanFlags::BACKLINK,
                    }
                })
                .collect();
            self.footnote_body(&f.body, &tail);
            self.pop_level();
            if self.out.lines.len() > before {
                produced = true;
            }
        }
        self.text_el = saved;
    }

    /// A footnote's blocks with the back-links at the end of its last
    /// paragraph (or on a line of their own).
    fn footnote_body(&mut self, body: &[Block], tail: &[Tail]) {
        let base = self.sty.el(Element::Footnote);
        match body.split_last() {
            Some((Block::Para(last), rest)) => {
                if !rest.is_empty() {
                    self.blocks(rest, false);
                    self.request_gap();
                }
                self.para_styled(last, base, tail);
            }
            _ => {
                self.blocks(body, false);
                let empty = Inlines::default();
                let trimmed: Vec<Tail> = tail
                    .iter()
                    .enumerate()
                    .map(|(i, t)| Tail {
                        text: if i == 0 {
                            t.text.trim_start().to_string()
                        } else {
                            t.text.clone()
                        },
                        ..t.clone()
                    })
                    .collect();
                self.para_styled(&empty, base, &trimmed);
            }
        }
    }

    /// Lines of text aligned inside the current content width.
    pub(super) fn aligned_lines(&mut self, c: &Composed<'_>, align: HAlign, off: u32) {
        let width = self.avail();
        let mut lines = Vec::new();
        let mut pieces = Vec::new();
        self.wrap_composed(c, width, width, &mut lines, &mut pieces);
        for (r, line) in lines.iter().enumerate() {
            self.begin();
            self.placed();
            let pad = match align {
                HAlign::Left => 0,
                HAlign::Center => width.saturating_sub(line.cols) / 2,
                HAlign::Right => width.saturating_sub(line.cols),
            };
            self.spaces(pad, StyleId(0));
            self.cell_line(c, &lines, &pieces, r);
            self.end(LineKind::Text, Fill::None, off);
        }
    }
}
