//! LaTeX math to Unicode text for the emde Markdown reader.
//!
//! * [`inline`] renders a formula as one line of Unicode (`α² + a/b`).
//! * [`display()`] lays a formula out in two dimensions (stacked fractions,
//!   limits above/below, tall delimiters), falling back to wrapped linear
//!   text when it does not fit.
//!
//! Formulas that do not parse come back as raw TeX with the
//! [`MathRole::Error`] role.
//!
//! Output is plain text plus *roles* per span; the caller maps roles to
//! terminal styles. Widths follow `unicode-width` (optionally treating East
//! Asian Ambiguous characters as wide).
//!
//! # Pipeline
//!
//! 1. `prepass` rewrites what pulldown-latex cannot parse (`\tag`,
//!    `\operatorname*`, a bare top-level `\\`, …) and caps the input size.
//! 2. `adapter` turns pulldown-latex's event stream into the owned `ast`.
//!    Any parser error sends the formula to the raw fallback.
//! 3. `linear` renders one line; `display` lays out 2D `boxes` and runs the
//!    fallback ladder. Both share `spacing`, `style` and `tables` (whose
//!    Unicode data `cargo xtask gen` generates into `src/gen/`).
//!
//! # Guarantees
//!
//! [`inline`] and [`display()`] never panic, whatever the input. Every
//! [`MathLine`]'s spans are contiguous and cover its text, and its `width`
//! is the text's width in columns. Every [`MathBox`] row is exactly `width`
//! columns wide and `baseline < height`. Panics inside pulldown-latex are
//! caught and shown as raw TeX, but the process panic hook runs before
//! unwinding, so a caller with its own hook should call these functions
//! inside a guarded section (emde uses `panic::guarded`).

mod adapter;
mod ast;
mod boxes;
mod display;
#[path = "gen/mod.rs"]
mod generated;
mod linear;
mod prepass;
mod spacing;
mod style;
mod tables;
mod width;

use ast::Formula;

/// How math letters are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Letters {
    /// Plain letters with the `Var` role (the caller styles them italic).
    #[default]
    Italic,
    /// Mathematical italic code points (𝑥, with ℎ for h). They are already
    /// slanted, so they carry [`MathRole::Plain`] rather than `Var`.
    UnicodeItalic,
    /// Plain upright letters, with [`MathRole::Plain`].
    Plain,
}

/// Which Unicode super/subscript characters may be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ScriptSet {
    /// Characters with good font coverage (68 superscripts, ~40 subscripts).
    #[default]
    Safe,
    /// Also Unicode 14–18 additions (superscript q, capital C/F/Q/S, …).
    Full,
}

/// How digit/digit fractions are shown inline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Fractions {
    /// Vulgar fraction characters (½) when one exists, `⅟` before a
    /// longer denominator (`⅟16`), else `a/b`.
    #[default]
    Vulgar,
    /// Always `a/b`.
    Slash,
}

/// How `\mathbf` / `\boldsymbol` are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Bold {
    /// The `bold` flag on spans (the caller uses SGR bold).
    #[default]
    Sgr,
    /// Mathematical bold code points (𝐱) where Unicode has them; other
    /// characters keep the `bold` flag.
    Unicode,
}

/// Rendering options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MathOptions {
    /// How letters are shown.
    pub letters: Letters,
    /// Which Unicode super/subscripts may be used.
    pub scripts: ScriptSet,
    /// How digit/digit fractions are shown inline.
    pub fractions: Fractions,
    /// How bold math is shown.
    pub bold: Bold,
    /// Measure East Asian Ambiguous characters as two columns. Display math
    /// is then linear: box drawing and bracket pieces are ambiguous too.
    pub ambiguous_wide: bool,
    /// Maximum height of a 2D display box before falling back to linear.
    pub max_height: u16,
}

impl Default for MathOptions {
    fn default() -> Self {
        MathOptions {
            letters: Letters::Italic,
            scripts: ScriptSet::Safe,
            fractions: Fractions::Vulgar,
            bold: Bold::Sgr,
            ambiguous_wide: false,
            max_height: 12,
        }
    }
}

