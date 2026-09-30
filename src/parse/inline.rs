//! Accumulating the inline content of one leaf block.
//!
//! [`InlineBuf`] turns a stream of text pieces into an [`Inlines`]. For
//! prose it normalises whitespace the way a browser does: tabs, line
//! separators and stray newlines become spaces, runs of spaces collapse, and
//! lines never start with a space. It resolves soft breaks (a space, except
//! between two East Asian wide characters), keeps hard breaks as `\n`,
//! replaces control characters (entities such as `&#27;` can produce them)
//! with control pictures, records atoms and break points, and merges
//! adjacent runs of the same style.

use std::ops::Range;

use crate::ir::{InlineFlags, Inlines, LinkId, Run, RunKind};
use crate::text::sanitize::push_picture;
use crate::text::width::is_wide_east_asian;

/// Emphasis flags and link of appended text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Style {
    pub(crate) flags: InlineFlags,
    pub(crate) link: Option<LinkId>,
}

/// Builder for one [`Inlines`].
#[derive(Debug, Default)]
pub(crate) struct InlineBuf {
    text: String,
    runs: Vec<Run>,
    atoms: Vec<Range<u32>>,
    extra_breaks: Vec<u32>,
    hard_breaks: Vec<u32>,
    /// A soft break waiting for the next character, with its style.
    pending_soft: Option<Style>,
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Push break opportunities for the URL in `text[range]`: after `/ ? & = #`,
/// except between the two slashes of `//` and at the end of the URL.
pub(crate) fn url_break_points(text: &str, range: Range<usize>, out: &mut Vec<u32>) {
    let bytes = text.as_bytes();
    let end = range.end.min(bytes.len());
    for i in range.start..end {
        let Some(&b) = bytes.get(i) else { break };
        let next = bytes.get(i + 1).copied();
        if matches!(b, b'/' | b'?' | b'&' | b'=' | b'#') && i + 1 < end && next != Some(b'/') {
            out.push(to_u32(i + 1));
        }
    }
}

/// Lead bytes of the characters [`needs_attention`] may flag.
static ATTENTION: [bool; 256] = attention_table(false);
/// [`ATTENTION`] plus the space, which may collapse in prose.
static PROSE_ATTENTION: [bool; 256] = attention_table(true);

const fn attention_table(space: bool) -> [bool; 256] {
    let mut t = [false; 256];
    let mut b = 0;
    while b < 0x20 {
        t[b] = true;
        b += 1;
    }
    t[0x7f] = true;
    t[0xc2] = true;
    t[0xe2] = true;
    t[0x20] = space;
    t
}

/// Whether prose needs no normalisation: no control characters (tab and
/// newline included), no DEL, C1 control or line separator, and no double
/// space. Checked eight bytes at a time; the rare `0xC2`/`0xE2` lead bytes
/// are then checked exactly.
fn is_plain_prose(bytes: &[u8]) -> bool {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    const SPACES: u64 = LO * 0x20;
    const DELS: u64 = LO * 0x7f;
    /// Whether any byte of `x` is zero.
    fn has_zero(x: u64) -> bool {
        x.wrapping_sub(LO) & !x & HI != 0
    }
    let (chunks, tail) = bytes.as_chunks::<8>();
    let mut prev_space = false;
    for chunk in chunks {
        let x = u64::from_le_bytes(*chunk);
        let below_space = x.wrapping_sub(SPACES) & !x & HI != 0;
        // A zero in `pairs` marks two adjacent spaces (the top byte has no
        // right neighbour in this word).
        let s = x ^ SPACES;
        let pairs = s | (s >> 8) | (0xff << 56);
        if below_space || has_zero(x ^ DELS) || has_zero(pairs) {
            return false;
        }
        if prev_space && chunk[0] == b' ' {
            return false;
        }
        prev_space = chunk[7] == b' ';
    }
    for &b in tail {
        if b < 0x20 || b == 0x7f || (b == b' ' && prev_space) {
            return false;
        }
        prev_space = b == b' ';
    }
    memchr::memchr2_iter(0xc2, 0xe2, bytes).all(|i| !needs_attention(bytes, i))
}

/// Whether the character starting at `bytes[i]` is whitespace to turn into
/// a space or a control character to replace: C0 controls (tab and newline
/// included), DEL, C1 controls (`0xC2 0x80..=0x9F`) and the line and
/// paragraph separators (`0xE2 0x80 0xA8`/`0xA9`).
fn needs_attention(bytes: &[u8], i: usize) -> bool {
    match bytes.get(i) {
        Some(0x00..=0x1f | 0x7f) => true,
        Some(0xc2) => bytes.get(i + 1).is_some_and(|b| (0x80..=0x9f).contains(b)),
        Some(0xe2) => {
            bytes.get(i + 1) == Some(&0x80) && matches!(bytes.get(i + 2), Some(0xa8 | 0xa9))
        }
        _ => false,
    }
}

/// Whitespace that becomes a plain space in inline content.
fn is_space_like(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

impl InlineBuf {
    /// A buffer expecting about `bytes` of text (the leaf's source length,
    /// an upper bound for typical Markdown).
    pub(crate) fn with_capacity(bytes: usize) -> InlineBuf {
        InlineBuf {
            text: String::with_capacity(bytes),
            runs: Vec::with_capacity((bytes / 32).clamp(1, 64)),
            ..InlineBuf::default()
        }
    }

    /// Whether nothing has been appended (pending soft breaks aside).
    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Bytes of text so far.
    pub(crate) fn len(&self) -> usize {
        self.text.len()
    }

    /// The text so far.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Append prose (a [`RunKind::Text`] run).
    ///
    /// One pass over the bytes: ordinary text is copied in chunks, and only
    /// the characters that need attention (spaces that may collapse,
    /// whitespace that becomes a space, control characters) are handled one
    /// by one.
    pub(crate) fn push_text(&mut self, s: &str, style: Style) {
        let Some(first) = s.chars().next() else {
            return;
        };
        self.resolve_soft(first);
        let start = self.text.len();
        let bytes = s.as_bytes();
        if is_plain_prose(bytes) && !(first == ' ' && self.drops_space(start)) {
            // Fast path: nothing to normalise or replace.
            self.text.push_str(s);
            self.close_run(start, RunKind::Text, style);
            return;
        }
        let mut chunk = 0;
        let mut i = 0;
        while let Some(&b) = bytes.get(i) {
            if !PROSE_ATTENTION[usize::from(b)] {
                i += 1;
                continue;
            }
            let special = if b == b' ' {
                // A space may collapse at the start of a chunk (after other
                // whitespace, or at the start of a line) or after a space.
                i == chunk || i.checked_sub(1).and_then(|j| bytes.get(j)) == Some(&b' ')
            } else {
                needs_attention(bytes, i)
            };
            if !special {
                i += 1;
                continue;
            }
            self.text.push_str(s.get(chunk..i).unwrap_or(""));
            let Some(c) = s.get(i..).and_then(|rest| rest.chars().next()) else {
                break;
            };
            i += c.len_utf8();
            chunk = i;
            if is_space_like(c) {
                if !self.drops_space(start) {
                    self.text.push(' ');
                }
            } else if !push_picture(&mut self.text, c) {
                self.text.push(c);
            }
        }
        self.text.push_str(s.get(chunk..).unwrap_or(""));
        self.close_run(start, RunKind::Text, style);
    }

    /// Append a code span: kept verbatim except that tabs and line breaks
    /// become spaces and control characters become pictures.
    pub(crate) fn push_code(&mut self, s: &str, style: Style) {
        let Some(first) = s.chars().next() else {
            return;
        };
        self.resolve_soft(first);
        let start = self.text.len();
        self.push_verbatim(s);
        self.close_run(start, RunKind::Code, style);
    }

    /// Append an unbreakable run (math, footnote reference) and record it
    /// as an atom.
    pub(crate) fn push_atom(&mut self, s: &str, kind: RunKind, style: Style) {
        let Some(first) = s.chars().next() else {
            return;
        };
        self.resolve_soft(first);
        let start = self.text.len();
        self.push_verbatim(s);
        if self.text.len() > start {
            self.atoms.push(to_u32(start)..to_u32(self.text.len()));
        }
        self.close_run(start, kind, style);
    }

    /// Append a run of another kind (image chip, raw HTML) verbatim.
    pub(crate) fn push_run(&mut self, s: &str, kind: RunKind, style: Style) {
        let Some(first) = s.chars().next() else {
            return;
        };
        self.resolve_soft(first);
        let start = self.text.len();
        self.push_verbatim(s);
        self.close_run(start, kind, style);
    }

    /// A soft line break: a space, unless it separates two East Asian wide
    /// characters or touches a zero-width space (decided when the next
    /// character arrives).
    pub(crate) fn soft_break(&mut self, style: Style) {
        if !self.text.is_empty() && self.pending_soft.is_none() {
            self.pending_soft = Some(style);
        }
    }

    /// A hard line break (`\n`). Ignored at the start of the content.
    pub(crate) fn hard_break(&mut self, style: Style) {
        self.pending_soft = None;
        if self.text.is_empty() {
            return;
        }
        self.trim_trailing_spaces();
        let start = self.text.len();
        self.hard_breaks.push(to_u32(start));
        self.text.push('\n');
        self.close_run(start, RunKind::Text, style);
    }

    /// A break opportunity at the current end (`<wbr>`).
    pub(crate) fn break_here(&mut self) {
        if !self.text.is_empty() {
            self.extra_breaks.push(to_u32(self.text.len()));
        }
    }

    /// Break opportunities inside `range`, which holds a URL (see
    /// [`url_break_points`]).
    pub(crate) fn url_breaks(&mut self, range: Range<usize>) {
        url_break_points(&self.text, range, &mut self.extra_breaks);
    }

    /// Finish: trim trailing whitespace, add key-cap atoms and normalise the
    /// constraint lists.
    pub(crate) fn finish(mut self) -> Inlines {
        self.pending_soft = None;
        while self.text.ends_with([' ', '\n'])
            && self.runs.last().is_some_and(|r| r.kind == RunKind::Text)
        {
            if self.text.pop() == Some('\n') {
                self.hard_breaks.pop();
            }
            self.clip_last_run();
        }
        let len = to_u32(self.text.len());
        self.kbd_atoms();
        self.atoms.retain(|a| a.start < a.end && a.end <= len);
        self.atoms.sort_by_key(|a| a.start);
        let mut atoms: Vec<Range<u32>> = Vec::with_capacity(self.atoms.len());
        for a in self.atoms {
            match atoms.last_mut() {
                Some(last) if a.start < last.end => last.end = last.end.max(a.end),
                _ => atoms.push(a),
            }
        }
        let text = &self.text;
        self.extra_breaks
            .retain(|&b| b > 0 && b < len && text.is_char_boundary(b as usize));
        self.extra_breaks.sort_unstable();
        self.extra_breaks.dedup();
        self.hard_breaks.retain(|&b| b < len);
        Inlines {
            text: self.text,
            runs: self.runs,
            atoms,
            extra_breaks: self.extra_breaks,
            hard_breaks: self.hard_breaks,
        }
    }

    /// Append text with tabs and line breaks as spaces and controls as
    /// pictures (no collapsing).
    fn push_verbatim(&mut self, s: &str) {
        let bytes = s.as_bytes();
        let mut chunk = 0;
        let mut i = 0;
        while let Some(&b) = bytes.get(i) {
            if !ATTENTION[usize::from(b)] || !needs_attention(bytes, i) {
                i += 1;
                continue;
            }
            self.text.push_str(s.get(chunk..i).unwrap_or(""));
            let Some(c) = s.get(i..).and_then(|rest| rest.chars().next()) else {
                break;
            };
            i += c.len_utf8();
            chunk = i;
            if is_space_like(c) {
                self.text.push(' ');
            } else if !push_picture(&mut self.text, c) {
                self.text.push(c);
            }
        }
        self.text.push_str(s.get(chunk..).unwrap_or(""));
    }

    /// Whether a prose space appended now would be dropped: at the start of
    /// a line, or after a space of this call (`start`) or of a text run.
    fn drops_space(&self, start: usize) -> bool {
        match self.text.as_bytes().last() {
            None | Some(b'\n') => true,
            Some(b' ') => {
                self.text.len() > start || self.runs.last().is_some_and(|r| r.kind == RunKind::Text)
            }
            Some(_) => false,
        }
    }

    /// Turn a pending soft break into a space if the next character needs one.
    fn resolve_soft(&mut self, next: char) {
        let Some(style) = self.pending_soft.take() else {
            return;
        };
        if is_space_like(next) || self.drops_space(self.text.len()) {
            return;
        }
        if let Some(prev) = self.text.chars().next_back()
            && (prev == '\u{200b}'
                || next == '\u{200b}'
                || (is_wide_east_asian(prev) && is_wide_east_asian(next)))
        {
            return;
        }
        let start = self.text.len();
        self.text.push(' ');
        self.close_run(start, RunKind::Text, style);
    }

    /// Record `[start, len)` as a run, merging with the previous run when
    /// the style matches and the kind allows it.
    fn close_run(&mut self, start: usize, kind: RunKind, style: Style) {
        if self.text.len() <= start {
            return;
        }
        let end = to_u32(self.text.len());
        let mergeable = matches!(kind, RunKind::Text | RunKind::Code | RunKind::Html);
        if let Some(last) = self.runs.last_mut()
            && mergeable
            && last.kind == kind
            && last.flags == style.flags
            && last.link == style.link
        {
            last.end = end;
            return;
        }
        self.runs.push(Run {
            end,
            kind,
            flags: style.flags,
            link: style.link,
        });
    }

    fn trim_trailing_spaces(&mut self) {
        while self.text.ends_with(' ') && self.runs.last().is_some_and(|r| r.kind == RunKind::Text)
        {
            self.text.pop();
            self.clip_last_run();
        }
    }

    /// Shorten the last run to the text length, dropping it if empty.
    fn clip_last_run(&mut self) {
        let len = to_u32(self.text.len());
        let prev_end = self
            .runs
            .len()
            .checked_sub(2)
            .and_then(|i| self.runs.get(i))
            .map_or(0, |r| r.end);
        if let Some(last) = self.runs.last_mut() {
            last.end = last.end.min(len);
            if last.end <= prev_end {
                self.runs.pop();
            }
        }
    }

    /// Key caps (`<kbd>`) never break: each maximal KBD stretch is an atom.
    fn kbd_atoms(&mut self) {
        let mut start = 0;
        let mut open: Option<u32> = None;
        for run in &self.runs {
            let kbd = run.flags.contains(InlineFlags::KBD);
            match (open, kbd) {
                (None, true) => open = Some(start),
                (Some(s), false) => {
                    self.atoms.push(s..start);
                    open = None;
                }
                _ => {}
            }
            start = run.end;
        }
        if let Some(s) = open {
            self.atoms.push(s..start);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Style = Style {
        flags: InlineFlags::empty(),
        link: None,
    };
    const EM: Style = Style {
        flags: InlineFlags::EMPH,
        link: None,
    };

    fn texts(inl: &Inlines) -> Vec<(&str, RunKind, InlineFlags)> {
        inl.runs_with_ranges()
            .map(|(r, run)| (inl.slice(r), run.kind, run.flags))
            .collect()
    }

    #[test]
    fn collapses_and_trims_prose_whitespace() {
        let mut b = InlineBuf::default();
        b.push_text("  a  \t b ", PLAIN);
        b.push_text(" c\u{2028}d  ", PLAIN);
        let inl = b.finish();
        assert_eq!(inl.text, "a b c d");
        assert_eq!(inl.runs.len(), 1);
        inl.validate().unwrap();
    }

    #[test]
    fn merges_runs_of_equal_style() {
        let mut b = InlineBuf::default();
        b.push_text("a ", PLAIN);
        b.push_text("b", EM);
        b.push_text("c", EM);
        b.push_text(" d", PLAIN);
        let inl = b.finish();
        assert_eq!(
            texts(&inl),
            [
                ("a ", RunKind::Text, InlineFlags::empty()),
                ("bc", RunKind::Text, InlineFlags::EMPH),
                (" d", RunKind::Text, InlineFlags::empty()),
            ]
        );
    }

    #[test]
    fn soft_breaks() {
        let mut b = InlineBuf::default();
        b.push_text("one", PLAIN);
        b.soft_break(PLAIN);
        b.push_text("two", PLAIN);
        assert_eq!(b.finish().text, "one two");

        // Between two CJK characters the break disappears...
        let mut b = InlineBuf::default();
        b.push_text("日本", PLAIN);
        b.soft_break(PLAIN);
        b.push_text("語", PLAIN);
        assert_eq!(b.finish().text, "日本語");
        // ...but not between CJK and Latin, or between Hangul syllables.
        for (a, c, want) in [
            ("日", "a", "日 a"),
            ("한", "국", "한 국"),
            ("a\u{200b}", "b", "a\u{200b}b"),
        ] {
            let mut b = InlineBuf::default();
            b.push_text(a, PLAIN);
            b.soft_break(PLAIN);
            b.push_text(c, PLAIN);
            assert_eq!(b.finish().text, want);
        }

        // A trailing soft break is dropped; the space keeps its own style.
        let mut b = InlineBuf::default();
        b.push_text("x", EM);
        b.soft_break(PLAIN);
        b.push_text("y", EM);
        b.soft_break(PLAIN);
        let inl = b.finish();
        assert_eq!(inl.text, "x y");
        assert_eq!(inl.runs.len(), 3);
    }

    #[test]
    fn hard_breaks_are_newlines() {
        let mut b = InlineBuf::default();
        b.hard_break(PLAIN); // ignored at the start
        b.push_text("a  ", PLAIN);
        b.hard_break(PLAIN);
        b.push_text("  b", PLAIN);
        b.hard_break(PLAIN);
        b.hard_break(PLAIN); // trailing breaks are trimmed
        let inl = b.finish();
        assert_eq!(inl.text, "a\nb");
        assert_eq!(inl.hard_breaks, [1]);
        inl.validate().unwrap();
    }

    #[test]
    fn code_is_verbatim_but_single_line() {
        let mut b = InlineBuf::default();
        b.push_text("run ", PLAIN);
        b.push_code("a  b\tc\nd", PLAIN);
        let inl = b.finish();
        assert_eq!(inl.text, "run a  b c d");
        assert_eq!(texts(&inl)[1].1, RunKind::Code);
    }

    #[test]
    fn controls_become_pictures() {
        let mut b = InlineBuf::default();
        b.push_text("esc \u{1b}[2J", PLAIN);
        b.push_code("\u{9b}x", PLAIN);
        let inl = b.finish();
        assert_eq!(inl.text, "esc ␛[2J␛[x");
        inl.validate().unwrap();
    }

    #[test]
    fn atoms_and_kbd() {
        let mut b = InlineBuf::default();
        b.push_text("x", PLAIN);
        b.push_atom("1", RunKind::Math, PLAIN);
        b.push_text(" ", PLAIN);
        let kbd = Style {
            flags: InlineFlags::KBD,
            link: None,
        };
        b.push_text("Ctrl", kbd);
        b.push_text("+", PLAIN);
        b.push_text("C", kbd);
        let inl = b.finish();
        assert_eq!(inl.text, "x1 Ctrl+C");
        assert_eq!(inl.atoms, [1..2, 3..7, 8..9]);
        inl.validate().unwrap();
    }

    #[test]
    fn url_break_points() {
        let mut b = InlineBuf::default();
        b.push_text("https://a.org/b?c=d&e#f/", PLAIN);
        let n = b.len();
        b.url_breaks(0..n);
        let inl = b.finish();
        // After "//", "/", "?", "=", "&", "#" but not inside "//" or at the end.
        assert_eq!(inl.extra_breaks, [8, 14, 16, 18, 20, 22]);
        inl.validate().unwrap();
    }

    #[test]
    fn trailing_code_space_is_kept() {
        let mut b = InlineBuf::default();
        b.push_code("a ", PLAIN);
        let inl = b.finish();
        assert_eq!(inl.text, "a ");
    }

    #[test]
    fn plain_prose_detection() {
        assert!(is_plain_prose(b"just some words, nothing odd."));
        assert!(is_plain_prose("smart “quotes” — and ©".as_bytes()));
        assert!(is_plain_prose(b""));
        for bad in [
            &b"double  space"[..],
            b"tab\there",
            b"new\nline",
            b"del\x7f",
            b"seven b  x", // double space straddling an 8-byte word
            b"12345678  x",
            b"1234567  x",
            b"trailing  ",
        ] {
            assert!(!is_plain_prose(bad), "{bad:?}");
        }
        assert!(!is_plain_prose("c1 \u{9b} here".as_bytes()));
        assert!(!is_plain_prose("line\u{2028}sep".as_bytes()));
        // Every position of a double space in a longer string.
        for at in 0..19 {
            let mut s = "a".repeat(20);
            s.replace_range(at..at + 2, "  ");
            assert!(!is_plain_prose(s.as_bytes()), "{at}");
        }
    }

    #[test]
    fn empty_pushes_are_ignored() {
        let mut b = InlineBuf::default();
        b.push_text("", PLAIN);
        b.push_code("", PLAIN);
        b.push_atom("", RunKind::Math, PLAIN);
        b.soft_break(PLAIN);
        let inl = b.finish();
        assert!(inl.is_empty() && inl.runs.is_empty() && inl.atoms.is_empty());
    }
}
