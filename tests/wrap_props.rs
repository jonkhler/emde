//! Property tests for the line breaker: on random text and widths 1–200,
//! no line exceeds its width (unless a single grapheme or an atom is wider
//! than the line), no grapheme is lost or split, lines appear in order, and
//! only breakable whitespace, soft hyphens and hard-break newlines fall
//! between lines.

use std::ops::Range;

use emde::text::width::next_grapheme_end;
use emde::text::{Constraints, Line, WrapOptions, str_width, wrap};
use proptest::prelude::*;

/// Pieces random texts are made of: words, spaces, CJK, emoji sequences,
/// combining marks, no-break and soft hyphens, URLs, hard breaks.
const PIECES: &[&str] = &[
    "a",
    "word",
    "longerword",
    "supercalifragilisticexpialidocious",
    " ",
    "  ",
    "\n",
    "-",
    "well-known",
    "日本語",
    "中",
    "한국어",
    "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
    "❤\u{fe0f}",
    "\u{1f44d}\u{1f3fd}",
    "\u{1f1fa}\u{1f1f8}",
    "e\u{301}",
    "\u{a0}",
    "\u{ad}",
    "\u{200b}",
    "\u{3000}",
    "https://example.com/some/path?q=1&r=2#frag",
    "(",
    ")",
    ".",
    ",",
    "…",
    "—",
];

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 0..40).prop_map(|v| v.concat())
}

/// Grapheme cluster boundaries of `s` (including 0 and `s.len()`).
fn boundaries(s: &str) -> Vec<usize> {
    let mut out = vec![0];
    let mut pos = 0;
    while pos < s.len() {
        pos = next_grapheme_end(s, pos);
        out.push(pos);
    }
    out
}

fn is_gap_char(c: char) -> bool {
    matches!(
        c,
        ' ' | '\n' | '\u{ad}' | '\u{3000}' | '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200b}'
    )
}

fn slice<'a>(s: &'a str, r: &Range<u32>) -> &'a str {
    &s[r.start as usize..r.end as usize]
}

/// Invariants every wrapping must satisfy; `width(i)` is the width of line
/// `i`.
fn check_lines(text: &str, lines: &[Line], width: impl Fn(usize) -> u16, atoms: &[Range<u32>]) {
    assert!(!lines.is_empty(), "at least one line");
    let bounds = boundaries(text);
    let mut prev_end = 0u32;
    for (i, line) in lines.iter().enumerate() {
        let r = &line.range;
        assert!(
            r.start <= r.end && r.start >= prev_end,
            "line {i} out of order: {lines:?}"
        );
        for p in [r.start, r.end] {
            assert!(
                bounds.contains(&(p as usize)),
                "line {i} splits a grapheme at {p}"
            );
        }
        // Only breakable whitespace, soft hyphens and newlines between lines.
        let gap = &text[prev_end as usize..r.start as usize];
        assert!(
            gap.chars().all(is_gap_char),
            "lost text {gap:?} before line {i}"
        );
        if i > 0 && lines[i - 1].hard {
            assert!(gap.contains('\n'), "hard break without newline");
        }
        assert!(!slice(text, r).contains('\n'), "newline inside line {i}");
        // The reported width is right.
        let shown = str_width(slice(text, r), false) + usize::from(line.hyphen);
        assert_eq!(usize::from(line.cols), shown, "line {i} width");
        // Lines fit, unless one grapheme or an atom is wider than the line.
        let width = width(i);
        if line.cols > width {
            let graphemes = bounds
                .iter()
                .filter(|&&b| b > r.start as usize && b <= r.end as usize)
                .count();
            let has_atom = atoms.iter().any(|a| a.start < r.end && a.end > r.start);
            assert!(
                graphemes == 1 || has_atom,
                "line {i} ({:?}, {} cols) exceeds width {width}",
                slice(text, r),
                line.cols
            );
        }
        prev_end = r.end;
    }
    let tail = &text[prev_end as usize..];
    assert!(tail.chars().all(is_gap_char), "lost trailing text {tail:?}");
    // Every newline is a hard break.
    let hard = lines.iter().filter(|l| l.hard).count();
    assert_eq!(hard, text.matches('\n').count());
    // Atoms are never split: no line starts strictly inside one. (Trailing
    // whitespace of an atom may still be trimmed at the end of a line.)
    for a in atoms {
        let split = lines
            .iter()
            .any(|l| a.start < l.range.start && l.range.start < a.end);
        assert!(!split, "atom {a:?} split: {lines:?}");
    }
}

/// Atoms at grapheme boundaries, sorted and disjoint, not containing
/// newlines.
fn pick_atoms(text: &str, seeds: &[(u8, u8)]) -> Vec<Range<u32>> {
    let bounds = boundaries(text);
    let mut atoms: Vec<Range<u32>> = Vec::new();
    for &(start, len) in seeds {
        let i = usize::from(start) % bounds.len();
        let j = (i + 1 + usize::from(len) % 6).min(bounds.len() - 1);
        let (s, e) = (bounds[i] as u32, bounds[j] as u32);
        if s < e
            && !text[s as usize..e as usize].contains('\n')
            && atoms.last().is_none_or(|last| s >= last.end)
        {
            atoms.push(s..e);
        }
    }
    atoms
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: ProptestConfig::default().cases.max(2000),
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn lines_fit_and_nothing_is_lost(text in text(), width in 1u16..=200) {
        let lines = wrap(&text, Constraints::default(), WrapOptions::uniform(width));
        check_lines(&text, &lines, |_| width, &[]);
    }

    #[test]
    fn first_and_rest_widths(text in text(), first in 1u16..=60, rest in 1u16..=60) {
        let opts = WrapOptions { first, rest, ambiguous_wide: false };
        let lines = wrap(&text, Constraints::default(), opts);
        check_lines(&text, &lines, |i| if i == 0 { first } else { rest }, &[]);
    }

    #[test]
    fn atoms_are_never_split(
        text in text(),
        seeds in prop::collection::vec((any::<u8>(), any::<u8>()), 0..6),
        width in 1u16..=80,
    ) {
        let atoms = pick_atoms(&text, &seeds);
        let constraints = Constraints { atoms: &atoms, extra_breaks: &[] };
        let lines = wrap(&text, constraints, WrapOptions::uniform(width));
        check_lines(&text, &lines, |_| width, &atoms);
    }

    #[test]
    fn extra_breaks_only_add_opportunities(
        text in text(),
        picks in prop::collection::vec(any::<u16>(), 0..8),
        width in 1u16..=80,
    ) {
        let bounds = boundaries(&text);
        let mut extra: Vec<u32> = picks
            .iter()
            .map(|&p| bounds[usize::from(p) % bounds.len()] as u32)
            .filter(|&b| b > 0 && (b as usize) < text.len())
            .collect();
        extra.sort_unstable();
        extra.dedup();
        let constraints = Constraints { atoms: &[], extra_breaks: &extra };
        let lines = wrap(&text, constraints, WrapOptions::uniform(width));
        check_lines(&text, &lines, |_| width, &[]);
    }

    #[test]
    fn wide_enough_means_one_line_per_hard_break(text in text()) {
        let lines = wrap(&text, Constraints::default(), WrapOptions::uniform(u16::MAX));
        prop_assert_eq!(lines.len(), text.matches('\n').count() + 1);
    }
}
