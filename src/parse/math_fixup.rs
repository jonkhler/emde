//! Math fixups on the pulldown-cmark event stream.
//!
//! pulldown-cmark 0.13 recognises `$…$` and `$$…$$`; this adapter corrects
//! and extends what it produces before the builder sees it:
//!
//! * **Digit rule.** A closing `$` followed by an ASCII digit does not
//!   close math (`$5-$10` stays prose): such `InlineMath` reverts to text.
//! * **GitHub's `` $`…`$ `` form**: the backticks are stripped.
//! * **`\(…\)` and `\[…\]`** (when enabled). CommonMark reads `\(` as an
//!   escaped `(`: the `Text` event starts at the `(` and the backslash
//!   belongs to no event. An opener is therefore an *unmerged* `Text` event
//!   starting at `(`/`[` preceded by an odd number of backslashes; the
//!   closer is the next such event starting at `)`/`]`. The TeX between them
//!   is taken from the source (line by line, so container prefixes like
//!   `> ` are skipped) and the events in between are dropped. Lifting gives
//!   up when a code span or other math intervenes, and `\[…\]` is only
//!   taken as math when it spans lines or contains TeX syntax (`\ ^ _ = {`
//!   …), so escaped brackets like `\[1\]` stay text. Emphasis that
//!   pulldown-cmark wrongly found inside the TeX (`_` and `*` are TeX
//!   syntax) is repaired: an `End` whose `Start` was dropped is dropped too,
//!   and an `End` whose `Start` survives is moved before the math.
//!
//! Events stream through unchanged (apart from the `$` fixes) until an
//! opener appears; the rest of that *inline run* (the inline events of one
//! leaf block, up to the next block-level event) is then buffered and
//! fixed as a whole.

use std::collections::VecDeque;
use std::ops::Range;

use pulldown_cmark::{CowStr, Event, Tag, TagEnd};

/// An event with its source range.
pub(crate) type Item<'a> = (Event<'a>, Range<usize>);

/// Iterator adapter applying the fixups; see the module docs.
pub(crate) struct MathFixup<'a, I> {
    inner: I,
    src: &'a str,
    tex_delimiters: bool,
    run: Vec<Item<'a>>,
    ready: VecDeque<Item<'a>>,
    /// For each inline tag open in the current run: whether its `Start`
    /// was emitted.
    open: Vec<bool>,
}

impl<'a, I: Iterator<Item = Item<'a>>> MathFixup<'a, I> {
    /// Wrap an offset event iterator over `src`. `tex_delimiters` enables
    /// `\(…\)` and `\[…\]`.
    pub(crate) fn new(inner: I, src: &'a str, tex_delimiters: bool) -> Self {
        MathFixup {
            inner,
            src,
            tex_delimiters,
            run: Vec::new(),
            ready: VecDeque::new(),
            open: Vec::new(),
        }
    }

    /// Buffer the rest of the inline run starting at the opener `first`,
    /// fix it, and queue it (with the event ending the run) in `ready`.
    fn fix_rest_of_run(&mut self, first: Item<'a>) {
        self.run.push(first);
        let mut terminator = None;
        for item in self.inner.by_ref() {
            if is_inline(&item.0) {
                self.run.push(item);
            } else {
                terminator = Some(item);
                break;
            }
        }
        fix_run(&mut self.run, self.src, &mut self.open, &mut self.ready);
        self.open.clear();
        self.ready.extend(terminator);
    }
}

impl<'a, I: Iterator<Item = Item<'a>>> Iterator for MathFixup<'a, I> {
    type Item = Item<'a>;

    fn next(&mut self) -> Option<Item<'a>> {
        if let Some(item) = self.ready.pop_front() {
            return Some(item);
        }
        let item = self.inner.next()?;
        if !is_inline(&item.0) {
            self.open.clear();
            return Some(item);
        }
        if self.tex_delimiters && opener_kind(&item, self.src).is_some() {
            self.fix_rest_of_run(item);
            return self.ready.pop_front();
        }
        match &item.0 {
            Event::Start(_) => self.open.push(true),
            Event::End(_) => {
                self.open.pop();
            }
            _ => {}
        }
        Some(fix_inline_math(item, self.src))
    }
}

