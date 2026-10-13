//! Horizontal windows on wide lines.
//!
//! The pager shows a block wider than the text column (a table or code at
//! its natural width, [`crate::layout::Wide`]) through a window: the
//! columns of the containers' prefix stay where they are, and the window
//! shows `cols` columns of the rest from column `from` on. [`Clip`] cuts the
//! pieces of a line ([`super::segments`]) to that window, before they
//! become bytes, so whatever the emitter does with whole pieces it does
//! with cut ones too: SGR transitions between their styles, and an OSC 8
//! link opened again in the visible part of a link that is cut. Text is cut
//! between graphemes, so combining marks stay with their base; a wide
//! character (CJK, emoji) cut in half shows its visible half as a space in
//! its style. Where the line goes on beyond an edge of the window, a `‹` or
//! `›` takes the edge column.
//!
//! The output is always exactly `keep + cols` columns wide: what the line
//! does not cover is padded with plain spaces.

use crate::style::StyleId;
use crate::text::grapheme_width;
use crate::text::width::{next_grapheme_end, str_width};

use super::emit::{Piece, SegStyle};

/// Which columns of a line are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// Columns at the start shown as they are (the prefix outside the
    /// block).
    pub keep: u16,
    /// The first column shown after them (at least `keep`).
    pub from: u16,
    /// How many columns are shown after them.
    pub cols: u16,
    /// Columns the line covers: whether it goes on beyond an edge.
    pub extent: u16,
}

/// `n` spaces in pieces of a static string.
pub(super) fn spaces(n: u16, f: &mut dyn FnMut(&str)) {
    const SPACES: &str = "                                                                ";
    let mut left = usize::from(n);
    while left > 0 {
        let take = left.min(SPACES.len());
        f(SPACES.get(..take).unwrap_or(""));
        left -= take;
    }
}

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// Cuts the pieces of one line to a [`Window`]; see the module docs. Feed
/// it the line's pieces in order with [`Clip::piece`], then call
/// [`Clip::finish`].
pub struct Clip<'g> {
    win: Window,
    /// The line goes on to the left of the window / to its right.
    left: bool,
    right: bool,
    /// Content columns of the window, in line columns: `from + left` to
    /// `from + cols - right`.
    shown: (u16, u16),
    marker: SegStyle,
    glyphs: [&'g str; 2],
    amb: bool,
    /// The line column of the next piece.
    col: u16,
    /// Columns written.
    out: u16,
}

