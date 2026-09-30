//! LaTeX math to Unicode text for the emde Markdown reader.
//!
//! * [`inline`] renders a formula as one line of Unicode (`α² + a/b`).
//! * [`display`] lays a formula out in two dimensions (stacked fractions,
//!   limits above/below, tall delimiters), falling back to linear text or
//!   raw TeX when it does not fit.
//!
//! Output is plain text plus *roles* per span; the caller maps roles to
//! terminal styles. Widths follow `unicode-width` (optionally treating East
//! Asian Ambiguous characters as wide).

/// How math letters are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Letters {
    /// Plain letters with the `Var` role (the caller styles them italic).
    #[default]
    Italic,
    /// Mathematical italic code points (𝑥, with ℎ for h).
    UnicodeItalic,
    /// Plain upright letters.
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
    /// Vulgar fraction characters (½) when one exists, else `a/b`.
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
    /// Mathematical bold code points (𝐱).
    Unicode,
}

/// Rendering options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MathOptions {
    pub letters: Letters,
    pub scripts: ScriptSet,
    pub fractions: Fractions,
    pub bold: Bold,
    /// Measure East Asian Ambiguous characters as two columns.
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
    /// Anything without a more specific role.
    #[default]
    Plain,
    /// Variables (letters).
    Var,
    /// Numbers.
    Num,
    /// Binary and large operators (`+`, `∑`).
    Op,
    /// Relations (`=`, `≤`, `→`).
    Rel,
    /// Function names (`sin`, `lim`).
    Func,
    /// `\text{…}` content.
    Text,
    /// Delimiters, fraction bars and radical strokes.
    Delim,
    /// Content that could not be rendered (raw TeX fallback).
    Error,
}

/// A styled run: bytes `[previous end, end)` of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MathSpan {
    pub end: u32,
    pub role: MathRole,
    /// `\mathbf` and friends (when [`Bold::Sgr`]).
    pub bold: bool,
    /// Fallback notation such as `^(…)` — shown dimmed.
    pub dim: bool,
}

/// One line of rendered math.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MathLine {
    pub text: String,
    /// Contiguous spans covering `text`.
    pub spans: Vec<MathSpan>,
    /// Byte offsets where a line break is acceptable (after top-level
    /// relations/operators); the text is otherwise atomic.
    pub breaks: Vec<u32>,
    /// Display width in terminal columns.
    pub width: u16,
    /// `false` when the formula could not be parsed and `text` is raw TeX.
    pub ok: bool,
}

/// A 2D layout: `rows.len() == height`, every row exactly `width` columns
/// wide (space padded); `baseline` is the row aligned with surrounding text.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MathBox {
    pub width: u16,
    pub height: u16,
    pub baseline: u16,
    pub rows: Vec<MathLine>,
}

/// Result of [`display`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MathDisplay {
    /// A 2D box that fits the available width and height limit.
    Box(MathBox),
    /// Linear fallback, already split into lines that fit where possible.
    Lines(Vec<MathLine>),
    /// Raw TeX (parse error or nothing fits).
    Raw(MathLine),
}

/// Render a formula as a single line of Unicode.
///
/// On parse errors the returned line has `ok == false` and contains the raw
/// TeX with the [`MathRole::Error`] role.
pub fn inline(tex: &str, opts: &MathOptions) -> MathLine {
    let _ = opts;
    raw(tex)
}

/// Render display math within `avail` columns.
pub fn display(tex: &str, opts: &MathOptions, avail: u16) -> MathDisplay {
    let _ = (opts, avail);
    MathDisplay::Raw(raw(tex))
}

/// Raw TeX as an error line (placeholder until the renderer lands).
fn raw(tex: &str) -> MathLine {
    let text = tex.trim().to_string();
    let width = unicode_width::UnicodeWidthStr::width(text.as_str());
    MathLine {
        spans: vec![MathSpan {
            end: text.len() as u32,
            role: MathRole::Error,
            bold: false,
            dim: false,
        }],
        width: u16::try_from(width).unwrap_or(u16::MAX),
        text,
        breaks: Vec::new(),
        ok: false,
    }
}