/// The semantic role of a piece of rendered math.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum MathRole {
    /// Anything without a more specific role: upright letters and symbols,
    /// punctuation, spaces and padding.
    #[default]
    Plain,
    /// Variables (letters shown italic, see [`Letters`]).
    Var,
    /// Numbers.
    Num,
    /// Binary and large operators (`+`, `∑`).
    Op,
    /// Relations (`=`, `≤`, `→`).
    Rel,
    /// Function names (`sin`, `lim`).
    Func,
    /// `\text{…}` content (and equation tags).
    Text,
    /// Delimiters, fraction bars and radical strokes.
    Delim,
    /// Content that could not be rendered (raw TeX fallback).
    Error,
}

/// A styled run: bytes `[previous end, end)` of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MathSpan {
    /// Byte offset just past the span.
    pub end: u32,
    /// What the span is.
    pub role: MathRole,
    /// `\mathbf` and friends (when [`Bold::Sgr`]).
    pub bold: bool,
    /// Fallback notation such as `^(…)` — shown dimmed.
    pub dim: bool,
}

/// One line of rendered math.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MathLine {
    /// The rendered text: no control characters, no line breaks.
    pub text: String,
    /// Contiguous spans covering `text`.
    pub spans: Vec<MathSpan>,
    /// Byte offsets where a line break is acceptable (after top-level
    /// relations/operators); the text is otherwise atomic. Each offset is
    /// where the next line would start; the space before it belongs to the
    /// line that ends there. Raw TeX may break after any space.
    pub breaks: Vec<u32>,
    /// Display width in terminal columns.
    pub width: u16,
    /// `false` when the formula could not be parsed and `text` is raw TeX.
    pub ok: bool,
}

/// A 2D layout: `rows.len() == height`, every row exactly `width` columns
/// wide (space padded); `baseline` is the row aligned with surrounding text.
///
/// Rows are padded on the right: a row's text is its content followed by
/// plain spaces up to `width` columns (so `row.width == width` for every
/// row). Trimming the trailing spaces gives the ragged form; leading spaces
/// are part of the layout and must be kept.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MathBox {
    /// Columns of every row.
    pub width: u16,
    /// Number of rows (at least 1).
    pub height: u16,
    /// The row that lines up with the text around the formula.
    pub baseline: u16,
    /// The rows, top to bottom; they carry no break hints.
    pub rows: Vec<MathLine>,
}

/// Result of [`display()`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MathDisplay {
    /// A 2D box that fits the available width and height limit.
    Box(MathBox),
    /// Linear fallback, already split into lines that fit where possible.
    Lines(Vec<MathLine>),
    /// Raw TeX: the formula did not parse, or is longer than 4 KiB.
    Raw(MathLine),
}

/// Render a formula as a single line of Unicode.
///
/// On parse errors the returned line has `ok == false` and contains the raw
/// TeX with the [`MathRole::Error`] role.
///
/// ```
/// use emde_math::{MathOptions, MathRole, inline};
///
/// let line = inline(r"\alpha^2 + \frac{a}{b}", &MathOptions::default());
/// assert_eq!(line.text, "α² + a/b");
/// assert_eq!(line.width, 8);
/// assert_eq!(line.spans[0].role, MathRole::Var);
/// // A line may break after the `+ `: the byte offset of `a`.
/// assert_eq!(line.breaks, [7]);
/// assert_eq!(&line.text[7..], "a/b");
///
/// let raw = inline(r"\frac{a}", &MathOptions::default());
/// assert!(!raw.ok);
/// assert_eq!(raw.text, r"\frac{a}");
/// ```
pub fn inline(tex: &str, opts: &MathOptions) -> MathLine {
    match parse(tex) {
        Some(formula) => linear::render(&formula, opts),
        None => raw(tex, opts),
    }
}