/// Whether an event belongs to inline content.
fn is_inline(ev: &Event<'_>) -> bool {
    match ev {
        Event::Text(_)
        | Event::Code(_)
        | Event::InlineMath(_)
        | Event::DisplayMath(_)
        | Event::InlineHtml(_)
        | Event::FootnoteReference(_)
        | Event::SoftBreak
        | Event::HardBreak
        | Event::TaskListMarker(_) => true,
        Event::Start(tag) => is_inline_tag(tag),
        Event::End(end) => is_inline_end(*end),
        Event::Html(_) | Event::Rule => false,
    }
}

fn is_inline_tag(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::Link { .. }
            | Tag::Image { .. }
    )
}

fn is_inline_end(end: TagEnd) -> bool {
    matches!(
        end,
        TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
            | TagEnd::Image
    )
}

/// `\(` (0) or `\[` (1) delimiters.
type Kind = usize;
const OPEN: [u8; 2] = *b"([";
const CLOSE: [u8; 2] = *b")]";

/// Whether `ev` is a `Text` event that starts with the escaped delimiter
/// `delim` (the source byte at its start is `delim`, preceded by an odd run
/// of backslashes).
fn starts_escaped(ev: &Event<'_>, range: &Range<usize>, src: &str, delim: u8) -> bool {
    let Event::Text(text) = ev else {
        return false;
    };
    if text.as_bytes().first() != Some(&delim) || src.as_bytes().get(range.start) != Some(&delim) {
        return false;
    }
    let before = src.as_bytes().get(..range.start).unwrap_or_default();
    before.iter().rev().take_while(|&&b| b == b'\\').count() % 2 == 1
}

fn opener_kind(item: &Item<'_>, src: &str) -> Option<Kind> {
    (0..2).find(|&k| starts_escaped(&item.0, &item.1, src, OPEN[k]))
}

/// Result of looking for a closing delimiter.
enum Search {
    /// The closer is at this index.
    Closer(usize),
    /// A code span or math at this index stops the search.
    Blocked(usize),
    /// No closer in the rest of the run.
    End,
}

fn find_closer(items: &[Option<Item<'_>>], from: usize, kind: Kind, src: &str) -> Search {
    for (m, item) in items.iter().enumerate().skip(from) {
        match item {
            Some((Event::Code(_) | Event::InlineMath(_) | Event::DisplayMath(_), _)) => {
                return Search::Blocked(m);
            }
            Some((ev, range)) if starts_escaped(ev, range, src, CLOSE[kind]) => {
                return Search::Closer(m);
            }
            _ => {}
        }
    }
    Search::End
}

/// Apply the fixups to (the rest of) one inline run, draining it into
/// `out`. `open` tracks the inline tags open so far in the run: for each,
/// whether its `Start` was emitted.
fn fix_run<'a>(
    run: &mut Vec<Item<'a>>,
    src: &'a str,
    open: &mut Vec<bool>,
    out: &mut VecDeque<Item<'a>>,
) {
    let mut items: Vec<Option<Item<'a>>> = run.drain(..).map(Some).collect();
    let mut blocked_until = [0usize; 2];
    let mut exhausted = [false; 2];
    let mut i = 0;
    while i < items.len() {
        let kind = items
            .get(i)
            .and_then(Option::as_ref)
            .and_then(|it| opener_kind(it, src));
        if let Some(k) = kind
            && !exhausted[k]
            && i >= blocked_until[k]
        {
            match find_closer(&items, i + 1, k, src) {
                Search::Closer(j) => {
                    let tex = tex_between(&items, i, j, src);
                    if k == 0 || looks_like_display_math(&tex) {
                        lift(&mut items, i, j, k, tex, open, out);
                        i = j + 1;
                        continue;
                    }
                    // Plain words in brackets are an escaped `[…]`, not
                    // math; later openers before `j` would see a suffix of
                    // the same text.
                    blocked_until[k] = j;
                }
                Search::Blocked(b) => blocked_until[k] = b,
                Search::End => exhausted[k] = true,
            }
        }
        if let Some(item) = items.get_mut(i).and_then(Option::take) {
            emit(item, src, open, out);
        }
        i += 1;
    }
}

