//! Helpers shared by the layout and rendering tests: fixture loading, a
//! deterministic highlighter, and one-call layout and rendering.

#![allow(dead_code, unreachable_pub)]

use emde::highlight::{CodeColors, Highlighter, HlBlock, HlSpan, LangId};
use emde::ir::Document;
use emde::layout::{Layout, NoImages, layout};
use emde::options::RenderOptions;
use emde::parse::{ParseOptions, parse_source};
use emde::source::{Origin, Source};
use emde::style::{Attrs, Color, Rgb, Style};
use emde::term::Caps;
use emde::theme::Theme;

/// The fixture documents, by name.
pub const FIXTURES: &[&str] = &[
    "kitchen-sink",
    "headings",
    "lists",
    "quotes-alerts",
    "code",
    "tables",
    "footnotes",
    "html",
    "math",
    "links",
    "cjk-emoji",
    "regressions",
    "llm-style",
    "llm-math",
    "readme-html",
    "edge-cases",
];

/// The widths every fixture is laid out at.
pub const WIDTHS: [u16; 4] = [24, 40, 80, 120];

/// The Markdown of a fixture.
pub fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/md/{name}.md", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Parse Markdown the way the program does (through `Source`).
pub fn parse(md: &str) -> Document {
    let source = Source::from_bytes(md.as_bytes().to_vec(), Origin::Memory);
    parse_source(&source, &ParseOptions::default())
}

/// Lay out with the frozen test theme, the fake highlighter and no images.
pub fn lay_out(doc: &Document, width: u16, caps: &Caps, opts: &RenderOptions) -> Layout {
    lay_out_themed(doc, width, &Theme::test(), caps, opts)
}

/// Lay out with `theme`, the fake highlighter and no images.
pub fn lay_out_themed(
    doc: &Document,
    width: u16,
    theme: &Theme,
    caps: &Caps,
    opts: &RenderOptions,
) -> Layout {
    layout(doc, width, theme, caps, opts, &FakeHighlighter, &NoImages)
}

/// A highlighter with fixed colours for a few languages: keywords, strings,
/// numbers and comments. Deterministic, so snapshots only change with
/// layout or rendering.
#[derive(Clone, Copy, Debug, Default)]
pub struct FakeHighlighter;

const KEYWORDS: &[&str] = &[
    "fn", "let", "pub", "use", "match", "if", "else", "for", "in", "return", "def", "import",
    "const", "while",
];

fn keyword() -> Style {
    Style::fg(Color::Rgb(Rgb(0xcb, 0xa6, 0xf7))).with(Attrs::BOLD)
}

fn string() -> Style {
    Style::fg(Color::Rgb(Rgb(0xa6, 0xe3, 0xa1)))
}

fn number() -> Style {
    Style::fg(Color::Rgb(Rgb(0xfa, 0xb3, 0x87)))
}

fn comment() -> Style {
    Style::fg(Color::Rgb(Rgb(0x6c, 0x70, 0x86))).with(Attrs::ITALIC)
}

impl FakeHighlighter {
    fn line(lang: u32, line: &str) -> Vec<HlSpan> {
        let comment_start = match lang {
            2 | 4 => "#",
            _ => "//",
        };
        let mut spans: Vec<HlSpan> = Vec::new();
        let mut push = |end: usize, style: Style| {
            let end = end as u32;
            match spans.last_mut() {
                Some(last) if last.style == style => last.end = end,
                Some(last) if last.end >= end => {}
                _ => spans.push(HlSpan { end, style }),
            }
        };
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let rest = &line[i..];
            let c = bytes[i];
            if rest.starts_with(comment_start) {
                push(line.len(), comment());
                break;
            } else if c == b'"' {
                let close = rest[1..].find('"').map_or(line.len(), |j| i + j + 2);
                push(close, string());
                i = close;
            } else if c.is_ascii_alphabetic() || c == b'_' {
                let end = rest
                    .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                    .map_or(line.len(), |j| i + j);
                let style = if KEYWORDS.contains(&&line[i..end]) {
                    keyword()
                } else {
                    Style::PLAIN
                };
                push(end, style);
                i = end;
            } else if c.is_ascii_digit() {
                let end = rest
                    .find(|ch: char| !ch.is_ascii_digit())
                    .map_or(line.len(), |j| i + j);
                push(end, number());
                i = end;
            } else {
                let end = i + rest.chars().next().map_or(1, char::len_utf8);
                push(end, Style::PLAIN);
                i = end;
            }
        }
        spans
    }
}

impl Highlighter for FakeHighlighter {
    fn resolve(&self, token: &str) -> Option<LangId> {
        match token {
            "rust" | "rs" => Some(LangId(1)),
            "python" | "py" => Some(LangId(2)),
            "js" | "javascript" => Some(LangId(3)),
            "sh" | "console" | "bash" => Some(LangId(4)),
            _ => None,
        }
    }

    fn highlight(&self, lang: LangId, code: &str) -> HlBlock {
        HlBlock {
            lines: code.split('\n').map(|l| Self::line(lang.0, l)).collect(),
        }
    }

    fn colors(&self) -> CodeColors {
        CodeColors::default()
    }

    fn language_name(&self, lang: LangId) -> String {
        match lang.0 {
            1 => "Rust",
            2 => "Python",
            3 => "JavaScript",
            _ => "Shell",
        }
        .to_string()
    }
}