/// Render display math within `avail` columns.
///
/// The result is a 2D [`MathDisplay::Box`] when one fits `avail` columns and
/// [`MathOptions::max_height`] rows (a formula too wide for one row may be
/// split before its top-level relations), else [`MathDisplay::Lines`] with
/// the linear form wrapped at top-level operators (a piece with no break
/// that fits stays too wide). Formulas that do not parse are
/// [`MathDisplay::Raw`]. A `\tag` makes the box `avail` columns wide, with
/// the formula centred and the tag flush right.
///
/// ```
/// use emde_math::{MathDisplay, MathOptions, display};
///
/// let opts = MathOptions::default();
/// let MathDisplay::Box(b) = display(r"\frac{a+b}{c}", &opts, 80) else {
///     panic!("expected a 2D box");
/// };
/// let rows: Vec<&str> = b.rows.iter().map(|r| r.text.as_str()).collect();
/// assert_eq!(rows, ["a+b", "───", " c "]);
/// assert_eq!((b.width, b.height, b.baseline), (3, 3, 1));
///
/// // Too narrow for the 2D form: the linear form, wrapped.
/// let MathDisplay::Lines(lines) = display(r"\frac{a+b}{c} + d", &opts, 2) else {
///     panic!("expected lines");
/// };
/// assert_eq!(lines[0].text, "(a+b)/c +");
/// ```
pub fn display(tex: &str, opts: &MathOptions, avail: u16) -> MathDisplay {
    match parse(tex) {
        Some(formula) => display::render(&formula, opts, avail),
        None => MathDisplay::Raw(raw(tex, opts)),
    }
}

/// Pre-pass and parse; `None` for anything that must be shown raw.
fn parse(tex: &str) -> Option<Formula> {
    let prepared = prepass::prepare(tex).ok()?;
    let body = adapter::parse(&prepared.tex).ok()?;
    Some(Formula {
        body,
        tag: prepared.tag,
    })
}

/// Raw TeX as an error line: whitespace collapsed to single spaces (a line
/// break may follow each), control characters replaced.
fn raw(tex: &str, opts: &MathOptions) -> MathLine {
    let text = width::sanitize(tex).trim().to_string();
    let end = u32::try_from(text.len()).unwrap_or(u32::MAX);
    let breaks = text
        .match_indices(' ')
        .filter_map(|(i, _)| u32::try_from(i + 1).ok())
        .collect();
    MathLine {
        spans: if text.is_empty() {
            Vec::new()
        } else {
            vec![MathSpan {
                end,
                role: MathRole::Error,
                bold: false,
                dim: false,
            }]
        },
        width: width::to_u16(width::str_width(&text, opts.ambiguous_wide)),
        text,
        breaks,
        ok: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_errors_are_raw() {
        let line = inline(r"\frac{a}", &MathOptions::default());
        assert!(!line.ok);
        assert_eq!(line.text, r"\frac{a}");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].role, MathRole::Error);
        match display(r"x^", &MathOptions::default(), 80) {
            MathDisplay::Raw(line) => assert_eq!(line.text, "x^"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn raw_text_is_sanitised_and_breakable() {
        let line = inline("  \\unknown  a\n\tb\u{1b} ", &MathOptions::default());
        assert_eq!(line.text, "\\unknown a b\u{FFFD}");
        assert_eq!(line.breaks, [9, 11]);
        assert_eq!(line.width, 13);
    }

    #[test]
    fn oversized_input_is_raw() {
        let tex = "x+".repeat(prepass::MAX_INPUT);
        assert!(!inline(&tex, &MathOptions::default()).ok);
    }

    #[test]
    fn empty_formulas() {
        let line = inline("", &MathOptions::default());
        assert!(line.ok && line.text.is_empty() && line.spans.is_empty());
        match display("  ", &MathOptions::default(), 80) {
            MathDisplay::Box(b) => {
                assert_eq!((b.width, b.height, b.baseline), (0, 1, 0));
                assert_eq!(b.rows.len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn inline_tags_follow_the_formula() {
        let line = inline(r"E = mc^2 \tag{1}", &MathOptions::default());
        assert_eq!(line.text, "E = mc²  (1)");
    }
}
