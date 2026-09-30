//! Text measurement (grapheme widths) and line breaking.
//!
//! * [`sanitize`] — control characters → visible control pictures.
//! * [`width`] — display width per grapheme cluster, with an ASCII fast path.
//! * [`wrap`] — greedy UAX #14 line breaking with atoms, extra break points,
//!   hard breaks and soft hyphens, plus [`wrap::split_runs`] to cut a styled
//!   run list at the computed lines.
//! * [`tabs`] — tab expansion for code lines.
//! * [`linebreak`] — a fast path for UAX #14 break opportunities in
//!   printable ASCII.

pub mod linebreak;
pub mod sanitize;
pub mod tabs;
pub mod width;
pub mod wrap;

use std::borrow::Cow;

pub use sanitize::sanitize;
pub use tabs::expand_tabs;
pub use width::{grapheme_width, str_width};
pub use wrap::{
    Constraints, Line, Piece, WrapOptions, Wrapper, break_opportunities, split_runs, wrap,
    wrap_with_breaks,
};

/// Remove soft hyphens (`U+00AD`), which must never reach the terminal
/// (terminals disagree on their width). Borrows when there are none.
pub fn strip_soft_hyphens(s: &str) -> Cow<'_, str> {
    if s.contains(wrap::SOFT_HYPHEN) {
        Cow::Owned(s.replace(wrap::SOFT_HYPHEN, ""))
    } else {
        Cow::Borrowed(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_soft_hyphens() {
        assert!(matches!(
            strip_soft_hyphens("plain"),
            Cow::Borrowed("plain")
        ));
        assert_eq!(strip_soft_hyphens("hy\u{ad}phen\u{ad}"), "hyphen");
    }
}
