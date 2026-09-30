//! Syntax highlighting with syntect and the two-face syntax and theme sets,
//! on the Oniguruma (default) or pure-Rust `fancy-regex` backend.
//!
//! * The syntax set (about 200 syntaxes, 1 MiB) is deserialised once, on
//!   first use; [`prewarm`] does that on a background thread.
//! * A grammar's regexes compile when a block in that language is first
//!   highlighted, which takes tens of milliseconds per language;
//!   [`SyntectHighlighter::warm_up`] does that on background threads.
//! * Every syntect call runs inside [`guarded`]: syntect panics when a regex
//!   fails to compile on first use, and such a block is shown as plain text.
//! * Code themes are two-face's embedded themes, matched by name ignoring
//!   case and punctuation (`one-half-dark` finds `OneHalfDark`), or
//!   `.tmTheme` files with the `tmtheme` feature.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use ::syntect::easy::HighlightLines;
use ::syntect::highlighting::{Color as SynColor, FontStyle, Style as SynStyle, Theme as SynTheme};
use ::syntect::parsing::{SyntaxReference, SyntaxSet};
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

use super::alias::{self, PLAIN};
use super::{CodeColors, Highlighter, HlBlock, HlSpan, LangId};
use crate::config::paths::looks_like_path;
use crate::config::suggest::did_you_mean;
use crate::options::CodeOptions;
use crate::panic::guarded;
use crate::style::{Attrs, Color, Rgb, Style, Underline};

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEMES: OnceLock<EmbeddedLazyThemeSet> = OnceLock::new();

/// The syntax set, loaded on first use.
fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

/// The embedded code themes, loaded on first use.
fn themes() -> &'static EmbeddedLazyThemeSet {
    THEMES.get_or_init(two_face::theme::extra)
}

/// Whether the syntax set has been loaded.
pub fn is_loaded() -> bool {
    SYNTAXES.get().is_some()
}

/// Load the syntax and theme sets on a background thread (about 3 ms), so
/// they are ready by the time the first code block is highlighted.
///
/// Returns the thread, or `None` when the sets are already loaded or no
/// thread could be started (they then load on first use).
pub fn prewarm() -> Option<JoinHandle<()>> {
    if is_loaded() {
        return None;
    }
    std::thread::Builder::new()
        .name("emde-syntaxes".into())
        .spawn(|| {
            let _ = guarded(|| {
                syntaxes();
                themes();
            });
        })
        .ok()
}

/// A code theme that could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeThemeError(String);

impl fmt::Display for CodeThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CodeThemeError {}

/// A theme name reduced to lowercase letters and digits.
fn theme_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The embedded code theme called `name`, ignoring case and punctuation.
pub fn find_code_theme(name: &str) -> Option<EmbeddedThemeName> {
    let key = theme_key(name);
    EmbeddedLazyThemeSet::theme_names()
        .iter()
        .copied()
        .find(|t| theme_key(t.as_name()) == key)
}

/// Names of the embedded code themes, for `--list-code-themes`.
pub fn code_theme_names() -> Vec<&'static str> {
    EmbeddedLazyThemeSet::theme_names()
        .iter()
        .map(|t| t.as_name())
        .collect()
}

/// Why `spec` names no embedded theme, with a suggestion.
fn unknown_theme(spec: &str) -> CodeThemeError {
    let names = code_theme_names();
    let keys: Vec<String> = names.iter().map(|n| theme_key(n)).collect();
    let key = theme_key(spec);
    let hint = did_you_mean(&key, keys.iter().map(String::as_str))
        .and_then(|k| keys.iter().position(|x| x == k))
        .and_then(|i| names.get(i))
        .map_or_else(
            || " (see `emde --list-code-themes`)".to_owned(),
            |n| format!(" (did you mean `{n}`?)"),
        );
    CodeThemeError(format!("unknown code theme `{spec}`{hint}"))
}

