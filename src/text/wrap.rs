//! Greedy line breaking of styled paragraphs.
//!
//! [`Wrapper::wrap_into`] breaks the plain text of a paragraph into lines
//! that fit a first-line width and a width for the remaining lines (hanging
//! indents, list markers and quote bars are the caller's business). It works
//! on the whole paragraph text at once, so break opportunities across style
//! runs are right, and it never looks at styles: [`split_runs`] then cuts a
//! run list at the computed lines.
//!
//! Rules, in order of precedence:
//!
//! * `\n` is a hard break (the only one; paragraph text has no other
//!   control characters).
//! * Break opportunities are UAX #14 ([`unicode_linebreak`]), plus
//!   caller-supplied extra break points (after `/` in URLs, `<wbr>`), minus
//!   anything strictly inside an *atom* (a range that is never split).
//! * Lines are filled greedily (first fit). Trailing breakable whitespace is
//!   trimmed and never causes an overflow; no-break spaces never break.
//! * A soft hyphen (`U+00AD`) is a break opportunity that shows a `-` only
//!   when the line actually breaks there (the renderer must never emit
//!   `U+00AD` itself, see [`super::strip_soft_hyphens`]).
//! * A word wider than the line is broken between grapheme clusters. Atoms
//!   are never broken: one that would straddle the end of the line moves
//!   whole to the next line, and an atom wider than the line overflows it,
//!   as does a single grapheme wider than the line (a CJK character at
//!   width 1).
//!
//! # Performance
//!
//! Break opportunities are computed once per paragraph
//! ([`break_opportunities`]; printable ASCII takes a fast path with the same
//! results, see [`super::linebreak`]). They do not depend on the width, so layout can
//! keep them and re-wrap with [`wrap_with_breaks`] after a resize. A
//! paragraph of printable ASCII is then filled from the break positions
//! alone; other text is walked grapheme by grapheme, taking stretches of
//! printable ASCII between break opportunities whole. Both shortcuts give
//! exactly the lines of the plain walk (property-tested). When a line breaks
//! at an earlier opportunity, the text carried over is measured again
//! against the next line's width, which may be narrower than the first.

use std::ops::Range;

use unicode_linebreak::linebreaks;

use super::linebreak::is_printable_ascii;
use super::width::{grapheme_width, next_grapheme_end};

/// The soft hyphen, `U+00AD`.
pub const SOFT_HYPHEN: char = '\u{ad}';

/// Line-breaking constraints besides the text itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Constraints<'a> {
    /// Byte ranges that must not be broken internally, sorted by start and
    /// non-overlapping. A break is still allowed at either edge.
    pub atoms: &'a [Range<u32>],
    /// Additional break opportunities (byte offsets where a new line may
    /// start), sorted ascending.
    pub extra_breaks: &'a [u32],
}

/// Wrapping parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrapOptions {
    /// Columns available on the first line (at least 1 is assumed).
    pub first: u16,
    /// Columns available on every following line (at least 1 is assumed).
    pub rest: u16,
    /// Measure East Asian Ambiguous characters as two columns.
    pub ambiguous_wide: bool,
}

impl WrapOptions {
    /// The same width for every line.
    pub const fn uniform(width: u16) -> WrapOptions {
        WrapOptions {
            first: width,
            rest: width,
            ambiguous_wide: false,
        }
    }
}

/// One output line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// Byte range of the text to show. Excludes trailing breakable
    /// whitespace, the soft hyphen the line broke at, and the `\n` of a hard
    /// break. May still contain soft hyphens that were not used as breaks.
    pub range: Range<u32>,
    /// Display width of `range`, plus one when `hyphen` is set.
    pub cols: u16,
    /// The line broke at a soft hyphen: draw a `-` after the text.
    pub hyphen: bool,
    /// The line ends with a hard break (`\n`) rather than a wrap.
    pub hard: bool,
}

/// Wrap `text` into lines; see the module docs for the rules. A convenience
/// for [`Wrapper::wrap_into`] with a fresh buffer.
///
/// Always produces at least one line (an empty text gives one empty line).
pub fn wrap(text: &str, constraints: Constraints<'_>, opts: WrapOptions) -> Vec<Line> {
    let mut out = Vec::new();
    Wrapper::new().wrap_into(text, constraints, opts, &mut out);
    out
}

/// A reusable line breaker: keeps its scratch buffer between paragraphs.
#[derive(Clone, Debug, Default)]
pub struct Wrapper {
    /// Break opportunities of the current text (UAX #14 and extra), sorted.
    breaks: Vec<u32>,
}

impl Wrapper {
    /// A wrapper with empty buffers.
    pub fn new() -> Wrapper {
        Wrapper::default()
    }

    /// Wrap `text` into `out` (cleared first); see the module docs for the
    /// rules. Always produces at least one line.
    pub fn wrap_into(
        &mut self,
        text: &str,
        constraints: Constraints<'_>,
        opts: WrapOptions,
        out: &mut Vec<Line>,
    ) {
        let ascii = is_printable_ascii(text.as_bytes());
        breaks_into(text, ascii, constraints.extra_breaks, &mut self.breaks);
        wrap_known(text, ascii, &self.breaks, constraints.atoms, opts, out);
    }
}

