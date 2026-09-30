//! Tab expansion for code lines.
//!
//! Tabs are expanded to spaces with stops every `tab_width` columns counted
//! from the code block's left edge, using display widths (so a tab after a
//! CJK character still lands on a tab stop). Highlight runs are byte offsets
//! into the *unexpanded* line, so [`Expanded::map_offset`] translates them.

use std::borrow::Cow;

use super::width::{grapheme_width, next_grapheme_end};

/// One line of code with its tabs expanded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expanded<'a> {
    /// The expanded text (borrowed when the line has no tabs).
    pub text: Cow<'a, str>,
    /// For each tab: (source offset just after it, bytes added so far).
    shifts: Vec<(u32, u32)>,
}

impl Expanded<'_> {
    /// Translate a byte offset in the source line into the expanded text.
    ///
    /// Offsets inside a tab map to the start of its expansion; offsets past
    /// the end map past the end.
    pub fn map_offset(&self, src: u32) -> u32 {
        let idx = self.shifts.partition_point(|&(after, _)| after <= src);
        let added = idx
            .checked_sub(1)
            .and_then(|i| self.shifts.get(i))
            .map_or(0, |&(_, added)| added);
        src.saturating_add(added)
    }

    /// Translate a byte offset in the expanded text back into the source
    /// line (the inverse of [`Expanded::map_offset`]).
    ///
    /// Offsets inside a tab's expansion map to the tab itself.
    pub fn source_offset(&self, expanded: u32) -> u32 {
        // Tabs whose expansion ends at or before `expanded`.
        let idx = self
            .shifts
            .partition_point(|&(after, added)| after.saturating_add(added) <= expanded);
        let added = idx
            .checked_sub(1)
            .and_then(|i| self.shifts.get(i))
            .map_or(0, |&(_, added)| added);
        let src = expanded.saturating_sub(added);
        // Inside the next tab's expansion: that tab.
        match self.shifts.get(idx) {
            Some(&(after, _)) => src.min(after.saturating_sub(1)),
            None => src,
        }
    }

    /// Whether any tab was expanded.
    pub fn changed(&self) -> bool {
        !self.shifts.is_empty()
    }
}

/// Expand the tabs in one line (which must not contain `\n`).
///
/// `tab_width` 0 is treated as 1.
pub fn expand_tabs(line: &str, tab_width: u8, ambiguous_wide: bool) -> Expanded<'_> {
    if !line.contains('\t') {
        return Expanded {
            text: Cow::Borrowed(line),
            shifts: Vec::new(),
        };
    }
    let tw = usize::from(tab_width.max(1));
    let mut out = String::with_capacity(line.len() + 8);
    let mut shifts = Vec::new();
    let mut col = 0usize;
    let mut added = 0usize;
    let mut pos = 0usize;
    while pos < line.len() {
        let end = next_grapheme_end(line, pos);
        let g = line.get(pos..end).unwrap_or_default();
        if g == "\t" {
            let n = tw - col % tw;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
            added += n - 1;
            shifts.push((to_u32(end), to_u32(added)));
        } else {
            out.push_str(g);
            col += grapheme_width(g, ambiguous_wide);
        }
        pos = end;
    }
    Expanded {
        text: Cow::Owned(out),
        shifts,
    }
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tabs_borrows() {
        let e = expand_tabs("fn main() {}", 4, false);
        assert!(matches!(e.text, Cow::Borrowed(_)));
        assert!(!e.changed());
        assert_eq!(e.map_offset(3), 3);
    }

    #[test]
    fn expands_to_tab_stops() {
        assert_eq!(expand_tabs("\tx", 4, false).text, "    x");
        assert_eq!(expand_tabs("ab\tc", 4, false).text, "ab  c");
        assert_eq!(expand_tabs("abcd\te", 4, false).text, "abcd    e");
        assert_eq!(expand_tabs("a\t\tb", 2, false).text, "a   b");
        assert_eq!(expand_tabs("\t", 0, false).text, " ");
    }

    #[test]
    fn wide_characters_count_two_columns() {
        assert_eq!(expand_tabs("日\tx", 4, false).text, "日  x");
        assert_eq!(expand_tabs("±\tx", 4, true).text, "±  x");
        assert_eq!(expand_tabs("±\tx", 4, false).text, "±   x");
    }

    #[test]
    fn maps_offsets_past_tabs() {
        // "a\tb\tc" with width 4 → "a   b   c"
        let e = expand_tabs("a\tb\tc", 4, false);
        assert_eq!(e.text, "a   b   c");
        assert_eq!(e.map_offset(0), 0); // a
        assert_eq!(e.map_offset(1), 1); // the tab starts where it was
        assert_eq!(e.map_offset(2), 4); // b
        assert_eq!(e.map_offset(4), 8); // c
        assert_eq!(e.map_offset(5), 9); // end
    }

    #[test]
    fn maps_offsets_back_to_the_source() {
        // "a\tb\tc" with width 4 → "a   b   c"
        let e = expand_tabs("a\tb\tc", 4, false);
        let back: Vec<u32> = (0..=9).map(|x| e.source_offset(x)).collect();
        //          a  ␠  ␠  ␠  b  ␠  ␠  ␠  c  end
        assert_eq!(back, [0, 1, 1, 1, 2, 3, 3, 3, 4, 5]);
        for src in 0..=5 {
            assert_eq!(e.source_offset(e.map_offset(src)), src, "{src}");
        }
        let plain = expand_tabs("abc", 4, false);
        assert_eq!(plain.source_offset(2), 2);
        let lead = expand_tabs("\t\tx", 2, false);
        assert_eq!(lead.text, "    x");
        assert_eq!(lead.source_offset(3), 1);
        assert_eq!(lead.source_offset(4), 2);
    }
}
