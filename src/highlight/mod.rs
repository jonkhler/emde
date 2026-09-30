//! Syntax highlighting behind a small trait.
//!
//! Highlight results are an *overlay*: layout wraps code from its plain text
//! and the emitter paints [`HlBlock`] runs by byte offset, so highlighting
//! never forces a re-layout and can arrive late.

use crate::style::{Color, Style};

/// Opaque language handle returned by [`Highlighter::resolve`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LangId(pub u32);

/// A highlighted run: bytes `[previous end, end)` of one source line get `style`
/// (only `fg`, `attrs` and `underline` are meaningful; backgrounds come from the theme).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlSpan {
    pub end: u32,
    pub style: Style,
}

/// Highlighting of one code block: one span list per source line
/// (lines split on `\n`, offsets exclude the newline, tabs not expanded).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HlBlock {
    pub lines: Vec<Vec<HlSpan>>,
}

/// Default colours of the active code theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodeColors {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
}

/// A syntax highlighter.
pub trait Highlighter: Send + Sync {
    /// Resolve a fence info-string token (its first word) to a language,
    /// after alias mapping. `None` means plain text.
    fn resolve(&self, token: &str) -> Option<LangId>;

    /// Highlight a whole code block.
    fn highlight(&self, lang: LangId, code: &str) -> HlBlock;

    /// The code theme's default colours.
    fn colors(&self) -> CodeColors;

    /// Display name of a language (for the code block label).
    fn language_name(&self, lang: LangId) -> String;
}

/// A highlighter that knows no languages (used when `highlight` is disabled
/// and for deterministic tests).
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainHighlighter;

impl Highlighter for PlainHighlighter {
    fn resolve(&self, _token: &str) -> Option<LangId> {
        None
    }

    fn highlight(&self, _lang: LangId, code: &str) -> HlBlock {
        HlBlock {
            lines: code.split('\n').map(|_| Vec::new()).collect(),
        }
    }

    fn colors(&self) -> CodeColors {
        CodeColors::default()
    }

    fn language_name(&self, _lang: LangId) -> String {
        String::new()
    }
}