/// The break opportunities of `text` (UAX #14 plus `extra`, sorted and
/// unique) into `out` (cleared first). They do not depend on the width, so
/// a caller can keep them per paragraph and re-wrap with
/// [`wrap_with_breaks`] when the width changes.
pub fn break_opportunities(text: &str, extra: &[u32], out: &mut Vec<u32>) {
    breaks_into(text, is_printable_ascii(text.as_bytes()), extra, out);
}

/// [`break_opportunities`] when it is known whether `text` is printable
/// ASCII.
fn breaks_into(text: &str, ascii: bool, extra: &[u32], out: &mut Vec<u32>) {
    out.clear();
    out.reserve(text.len() / 4 + extra.len() + 1);
    if !text.is_empty() && ascii {
        // Same result as `linebreaks`, several times faster.
        super::linebreak::ascii_breaks(text.as_bytes(), out);
        if !extra.is_empty() {
            out.extend_from_slice(extra);
            out.sort_unstable();
            out.dedup();
        }
        return;
    }
    let mut extra = extra.iter().copied().peekable();
    for (pos, _) in linebreaks(text) {
        let pos = to_u32(pos);
        while let Some(e) = extra.next_if(|&e| e < pos) {
            out.push(e);
        }
        extra.next_if_eq(&pos);
        out.push(pos);
    }
    out.extend(extra);
    if !out.is_sorted() {
        out.sort_unstable();
    }
    out.dedup();
}

/// Wrap `text` into `out` (cleared first) given its break opportunities
/// (from [`break_opportunities`] for the same text; sorted, other values are
/// harmless but meaningless) and its atoms.
pub fn wrap_with_breaks(
    text: &str,
    breaks: &[u32],
    atoms: &[Range<u32>],
    opts: WrapOptions,
    out: &mut Vec<Line>,
) {
    let ascii = is_printable_ascii(text.as_bytes());
    wrap_known(text, ascii, breaks, atoms, opts, out);
}

/// [`wrap_with_breaks`] when it is known whether `text` is printable ASCII.
fn wrap_known(
    text: &str,
    ascii: bool,
    breaks: &[u32],
    atoms: &[Range<u32>],
    opts: WrapOptions,
    out: &mut Vec<Line>,
) {
    out.clear();
    let widths = (
        usize::from(opts.first.max(1)),
        usize::from(opts.rest.max(1)),
    );
    if atoms.is_empty() && ascii && wrap_ascii(text.as_bytes(), breaks, widths, out) {
        return;
    }
    out.clear();
    wrap_general(text, breaks, atoms, opts, out, true);
}

/// The walk over grapheme clusters. `fast_stretches` enables the ASCII
/// stretch shortcut, which gives identical results (tests compare both).
fn wrap_general(
    text: &str,
    breaks: &[u32],
    atoms: &[Range<u32>],
    opts: WrapOptions,
    out: &mut Vec<Line>,
    fast_stretches: bool,
) {
    let mut cursor = Cursor {
        breaks,
        atoms,
        next_break: 0,
        next_atom: 0,
    };
    let mut f = Filler {
        out,
        first: usize::from(opts.first.max(1)),
        rest: usize::from(opts.rest.max(1)),
        line_start: 0,
        acc: 0,
        ws: 0,
        ws_start: 0,
        in_ws: false,
        cand: None,
        atom_cand: None,
        fast: StretchCache::default(),
    };
    let mut prev_soft_hyphen = false;
    let mut pos = 0;
    while pos < text.len() {
        let Cluster {
            end,
            width: w,
            kind,
        } = cluster(text, pos, opts.ambiguous_wide);
        if kind == ClusterKind::Newline {
            f.end_line(pos, true);
            f.start_line(end);
            prev_soft_hyphen = false;
            pos = end;
            continue;
        }
        let in_atom = cursor.inside_atom(pos);
        if pos > f.line_start && !in_atom {
            if cursor.allowed(pos) {
                f.candidate(pos, prev_soft_hyphen);
            } else if cursor.atom_starts_at(pos) {
                f.atom_candidate(pos);
            }
        }
        // Fast path: printable ASCII up to the next break opportunity
        // (one column per byte, no break inside) that fits the line.
        if fast_stretches
            && !in_atom
            && let Some(next) = f.fit_ascii_stretch(text.as_bytes(), pos, &mut cursor)
        {
            prev_soft_hyphen = false;
            pos = next;
            continue;
        }
        if kind == ClusterKind::Space {
            if !f.in_ws {
                f.in_ws = true;
                f.ws_start = pos;
                f.ws = 0;
            }
            f.ws += w;
            f.acc += w;
        } else {
            if f.acc + w > f.limit() {
                // Break at the last opportunity (or, inside an atom of a
                // word longer than the line, where that atom starts) and
                // measure what follows again against the next line's width.
                let at_atom = if in_atom { f.atom_cand.take() } else { None };
                if let Some(c) = f.cand.take().or(at_atom) {
                    f.emit(c.end, c.cols, c.hyphen, false);
                    f.start_line(c.at);
                    pos = c.at;
                    cursor.seek(pos);
                    prev_soft_hyphen = false;
                    continue;
                }
                if pos > f.line_start && !in_atom {
                    // A word longer than the line: break between graphemes.
                    f.end_line(pos, false);
                    f.start_line(pos);
                }
            }
            f.acc += w;
            f.in_ws = false;
            f.ws = 0;
        }
        prev_soft_hyphen = kind == ClusterKind::SoftHyphen;
        pos = end;
    }
    f.end_line(text.len(), false);
}

