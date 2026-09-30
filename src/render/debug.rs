//! A readable rendering of styled output, for snapshot tests.
//!
//! Every line is written as its text, with styled pieces in tags that name
//! their (downsampled) style and link:
//!
//! ```text
//! ⟦fg=#89b4fa ul link 3⟧docs⟦/⟧ and ⟦bold⟧strong⟦/⟧ text
//! ⟦grad #89b4fa→#cba6f7 0..76⟧⟦fg=#1e1e2e bold⟧ Title⟦/⟧⟦/grad⟧
//! ```
//!
//! Colours are `#rrggbb`, `idxN` (256-colour palette) or `ansiN`;
//! attributes are `bold dim italic strike reverse overline`; underlines
//! `ul` or `ul=curly` (and the like) with `ulc=` for their colour. Gradient
//! lines name the gradient once instead of a colour per column. Adjacent
//! pieces with the same style and link share a tag.

use std::fmt::Write as _;

use crate::ir::LinkId;
use crate::layout::{Fill, Layout};
use crate::style::{Attrs, Color, Rgb, Style, Underline};
use crate::term::ColorDepth;

use super::emit::{SegStyle, segments};
use super::sgr::Palette;

fn color_name(c: Color) -> String {
    match c {
        Color::Default => "default".into(),
        Color::Ansi(n) => format!("ansi{n}"),
        Color::Indexed(n) => format!("idx{n}"),
        Color::Rgb(Rgb(r, g, b)) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

/// The tag body for a style and link; empty for plain unlinked text.
fn tag(s: &Style, link: Option<LinkId>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if s.fg != Color::Default {
        parts.push(format!("fg={}", color_name(s.fg)));
    }
    if s.bg != Color::Default {
        parts.push(format!("bg={}", color_name(s.bg)));
    }
    for (a, name) in [
        (Attrs::BOLD, "bold"),
        (Attrs::DIM, "dim"),
        (Attrs::ITALIC, "italic"),
        (Attrs::STRIKE, "strike"),
        (Attrs::REVERSE, "reverse"),
        (Attrs::OVERLINE, "overline"),
    ] {
        if s.attrs.contains(a) {
            parts.push(name.into());
        }
    }
    match s.underline {
        Underline::None => {}
        Underline::Single => parts.push("ul".into()),
        Underline::Double => parts.push("ul=double".into()),
        Underline::Curly => parts.push("ul=curly".into()),
        Underline::Dotted => parts.push("ul=dotted".into()),
        Underline::Dashed => parts.push("ul=dashed".into()),
    }
    if s.underline_color != Color::Default {
        parts.push(format!("ulc={}", color_name(s.underline_color)));
    }
    if let Some(l) = link {
        parts.push(format!("link {}", l.0));
    }
    parts.join(" ")
}

/// Render a layout with style tags, downsampled to `depth`.
pub fn debug_text(layout: &Layout, depth: ColorDepth, styled_underline: bool) -> String {
    let mut palette = Palette::new(depth, styled_underline);
    let styles: Vec<Style> = layout
        .styles
        .styles()
        .iter()
        .map(|s| palette.style(s))
        .collect();
    let mut out = String::new();
    for (i, line) in layout.lines.iter().enumerate() {
        let empty = line.n == 0 && matches!(line.fill, Fill::None);
        if !empty {
            out.extend(std::iter::repeat_n(' ', usize::from(layout.indent)));
        }
        let gradient = match line.fill {
            Fill::Gradient { from, to, x0, x1 } if depth != ColorDepth::None => {
                let (f, t) = (
                    palette.color(Color::Rgb(from)),
                    palette.color(Color::Rgb(to)),
                );
                let _ = write!(out, "⟦grad {}→{} {x0}..{x1}⟧", color_name(f), color_name(t));
                true
            }
            _ => false,
        };
        let mut group: Option<(String, String)> = None; // (tag, text)
        let flush = |group: &mut Option<(String, String)>, out: &mut String| {
            if let Some((t, text)) = group.take() {
                if t.is_empty() {
                    out.push_str(&text);
                } else {
                    let _ = write!(out, "⟦{t}⟧{text}⟦/⟧");
                }
            }
        };
        let mut piece = |text: &str, style: SegStyle, link: Option<LinkId>| {
            let s = match style {
                SegStyle::Id(id) => styles.get(usize::from(id.0)).copied().unwrap_or_default(),
                SegStyle::Style(s) => palette.style(&s),
            };
            let t = tag(&s, link);
            match &mut group {
                Some((gt, gtext)) if *gt == t => gtext.push_str(text),
                _ => {
                    flush(&mut group, &mut out);
                    group = Some((t, text.to_string()));
                }
            }
        };
        if gradient {
            // Spans with their own styles; the gradient replaces backgrounds.
            for span in layout.line_spans(i) {
                piece(layout.span_text(span), SegStyle::Id(span.style), span.link);
            }
        } else {
            segments(layout, i, &mut piece);
        }
        flush(&mut group, &mut out);
        if gradient {
            out.push_str("⟦/grad⟧");
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags() {
        let s = Style::fg(Color::Rgb(Rgb(0x89, 0xb4, 0xfa)))
            .with(Attrs::BOLD)
            .underlined(Underline::Curly);
        assert_eq!(tag(&s, None), "fg=#89b4fa bold ul=curly");
        assert_eq!(tag(&Style::PLAIN, Some(LinkId(3))), "link 3");
        assert_eq!(tag(&Style::PLAIN, None), "");
        let t = Style::fg(Color::Indexed(110)).on(Color::Ansi(0));
        assert_eq!(tag(&t, None), "fg=idx110 bg=ansi0");
    }
}
