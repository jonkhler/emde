//! Display width of text in terminal columns.
//!
//! Width is measured per *extended grapheme cluster* with
//! [`UnicodeWidthStr::width`] (or `width_cjk` when East Asian Ambiguous
//! characters are wide), so emoji ZWJ sequences and VS16 presentation
//! sequences count as the two columns terminals draw, and combining marks
//! count as zero. Summing per-`char` widths gets both wrong.
//!
//! The text measured here never contains escape sequences or control
//! characters other than `\n` and `\t` (see [`super::sanitize()`]); callers
//! expand tabs ([`super::tabs`]) and split at newlines before measuring.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Width of one grapheme cluster.
///
/// A single ASCII byte is one column (the fast path); anything else goes
/// through `unicode-width`'s string rules, which understand emoji sequences.
#[inline]
pub fn grapheme_width(g: &str, ambiguous_wide: bool) -> usize {
    if g.len() == 1 {
        return 1;
    }
    if ambiguous_wide {
        g.width_cjk()
    } else {
        g.width()
    }
}

/// Width of a string: the sum of its grapheme widths.
///
/// ASCII strings take a fast path (one column per byte).
pub fn str_width(s: &str, ambiguous_wide: bool) -> usize {
    if s.is_ascii() {
        return s.len();
    }
    s.graphemes(true)
        .map(|g| grapheme_width(g, ambiguous_wide))
        .sum()
}

/// Byte offset where the grapheme cluster starting at `pos` ends.
///
/// `pos` must be a grapheme boundary (callers walk forwards from 0 or from a
/// previous result). An ASCII byte followed by an ASCII byte is always a
/// complete cluster (CR LF is the only joining ASCII pair, and sanitised text
/// has no CR), so the common case never touches the segmentation tables.
/// Returns `s.len()` when `pos` is at or past the end or not a char boundary.
#[inline]
pub fn next_grapheme_end(s: &str, pos: usize) -> usize {
    let b = s.as_bytes();
    if let Some(&c) = b.get(pos)
        && c < 0x80
        && c != b'\r'
        && b.get(pos + 1).is_none_or(|&n| n < 0x80)
    {
        return pos + 1;
    }
    s.get(pos..)
        .and_then(|rest| rest.graphemes(true).next())
        .map_or(s.len(), |g| pos + g.len())
}

/// Whether a character is East Asian Wide or Fullwidth (two columns).
///
/// Used for the soft-break rule: a line break between two such characters is
/// removed instead of becoming a space (CSS Text 3 segment-break rules), except
/// for Hangul, which separates words with spaces.
pub fn is_wide_east_asian(c: char) -> bool {
    unicode_width::UnicodeWidthChar::width(c) == Some(2) && !is_hangul(c)
}

/// Hangul jamo, compatibility jamo and syllables.
fn is_hangul(c: char) -> bool {
    matches!(
        c,
        '\u{1100}'..='\u{11ff}'
            | '\u{3130}'..='\u{318f}'
            | '\u{a960}'..='\u{a97f}'
            | '\u{ac00}'..='\u{d7ff}'
            | '\u{ffa0}'..='\u{ffdc}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_fast_path() {
        assert_eq!(str_width("hello, world", false), 12);
        assert_eq!(str_width("", false), 0);
    }

    #[test]
    fn cjk_is_double_width() {
        assert_eq!(str_width("日本語", false), 6);
        assert_eq!(str_width("a日b", false), 4);
        assert_eq!(str_width("ｱｲ", false), 2, "halfwidth katakana");
    }

    #[test]
    fn emoji_sequences_are_two_columns() {
        // Family: man ZWJ woman ZWJ girl — one cluster, two columns.
        assert_eq!(
            str_width("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}", false),
            2
        );
        // Red heart + VS16.
        assert_eq!(str_width("\u{2764}\u{fe0f}", false), 2);
        // Thumbs up + skin tone modifier.
        assert_eq!(str_width("\u{1f44d}\u{1f3fd}", false), 2);
        // Flag: two regional indicators.
        assert_eq!(str_width("\u{1f1fa}\u{1f1f8}", false), 2);
    }

    #[test]
    fn combining_marks_and_invisible() {
        assert_eq!(str_width("e\u{301}", false), 1);
        assert_eq!(str_width("soft\u{ad}hyphen", false), 10);
        assert_eq!(str_width("a\u{200b}b", false), 2);
        assert_eq!(str_width("\u{a0}", false), 1, "NBSP is a normal space");
    }

    #[test]
    fn ambiguous_width() {
        // U+00B1 PLUS-MINUS SIGN is East Asian Ambiguous.
        assert_eq!(str_width("±", false), 1);
        assert_eq!(str_width("±", true), 2);
        assert_eq!(grapheme_width("→", true), 2);
        assert_eq!(grapheme_width("→", false), 1);
        assert_eq!(grapheme_width("a", true), 1);
    }

    #[test]
    fn grapheme_stepping() {
        let s = "ae\u{301}👨\u{200d}👩x";
        let mut ends = Vec::new();
        let mut pos = 0;
        while pos < s.len() {
            pos = next_grapheme_end(s, pos);
            ends.push(pos);
        }
        assert_eq!(ends, vec![1, 4, 15, 16]);
        assert_eq!(next_grapheme_end(s, 99), s.len());
        // Not a char boundary: total, returns the end.
        assert_eq!(next_grapheme_end("é", 1), 2);
    }

    #[test]
    fn east_asian_wide_classification() {
        assert!(is_wide_east_asian('中'));
        assert!(is_wide_east_asian('あ'));
        assert!(is_wide_east_asian('Ａ'));
        assert!(!is_wide_east_asian('한'), "Hangul uses spaces");
        assert!(!is_wide_east_asian('a'));
        assert!(!is_wide_east_asian('ｱ'), "halfwidth");
    }
}