/// Emit one event, keeping inline tags balanced.
fn emit<'a>(item: Item<'a>, src: &str, open: &mut Vec<bool>, out: &mut VecDeque<Item<'a>>) {
    match &item.0 {
        Event::Start(tag) if is_inline_tag(tag) => {
            open.push(true);
            out.push_back(item);
        }
        Event::End(end) if is_inline_end(*end) => {
            // Drop an End whose Start was dropped inside lifted math.
            if open.pop() != Some(false) {
                out.push_back(item);
            }
        }
        _ => out.push_back(fix_inline_math(item, src)),
    }
}

/// The raw TeX between the opener at `items[i]` and the closer at `items[j]`.
fn tex_between(items: &[Option<Item<'_>>], i: usize, j: usize, src: &str) -> String {
    let start = |k: usize| items.get(k).and_then(Option::as_ref).map(|(_, r)| r.start);
    match (start(i), start(j)) {
        (Some(open), Some(close)) => {
            raw_tex(src, open, items.get(i + 1..j).unwrap_or_default(), close)
        }
        _ => String::new(),
    }
}

/// Whether the content of `\[…\]` is display math rather than an escaped
/// bracket (`\[1\]`, `\[not a link\](x)`): it spans lines or contains TeX
/// syntax.
fn looks_like_display_math(tex: &str) -> bool {
    tex.contains([
        '\n', '\\', '^', '_', '=', '{', '}', '+', '<', '>', '|', '(', ')',
    ])
}

/// Replace `items[i..=j]` (opener to closer) with one math event holding
/// `tex`.
fn lift<'a>(
    items: &mut [Option<Item<'a>>],
    i: usize,
    j: usize,
    kind: Kind,
    tex: String,
    open: &mut Vec<bool>,
    out: &mut VecDeque<Item<'a>>,
) {
    let (Some((_, open_range)), Some((closer, close_range))) = (
        items.get_mut(i).and_then(Option::take),
        items.get_mut(j).and_then(Option::take),
    ) else {
        return;
    };
    let dropped = items.get_mut(i + 1..j).unwrap_or_default();
    for slot in dropped.iter_mut() {
        let Some((ev, range)) = slot.take() else {
            continue;
        };
        match ev {
            Event::Start(ref tag) if is_inline_tag(tag) => open.push(false),
            Event::End(end) if is_inline_end(end) => {
                // An End whose Start was emitted before the math closes
                // there; one whose Start was dropped is dropped with it.
                let start_emitted = open.pop() == Some(true);
                if start_emitted {
                    out.push_back((ev, range));
                }
            }
            _ => {}
        }
    }
    let span = open_range.start.saturating_sub(1)..close_range.end.min(close_range.start + 1);
    let math = if kind == 0 {
        Event::InlineMath(CowStr::from(tex))
    } else {
        Event::DisplayMath(CowStr::from(tex))
    };
    out.push_back((math, span));
    // Whatever followed the closing delimiter in its Text event.
    if let Event::Text(text) = closer
        && let Some(rest) = text.get(1..)
        && !rest.is_empty()
    {
        out.push_back((
            Event::Text(CowStr::from(rest.to_string())),
            close_range.start + 1..close_range.end,
        ));
    }
}

/// The TeX between an opener at `open` and a closer at `close`, read from
/// the source line by line.
fn raw_tex(src: &str, open: usize, dropped: &[Option<Item<'_>>], close: usize) -> String {
    let mut tex = String::new();
    let mut line_start = Some(open + 1);
    for (ev, range) in dropped.iter().flatten() {
        match ev {
            Event::SoftBreak | Event::HardBreak => {
                if let Some(s) = line_start.take() {
                    tex.push_str(src.get(s..range.start).unwrap_or(""));
                }
                tex.push('\n');
            }
            // End events span their whole element; they say nothing about
            // where a line starts.
            Event::End(_) => {}
            _ if line_start.is_none() => {
                // Include the backslash of an escape that starts the line.
                let bytes = src.as_bytes();
                let mut s = range.start;
                while s > 0 && bytes.get(s - 1) == Some(&b'\\') {
                    s -= 1;
                }
                line_start = Some(s);
            }
            _ => {}
        }
    }
    if let Some(s) = line_start {
        tex.push_str(src.get(s..close.saturating_sub(1)).unwrap_or(""));
    }
    tex
}

/// Digit rule and backtick stripping for `InlineMath` events.
fn fix_inline_math<'a>(item: Item<'a>, src: &str) -> Item<'a> {
    let (Event::InlineMath(tex), range) = item else {
        return item;
    };
    if src
        .as_bytes()
        .get(range.end)
        .is_some_and(u8::is_ascii_digit)
    {
        return (Event::Text(CowStr::from(format!("${tex}$"))), range);
    }
    match strip_backticks(&tex) {
        Some(inner) => (Event::InlineMath(CowStr::from(inner.to_string())), range),
        None => (Event::InlineMath(tex), range),
    }
}