/// Greedy fill of printable ASCII (one column per byte, no atoms), from
/// break opportunities alone. Gives the same lines as the general walk;
/// returns `false` (with `out` incomplete) when a word is longer than a
/// line, which the general walk breaks between graphemes.
fn wrap_ascii(
    bytes: &[u8],
    breaks: &[u32],
    (first, rest): (usize, usize),
    out: &mut Vec<Line>,
) -> bool {
    // End of the text of [start, b) without trailing spaces.
    let visible_end = |start: usize, b: usize| {
        let spaces = bytes
            .get(start..b)
            .map_or(0, |s| s.iter().rev().take_while(|&&c| c == b' ').count());
        b - spaces
    };
    let push = |out: &mut Vec<Line>, start: usize, end: usize| {
        out.push(Line {
            range: to_u32(start)..to_u32(end),
            cols: u16::try_from(end - start).unwrap_or(u16::MAX),
            hyphen: false,
            hard: false,
        });
    };
    let len = bytes.len();
    let mut line_start = 0;
    // The last opportunity whose line fits: (next line start, line end).
    let mut fit: Option<(usize, usize)> = None;
    // The end of the text is always a candidate; stray positions past it
    // count as the end.
    let mut candidates = breaks
        .iter()
        .map(|&b| (b as usize).min(len))
        .chain(std::iter::once(len))
        .peekable();
    while let Some(&b) = candidates.peek() {
        if b <= line_start && b < len {
            candidates.next();
            continue;
        }
        let limit = if out.is_empty() { first } else { rest };
        let end = visible_end(line_start, b);
        if end - line_start <= limit {
            fit = Some((b, end));
            candidates.next();
            continue;
        }
        // The line up to `b` is too wide: end it at the last fit and
        // look at `b` again from the new line.
        let Some((next, fit_end)) = fit.take() else {
            return false;
        };
        push(out, line_start, fit_end);
        line_start = next;
    }
    // The last candidate is the end of the text, and it fit.
    let end = fit.map_or(line_start, |(_, e)| e);
    push(out, line_start, end.max(line_start));
    true
}

/// What the walk needs to know about one grapheme cluster.
#[derive(Clone, Copy, Debug)]
struct Cluster {
    end: usize,
    width: usize,
    kind: ClusterKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClusterKind {
    Newline,
    /// Breakable whitespace (trimmed at line ends).
    Space,
    SoftHyphen,
    Other,
}

/// The grapheme cluster starting at `pos`. ASCII followed by ASCII (the
/// common case) is classified from the byte alone.
#[inline]
fn cluster(text: &str, pos: usize, ambiguous_wide: bool) -> Cluster {
    let bytes = text.as_bytes();
    if let Some(&b) = bytes.get(pos)
        && b < 0x80
        && b != b'\r'
        && bytes.get(pos + 1).is_none_or(|&n| n < 0x80)
    {
        let kind = match b {
            b'\n' => ClusterKind::Newline,
            b' ' => ClusterKind::Space,
            _ => ClusterKind::Other,
        };
        return Cluster {
            end: pos + 1,
            width: 1,
            kind,
        };
    }
    let end = next_grapheme_end(text, pos);
    let g = text.get(pos..end).unwrap_or_default();
    let kind = if g == "\n" {
        ClusterKind::Newline
    } else if g == "\u{ad}" {
        ClusterKind::SoftHyphen
    } else if is_breakable_space(g) {
        ClusterKind::Space
    } else {
        ClusterKind::Other
    };
    Cluster {
        end,
        width: grapheme_width(g, ambiguous_wide),
        kind,
    }
}

/// A break candidate on the current line.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    /// Where the next line would start.
    at: usize,
    /// Where this line's shown text would end.
    end: usize,
    /// Display width of this line if broken here (hyphen included).
    cols: usize,
    hyphen: bool,
}

/// Position in the break opportunities and atoms; moves forwards, or jumps
/// with [`Cursor::seek`].
struct Cursor<'a> {
    breaks: &'a [u32],
    atoms: &'a [Range<u32>],
    next_break: usize,
    next_atom: usize,
}