/// Check cheaply that a code theme name or `.tmTheme` path can be used: the
/// name exists, or the file does. [`load_code_theme`] also reads the file.
pub fn check_code_theme(spec: &str) -> Result<(), CodeThemeError> {
    if find_code_theme(spec).is_some() {
        return Ok(());
    }
    if looks_like_path(spec) {
        if !cfg!(feature = "tmtheme") {
            return Err(CodeThemeError(format!(
                "`{spec}`: loading .tmTheme files needs emde's `tmtheme` feature"
            )));
        }
        return if std::path::Path::new(spec).is_file() {
            Ok(())
        } else {
            Err(CodeThemeError(format!(
                "code theme file `{spec}` not found"
            )))
        };
    }
    Err(unknown_theme(spec))
}

/// Load a code theme: an embedded theme's name, or a `.tmTheme` path.
pub fn load_code_theme(spec: &str) -> Result<Cow<'static, SynTheme>, CodeThemeError> {
    if let Some(name) = find_code_theme(spec) {
        return guarded(|| themes().get(name))
            .map(Cow::Borrowed)
            .ok_or_else(|| CodeThemeError(format!("the embedded code theme `{spec}` is broken")));
    }
    if looks_like_path(spec) {
        return load_tmtheme(spec);
    }
    Err(unknown_theme(spec))
}

/// Load a `.tmTheme` file. It is read with emde's size cap first, because
/// syntect reads a file to its end (`/dev/zero` would never return).
#[cfg(feature = "tmtheme")]
fn load_tmtheme(path: &str) -> Result<Cow<'static, SynTheme>, CodeThemeError> {
    let cannot =
        |why: &dyn fmt::Display| CodeThemeError(format!("cannot load code theme `{path}`: {why}"));
    let bytes =
        crate::config::paths::read_bytes(std::path::Path::new(path)).map_err(|e| cannot(&e))?;
    let mut reader = std::io::Cursor::new(bytes);
    match guarded(|| ::syntect::highlighting::ThemeSet::load_from_reader(&mut reader)) {
        Some(Ok(theme)) => Ok(Cow::Owned(theme)),
        Some(Err(e)) => Err(cannot(&e)),
        None => Err(cannot(&"the file is not a valid theme")),
    }
}

#[cfg(not(feature = "tmtheme"))]
fn load_tmtheme(path: &str) -> Result<Cow<'static, SynTheme>, CodeThemeError> {
    Err(CodeThemeError(format!(
        "`{path}`: loading .tmTheme files needs emde's `tmtheme` feature"
    )))
}

/// A syntect colour as a terminal colour.
///
/// two-face's `ansi`, `base16` and `base16-256` themes encode terminal
/// palette colours the way bat does: alpha 0 means palette index `r`
/// (0–15 as ANSI colours, so the user's scheme applies), alpha 1 means the
/// terminal's default colour. Anything else is 24-bit colour.
pub(crate) fn to_color(c: SynColor) -> Color {
    match c.a {
        0 if c.r < 16 => Color::Ansi(c.r),
        0 => Color::Indexed(c.r),
        1 => Color::Default,
        _ => Color::Rgb(Rgb(c.r, c.g, c.b)),
    }
}

/// A syntect style as an emde style (foreground and font style only).
fn to_style(s: SynStyle) -> Style {
    let mut attrs = Attrs::empty();
    if s.font_style.contains(FontStyle::BOLD) {
        attrs |= Attrs::BOLD;
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        attrs |= Attrs::ITALIC;
    }
    let underline = if s.font_style.contains(FontStyle::UNDERLINE) {
        Underline::Single
    } else {
        Underline::None
    };
    Style {
        fg: to_color(s.foreground),
        attrs,
        underline,
        ..Style::PLAIN
    }
}

