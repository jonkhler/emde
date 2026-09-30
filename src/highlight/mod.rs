//! Syntax highlighting behind a small trait.
//!
//! Highlight results are an *overlay*: layout wraps code from its plain text
//! and the emitter paints [`HlBlock`] runs by byte offset, so highlighting
//! never forces a re-layout and can arrive late.
//!
//! [`syntect::SyntectHighlighter`] (feature `highlight`) is the real
//! implementation; [`PlainHighlighter`] is used without the feature and in
//! deterministic tests. [`create`] picks the right one for a code theme.

pub mod alias;
#[cfg(feature = "highlight")]
pub mod syntect;

use crate::options::CodeOptions;
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

impl HlBlock {
    /// An unhighlighted block: one empty span list per line of `code`.
    pub fn plain(code: &str) -> HlBlock {
        HlBlock {
            lines: code.split('\n').map(|_| Vec::new()).collect(),
        }
    }
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
        HlBlock::plain(code)
    }

    fn colors(&self) -> CodeColors {
        CodeColors::default()
    }

    fn language_name(&self, _lang: LangId) -> String {
        String::new()
    }
}

/// The highlighter for a code theme (an embedded theme's name or a
/// `.tmTheme` path) and the `[code]` options.
///
/// A theme that cannot be loaded falls back to two-face's `ansi` theme (the
/// terminal's own colours) and the reason is returned alongside. Without the
/// `highlight` feature this is always a [`PlainHighlighter`].
pub fn create(code_theme: &str, opts: &CodeOptions) -> (Box<dyn Highlighter>, Option<String>) {
    #[cfg(feature = "highlight")]
    {
        match syntect::SyntectHighlighter::new(code_theme, opts) {
            Ok(h) => (Box::new(h), None),
            Err(e) => {
                let fallback = syntect::SyntectHighlighter::new("ansi", opts)
                    .map(|h| Box::new(h) as Box<dyn Highlighter>)
                    .unwrap_or_else(|_| Box::new(PlainHighlighter));
                (fallback, Some(e.to_string()))
            }
        }
    }
    #[cfg(not(feature = "highlight"))]
    {
        let _ = (code_theme, opts);
        (Box::new(PlainHighlighter), None)
    }
}

/// Start loading the syntax set in the background (a no-op without the
/// `highlight` feature). Call it as soon as the document is known to
/// contain code.
pub fn prewarm() {
    #[cfg(feature = "highlight")]
    {
        let _ = syntect::prewarm();
    }
}

/// Check that a code theme name or path can be used (always fine without
/// the `highlight` feature, which ignores code themes).
pub fn check_code_theme(spec: &str) -> Result<(), String> {
    #[cfg(feature = "highlight")]
    {
        syntect::check_code_theme(spec).map_err(|e| e.to_string())
    }
    #[cfg(not(feature = "highlight"))]
    {
        let _ = spec;
        Ok(())
    }
}

/// `--list-languages`: `(language, fence tokens)` pairs, sorted by name.
pub fn list_languages() -> Vec<(String, Vec<String>)> {
    #[cfg(feature = "highlight")]
    {
        syntect::languages()
            .into_iter()
            .map(|l| (l.name, l.tokens))
            .collect()
    }
    #[cfg(not(feature = "highlight"))]
    {
        Vec::new()
    }
}

/// `--list-code-themes`: the embedded code theme names.
pub fn list_code_themes() -> Vec<&'static str> {
    #[cfg(feature = "highlight")]
    {
        syntect::code_theme_names()
    }
    #[cfg(not(feature = "highlight"))]
    {
        Vec::new()
    }
}

/// `--credits`: licence notices of the embedded syntaxes and code themes.
pub fn credits() -> String {
    #[cfg(feature = "highlight")]
    {
        syntect::credits()
    }
    #[cfg(not(feature = "highlight"))]
    {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_blocks_have_one_list_per_line() {
        assert_eq!(HlBlock::plain("").lines.len(), 1);
        assert_eq!(HlBlock::plain("a\nb\n").lines.len(), 3);
        let h = PlainHighlighter;
        assert_eq!(h.resolve("rust"), None);
        assert_eq!(h.highlight(LangId(0), "x\ny"), HlBlock::plain("x\ny"));
    }

    #[test]
    fn create_falls_back_on_bad_themes() {
        let (h, warning) = create("OneHalfDark", &CodeOptions::default());
        assert!(warning.is_none());
        let (h2, warning) = create("no such theme", &CodeOptions::default());
        if cfg!(feature = "highlight") {
            assert!(h.resolve("rust").is_some());
            assert!(warning.is_some_and(|w| w.contains("unknown code theme")));
            assert_eq!(h2.colors().fg, Some(Color::Default), "falls back to `ansi`");
            assert!(!list_code_themes().is_empty());
            assert!(!list_languages().is_empty());
            assert!(check_code_theme("nord").is_ok());
            assert!(check_code_theme("nrod").is_err());
        } else {
            assert!(warning.is_none());
            assert!(list_languages().is_empty());
            assert!(check_code_theme("anything").is_ok());
        }
        prewarm();
    }
}