impl Cursor<'_> {
    /// Whether a line may start at `pos` (atoms aside).
    fn allowed(&mut self, pos: usize) -> bool {
        while self
            .breaks
            .get(self.next_break)
            .is_some_and(|&b| (b as usize) < pos)
        {
            self.next_break += 1;
        }
        self.breaks
            .get(self.next_break)
            .is_some_and(|&b| b as usize == pos)
    }

    /// Whether `pos` is strictly inside an atom.
    fn inside_atom(&mut self, pos: usize) -> bool {
        while self
            .atoms
            .get(self.next_atom)
            .is_some_and(|a| (a.end as usize) <= pos)
        {
            self.next_atom += 1;
        }
        self.atoms
            .get(self.next_atom)
            .is_some_and(|a| (a.start as usize) < pos)
    }

    /// The first break opportunity after `pos` (or `None`).
    fn next_break_after(&mut self, pos: usize) -> Option<usize> {
        while self
            .breaks
            .get(self.next_break)
            .is_some_and(|&b| (b as usize) <= pos)
        {
            self.next_break += 1;
        }
        self.breaks.get(self.next_break).map(|&b| b as usize)
    }

    /// The break opportunity before the one [`Cursor::next_break_after`]
    /// returned (0 if none): where that stretch starts.
    fn previous_break(&self) -> usize {
        self.next_break
            .checked_sub(1)
            .and_then(|i| self.breaks.get(i))
            .map_or(0, |&b| b as usize)
    }

    /// Whether an atom starts at `pos` (call after `inside_atom(pos)`).
    fn atom_starts_at(&self, pos: usize) -> bool {
        self.atoms
            .get(self.next_atom)
            .is_some_and(|a| a.start as usize == pos && a.start < a.end)
    }

    /// Whether no atom starts in `(pos, end)` (call after `inside_atom(pos)`).
    fn no_atom_before(&self, end: usize) -> bool {
        self.atoms
            .get(self.next_atom)
            .is_none_or(|a| a.start as usize >= end)
    }

    /// Continue from an earlier `pos`.
    fn seek(&mut self, pos: usize) {
        self.next_break = self.breaks.partition_point(|&b| (b as usize) < pos);
        self.next_atom = self.atoms.partition_point(|a| (a.end as usize) <= pos);
    }
}

/// Mutable state of the line being filled.
struct Filler<'o> {
    out: &'o mut Vec<Line>,
    first: usize,
    rest: usize,
    line_start: usize,
    /// Width of `[line_start, pos)`, trailing whitespace included.
    acc: usize,
    /// Width of the whitespace run ending at `pos` (0 if none).
    ws: usize,
    /// Start of that whitespace run.
    ws_start: usize,
    in_ws: bool,
    cand: Option<Candidate>,
    /// The start of the latest atom on the line that is no break
    /// opportunity: where a word too long for the line breaks if it
    /// overflows inside that atom, so the atom moves whole to the next line.
    atom_cand: Option<Candidate>,
    fast: StretchCache,
}

/// Bookkeeping of [`Filler::fit_ascii_stretch`].
#[derive(Clone, Copy, Debug, Default)]
struct StretchCache {
    /// The stretch whose visible end is known (its end, 0 if none).
    stretch_end: usize,
    /// Where its trailing spaces start.
    visible_end: usize,
    /// No shortcut before this position (a stretch that is not plain ASCII).
    skip_before: usize,
}

impl Filler<'_> {
    fn limit(&self) -> usize {
        if self.out.is_empty() {
            self.first
        } else {
            self.rest
        }
    }

    /// End offset and width of the current line if it ended at `pos`.
    fn trimmed(&self, pos: usize) -> (usize, usize) {
        if self.in_ws {
            (self.ws_start, self.acc - self.ws)
        } else {
            (pos, self.acc)
        }
    }

    /// If `bytes[pos..next)`, up to the next break opportunity, is printable
    /// ASCII without atoms and fits the line, account for all of it at once
    /// and return `next`. This is exactly what the per-cluster walk would
    /// do: each byte is one column, the only overflow checks are at
    /// non-space bytes, and there is no break opportunity inside.
    ///
    /// Retries inside a stretch stay cheap: its visible end is cached, and a
    /// stretch that is not plain ASCII is not looked at again.
    fn fit_ascii_stretch(
        &mut self,
        bytes: &[u8],
        pos: usize,
        cursor: &mut Cursor<'_>,
    ) -> Option<usize> {
        if pos < self.fast.skip_before {
            return None;
        }
        let next = cursor.next_break_after(pos)?;
        if self.fast.stretch_end != next {
            // Measured over the whole stretch, so it holds for any `pos` in it.
            let spaces = bytes
                .get(cursor.previous_break()..next)?
                .iter()
                .rev()
                .take_while(|&&b| b == b' ')
                .count();
            self.fast.stretch_end = next;
            self.fast.visible_end = next - spaces;
        }
        let visible = self.fast.visible_end.saturating_sub(pos);
        let spaces = next - pos - visible;
        if visible > 0 && self.acc + visible > self.limit() {
            return None;
        }
        let stretch = bytes.get(pos..next)?;
        let printable = stretch.iter().all(|&b| (0x20..0x7f).contains(&b));
        // `next` must also be a grapheme boundary (no combining mark follows).
        let boundary = bytes.get(next).is_none_or(|&b| b < 0x80);
        if !printable || !boundary || !cursor.no_atom_before(next) {
            self.fast.skip_before = next;
            return None;
        }
        if visible > 0 {
            self.in_ws = false;
            self.ws = 0;
        }
        if spaces > 0 {
            if !self.in_ws {
                self.in_ws = true;
                self.ws_start = pos + visible;
                self.ws = 0;
            }
            self.ws += spaces;
        }
        self.acc += next - pos;
        Some(next)
    }

    /// Remember `pos` as the latest place to break; after a soft hyphen the
    /// break shows a `-`, so it only counts if that still fits.
    fn candidate(&mut self, pos: usize, after_soft_hyphen: bool) {
        let (end, cols) = self.trimmed(pos);
        if !after_soft_hyphen {
            self.cand = Some(Candidate {
                at: pos,
                end,
                cols,
                hyphen: false,
            });
        } else if cols < self.limit() {
            self.cand = Some(Candidate {
                at: pos,
                end: end.saturating_sub(SOFT_HYPHEN.len_utf8()),
                cols: cols + 1,
                hyphen: true,
            });
        }
    }

    /// Remember the start of an atom at `pos` (no break opportunity there)
    /// as the place a too-long word breaks if it overflows inside the atom.
    fn atom_candidate(&mut self, pos: usize) {
        let (end, cols) = self.trimmed(pos);
        self.atom_cand = Some(Candidate {
            at: pos,
            end,
            cols,
            hyphen: false,
        });
    }

    fn emit(&mut self, end: usize, cols: usize, hyphen: bool, hard: bool) {
        self.out.push(Line {
            range: to_u32(self.line_start)..to_u32(end.max(self.line_start)),
            cols: u16::try_from(cols).unwrap_or(u16::MAX),
            hyphen,
            hard,
        });
    }

    /// End the current line at `pos` (trailing whitespace trimmed).
    fn end_line(&mut self, pos: usize, hard: bool) {
        let (end, cols) = self.trimmed(pos);
        self.emit(end, cols, false, hard);
    }

    /// Start a new line at `pos`.
    fn start_line(&mut self, pos: usize) {
        self.line_start = pos;
        self.acc = 0;
        self.ws = 0;
        self.in_ws = false;
        self.cand = None;
        self.atom_cand = None;
    }
}