/// The spans of one line from syntect's ranges (which include the `\n`).
fn spans(ranges: &[(SynStyle, &str)], line_len: usize) -> Vec<HlSpan> {
    let mut out: Vec<HlSpan> = Vec::with_capacity(ranges.len());
    let mut end = 0usize;
    for (style, text) in ranges {
        end = end.saturating_add(text.len()).min(line_len);
        let end = u32::try_from(end).unwrap_or(u32::MAX);
        if end <= out.last().map_or(0, |s| s.end) {
            continue;
        }
        let style = to_style(*style);
        match out.last_mut() {
            Some(last) if last.style == style => last.end = end,
            _ => out.push(HlSpan { end, style }),
        }
    }
    out
}

/// The language a fence token selects, if any.
fn resolve_token(token: &str, aliases: &[(String, String)]) -> Option<LangId> {
    let token = alias::resolve(token, aliases);
    if token.is_empty() || token == PLAIN {
        return None;
    }
    let ss = syntaxes();
    let found = ss.find_syntax_by_token(&token)?;
    if found.name == "Plain Text" {
        return None;
    }
    let index = ss.syntaxes().iter().position(|s| std::ptr::eq(s, found))?;
    u32::try_from(index).ok().map(LangId)
}

fn syntax(lang: LangId) -> Option<&'static SyntaxReference> {
    syntaxes().syntaxes().get(usize::try_from(lang.0).ok()?)
}

/// Check that a fence token (a `[code.aliases]` target) selects a language,
/// or plain text by name (`text`). Loads the syntax set.
pub fn check_language(token: &str) -> Result<(), String> {
    if alias::resolve(token, &[]) == PLAIN {
        return Ok(());
    }
    if guarded(|| resolve_token(token, &[])).flatten().is_some() {
        return Ok(());
    }
    let words: Vec<String> = guarded(|| {
        syntaxes()
            .syntaxes()
            .iter()
            .filter(|s| !s.hidden)
            .flat_map(|s| {
                s.file_extensions
                    .iter()
                    .cloned()
                    .chain([s.name.to_lowercase()])
            })
            .chain(alias::BUILTIN.iter().map(|(from, _)| (*from).to_owned()))
            .collect()
    })
    .unwrap_or_default();
    let key = alias::normalize(token);
    let hint = did_you_mean(&key, words.iter().map(String::as_str)).map_or_else(
        || " (see `emde --list-languages`)".to_owned(),
        |s| format!(" (did you mean `{s}`?)"),
    );
    Err(format!("unknown language `{token}`{hint}"))
}

/// Code that exercises the common contexts of most grammars (comments,
/// strings, numbers, calls, blocks, markup), used to compile regexes early.
const WARM_UP_SAMPLE: &str = "#!/bin/sh\n// line comment\n/* block */ # hash comment\n\
    fn main() { let x = \"text\\n\" + 'c'; return f(42, 3.14); }\n\
    <tag attr=\"v\">text</tag>\n$var = @{ key: [1, 2] } -- end\n\tindented\n";

/// Lines longer than this (minified code) are not highlighted: syntect's
/// time grows quickly with line length. Highlighting stops at the first such
/// line, because skipping it would leave the parser in the wrong state.
const MAX_LINE_BYTES: usize = 16 * 1024;

/// Highlight a whole block with syntect (may panic inside syntect).
fn highlight_block(syntax: &SyntaxReference, theme: &SynTheme, code: &str) -> Option<HlBlock> {
    let ss = syntaxes();
    let mut h = HighlightLines::new(syntax, theme);
    let mut buf = String::new();
    let mut lines = Vec::new();
    let mut plain_from_here = false;
    for line in code.split('\n') {
        plain_from_here |= line.len() > MAX_LINE_BYTES;
        if plain_from_here {
            lines.push(Vec::new());
            continue;
        }
        buf.clear();
        buf.push_str(line);
        buf.push('\n');
        let ranges = h.highlight_line(&buf, ss).ok()?;
        lines.push(spans(&ranges, line.len()));
    }
    Some(HlBlock { lines })
}

