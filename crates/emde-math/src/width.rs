//! Display widths in terminal columns.
//!
//! Widths are `unicode-width`'s string widths (with East Asian Ambiguous
//! characters as two columns when `cjk`, see
//! [`MathOptions::ambiguous_wide`](crate::MathOptions)), so a line measures
//! the same here as anywhere else that uses the crate.
//!
//! The 2D renderer puts one *cluster* in each cell (two cells when it is
//! wide). A cluster is a character plus what `unicode-width` measures with
//! it: the zero-width characters that follow (combining marks, variation
//! selectors, joiners) and any character that would change the width by
//! joining (an emoji ZWJ sequence or skin tone, Arabic lam-alef). A combining
//! mark therefore never starts a cell.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The width of one character (0 for combining marks and controls).
pub(crate) fn char_width(c: char, cjk: bool) -> usize {
    let w = if cjk { c.width_cjk() } else { c.width() };
    w.unwrap_or(0)
}

/// Whether `c` attaches to the preceding character instead of starting a
/// cluster of its own.
pub(crate) fn is_zero_width(c: char) -> bool {
    char_width(c, false) == 0 && !c.is_control()
}

/// The width of `s`.
pub(crate) fn str_width(s: &str, cjk: bool) -> usize {
    if cjk { s.width_cjk() } else { s.width() }
}

/// Split `s` into clusters: `(cluster, width)`. A cluster whose first
/// character is zero-width only occurs at the start of `s` (an orphan mark);
/// it has width 0.
pub(crate) fn clusters(s: &str, cjk: bool) -> impl Iterator<Item = (&str, usize)> + '_ {
    let mut rest = s;
    std::iter::from_fn(move || {
        let mut chars = rest.char_indices();
        let (_, first) = chars.next()?;
        let mut end = first.len_utf8();
        let mut width = str_width(&rest[..end], cjk);
        for (i, c) in chars {
            let next = i + c.len_utf8();
            let joined = str_width(&rest[..next], cjk);
            if !is_zero_width(c) && joined >= width + str_width(&rest[i..next], cjk) {
                break;
            }
            end = next;
            width = joined;
        }
        let (cluster, tail) = rest.split_at(end);
        rest = tail;
        Some((cluster, width))
    })
}

/// Whether two adjacent clusters would be measured together (and so need a
/// zero-width non-joiner between them to keep their own widths).
pub(crate) fn joins(a: &str, b: &str, cjk: bool, buf: &mut String) -> bool {
    if a.is_ascii() || b.is_ascii() {
        return false;
    }
    buf.clear();
    buf.push_str(a);
    buf.push_str(b);
    str_width(buf, cjk) != str_width(a, cjk) + str_width(b, cjk)
}

/// Make arbitrary text safe for output: control characters become U+FFFD,
/// runs of whitespace become one space, and a zero-width character at the
/// start gets a no-break space to sit on (a combining mark never starts a
/// cell).
pub(crate) fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        if c.is_control() {
            out.push('\u{FFFD}');
        } else {
            if out.is_empty() && is_zero_width(c) {
                out.push('\u{A0}');
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

/// Clamp a column count to the `u16` of the public API.
pub(crate) fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clusters_keep_marks_with_their_base() {
        let got: Vec<_> = clusters("x\u{302}y", false).collect();
        assert_eq!(got, [("x\u{302}", 1), ("y", 1)]);
        assert_eq!(str_width("x\u{302}y", false), 2);
    }

    #[test]
    fn clusters_keep_joining_sequences_together() {
        let family = "\u{1F468}\u{200D}\u{1F469}";
        assert_eq!(clusters(family, false).collect::<Vec<_>>(), [(family, 2)]);
        let skin = "\u{1F468}\u{1F3FB}";
        assert_eq!(clusters(skin, false).collect::<Vec<_>>(), [(skin, 2)]);
        let lam_alef = "\u{644}\u{627}";
        assert_eq!(
            clusters(lam_alef, false).collect::<Vec<_>>(),
            [(lam_alef, 1)]
        );
        // Flags stay two clusters of one column each.
        assert_eq!(clusters("\u{1F1FA}\u{1F1F8}", false).count(), 2);
        let mut buf = String::new();
        assert!(joins("\u{1F468}\u{200D}", "\u{1F469}", false, &mut buf));
        assert!(!joins("α", "²", false, &mut buf));
        assert!(!joins("a", "b", false, &mut buf));
    }

    #[test]
    fn wide_and_ambiguous_characters() {
        assert_eq!(str_width("日本", false), 4);
        assert_eq!(str_width("α─√", false), 3);
        // unicode-width keeps Greek narrow even where ambiguous means wide.
        assert_eq!(str_width("α─√", true), 5);
        // Emoji presentation selector widens its base.
        assert_eq!(str_width("\u{263A}\u{FE0F}", false), 2);
        assert_eq!(clusters("\u{263A}\u{FE0F}", false).count(), 1);
    }

    #[test]
    fn orphan_marks_have_no_width() {
        let got: Vec<_> = clusters("\u{301}a", false).collect();
        assert_eq!(got, [("\u{301}", 0), ("a", 1)]);
    }

    #[test]
    fn sanitize_collapses_space_and_replaces_controls() {
        assert_eq!(sanitize("a \n\t b\u{7}"), "a b\u{FFFD}");
        assert_eq!(sanitize("  x  "), " x ");
        assert_eq!(sanitize("\u{301}e"), "\u{A0}\u{301}e");
        assert_eq!(sanitize(""), "");
    }

    #[test]
    fn to_u16_saturates() {
        assert_eq!(to_u16(7), 7);
        assert_eq!(to_u16(1 << 20), u16::MAX);
    }
}