impl<'g> Clip<'g> {
    /// A clip to `win`, with `glyphs` (`‹`, `›`, one column each) in
    /// `marker` at clipped edges; widths measured with `amb` (East Asian
    /// Ambiguous characters wide).
    pub fn new(win: Window, marker: SegStyle, glyphs: [&'g str; 2], amb: bool) -> Clip<'g> {
        let win = Window {
            from: win.from.max(win.keep),
            ..win
        };
        let end = win.from.saturating_add(win.cols);
        let left = win.cols > 0 && win.from > win.keep && win.extent > win.keep;
        let right = win.cols > u16::from(left) && win.extent > end;
        let shown = (
            win.from.saturating_add(u16::from(left)),
            end.saturating_sub(u16::from(right)),
        );
        Clip {
            win,
            left,
            right,
            shown,
            marker,
            glyphs,
            amb,
            col: 0,
            out: 0,
        }
    }

    /// Where line column `c` is written, if it is shown.
    fn place(&self, c: u16) -> Option<u16> {
        if c < self.win.keep {
            Some(c)
        } else if (self.shown.0..self.shown.1).contains(&c) {
            Some(self.win.keep.saturating_add(c - self.win.from))
        } else {
            None
        }
    }

    /// Spaces up to output column `to`, the `‹` on the way.
    fn pad_to(&mut self, to: u16, f: &mut dyn FnMut(Piece<'_>)) {
        let keep = self.win.keep;
        if self.left && self.out <= keep && to > keep {
            self.blank(keep - self.out, SegStyle::Id(StyleId(0)), f);
            f(Piece {
                text: self.glyphs[0],
                style: self.marker,
                link: None,
                off: None,
            });
            self.out = keep.saturating_add(1);
        }
        if to > self.out {
            self.blank(to - self.out, SegStyle::Id(StyleId(0)), f);
            self.out = to;
        }
    }

    /// `n` spaces in `style` (not counted in `out`).
    fn blank(&self, n: u16, style: SegStyle, f: &mut dyn FnMut(Piece<'_>)) {
        spaces(n, &mut |pad| {
            f(Piece {
                text: pad,
                style,
                link: None,
                off: None,
            })
        });
    }

    /// Hand out the part of `piece` the window shows.
    pub fn piece(&mut self, piece: Piece<'_>, f: &mut dyn FnMut(Piece<'_>)) {
        let start = self.col;
        let w = to_u16(str_width(piece.text, self.amb));
        self.col = start.saturating_add(w);
        // Whole pieces: inside one region, or nowhere.
        if w > 0 {
            let end = self.col;
            if let Some(at) = self.place(start)
                && self.place(end - 1) == Some(at.saturating_add(w - 1))
            {
                self.pad_to(at, f);
                f(piece);
                self.out = at.saturating_add(w);
                return;
            }
            let hidden = |a: u16, b: u16| end <= a || start >= b;
            if hidden(0, self.win.keep) && hidden(self.shown.0, self.shown.1) {
                return;
            }
        }
        // Grapheme by grapheme, in runs of what is shown whole.
        let text = piece.text;
        let sub = |a: usize, b: usize| Piece {
            text: text.get(a..b).unwrap_or(""),
            off: piece
                .off
                .map(|o| o.saturating_add(u32::try_from(a).unwrap_or(u32::MAX))),
            ..piece
        };
        let mut run: Option<(usize, u16)> = None; // byte start, output column
        let mut c = start;
        let mut pos = 0;
        while pos < text.len() {
            let next = next_grapheme_end(text, pos);
            let gw = to_u16(grapheme_width(text.get(pos..next).unwrap_or(""), self.amb));
            let whole = match gw {
                0 => self.place(c).map(|_| self.out),
                _ => self.place(c).filter(|&at| {
                    self.place(c.saturating_add(gw - 1)) == Some(at.saturating_add(gw - 1))
                }),
            };
            match (whole, run) {
                (Some(at), Some(_)) if at == self.out => {}
                (Some(at), _) => {
                    if let Some((a, _)) = run.take() {
                        f(sub(a, pos));
                    }
                    self.pad_to(at, f);
                    run = Some((pos, at));
                }
                (None, _) => {
                    if let Some((a, _)) = run.take() {
                        f(sub(a, pos));
                    }
                    // Cut in half (or hidden): its shown columns as spaces.
                    for k in c..c.saturating_add(gw) {
                        if let Some(at) = self.place(k) {
                            self.pad_to(at, f);
                            self.blank(1, piece.style, f);
                            self.out = at.saturating_add(1);
                        }
                    }
                }
            }
            if let (Some(_), Some(at)) = (run, whole) {
                self.out = at.saturating_add(gw);
            }
            c = c.saturating_add(gw);
            pos = next;
        }
        if let Some((a, _)) = run {
            f(sub(a, text.len()));
        }
    }

    /// Pad the window to its end, with the `›` when the line goes on.
    pub fn finish(&mut self, f: &mut dyn FnMut(Piece<'_>)) {
        let end = self.win.keep.saturating_add(self.win.cols);
        self.pad_to(end.saturating_sub(u16::from(self.right)), f);
        if self.right {
            f(Piece {
                text: self.glyphs[1],
                style: self.marker,
                link: None,
                off: None,
            });
            self.out = end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::LinkId;

    const GLYPHS: [&str; 2] = ["‹", "›"];

    /// The pieces `texts` (style `i`, link on odd ones) cut to `win`:
    /// `(text, style, link)` per piece out.
    fn cut(texts: &[&str], win: Window) -> Vec<(String, SegStyle, Option<LinkId>)> {
        let mut clip = Clip::new(win, SegStyle::Id(StyleId(9)), GLYPHS, false);
        let mut out = Vec::new();
        let mut push = |p: Piece<'_>| out.push((p.text.to_owned(), p.style, p.link));
        for (i, t) in texts.iter().enumerate() {
            let piece = Piece {
                text: t,
                style: SegStyle::Id(StyleId(u16::try_from(i).unwrap() + 1)),
                link: (i % 2 == 1).then_some(LinkId(0)),
                off: Some(0),
            };
            clip.piece(piece, &mut push);
        }
        clip.finish(&mut push);
        out
    }

    fn text(pieces: &[(String, SegStyle, Option<LinkId>)]) -> String {
        pieces.iter().map(|p| p.0.as_str()).collect()
    }

    fn win(keep: u16, from: u16, cols: u16, extent: u16) -> Window {
        Window {
            keep,
            from,
            cols,
            extent,
        }
    }

    #[test]
    fn a_window_that_shows_everything_changes_nothing_but_padding() {
        let out = cut(&["| ", "abc", "def"], win(2, 2, 10, 8));
        assert_eq!(text(&out), "| abcdef    ");
        assert_eq!(out[1].1, SegStyle::Id(StyleId(2)), "pieces keep styles");
    }

    #[test]
    fn shifted_windows_keep_the_prefix_and_mark_the_edges() {
        // Prefix `| `, then 12 columns of content; 6 shown from column 5.
        let out = cut(&["| ", "abcdef", "ghijkl"], win(2, 5, 6, 14));
        assert_eq!(text(&out), "| ‹efgh›");
        let markers: Vec<&str> = out
            .iter()
            .filter(|p| p.1 == SegStyle::Id(StyleId(9)))
            .map(|p| p.0.as_str())
            .collect();
        assert_eq!(markers, ["‹", "›"]);
        // At the end: no `›`.
        assert_eq!(
            text(&cut(&["| ", "abcdef", "ghijkl"], win(2, 8, 6, 14))),
            "| ‹hijkl"
        );
        // Not shifted: no `‹`.
        assert_eq!(
            text(&cut(&["| ", "abcdef", "ghijkl"], win(2, 2, 6, 14))),
            "| abcde›"
        );
    }

    #[test]
    fn links_are_cut_with_their_text() {
        // The second piece is a link: its visible part keeps the link.
        let out = cut(&["", "linktext", "rest"], win(0, 3, 6, 12));
        assert_eq!(text(&out), "‹text›");
        assert!(out.iter().any(|p| p.0 == "text" && p.2 == Some(LinkId(0))));
    }

    #[test]
    fn wide_characters_cut_in_half_become_spaces() {
        // 日本語 is six columns; from column 1 the first is cut in half.
        let out = cut(&["日本語x"], win(0, 1, 7, 7));
        assert_eq!(text(&out), "‹本語x ");
        let out = cut(&["a日本語"], win(0, 2, 4, 7));
        assert_eq!(text(&out), "‹本›", "{out:?}");
        // Only the left half of 本 between the markers.
        let out = cut(&["a日本語"], win(0, 2, 3, 7));
        assert_eq!(text(&out), "‹ ›", "{out:?}");
        // The half of 本 left of the right edge: a space in its style.
        let out = cut(&["ab本cd"], win(0, 0, 4, 6));
        assert_eq!(text(&out), "ab ›");
        assert_eq!(out[1], (" ".to_owned(), SegStyle::Id(StyleId(1)), None));
    }

    #[test]
    fn combining_marks_stay_with_their_base() {
        let out = cut(&["e\u{301}e\u{301}e\u{301}e\u{301}"], win(0, 1, 3, 4));
        assert_eq!(text(&out), "‹e\u{301}e\u{301}");
        let out = cut(&["ae\u{301}b"], win(0, 0, 2, 3));
        assert_eq!(text(&out), "a›");
    }

    #[test]
    fn emoji_at_the_cut_edge() {
        let out = cut(&["😀😀😀"], win(0, 1, 4, 6));
        assert_eq!(text(&out), "‹😀›");
        let out = cut(&["😀😀😀"], win(0, 3, 3, 6));
        assert_eq!(text(&out), "‹😀");
    }

    #[test]
    fn degenerate_windows() {
        assert_eq!(text(&cut(&["abc"], win(0, 0, 0, 3))), "");
        assert_eq!(text(&cut(&["abc"], win(0, 2, 1, 3))), "‹");
        assert_eq!(
            text(&cut(&["ab", "cd"], win(4, 0, 2, 4))),
            "abcd  ",
            "from < keep"
        );
        assert_eq!(text(&cut(&[], win(1, 1, 2, 0))), "   ");
    }

    /// Graphemes for generated lines: ASCII, CJK, emoji, a combining
    /// sequence, an ambiguous-width character.
    const ALPHABET: [&str; 7] = ["a", "b", "日", "😀", "e\u{301}", "·", " "];

    proptest::proptest! {
        #[test]
        fn windows_are_exactly_as_wide_as_asked(
            pieces in proptest::collection::vec(
                proptest::collection::vec(0..ALPHABET.len(), 0..8), 0..8),
            keep in 0u16..6,
            shift in 0u16..30,
            cols in 0u16..30,
        ) {
            let texts: Vec<String> = pieces
                .iter()
                .map(|p| p.iter().map(|&i| ALPHABET[i]).collect())
                .collect();
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            let extent = u16::try_from(str_width(&texts.concat(), false)).unwrap();
            let w = win(keep, keep + shift, cols, extent);
            let out = cut(&refs, w);
            let width: usize = out.iter().map(|p| str_width(&p.0, false)).sum();
            proptest::prop_assert_eq!(width, usize::from(keep + cols));
            // Everything shown is text of the line, a space or a marker.
            for (t, _, _) in &out {
                proptest::prop_assert!(
                    texts.iter().any(|x| x.contains(t.as_str()))
                        || t.chars().all(|c| c == ' ')
                        || GLYPHS.contains(&t.as_str()),
                    "{t:?}"
                );
            }
        }
    }
}