/// A [`Highlighter`] backed by syntect and two-face.
#[derive(Clone, Debug)]
pub struct SyntectHighlighter {
    theme: Cow<'static, SynTheme>,
    /// `[code.aliases]`, checked before the built-in aliases.
    aliases: Vec<(String, String)>,
    /// Larger blocks are not highlighted.
    max_bytes: usize,
}

impl SyntectHighlighter {
    /// A highlighter for a code theme (embedded name or `.tmTheme` path)
    /// and the `[code]` options.
    pub fn new(code_theme: &str, opts: &CodeOptions) -> Result<SyntectHighlighter, CodeThemeError> {
        Ok(SyntectHighlighter::with_theme(
            load_code_theme(code_theme)?,
            opts,
        ))
    }

    /// A highlighter for an already loaded code theme.
    pub fn with_theme(theme: Cow<'static, SynTheme>, opts: &CodeOptions) -> SyntectHighlighter {
        SyntectHighlighter {
            theme,
            aliases: opts.aliases.clone(),
            max_bytes: opts.max_highlight_bytes,
        }
    }

    /// Compile the grammars of `langs` on background threads, so that
    /// highlighting them later is fast. Languages are taken in the order
    /// given (put the first blocks' first), by at most one thread per CPU.
    /// The threads only warm shared caches; joining them is optional.
    pub fn warm_up(&self, langs: &[LangId]) -> Vec<JoinHandle<()>> {
        let mut unique: VecDeque<LangId> = VecDeque::new();
        for &lang in langs {
            if !unique.contains(&lang) {
                unique.push_back(lang);
            }
        }
        let cpus = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        let threads = cpus.min(unique.len());
        let queue = Arc::new(Mutex::new(unique));
        (0..threads)
            .filter_map(|_| {
                let queue = Arc::clone(&queue);
                std::thread::Builder::new()
                    .name("emde-warm-up".into())
                    .spawn(move || {
                        while let Some(lang) = queue.lock().ok().and_then(|mut q| q.pop_front()) {
                            let _ = guarded(|| {
                                let theme = themes().get(EmbeddedThemeName::Ansi);
                                highlight_block(syntax(lang)?, theme, WARM_UP_SAMPLE)
                            });
                        }
                    })
                    .ok()
            })
            .collect()
    }
}

impl Highlighter for SyntectHighlighter {
    fn resolve(&self, token: &str) -> Option<LangId> {
        guarded(|| resolve_token(token, &self.aliases)).flatten()
    }

    fn highlight(&self, lang: LangId, code: &str) -> HlBlock {
        if code.len() <= self.max_bytes
            && let Some(block) =
                guarded(|| highlight_block(syntax(lang)?, &self.theme, code)).flatten()
        {
            return block;
        }
        HlBlock::plain(code)
    }

    fn colors(&self) -> CodeColors {
        let settings = &self.theme.settings;
        CodeColors {
            fg: settings.foreground.map(to_color),
            bg: settings.background.map(to_color),
        }
    }

    fn language_name(&self, lang: LangId) -> String {
        guarded(|| syntax(lang).map(|s| s.name.clone()))
            .flatten()
            .unwrap_or_default()
    }
}

/// A language for `--list-languages`: its name and the fence tokens that
/// select it (file extensions and built-in aliases).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Language {
    pub name: String,
    pub tokens: Vec<String>,
}

/// The visible languages, sorted by name.
pub fn languages() -> Vec<Language> {
    guarded(|| {
        let ss = syntaxes();
        let mut out: Vec<Language> = ss
            .syntaxes()
            .iter()
            .filter(|s| !s.hidden && s.name != "Plain Text")
            .map(|s| Language {
                name: s.name.clone(),
                tokens: s.file_extensions.clone(),
            })
            .collect();
        for (from, to) in alias::BUILTIN {
            let Some(target) = ss.find_syntax_by_token(to) else {
                continue;
            };
            if let Some(lang) = out.iter_mut().find(|l| l.name == target.name)
                && !lang.tokens.iter().any(|t| t.eq_ignore_ascii_case(from))
            {
                lang.tokens.push((*from).to_owned());
            }
        }
        out.sort_by_key(|l| l.name.to_lowercase());
        out
    })
    .unwrap_or_default()
}