/// Whitespace that may end a line and is trimmed there: ASCII space, the
/// ideographic space, the typographic spaces `U+2000..U+200A` except the
/// figure space, and the zero-width space. No-break spaces are not included.
fn is_breakable_space(g: &str) -> bool {
    match g {
        " " => true,
        _ => {
            let mut chars = g.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => matches!(
                    c,
                    '\u{3000}' | '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200b}'
                ),
                _ => false,
            }
        }
    }
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A piece of one run on one line, produced by [`split_runs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    /// Index of the line in the wrapped output.
    pub line: u32,
    /// Index of the run in the run list.
    pub run: u32,
    /// Byte range of the text (within both the run and the line).
    pub range: Range<u32>,
}

/// Cut a run list at wrapped lines.
///
/// Runs are contiguous: run `i` covers `[end(i - 1), end(i))` with the first
/// starting at 0, as in [`crate::ir::Run`]. `out` (cleared first) receives
/// the non-empty intersections of runs with each line's range, in line order
/// and then run order, so a line's pieces are contiguous in `out`. Text in
/// the gaps between lines (trimmed spaces, the `\n` of a hard break) belongs
/// to no piece.
pub fn split_runs<R>(
    runs: &[R],
    run_end: impl Fn(&R) -> u32,
    lines: &[Line],
    out: &mut Vec<Piece>,
) {
    out.clear();
    let mut first_run = 0usize;
    for (li, line) in lines.iter().enumerate() {
        let (ls, le) = (line.range.start, line.range.end);
        // Skip runs that end before this line starts (lines are in order).
        while first_run < runs.len() && runs.get(first_run).is_some_and(|r| run_end(r) <= ls) {
            first_run += 1;
        }
        let mut start = first_run
            .checked_sub(1)
            .and_then(|i| runs.get(i))
            .map_or(0, &run_end);
        for (ri, run) in runs.iter().enumerate().skip(first_run) {
            if start >= le {
                break;
            }
            let end = run_end(run);
            let (s, e) = (start.max(ls), end.min(le));
            if s < e {
                out.push(Piece {
                    line: to_u32(li),
                    run: to_u32(ri),
                    range: s..e,
                });
            }
            start = end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrap and return the shown text of each line (with `-` for hyphens).
    fn lines(text: &str, width: u16) -> Vec<String> {
        lines_c(text, width, Constraints::default())
    }

    fn lines_c(text: &str, width: u16, c: Constraints<'_>) -> Vec<String> {
        wrap(text, c, WrapOptions::uniform(width))
            .iter()
            .map(|l| {
                let mut s = text[l.range.start as usize..l.range.end as usize].to_string();
                if l.hyphen {
                    s.push('-');
                }
                s
            })
            .collect()
    }

    #[test]
    fn fills_greedily_and_trims_spaces() {
        assert_eq!(
            lines("the quick brown fox jumps", 10),
            ["the quick", "brown fox", "jumps"]
        );
        assert_eq!(lines("a  b", 1), ["a", "b"]);
        assert_eq!(lines("fits exactly", 12), ["fits exactly"]);
        assert_eq!(lines("trailing   ", 20), ["trailing"]);
    }

    /// Text built from pieces that exercise UAX #14 on ASCII (punctuation,
    /// hyphens, brackets, quotes, slashes, runs of spaces) plus, optionally,
    /// some non-ASCII.
    fn pieces(non_ascii: bool) -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        let mut pool = vec![
            "a",
            "word",
            "longerword",
            "x",
            " ",
            "  ",
            "-",
            "well-known",
            "(",
            ")",
            "[",
            "]",
            "!",
            "?",
            ",",
            ".",
            ";",
            ":",
            "\"",
            "'",
            "/",
            "https://a.b/c?d=e",
            "$1.00",
            "50%",
            "#tag",
            "+",
            "|",
            "{",
            "}",
            "a/b",
            "1-2",
        ];
        if non_ascii {
            pool.extend([
                "日本",
                "e\u{301}",
                "\u{a0}",
                "\u{ad}",
                "—",
                "“q”",
                "\n",
                "❤\u{fe0f}",
            ]);
        }
        proptest::collection::vec(proptest::sample::select(pool), 0..30).prop_map(|v| v.concat())
    }

    fn general(text: &str, extra: &[u32], opts: WrapOptions, fast: bool) -> Vec<Line> {
        general_atoms(text, extra, &[], opts, fast)
    }

    fn general_atoms(
        text: &str,
        extra: &[u32],
        atoms: &[Range<u32>],
        opts: WrapOptions,
        fast: bool,
    ) -> Vec<Line> {
        let mut breaks = Vec::new();
        break_opportunities(text, extra, &mut breaks);
        let mut out = Vec::new();
        wrap_general(text, &breaks, atoms, opts, &mut out, fast);
        out
    }

    /// Sorted, disjoint atoms on char boundaries of `text`.
    fn atoms_from(text: &str, seeds: &[(usize, usize)]) -> Vec<Range<u32>> {
        let bounds: Vec<usize> = (0..=text.len())
            .filter(|&i| text.is_char_boundary(i))
            .collect();
        let mut atoms: Vec<Range<u32>> = Vec::new();
        for &(a, len) in seeds {
            let i = a % bounds.len();
            let j = (i + 1 + len % 8).min(bounds.len() - 1);
            let (s, e) = (bounds[i] as u32, bounds[j] as u32);
            if s < e && atoms.last().is_none_or(|l| s >= l.end) {
                atoms.push(s..e);
            }
        }
        atoms
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig {
            cases: proptest::prelude::ProptestConfig::default().cases.max(3000),
            failure_persistence: None,
            ..proptest::prelude::ProptestConfig::default()
        })]

        /// The printable-ASCII fill and the stretch shortcut change nothing.
        #[test]
        fn ascii_fast_paths_match_the_general_walk(
            text in pieces(false),
            first in 1u16..40,
            rest in 1u16..40,
            picks in proptest::collection::vec(0usize..200, 0..4),
        ) {
            let mut extra: Vec<u32> = picks
                .iter()
                .map(|&p| (p % (text.len() + 1)) as u32)
                .filter(|&b| b > 0 && (b as usize) < text.len())
                .collect();
            extra.sort_unstable();
            extra.dedup();
            let opts = WrapOptions { first, rest, ambiguous_wide: false };
            let slow = general(&text, &extra, opts, false);
            proptest::prop_assert_eq!(&general(&text, &extra, opts, true), &slow);
            let c = Constraints { atoms: &[], extra_breaks: &extra };
            proptest::prop_assert_eq!(&wrap(&text, c, opts), &slow);
        }

        /// The stretch shortcut changes nothing on mixed text with atoms.
        #[test]
        fn stretch_shortcut_matches_on_mixed_text(
            text in pieces(true),
            width in 1u16..40,
            rest in 1u16..40,
            seeds in proptest::collection::vec((0usize..400, 0usize..8), 0..5),
        ) {
            let atoms = atoms_from(&text, &seeds);
            let opts = WrapOptions { first: width, rest, ambiguous_wide: false };
            proptest::prop_assert_eq!(
                general_atoms(&text, &[], &atoms, opts, true),
                general_atoms(&text, &[], &atoms, opts, false)
            );
            let c = Constraints { atoms: &atoms, extra_breaks: &[] };
            proptest::prop_assert_eq!(wrap(&text, c, opts), general_atoms(&text, &[], &atoms, opts, false));
        }
    }

    #[test]
    fn narrower_rest_lines_rewrap_carried_text() {
        // "well-" breaks at the hyphen; the rest must then fit width 5.
        let opts = WrapOptions {
            first: 12,
            rest: 5,
            ambiguous_wide: false,
        };
        let t = "well-knownsupercalifragil";
        let shown: Vec<&str> = wrap(t, Constraints::default(), opts)
            .iter()
            .map(|l| &t[l.range.start as usize..l.range.end as usize])
            .collect();
        assert_eq!(shown, ["well-", "known", "super", "calif", "ragil"]);
    }

    #[test]
    fn pathological_inputs_wrap_in_linear_time() {
        // One huge word: broken every 80 columns, without rescanning it.
        let word = "x".repeat(400_000);
        let t = std::time::Instant::now();
        let l = wrap(&word, Constraints::default(), WrapOptions::uniform(80));
        assert_eq!(l.len(), 5000);
        // Plain ASCII, but a non-ASCII byte at its very end.
        let tail = format!("{}é", "y".repeat(200_000));
        assert_eq!(
            wrap(&tail, Constraints::default(), WrapOptions::uniform(80)).len(),
            2501
        );
        // A word followed by a long run of spaces, then more text.
        let spaced = format!("{}{}z", "w".repeat(100_000), " ".repeat(100_000));
        assert_eq!(
            wrap(&spaced, Constraints::default(), WrapOptions::uniform(80)).len(),
            1251
        );
        assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
    }

    #[test]
    fn stray_break_positions_are_harmless() {
        let mut out = Vec::new();
        wrap_with_breaks(
            "ab cd",
            &[3, 99, 1000],
            &[],
            WrapOptions::uniform(2),
            &mut out,
        );
        assert_eq!(
            out.iter().map(|l| l.range.clone()).collect::<Vec<_>>(),
            [0..2, 3..5]
        );
        // No opportunities at all: broken between graphemes (the space at
        // the break is still trimmed).
        wrap_with_breaks("ab cd", &[], &[], WrapOptions::uniform(2), &mut out);
        assert_eq!(
            out.iter().map(|l| l.range.clone()).collect::<Vec<_>>(),
            [0..2, 3..5]
        );
    }

    #[test]
    fn cached_breaks_rewrap_at_any_width() {
        let text = "one two three four five six seven";
        let mut breaks = Vec::new();
        break_opportunities(text, &[], &mut breaks);
        let mut out = Vec::new();
        for width in [1, 5, 9, 40] {
            let opts = WrapOptions::uniform(width);
            wrap_with_breaks(text, &breaks, &[], opts, &mut out);
            assert_eq!(out, wrap(text, Constraints::default(), opts), "{width}");
        }
        assert_eq!(breaks.first(), Some(&4));
        assert_eq!(breaks.last(), Some(&(text.len() as u32)));
    }

    #[test]
    fn wrapper_reuses_its_buffer() {
        let mut w = Wrapper::new();
        let mut out = Vec::new();
        w.wrap_into(
            "aaa bbb",
            Constraints::default(),
            WrapOptions::uniform(3),
            &mut out,
        );
        assert_eq!(out.len(), 2);
        w.wrap_into(
            "c",
            Constraints::default(),
            WrapOptions::uniform(3),
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].range, 0..1);
    }

    #[test]
    fn unsorted_extra_breaks_are_tolerated() {
        let extra = [5, 2, 5];
        let c = Constraints {
            atoms: &[],
            extra_breaks: &extra,
        };
        assert_eq!(lines_c("aaaaaaa", 3, c), ["aa", "aaa", "aa"]);
    }

    #[test]
    fn empty_and_blank_text() {
        let l = wrap("", Constraints::default(), WrapOptions::uniform(10));
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].range, 0..0);
        assert_eq!(lines("   ", 10), [""]);
    }

    #[test]
    fn widths_are_reported() {
        let l = wrap("ab cd", Constraints::default(), WrapOptions::uniform(3));
        assert_eq!(l.iter().map(|l| l.cols).collect::<Vec<_>>(), [2, 2]);
        let l = wrap("日本 語", Constraints::default(), WrapOptions::uniform(40));
        assert_eq!(l[0].cols, 7);
    }

    #[test]
    fn first_and_rest_widths() {
        let opts = WrapOptions {
            first: 5,
            rest: 11,
            ambiguous_wide: false,
        };
        let l = wrap("aaaa bbbb cccc dddd", Constraints::default(), opts);
        let shown: Vec<_> = l
            .iter()
            .map(|l| &"aaaa bbbb cccc dddd"[l.range.start as usize..l.range.end as usize])
            .collect();
        assert_eq!(shown, ["aaaa", "bbbb cccc", "dddd"]);
    }

    #[test]
    fn cjk_breaks_between_ideographs() {
        assert_eq!(lines("日本語のテキスト", 6), ["日本語", "のテキ", "スト"]);
        // Odd width: a wide character never straddles the edge.
        assert_eq!(lines("日本語", 5), ["日本", "語"]);
    }

    #[test]
    fn cjk_at_width_one_overflows_one_grapheme_per_line() {
        let l = wrap("日本", Constraints::default(), WrapOptions::uniform(1));
        assert_eq!(l.len(), 2);
        assert!(l.iter().all(|l| l.cols == 2));
    }

    #[test]
    fn zwj_and_vs16_emoji_stay_whole() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let text = format!("{family}{family}{family}");
        let l = lines(&text, 4);
        assert_eq!(l, [format!("{family}{family}"), family.to_string()]);
        let hearts = "❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}";
        assert_eq!(lines(hearts, 2), ["❤\u{fe0f}", "❤\u{fe0f}", "❤\u{fe0f}"]);
    }

    #[test]
    fn nbsp_never_breaks() {
        assert_eq!(lines("aaa\u{a0}bbb ccc", 5), ["aaa\u{a0}b", "bb", "ccc"]);
        // With room, the NBSP pair moves as one unit.
        assert_eq!(lines("x aaa\u{a0}bbb", 8), ["x", "aaa\u{a0}bbb"]);
    }

    #[test]
    fn soft_hyphen_shows_only_at_a_break() {
        let t = "hyper\u{ad}text";
        assert_eq!(lines(t, 20), [t]);
        assert_eq!(lines(t, 7), ["hyper-", "text"]);
        // The hyphen must fit: at width 5 "hyper-" is 6 columns, so the word
        // is broken between graphemes instead (after the invisible hyphen).
        assert_eq!(lines(t, 5), ["hyper\u{ad}", "text"]);
        let l = wrap(t, Constraints::default(), WrapOptions::uniform(7));
        assert!(l[0].hyphen);
        assert_eq!(l[0].cols, 6);
        assert!(!l[1].hyphen);
    }

    #[test]
    fn long_words_break_between_graphemes() {
        assert_eq!(lines("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(lines("ab abcdefghij", 4), ["ab", "abcd", "efgh", "ij"]);
        assert_eq!(
            lines("e\u{301}e\u{301}e\u{301}", 2),
            ["e\u{301}e\u{301}", "e\u{301}"]
        );
    }

    #[test]
    fn long_url_uses_break_points() {
        let url = "https://example.com/some/very/long/path";
        let l = lines(url, 20);
        assert!(l.iter().all(|s| s.len() <= 20), "{l:?}");
        assert_eq!(l.concat(), url);
        assert_eq!(l[0], "https://example.com/");
    }

    #[test]
    fn extra_breaks_add_opportunities() {
        let t = "aaaa_bbbb";
        assert_eq!(lines(t, 6), ["aaaa_b", "bbb"]);
        let extra = [5];
        let c = Constraints {
            atoms: &[],
            extra_breaks: &extra,
        };
        assert_eq!(lines_c(t, 6, c), ["aaaa_", "bbbb"]);
    }

    #[test]
    fn atoms_are_never_split() {
        let t = "see code_span_atom now";
        let atoms = [Range { start: 4, end: 18 }];
        let c = Constraints {
            atoms: &atoms,
            extra_breaks: &[],
        };
        // The atom moves to its own line and overflows it instead of splitting.
        assert_eq!(lines_c(t, 8, c), ["see", "code_span_atom", "now"]);
        // Break opportunities inside an atom are ignored.
        let t = "a b c d";
        let atoms = [Range { start: 0, end: 5 }];
        let c = Constraints {
            atoms: &atoms,
            extra_breaks: &[],
        };
        assert_eq!(lines_c(t, 3, c), ["a b c", "d"]);
    }

    #[test]
    fn atoms_in_words_too_long_for_the_line_move_whole() {
        // "aaaaaaa[Q]" has no break opportunity: it is broken between
        // graphemes, but never inside the key cap `[Q]` (bytes 7..10).
        let atoms = [Range { start: 7, end: 10 }];
        let c = Constraints {
            atoms: &atoms,
            extra_breaks: &[],
        };
        assert_eq!(lines_c("aaaaaaa[Q]", 8, c), ["aaaaaaa", "[Q]"]);
        assert_eq!(lines_c("aaaaaaa[Q]", 7, c), ["aaaaaaa", "[Q]"]);
        assert_eq!(lines_c("aaaaaaa[Q]", 9, c), ["aaaaaaa", "[Q]"]);
        assert_eq!(lines_c("aaaaaaa[Q]", 10, c), ["aaaaaaa[Q]"]);
        // Text after the atom still breaks between graphemes.
        assert_eq!(lines_c("aaaaaaa[Q]bbbb", 5, c), ["aaaaa", "aa[Q]", "bbbb"]);
        assert_eq!(lines_c("aaaaaaa[Q]bbbb", 6, c), ["aaaaaa", "a[Q]bb", "bb"]);
        // A break opportunity before the atom is preferred.
        let t = "x aaaaaa[Q]";
        let atoms = [Range { start: 8, end: 11 }];
        let c = Constraints {
            atoms: &atoms,
            extra_breaks: &[],
        };
        assert_eq!(lines_c(t, 8, c), ["x", "aaaaaa", "[Q]"]);
        // An atom that starts the line and is wider than it still overflows.
        let atoms = [Range { start: 0, end: 5 }];
        let c = Constraints {
            atoms: &atoms,
            extra_breaks: &[],
        };
        assert_eq!(lines_c("[abc]", 3, c), ["[abc]"]);
    }

    #[test]
    fn hard_breaks() {
        assert_eq!(lines("one\ntwo", 20), ["one", "two"]);
        assert_eq!(lines("one  \n two", 20), ["one", " two"]);
        assert_eq!(lines("a\n\nb", 20), ["a", "", "b"]);
        let l = wrap("a\nb", Constraints::default(), WrapOptions::uniform(9));
        assert!(l[0].hard && !l[1].hard);
        assert_eq!(lines("x\n", 5), ["x", ""]);
    }

    #[test]
    fn width_one() {
        assert_eq!(lines("ab c", 1), ["a", "b", "c"]);
        assert_eq!(lines("a-b", 1), ["a", "-", "b"]);
    }

    #[test]
    fn zero_width_is_treated_as_one() {
        assert_eq!(lines("ab", 0), ["a", "b"]);
    }

    #[test]
    fn split_runs_cuts_at_lines() {
        // Runs: "the " | "quick brown" | " fox"
        let text = "the quick brown fox";
        let ends = [4u32, 15, 19];
        let l = wrap(text, Constraints::default(), WrapOptions::uniform(10));
        let mut pieces = Vec::new();
        split_runs(&ends, |&e| e, &l, &mut pieces);
        let got: Vec<(u32, u32, &str)> = pieces
            .iter()
            .map(|p| {
                (
                    p.line,
                    p.run,
                    &text[p.range.start as usize..p.range.end as usize],
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (0, 0, "the "),
                (0, 1, "quick"),
                (1, 1, "brown"),
                (1, 2, " fox"),
            ]
        );
    }

    #[test]
    fn split_runs_skips_empty_runs_and_gaps() {
        let text = "ab\ncd";
        let ends = [0u32, 2, 3, 5];
        let l = wrap(text, Constraints::default(), WrapOptions::uniform(10));
        let mut pieces = Vec::new();
        split_runs(&ends, |&e| e, &l, &mut pieces);
        let got: Vec<(u32, u32)> = pieces.iter().map(|p| (p.line, p.run)).collect();
        assert_eq!(got, [(0, 1), (1, 3)]);
    }
}