/// `` `x` `` (GitHub's `` $`x`$ `` form) → `x`; `None` if not in that form.
fn strip_backticks(tex: &str) -> Option<&str> {
    let lead = tex.bytes().take_while(|&b| b == b'`').count();
    let trail = tex.bytes().rev().take_while(|&b| b == b'`').count();
    let n = lead.min(trail);
    if n == 0 || tex.len() <= 2 * n {
        return None;
    }
    tex.get(n..tex.len() - n).map(str::trim)
}

#[cfg(test)]
mod tests {
    use pulldown_cmark::{Options, Parser};

    use super::*;

    fn events(src: &str, tex: bool) -> Vec<Event<'_>> {
        let opts = Options::ENABLE_MATH | Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
        let parser = Parser::new_ext(src, opts).into_offset_iter();
        MathFixup::new(parser, src, tex).map(|(ev, _)| ev).collect()
    }

    /// Compact rendering of the inline events of `src`.
    fn show(src: &str, tex: bool) -> String {
        let mut out = Vec::new();
        for ev in events(src, tex) {
            out.push(match ev {
                Event::Text(t) => format!("t{:?}", t.as_ref()),
                Event::Code(t) => format!("code{:?}", t.as_ref()),
                Event::InlineMath(t) => format!("${:?}", t.as_ref()),
                Event::DisplayMath(t) => format!("$${:?}", t.as_ref()),
                Event::Start(Tag::Emphasis) => "<em>".into(),
                Event::End(TagEnd::Emphasis) => "</em>".into(),
                Event::Start(Tag::Strong) => "<b>".into(),
                Event::End(TagEnd::Strong) => "</b>".into(),
                Event::SoftBreak => "sb".into(),
                Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => continue,
                other => format!("{other:?}"),
            });
        }
        out.join(" ")
    }

    #[test]
    fn dollars_before_digits_stay_prose() {
        assert_eq!(show("$5 and $10", true), r#"t"$" t"5 and " t"$" t"10""#);
        assert_eq!(show("$5-$10", true), r#"t"$5-$" t"10""#);
        assert_eq!(
            show("from $x$ to $y$2", true),
            r#"t"from " $"x" t" to " t"$y$" t"2""#
        );
    }

    #[test]
    fn backtick_form_is_stripped() {
        assert_eq!(show("a $`x^2`$ b", true), r#"t"a " $"x^2" t" b""#);
        assert_eq!(show("$``a`b``$", true), r#"$"a`b""#);
        assert_eq!(strip_backticks("``"), None);
        assert_eq!(strip_backticks("x"), None);
    }

    #[test]
    fn paren_delimiters_become_inline_math() {
        assert_eq!(show(r"a \(x_1 + y\) b", true), r#"t"a " $"x_1 + y" t" b""#);
        assert_eq!(show(r"\(a\)\(b\)", true), r#"$"a" $"b""#);
        // Off: the escapes are plain text.
        assert_eq!(show(r"a \(x\) b", false), r#"t"a " t"(x" t") b""#);
    }

    #[test]
    fn bracket_delimiters_become_display_math() {
        assert_eq!(
            show(r"see \[E = mc^2\] now", true),
            r#"t"see " $$"E = mc^2" t" now""#
        );
        assert_eq!(
            show("before\n\\[\nx = 1\n\\]\nafter", true),
            r#"t"before" sb $$"\nx = 1\n" sb t"after""#
        );
    }

    #[test]
    fn escaped_brackets_with_plain_words_stay_text() {
        assert_eq!(
            show(r"see \[1\] and \[not a link\](x)", true),
            r#"t"see " t"[1" t"] and " t"[not a link" t"](x)""#
        );
        // TeX syntax or a line break makes it display math.
        assert_eq!(show(r"\[\pi\]", true), r#"$$"\\pi""#);
        assert_eq!(show(r"\[f(x)\]", true), r#"$$"f(x)""#);
        assert_eq!(show("\\[\nab\n\\]", true), r#"$$"\nab\n""#);
        // A later opener can still pair with a later closer.
        assert_eq!(show(r"\[a\] \[b^2\]", true), r#"t"[a" t"] " $$"b^2""#);
    }

    #[test]
    fn escaped_backslash_is_not_an_opener() {
        assert_eq!(show(r"a \\(x\\) b", true), r#"t"a " t"\\(x" t"\\) b""#);
        // Three backslashes: a literal one, then an escaped paren.
        assert_eq!(show(r"\\\(x\)", true), r#"t"\\" $"x""#);
    }

    #[test]
    fn code_spans_block_lifting() {
        assert_eq!(
            show(r"\( a `code` b \)", true),
            r#"t"( a " code"code" t" b " t")""#
        );
        // An opener after the code span still works.
        assert_eq!(show(r"\( `c` \(y\)", true), r#"t"( " code"c" t" " $"y""#);
        // Pulldown math in between blocks it too.
        assert_eq!(show(r"\( $x$ \)", true), r#"t"( " $"x" t" " t")""#);
    }

    #[test]
    fn unclosed_opener_is_text() {
        assert_eq!(show(r"a \(b c", true), r#"t"a " t"(b c""#);
        assert_eq!(show(r"\(a \[b^2\]", true), r#"t"(a " $$"b^2""#);
    }

    #[test]
    fn emphasis_inside_tex_is_repaired() {
        // pulldown-cmark pairs `_` across the two formulas; " and " must not
        // end up emphasised.
        assert_eq!(
            show(r"\(x^{2}_{1}\) and \(y^{2}_{3}\)", true),
            r#"$"x^{2}_{1}" t" and " $"y^{2}_{3}""#
        );
        // Emphasis inside one formula is simply dropped.
        assert_eq!(show(r"\(a*b*c\)", true), r#"$"a*b*c""#);
        // A Start before the math whose End is inside it: closed before it.
        assert_eq!(
            show(r"*a \(b* c\) d", true),
            r#"<em> t"a " </em> $"b* c" t" d""#
        );
        // Emphasis around the math survives.
        assert_eq!(show(r"*x \(y\) z*", true), r#"<em> t"x " $"y" t" z" </em>"#);
    }

    #[test]
    fn multi_line_tex_skips_container_prefixes() {
        let src = "> \\[\n> \\{ x \\} = *a\n> b* \\]\n> after\n";
        let got: Vec<String> = events(src, true)
            .into_iter()
            .filter_map(|ev| match ev {
                Event::DisplayMath(t) => Some(t.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(got, ["\n\\{ x \\} = *a\nb* "]);
    }

    #[test]
    fn events_stay_balanced() {
        let srcs = [
            r"\(x^{2}_{1}\) and \(y^{2}_{3}\)",
            r"*a \(b* c\) d",
            r"**a \(b** c \(d\) e",
            r"_a \(x_ b\) c_ d",
            r"[link \(a](b) c\)",
        ];
        for src in srcs {
            let mut depth = 0i32;
            for ev in events(src, true) {
                match ev {
                    Event::Start(_) => depth += 1,
                    Event::End(_) => depth -= 1,
                    _ => {}
                }
                assert!(depth >= 0, "{src}");
            }
            assert_eq!(depth, 0, "{src}");
        }
    }
}