/// Licence notices of the embedded syntaxes and themes, for `--credits`.
pub fn credits() -> String {
    guarded(|| {
        let ack = two_face::acknowledgement::listing();
        let mut out = String::new();
        for (what, list) in [("syntax", ack.for_syntaxes()), ("theme", ack.for_themes())] {
            for license in list.iter().filter(|l| l.needs_acknowledgement()) {
                let _ = writeln!(
                    out,
                    "── {what}: {} ──\n{}\n",
                    license.rel_path.display(),
                    license.text.trim_end()
                );
            }
        }
        out
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn highlighter(theme: &str) -> SyntectHighlighter {
        SyntectHighlighter::new(theme, &CodeOptions::default()).unwrap()
    }

    fn name_of(h: &SyntectHighlighter, token: &str) -> Option<String> {
        h.resolve(token).map(|l| h.language_name(l))
    }

    #[test]
    fn aliases_resolve() {
        let h = highlighter("OneHalfDark");
        let bash = "Bourne Again Shell (bash)";
        for token in [
            "console",
            "shell",
            "sh",
            "zsh",
            "ksh",
            "bash",
            "shell-session",
        ] {
            assert_eq!(name_of(&h, token).as_deref(), Some(bash), "{token}");
        }
        for token in ["json", "jsonc", "json5"] {
            assert_eq!(name_of(&h, token).as_deref(), Some("JSON"), "{token}");
        }
        assert_eq!(name_of(&h, "yml").as_deref(), Some("YAML"));
        for token in ["text", "txt", "plaintext", "mermaid", "", "no-such-lang"] {
            assert_eq!(h.resolve(token), None, "{token}");
        }
        // PowerShell is only in the Oniguruma syntax set.
        for token in ["ps", "pwsh"] {
            if cfg!(feature = "onig") {
                assert_eq!(name_of(&h, token).as_deref(), Some("PowerShell"), "{token}");
            } else {
                assert_eq!(h.resolve(token), None, "{token}");
            }
        }
        assert_eq!(name_of(&h, "Rust").as_deref(), Some("Rust"));
        assert_eq!(name_of(&h, "rust,ignore").as_deref(), Some("Rust"));
        assert_eq!(name_of(&h, "{.python}").as_deref(), Some("Python"));
    }

    #[test]
    fn every_builtin_alias_target_resolves() {
        let h = highlighter("ansi");
        for (from, to) in alias::BUILTIN {
            if *to == PLAIN {
                assert_eq!(h.resolve(from), None, "{from}");
            } else if *to == "ps1" && !cfg!(feature = "onig") {
                // PowerShell needs the Oniguruma syntax set.
                assert_eq!(h.resolve(from), None, "{from}");
            } else {
                assert!(h.resolve(from).is_some(), "{from} → {to} does not resolve");
            }
        }
    }

    #[test]
    fn user_aliases_come_first() {
        let opts = CodeOptions {
            aliases: vec![
                ("conf".into(), "ini".into()),
                ("rust".into(), "text".into()),
            ],
            ..CodeOptions::default()
        };
        let h = SyntectHighlighter::new("Nord", &opts).unwrap();
        assert_eq!(name_of(&h, "conf").as_deref(), Some("INI"));
        assert_eq!(h.resolve("rust"), None);
    }

    #[test]
    fn rust_gets_coloured_spans() {
        let h = highlighter("OneHalfDark");
        let lang = h.resolve("rust").unwrap();
        let code = "fn main() {\n    let x = \"hi\"; // greet\n}\n";
        let block = h.highlight(lang, code);
        assert_eq!(block.lines.len(), code.split('\n').count());
        for (spans, line) in block.lines.iter().zip(code.split('\n')) {
            let mut prev = 0;
            for s in spans {
                assert!(s.end > prev, "ends increase");
                assert!(line.is_char_boundary(s.end as usize));
                prev = s.end;
            }
            assert_eq!(prev as usize, line.len(), "spans cover the line: {line:?}");
        }
        let fgs: std::collections::BTreeSet<_> = block
            .lines
            .iter()
            .flatten()
            .map(|s| format!("{:?}", s.style.fg))
            .collect();
        assert!(fgs.len() >= 3, "several colours: {fgs:?}");
        assert!(
            block
                .lines
                .iter()
                .flatten()
                .all(|s| matches!(s.style.fg, Color::Rgb(_)))
        );
        assert!(
            block
                .lines
                .iter()
                .flatten()
                .all(|s| s.style.bg == Color::Default)
        );
        assert_eq!(block.lines[2].len(), 1, "`}}` is one span");
        assert!(
            block.lines[3].is_empty(),
            "the empty last line has no spans"
        );
    }

    #[test]
    fn ansi_theme_uses_palette_colours() {
        let h = highlighter("ansi");
        assert_eq!(
            h.colors(),
            CodeColors {
                fg: Some(Color::Default),
                bg: Some(Color::Default)
            }
        );
        let lang = h.resolve("rust").unwrap();
        let block = h.highlight(lang, "fn main() { let s = \"x\"; }");
        let fgs: Vec<Color> = block.lines.iter().flatten().map(|s| s.style.fg).collect();
        assert!(
            fgs.iter().any(|c| matches!(c, Color::Ansi(1..=7))),
            "{fgs:?}"
        );
        assert!(
            fgs.iter()
                .all(|c| matches!(c, Color::Ansi(_) | Color::Default)),
            "{fgs:?}"
        );
    }

    #[test]
    fn base16_256_uses_indexed_colours() {
        let h = highlighter("base16-256");
        assert_eq!(h.colors().bg, Some(Color::Ansi(0)));
        let lang = h.resolve("py").unwrap();
        let block = h.highlight(lang, "def f(x):\n    return 42  # c\n");
        let fgs: Vec<Color> = block.lines.iter().flatten().map(|s| s.style.fg).collect();
        assert!(
            fgs.iter()
                .all(|c| matches!(c, Color::Ansi(_) | Color::Indexed(_))),
            "{fgs:?}"
        );
    }

    #[test]
    fn colour_decoding() {
        let c = |r, g, b, a| to_color(SynColor { r, g, b, a });
        assert_eq!(c(1, 0, 0, 0), Color::Ansi(1));
        assert_eq!(c(15, 0, 0, 0), Color::Ansi(15));
        assert_eq!(c(16, 0, 0, 0), Color::Indexed(16));
        assert_eq!(c(0, 0, 0, 1), Color::Default);
        assert_eq!(c(1, 2, 3, 0xff), Color::Rgb(Rgb(1, 2, 3)));
        assert_eq!(c(1, 2, 3, 0x80), Color::Rgb(Rgb(1, 2, 3)));
    }

    #[test]
    fn lines_match_the_source() {
        let h = highlighter("Nord");
        let lang = h.resolve("toml").unwrap();
        for code in ["", "a = 1", "a = 1\n", "\n\n", "x = \"é\"\r\ny = 2"] {
            let block = h.highlight(lang, code);
            assert_eq!(block.lines.len(), code.split('\n').count(), "{code:?}");
        }
    }

    #[test]
    fn large_blocks_are_plain() {
        let opts = CodeOptions {
            max_highlight_bytes: 10,
            ..CodeOptions::default()
        };
        let h = SyntectHighlighter::new("Nord", &opts).unwrap();
        let lang = h.resolve("rust").unwrap();
        let block = h.highlight(lang, "fn main() { println!(\"long\"); }\nfn f() {}");
        assert_eq!(
            block,
            HlBlock::plain("fn main() { println!(\"long\"); }\nfn f() {}")
        );
        assert!(!h.highlight(lang, "fn f() {}").lines[0].is_empty());
    }

    #[test]
    fn very_long_lines_end_highlighting() {
        let h = highlighter("Nord");
        let lang = h.resolve("js").unwrap();
        let long = format!("var a = [{}];", "1,".repeat(MAX_LINE_BYTES));
        let code = format!("let x = 1;\n{long}\nlet y = 2;");
        let block = h.highlight(lang, &code);
        assert_eq!(block.lines.len(), 3);
        assert!(!block.lines[0].is_empty());
        assert!(block.lines[1].is_empty() && block.lines[2].is_empty());
    }

    #[test]
    fn unknown_language_ids_are_plain() {
        let h = highlighter("Nord");
        assert_eq!(
            h.highlight(LangId(u32::MAX), "a\nb"),
            HlBlock::plain("a\nb")
        );
        assert_eq!(h.language_name(LangId(u32::MAX)), "");
    }

    #[test]
    fn code_theme_lookup() {
        for (spec, name) in [
            ("OneHalfDark", "OneHalfDark"),
            ("onehalfdark", "OneHalfDark"),
            ("one-half-dark", "OneHalfDark"),
            ("solarized-dark", "Solarized (dark)"),
            ("Solarized (light)", "Solarized (light)"),
            ("base16_ocean_dark", "base16-ocean.dark"),
            ("1337", "1337"),
            ("catppuccin mocha", "Catppuccin Mocha"),
        ] {
            assert_eq!(
                find_code_theme(spec).map(|t| t.as_name()),
                Some(name),
                "{spec}"
            );
        }
        let keys: std::collections::BTreeSet<String> =
            code_theme_names().iter().map(|n| theme_key(n)).collect();
        assert_eq!(
            keys.len(),
            code_theme_names().len(),
            "normalised names are unique"
        );
        assert_eq!(code_theme_names().len(), 32);

        let err = load_code_theme("Nrod").unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown code theme `Nrod` (did you mean `Nord`?)"
        );
        let err = load_code_theme("zzzz").unwrap_err();
        assert!(err.to_string().contains("--list-code-themes"));
        assert!(check_code_theme("one half light").is_ok());
        assert!(check_code_theme("/no/such/file.tmTheme").is_err());
    }

    #[test]
    fn theme_colours() {
        let h = highlighter("OneHalfDark");
        assert_eq!(
            h.colors(),
            CodeColors {
                fg: Some(Color::Rgb(Rgb(0xdc, 0xdf, 0xe4))),
                bg: Some(Color::Rgb(Rgb(0x28, 0x2c, 0x34)))
            }
        );
    }

    #[cfg(feature = "tmtheme")]
    #[test]
    fn tmtheme_files_load() {
        let dir = crate::config::paths::testdir::TestDir::new("tmtheme");
        let path = dir.write(
            "Tiny.tmTheme",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>name</key><string>Tiny</string>
<key>settings</key><array>
<dict><key>settings</key><dict>
<key>foreground</key><string>#112233</string>
<key>background</key><string>#445566</string>
</dict></dict>
<dict><key>scope</key><string>keyword, storage</string><key>settings</key><dict>
<key>foreground</key><string>#ff0000</string><key>fontStyle</key><string>bold</string>
</dict></dict>
</array></dict></plist>
"#,
        );
        let spec = path.display().to_string();
        assert!(check_code_theme(&spec).is_ok());
        let h = SyntectHighlighter::new(&spec, &CodeOptions::default()).unwrap();
        assert_eq!(h.colors().fg, Some(Color::Rgb(Rgb(0x11, 0x22, 0x33))));
        let lang = h.resolve("rust").unwrap();
        let block = h.highlight(lang, "fn main() {}");
        let bold_red = Style::fg(Color::Rgb(Rgb(0xff, 0, 0))).with(Attrs::BOLD);
        assert!(
            block.lines[0].iter().any(|s| s.style == bold_red),
            "{:?}",
            block.lines[0]
        );

        let broken = dir.write("Broken.tmTheme", "<plist>not really</plist>");
        let broken = broken.display().to_string();
        assert!(SyntectHighlighter::new(&broken, &CodeOptions::default()).is_err());
        assert!(
            check_code_theme(&broken).is_ok(),
            "the quick check only sees the file"
        );
        assert!(load_code_theme(&broken).is_err());
    }

    #[cfg(all(unix, feature = "tmtheme"))]
    #[test]
    fn endless_theme_files_are_refused() {
        // syntect alone would read /dev/zero forever.
        if std::path::Path::new("/dev/zero").exists() {
            let err = load_code_theme("/dev/zero").unwrap_err();
            assert!(err.to_string().contains("larger than"), "{err}");
        }
    }

    #[test]
    fn every_syntax_highlights_a_sample_without_panicking() {
        let theme = themes().get(EmbeddedThemeName::OneHalfDark);
        let ss = syntaxes();
        let mut failed = Vec::new();
        for s in ss.syntaxes() {
            let result = std::panic::catch_unwind(|| highlight_block(s, theme, WARM_UP_SAMPLE));
            match result {
                Ok(Some(block)) => {
                    assert_eq!(
                        block.lines.len(),
                        WARM_UP_SAMPLE.split('\n').count(),
                        "{}",
                        s.name
                    );
                }
                Ok(None) => failed.push(format!("{}: error", s.name)),
                Err(_) => failed.push(format!("{}: panic", s.name)),
            }
        }
        assert!(failed.is_empty(), "{failed:?}");
        // two-face ships 220 syntaxes for Oniguruma and 213 for fancy-regex.
        let expected = if cfg!(feature = "onig") { 220 } else { 213 };
        assert_eq!(ss.syntaxes().len(), expected);
    }

    #[test]
    fn panics_become_plain_blocks() {
        // What `highlight` does when syntect panics mid-block.
        let block = guarded(|| -> Option<HlBlock> { panic!("regex string should be pre-tested") })
            .flatten()
            .unwrap_or_else(|| HlBlock::plain("a\nb"));
        assert_eq!(block, HlBlock::plain("a\nb"));
    }

    #[test]
    fn prewarm_loads_the_syntax_set() {
        if let Some(handle) = prewarm() {
            handle.join().unwrap();
        }
        assert!(is_loaded());
        assert!(prewarm().is_none(), "nothing to do once loaded");
    }

    #[test]
    fn warm_up_compiles_in_the_background() {
        let h = highlighter("Nord");
        let langs: Vec<LangId> = ["go", "go", "lua"]
            .iter()
            .filter_map(|t| h.resolve(t))
            .collect();
        let handles = h.warm_up(&langs);
        let cpus = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        assert_eq!(
            handles.len(),
            cpus.min(2),
            "at most one per distinct language"
        );
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(h.warm_up(&[]).is_empty());
    }

    #[test]
    fn warm_up_threads_are_bounded_by_the_cpus() {
        let h = highlighter("Nord");
        let cpus = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        // More distinct languages than CPUs (ids that name no syntax, so
        // nothing is compiled).
        let many: Vec<LangId> = (0..u32::try_from(cpus + 3).unwrap())
            .map(|i| LangId(u32::MAX - i))
            .collect();
        let handles = h.warm_up(&many);
        assert_eq!(handles.len(), cpus);
        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn language_listing() {
        let langs = languages();
        assert!(langs.len() > 150, "{}", langs.len());
        let bash = langs
            .iter()
            .find(|l| l.name == "Bourne Again Shell (bash)")
            .unwrap();
        assert!(
            bash.tokens.iter().any(|t| t == "console"),
            "{:?}",
            bash.tokens
        );
        assert!(langs.iter().all(|l| l.name != "Plain Text"));
        let names: Vec<String> = langs.iter().map(|l| l.name.to_lowercase()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn credits_are_listed() {
        let text = credits();
        assert!(
            text.contains("── syntax:"),
            "{}",
            &text[..text.len().min(200)]
        );
        assert!(text.contains("── theme:"));
    }
}
