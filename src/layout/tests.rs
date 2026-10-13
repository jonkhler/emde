//! Unit tests of whole layouts: small documents, exact lines.

use super::*;
use crate::highlight::{CodeColors, HlBlock, HlSpan, LangId, PlainHighlighter};
use crate::ir::{Anchor, HeadingId};
use crate::options::{Align, CodeStyle, H1Style, Height, When};
use crate::parse::{ParseOptions, parse};
use crate::style::{Attrs, Color, Style};
use crate::term::ColorDepth;

fn doc(md: &str) -> Document {
    parse(md, &ParseOptions::default())
}

fn plain_caps() -> Caps {
    Caps::plain()
}

fn lay(md: &str, width: u16) -> (Document, Layout) {
    lay_with(md, width, &plain_caps(), &RenderOptions::default())
}

fn lay_with(md: &str, width: u16, caps: &Caps, opts: &RenderOptions) -> (Document, Layout) {
    let d = doc(md);
    let l = layout(
        &d,
        width,
        &Theme::test(),
        caps,
        opts,
        &PlainHighlighter,
        &NoImages,
    );
    (d, l)
}

fn lines(l: &Layout) -> Vec<String> {
    (0..l.len()).map(|i| l.line_text(i)).collect()
}

/// Structural invariants every layout must satisfy.
fn check(l: &Layout, d: &Document) {
    let width = usize::from(l.width);
    for (i, line) in l.lines.iter().enumerate() {
        let text = l.line_text(i);
        let cols = crate::text::str_width(&text, false);
        assert_eq!(cols, usize::from(line.cols), "line {i} {text:?}");
        assert!(
            usize::from(l.indent) + cols <= width,
            "line {i} too wide: {text:?}"
        );
        assert!(line.cols <= l.measure, "line {i} wider than the measure");
        if let Fill::Panel { to_col, .. } = line.fill {
            assert!(to_col <= l.measure);
        }
        if i > 0 {
            assert!(
                l.lines[i - 1].pos <= line.pos,
                "position decreases at line {i}"
            );
        }
        assert!(!text.contains(['\u{ad}', '\u{1b}', '\n']), "{text:?}");
    }
    // Block ranges tile the lines.
    let mut next = 0;
    for r in &l.block_lines {
        assert_eq!(r.start, next);
        assert!(r.end >= r.start);
        next = r.end;
    }
    if !l.block_lines.is_empty() {
        assert_eq!(next as usize, l.lines.len());
    }
    assert_eq!(l.block_lines.len(), d.blocks.len());
    assert_eq!(l.heading_line.len(), d.headings.len());
    for hit in &l.link_hits {
        assert!((hit.line as usize) < l.lines.len());
        assert!(hit.cols.start < hit.cols.end);
        assert!(d.link(hit.link).is_some());
    }
}

#[test]
fn geometry_follows_the_plan() {
    let opts = RenderOptions::default();
    let tty = Caps::full();
    let pipe = Caps::plain();
    // Centred on a terminal: 100 columns at most, 2-column margins.
    assert_eq!(geometry(200, &opts, &tty), (50, 100));
    assert_eq!(geometry(80, &opts, &tty), (2, 76));
    // Left-aligned (and unindented) when piped.
    assert_eq!(geometry(80, &opts, &pipe), (0, 76));
    assert_eq!(geometry(200, &opts, &pipe), (0, 100));
    let left = RenderOptions {
        align: Align::Left,
        ..RenderOptions::default()
    };
    assert_eq!(geometry(200, &left, &tty), (2, 100));
    // Narrow terminals drop the margins first.
    assert_eq!(geometry(22, &opts, &tty), (1, 20));
    assert_eq!(geometry(10, &opts, &tty), (0, 10));
    assert_eq!(geometry(1, &opts, &tty), (0, 1));
    assert_eq!(geometry(0, &opts, &tty), (0, 1));
    let uncapped = RenderOptions {
        max_width: 0,
        ..RenderOptions::default()
    };
    assert_eq!(geometry(300, &uncapped, &pipe), (0, 296));
}

#[test]
fn paragraphs_wrap_and_blocks_are_one_blank_line_apart() {
    let (d, l) = lay("one two three four five\n\nsix", 13);
    assert_eq!(lines(&l), ["one two three", "four five", "", "six"]);
    assert_eq!(l.lines[2].kind, LineKind::Blank);
    check(&l, &d);
    // Blocks without output never double the gap.
    let md = "---\ntitle: x\n---\n\n# A";
    let (_, shown) = lay(md, 30);
    let opts = RenderOptions {
        front_matter: crate::options::FrontMatterMode::Hide,
        ..RenderOptions::default()
    };
    let (_, hidden) = lay_with(md, 30, &plain_caps(), &opts);
    assert!(shown.len() > hidden.len());
    assert_eq!(hidden.line_text(0), "A", "no gap at the top");
}

#[test]
fn positions_are_width_stable() {
    let md = "# Title\n\nalpha bravo charlie delta echo foxtrot golf hotel\n\n- one\n- two";
    let (_, narrow) = lay(md, 12);
    let (_, wide) = lay(md, 80);
    // The line showing "echo" starts at or before its position at every width.
    let find = |l: &Layout| {
        (0..l.len())
            .find(|&i| l.line_text(i).contains("echo"))
            .map(|i| l.lines[i].pos)
    };
    let (a, b) = (find(&narrow).unwrap(), find(&wide).unwrap());
    assert_eq!(a.top, b.top);
    assert!(a.off >= b.off, "the wide line starts earlier");
    // Re-anchoring finds the line showing a position: the first of the
    // lines sharing it (a heading's text, not its rule).
    let i = narrow.line_at(b);
    assert!(narrow.lines[i].pos <= b);
    assert_eq!(narrow.line_at(SrcPos::default()), 0);
    assert_eq!(narrow.line_at(a), 5, "delta echo");
    assert_eq!(narrow.line_at(SrcPos { top: 1, off: 25 }), 5);
    assert_eq!(
        wide.line_at(SrcPos { top: 9, off: 0 }),
        6,
        "past the end: the last line"
    );
}

#[test]
fn heading_lines_and_block_ranges() {
    let md = "# One\n\ntext\n\n## Two\n\nmore";
    let (d, l) = lay(md, 40);
    check(&l, &d);
    assert_eq!(l.line_text(l.heading_line[0] as usize), "One");
    assert_eq!(l.line_text(l.heading_line[1] as usize), "Two");
    assert_eq!(d.resolve_anchor("two"), Some(Anchor::Heading(HeadingId(1))));
    // Lines: One, ═══, blank | text, blank | Two, ───, blank | more.
    assert_eq!(l.block_lines, [0..3, 3..5, 5..8, 8..9]);
}

#[test]
fn list_markers_hang_and_align() {
    let md = "8. eight is a long item\n9. nine\n10. ten";
    let (d, l) = lay(md, 14);
    check(&l, &d);
    assert_eq!(
        lines(&l),
        [" 8. eight is a", "    long item", " 9. nine", "10. ten"]
    );
    let (_, l) = lay("- [x] done\n- [ ] open\n- plain", 20);
    assert_eq!(lines(&l), ["☒ done", "☐ open", "• plain"]);
}

#[test]
fn quote_bars_on_every_line() {
    let (d, l) = lay("> alpha bravo charlie delta\n>\n> > echo", 12);
    check(&l, &d);
    assert_eq!(
        lines(&l),
        [
            "▎ alpha",
            "▎ bravo",
            "▎ charlie",
            "▎ delta",
            "▎",
            "▎ ▎ echo"
        ]
    );
}

#[test]
fn deep_nesting_keeps_content_room() {
    let md = "- a\n  - b\n    - c\n      - d\n        - e\n          - f\n            - deepest words here";
    let (d, l) = lay(md, 20);
    check(&l, &d);
    let all = lines(&l);
    assert!(all.iter().any(|s| s.contains("here")), "{all:?}");
    // The innermost marker survives the cut (bullets cycle • ◦ ‣ ⁃).
    assert!(all.iter().any(|s| s.contains("‣ deepest")), "{all:?}");
}

#[test]
fn trailing_spaces_are_trimmed_without_colour() {
    let (_, l) = lay("| a | b |\n|---|---|\n| x | y |\n\n```\nx\n```", 40);
    for i in 0..l.len() {
        assert!(!l.line_text(i).ends_with(' '), "{:?}", l.line_text(i));
    }
}

#[test]
fn link_refs_follow_capabilities() {
    let md = "[a](https://a.org) and [b](https://a.org) and <https://c.org>";
    let (d, l) = lay(md, 80);
    check(&l, &d);
    assert_eq!(
        lines(&l),
        ["a[1] and b[1] and https://c.org", "", "[1]: https://a.org"]
    );
    // With OSC 8 there is nothing to number.
    let (_, l) = lay_with(md, 80, &Caps::full(), &RenderOptions::default());
    assert_eq!(l.line_text(0), "a and b and https://c.org");
    // `always` numbers them anyway; `never` never does.
    let opts = RenderOptions {
        link_refs: When::Always,
        ..RenderOptions::default()
    };
    let (_, l) = lay_with(md, 80, &Caps::full(), &opts);
    assert!(l.line_text(0).starts_with("a[1]"));
    let opts = RenderOptions {
        link_refs: When::Never,
        ..RenderOptions::default()
    };
    let (_, l) = lay_with(md, 80, &plain_caps(), &opts);
    assert_eq!(l.len(), 1);
}

#[test]
fn link_refs_are_listed_per_section() {
    let md = "# A\n\n[x](https://x.org)\n\n# B\n\n[y](https://y.org)";
    let (d, l) = lay(md, 40);
    check(&l, &d);
    let text = lines(&l).join("\n");
    let first = text.find("[1]: https://x.org").unwrap();
    let b = text.find("\nB\n").unwrap();
    let second = text.find("[2]: https://y.org").unwrap();
    assert!(first < b && b < second, "{text}");
}

#[test]
fn link_hits_cover_wrapped_fragments() {
    let md = "see [a long link text](https://example.com) here";
    let (d, l) = lay_with(md, 12, &Caps::full(), &RenderOptions::default());
    check(&l, &d);
    let hits: Vec<_> = l
        .link_hits
        .iter()
        .map(|h| (h.line, h.cols.clone()))
        .collect();
    assert!(hits.len() >= 2, "{hits:?}");
    assert!(l.link_hits.iter().all(|h| h.link == l.link_hits[0].link));
}

#[test]
fn inline_math_is_found_on_its_lines() {
    let md = "Let $x^2$ and $y$ be.\n\n> - in a quote $\\alpha$\n\n<div align=\"center\">\n\ncentred $z$\n\n</div>";
    let (d, l) = lay(md, 40);
    check(&l, &d);
    let at = |h: &MathHit| {
        let text = l.line_text(h.line as usize);
        let rest: String = text.chars().skip(usize::from(h.col)).collect();
        (h.pos.top, h.pos.off, h.len, rest)
    };
    let hits: Vec<_> = l.math_hits.iter().map(at).collect();
    assert_eq!(hits.len(), 4, "{hits:?}");
    assert_eq!((hits[0].0, hits[0].1, hits[0].2), (0, 4, 3), "x^2 at 4");
    assert!(hits[0].3.starts_with("x²"), "{hits:?}");
    assert!(hits[1].3.starts_with('y'), "{hits:?}");
    assert!(hits[2].3.starts_with('α'), "{hits:?}");
    assert!(hits[3].3.starts_with('z'), "aligned lines too: {hits:?}");
}

#[test]
fn footnote_back_links_are_flagged() {
    let md = "a[^x] b[^x]\n\n[^x]: note";
    let (d, l) = lay(md, 40);
    check(&l, &d);
    let back: Vec<_> = l.link_hits.iter().filter(|h| h.back).collect();
    assert_eq!(back.len(), 2);
    let text = lines(&l);
    assert!(text.iter().any(|t| t == "1. note ↑ ↑²"), "{text:?}");
}

/// Highlights every line in bold.
struct Bold;

impl Highlighter for Bold {
    fn resolve(&self, token: &str) -> Option<LangId> {
        (token == "rust").then_some(LangId(1))
    }
    fn highlight(&self, _: LangId, code: &str) -> HlBlock {
        let bold = Style::PLAIN.with(Attrs::BOLD);
        HlBlock {
            lines: code
                .split('\n')
                .map(|l| {
                    vec![HlSpan {
                        end: l.len() as u32,
                        style: bold,
                    }]
                })
                .collect(),
        }
    }
    fn colors(&self) -> CodeColors {
        CodeColors::default()
    }
    fn language_name(&self, _: LangId) -> String {
        "Rust".into()
    }
}

#[test]
fn code_lines_carry_overlay_positions() {
    let d = doc("```rust\nfn main() {}\n\tx\n```");
    let opts = RenderOptions::default();
    let l = layout(
        &d,
        40,
        &Theme::test(),
        &Caps::full(),
        &opts,
        &Bold,
        &NoImages,
    );
    check(&l, &d);
    let code: Vec<_> = l
        .lines
        .iter()
        .filter_map(|line| match line.kind {
            LineKind::Code { block, line, byte0 } => Some((block, line, byte0)),
            _ => None,
        })
        .collect();
    assert_eq!(code, [(0, 0, 0), (0, 1, 0)]);
    // The tab is expanded, and the highlight moved past it.
    let hl = l.code[0].hl.as_ref().unwrap();
    assert_eq!(hl.lines[1][0].end, 5);
    assert!(lines(&l).iter().any(|s| s.contains("     x")));
}

#[test]
fn a_panicking_highlighter_is_contained() {
    struct Boom;
    impl Highlighter for Boom {
        fn resolve(&self, _: &str) -> Option<LangId> {
            Some(LangId(0))
        }
        fn highlight(&self, _: LangId, _: &str) -> HlBlock {
            panic!("regex compile failed")
        }
        fn colors(&self) -> CodeColors {
            CodeColors::default()
        }
        fn language_name(&self, _: LangId) -> String {
            String::new()
        }
    }
    let d = doc("```x\ncode\n```");
    let opts = RenderOptions::default();
    let l = layout(
        &d,
        40,
        &Theme::test(),
        &Caps::full(),
        &opts,
        &Boom,
        &NoImages,
    );
    assert_eq!(l.code.len(), 1);
    assert!(l.code[0].hl.is_none());
    assert!(lines(&l).iter().any(|s| s.contains("code")));
}

#[test]
fn code_styles_by_depth() {
    let md = "```\nx\n```";
    let (_, panel) = lay_with(md, 20, &Caps::full(), &RenderOptions::default());
    assert!(matches!(panel.lines[0].fill, Fill::Panel { .. }));
    let caps16 = Caps {
        color: ColorDepth::Ansi16,
        ..Caps::full()
    };
    let (_, frame) = lay_with(md, 20, &caps16, &RenderOptions::default());
    assert!(frame.line_text(0).starts_with('┌'));
    let mut opts = RenderOptions::default();
    opts.code.style = CodeStyle::Gutter;
    let (_, gutter) = lay_with(md, 20, &caps16, &opts);
    assert_eq!(lines(&gutter), ["▎ x"]);
}

#[test]
fn h1_bars_by_depth() {
    let md = "# Title";
    let fill_of = |depth: ColorDepth| {
        let caps = Caps {
            color: depth,
            ..Caps::full()
        };
        let (_, l) = lay_with(md, 30, &caps, &RenderOptions::default());
        l.lines[0].fill
    };
    assert!(matches!(
        fill_of(ColorDepth::TrueColor),
        Fill::Gradient { .. }
    ));
    assert!(matches!(fill_of(ColorDepth::Ansi256), Fill::Panel { .. }));
    assert!(matches!(fill_of(ColorDepth::Ansi16), Fill::Panel { .. }));
    // No escapes: the text over a double rule.
    let (_, l) = lay(md, 20);
    assert_eq!(lines(&l), ["Title", "════════════════════"]);
    // Plain h1s are styled text only.
    let mut opts = RenderOptions::default();
    opts.heading.h1 = H1Style::Plain;
    let (_, l) = lay_with(md, 30, &Caps::full(), &opts);
    assert_eq!(l.lines[0].fill, Fill::None);
    // Gradients can be switched off.
    let opts = RenderOptions {
        gradients: When::Never,
        ..RenderOptions::default()
    };
    let (_, l) = lay_with(md, 30, &Caps::full(), &opts);
    assert!(matches!(l.lines[0].fill, Fill::Panel { .. }));
}

/// Found in the plan audit: `heading.h1 = "plain"` was `"underline"`.
#[test]
fn plain_h1s_drop_the_underline() {
    use crate::style::{StylePatch, Underline};
    let patch = StylePatch {
        underline: Some(Underline::Curly),
        ..StylePatch::default()
    };
    let theme = Theme::from_patches(
        "t",
        crate::theme::Variant::Dark,
        std::collections::BTreeMap::new(),
        Rgb(0, 0, 0),
        Rgb(30, 30, 30),
        "",
        &[(crate::theme::Element::H1, patch)],
        &[],
    );
    let underline = |h1: H1Style| {
        let mut opts = RenderOptions::default();
        opts.heading.h1 = h1;
        let d = doc("# Title");
        let l = layout(
            &d,
            30,
            &theme,
            &Caps::full(),
            &opts,
            &PlainHighlighter,
            &NoImages,
        );
        l.styles.get(l.line_spans(0)[0].style).underline
    };
    assert_eq!(underline(H1Style::Underline), Underline::Curly);
    assert_eq!(underline(H1Style::Plain), Underline::None);
}

#[test]
fn reverse_bar_at_16_colours() {
    let caps = Caps {
        color: ColorDepth::Ansi16,
        ..Caps::full()
    };
    let (_, l) = lay_with("# T", 30, &caps, &RenderOptions::default());
    let span = l.line_spans(0)[0];
    let s = l.styles.get(span.style);
    assert!(s.attrs.contains(Attrs::BOLD | Attrs::REVERSE), "{s:?}");
    assert_eq!(s.bg, Color::Default);
}

#[test]
fn figures_reserve_their_size() {
    struct Sized;
    impl ImageSizer for Sized {
        fn cells(&self, _: ImageId, max_cols: u16, _: u16) -> Option<(u16, u16)> {
            Some((max_cols.min(10), 4))
        }
    }
    let d = doc("![Alt text](a.png \"Title\")");
    let opts = RenderOptions::default();
    let l = layout(
        &d,
        30,
        &Theme::test(),
        &plain_caps(),
        &opts,
        &PlainHighlighter,
        &Sized,
    );
    check(&l, &d);
    assert_eq!(l.images.len(), 1);
    let p = l.images[0];
    assert_eq!((p.cols, p.rows, p.line), (10, 4, 0));
    assert_eq!(p.col, 8, "centred in 26 columns");
    let rows = l
        .lines
        .iter()
        .filter(|line| matches!(line.kind, LineKind::Image { .. }))
        .count();
    assert_eq!(rows, 4);
    assert_eq!(l.line_text(0).trim(), "┌────────┐");
    assert!(l.line_text(2).contains("▣ Alt"), "{:?}", l.line_text(2));
    assert_eq!(l.line_text(4).trim(), "Title", "caption below the box");
    // Without a size: a one-line alt box, no placement.
    let (_, l) = lay("![Alt text](a.png)", 30);
    assert!(l.images.is_empty());
    assert_eq!(lines(&l), ["        ▣ Alt text"], "centred in 26 columns");
}

#[test]
fn figure_height_caps() {
    struct Tall;
    impl ImageSizer for Tall {
        fn cells(&self, _: ImageId, max_cols: u16, max_rows: u16) -> Option<(u16, u16)> {
            Some((max_cols, max_rows.saturating_add(50)))
        }
    }
    let d = doc("![x](a.png)");
    let mut opts = RenderOptions::default();
    opts.images.max_height = Height::Rows(5);
    let l = layout(
        &d,
        30,
        &Theme::test(),
        &plain_caps(),
        &opts,
        &PlainHighlighter,
        &Tall,
    );
    assert_eq!(l.images[0].rows, 5);
    opts.images.max_height = Height::Percent(50);
    let caps = Caps {
        size: Some((30, 40)),
        ..plain_caps()
    };
    let l = layout(
        &d,
        30,
        &Theme::test(),
        &caps,
        &opts,
        &PlainHighlighter,
        &Tall,
    );
    assert_eq!(l.images[0].rows, 20);
}

#[test]
fn alignment_centres_lines() {
    let (d, l) = lay("<div align=\"center\">\n\nhi there\n\n</div>", 20);
    check(&l, &d);
    assert_eq!(lines(&l), ["      hi there"]);
    let (_, l) = lay("<p align=\"right\">end</p>", 20);
    assert_eq!(lines(&l), ["                 end"]);
}

#[test]
fn tables_fall_back_to_records() {
    let md = "| a | b | c | d | e |\n|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 |";
    let (d, l) = lay(md, 20);
    check(&l, &d);
    assert_eq!(lines(&l), ["a: 1", "b: 2", "c: 3", "d: 4", "e: 5"]);
    let (_, l) = lay(md, 40);
    assert!(l.line_text(0).starts_with('╭'));
    assert!(l.lines.iter().all(|line| line.kind == LineKind::Table));
}

#[test]
fn tables_wrap_cells_and_align() {
    let md = "| left | right |\n|:-----|------:|\n| a b c d e f | 1 |";
    let (d, l) = lay(md, 20);
    check(&l, &d);
    let t = lines(&l);
    assert!(t.iter().any(|s| s.ends_with("     1 │")), "{t:?}");
}

#[test]
fn inline_pills_and_markers_by_depth() {
    let md = "run `x` now, press <kbd>Q</kbd>";
    let (_, plain) = lay(md, 80);
    assert_eq!(plain.line_text(0), "run `x` now, press [Q]");
    let (_, full) = lay_with(md, 80, &Caps::full(), &RenderOptions::default());
    assert_eq!(full.line_text(0), "run  x  now, press  Q ");
    let caps16 = Caps {
        color: ColorDepth::Ansi16,
        ..Caps::full()
    };
    let (_, c16) = lay_with(md, 80, &caps16, &RenderOptions::default());
    assert_eq!(c16.line_text(0), "run x now, press [Q]");
}

#[test]
fn ascii_decorations() {
    let opts = RenderOptions {
        ascii: true,
        ..RenderOptions::default()
    };
    let md =
        "- a\n\n> q\n\n---\n\n| x |\n|---|\n| y |\n\n[^1]\n\n[^1]: n\n\n# T\n\n## U\n\n```\nc\n```";
    let (d, l) = lay_with(md, 20, &plain_caps(), &opts);
    check(&l, &d);
    for s in lines(&l) {
        assert!(s.is_ascii(), "{s:?}");
    }
}

#[test]
fn nothing_escapes_the_measure() {
    let long = "x".repeat(300);
    let md = format!(
        "# {long}\n\n{long}\n\n`{long}`\n\n| {long} |\n|---|\n| y |\n\n> - {long}\n\n```\n{long}\n```\n\n日本語{long}"
    );
    for width in [1, 2, 3, 5, 9, 24] {
        for caps in [plain_caps(), Caps::full()] {
            let (d, l) = lay_with(&md, width, &caps, &RenderOptions::default());
            check(&l, &d);
        }
    }
}

#[test]
fn empty_documents() {
    let (d, l) = lay("", 40);
    assert!(l.is_empty());
    check(&l, &d);
    let (d, l) = lay("\n\n\n", 40);
    assert!(l.is_empty());
    check(&l, &d);
}

#[test]
fn dump_is_readable() {
    let (_, l) = lay("# T\n\ntext", 20);
    let dump = l.dump();
    assert!(dump.contains("|T|"), "{dump}");
    assert!(dump.contains("blank"), "{dump}");
}

#[test]
fn raw_html_is_shown_dim() {
    let opts = ParseOptions {
        html: crate::options::HtmlMode::Raw,
        ..ParseOptions::default()
    };
    let d = parse(
        "<div class=\"x\">\nblock\n</div>\n\ninline <b>tag</b>",
        &opts,
    );
    let caps = Caps::full();
    let l = layout(
        &d,
        40,
        &Theme::test(),
        &caps,
        &RenderOptions::default(),
        &PlainHighlighter,
        &NoImages,
    );
    check(&l, &d);
    let text = lines(&l);
    assert_eq!(text[0], "<div class=\"x\">");
    assert!(text.iter().any(|t| t == "inline <b>tag</b>"), "{text:?}");
    let html = l.styles.get(l.line_spans(0)[0].style);
    assert_eq!(html.fg, Theme::test().style(crate::theme::Element::Html).fg);
}

#[test]
fn pills_stay_with_opening_punctuation() {
    // Width 10: "see (" fits, but "(" must not end a line before the pill.
    let (_, l) = lay_with("see (`code`)", 10, &Caps::full(), &RenderOptions::default());
    let text = lines(&l);
    assert!(
        text.iter().all(|t| !t.trim_end().ends_with('(')),
        "{text:?}"
    );
    // After a space the pill may start a line.
    let (_, l) = lay_with("see a `code`", 8, &Caps::full(), &RenderOptions::default());
    assert_eq!(lines(&l), ["see a", " code "]);
}

// ----- regressions found in review ----------------------------------------------

/// Columns of line `i` before its first non-space character.
fn lead(l: &Layout, i: usize) -> usize {
    let text = l.line_text(i);
    text.chars().take_while(|&c| c == ' ').count()
}

#[test]
fn an_empty_item_of_a_loose_list_keeps_its_gap() {
    let (d, l) = lay("- a\n\n-\n- b", 40);
    check(&l, &d);
    assert_eq!(lines(&l), ["• a", "", "•", "", "• b"]);
    // Tight lists stay tight.
    let (_, l) = lay("- a\n-\n- b", 40);
    assert_eq!(lines(&l), ["• a", "•", "• b"]);
}

#[test]
fn an_empty_heading_takes_no_line() {
    let (d, l) = lay("# One\n\n###\n\ntext after", 30);
    check(&l, &d);
    assert_eq!(l.heading_line.len(), 2);
    assert_eq!(l.line_text(l.heading_line[0] as usize), "One");
    assert_eq!(l.heading_line[1], u32::MAX, "nothing was shown for it");
}

#[test]
fn code_blocks_keep_their_shape_in_aligned_containers() {
    let code = "```rust\nsome code\n```";
    let md = format!("<div align=\"center\">\n\n{code}\n\n</div>");
    let caps16 = Caps {
        color: ColorDepth::Ansi16,
        ..Caps::full()
    };
    for caps in [Caps::full(), caps16] {
        let (d, centred) = lay_with(&md, 40, &caps, &RenderOptions::default());
        check(&centred, &d);
        let (_, plain) = lay_with(code, 40, &caps, &RenderOptions::default());
        // Exactly the rows (text and panel) of the block on its own.
        assert_eq!(lines(&centred), lines(&plain), "{:?}", caps.color);
        let fills = |l: &Layout| l.lines.iter().map(|line| line.fill).collect::<Vec<_>>();
        assert_eq!(fills(&centred), fills(&plain));
    }
}

#[test]
fn a_centred_h2_keeps_its_heavy_rule_under_the_text() {
    for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
        let caps = Caps {
            color: depth,
            ..Caps::full()
        };
        let (d, l) = lay_with(
            "<h2 align=\"center\">Features</h2>",
            40,
            &caps,
            &RenderOptions::default(),
        );
        check(&l, &d);
        let text = l.line_text(0);
        let rule = l.line_text(1);
        let start = lead(&l, 0);
        assert_eq!(text.trim(), "Features");
        let heavy: Vec<usize> = rule
            .chars()
            .enumerate()
            .filter(|&(_, c)| c == '━')
            .map(|(i, _)| i)
            .collect();
        assert_eq!(heavy.first(), Some(&start), "{depth:?} {rule:?}");
        assert_eq!(heavy.len(), "Features".len());
        assert_eq!(rule.chars().count(), 36, "the rule spans the width");
    }
    // Left-aligned headings keep the heavy part at the start.
    let (_, l) = lay_with("## Features", 40, &Caps::full(), &RenderOptions::default());
    assert!(l.line_text(1).starts_with("━━━━━━━━─"));
}

#[test]
fn frame_headers_fit_any_width() {
    let caps16 = Caps {
        color: ColorDepth::Ansi16,
        ..Caps::full()
    };
    let long = "l".repeat(70_000);
    for md in [
        "```rust title=\"a-long-title.rs\"\nx\n```".to_string(),
        "```rust\nx\n```".to_string(),
        format!("```{long}\nx\n```"),
        format!("```rust title=\"{long}\"\nx\n```"),
    ] {
        for width in 1..=40 {
            for caps in [plain_caps(), caps16.clone(), Caps::full()] {
                let (d, l) = lay_with(&md, width, &caps, &RenderOptions::default());
                check(&l, &d);
                let first = l.line_text(0);
                if first.starts_with('┌') {
                    assert!(first.ends_with('┐'), "{width}: {first:?}");
                    let bottom = l.line_text(l.len() - 1);
                    assert_eq!(
                        first.chars().count(),
                        bottom.chars().count(),
                        "{width}: {first:?}"
                    );
                }
            }
        }
    }
    // With room, a dash separates the title from the label.
    let (_, l) = lay("```rust title=\"hello.rs\"\nx\n```", 24);
    assert_eq!(l.line_text(0), "┌─ hello.r ─ rust ─┐");
}

#[test]
fn only_panels_have_a_background() {
    let md = "```diff\n-a\n+b\n```";
    let caps16 = Caps {
        color: ColorDepth::Ansi16,
        ..Caps::full()
    };
    let (_, l) = lay_with(md, 30, &caps16, &RenderOptions::default());
    for span in &l.spans {
        assert_eq!(l.styles.get(span.style).bg, Color::Default, "{span:?}");
    }
    let mut opts = RenderOptions::default();
    opts.code.style = CodeStyle::Gutter;
    let (_, l) = lay_with(md, 30, &Caps::full(), &opts);
    for span in &l.spans {
        assert_eq!(l.styles.get(span.style).bg, Color::Default, "{span:?}");
    }
}

#[test]
fn diff_lines_stay_apart_at_every_depth() {
    let md = "```diff\n-removed\n+added\n```";
    for depth in [
        ColorDepth::TrueColor,
        ColorDepth::Ansi256,
        ColorDepth::Ansi16,
    ] {
        let caps = Caps {
            color: depth,
            ..Caps::full()
        };
        let (_, l) = lay_with(md, 30, &caps, &RenderOptions::default());
        let style_of = |needle: &str| {
            let i = (0..l.len())
                .find(|&i| l.line_text(i).contains(needle))
                .unwrap();
            let span = l
                .line_spans(i)
                .iter()
                .find(|s| l.span_text(s).contains(needle))
                .copied()
                .unwrap();
            let mut palette = crate::render::sgr::Palette::new(depth, true);
            palette.style(l.styles.get(span.style))
        };
        assert_ne!(style_of("removed"), style_of("added"), "{depth:?}");
    }
}

#[test]
fn link_references_come_before_the_footnotes() {
    let md = "a [link](https://a.org) and a note[^1].\n\n[^1]: With [another](https://b.org).";
    let (d, l) = lay(md, 40);
    check(&l, &d);
    let text = lines(&l).join("\n");
    let first = text.find("[1]: https://a.org").unwrap();
    let section = text.find("── Footnotes").unwrap();
    let second = text.find("[2]: https://b.org").unwrap();
    assert!(first < section && section < second, "{text}");
}

#[test]
fn links_are_numbered_when_no_escapes_can_link_them() {
    let caps = Caps {
        hyperlinks: true,
        ..Caps::plain()
    };
    let (_, l) = lay_with("[a](https://a.org)", 40, &caps, &RenderOptions::default());
    assert_eq!(lines(&l), ["a[1]", "", "[1]: https://a.org"]);
}

#[test]
fn many_runs_with_url_breaks_compose_in_linear_time() {
    // Every run used to scan every extra break of the paragraph.
    let md = "`c` https://x.org/a/b?c=d&e=f ".repeat(20_000);
    let t = std::time::Instant::now();
    let (d, l) = lay(&md, 80);
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
    assert!(l.len() > 1000);
    // The URL break points still apply, and "https://" stays with its host.
    let (_, small) = lay("`c` https://x.org/a/b?c=d", 14);
    assert_eq!(lines(&small), ["`c`", "https://x.org/", "a/b?c=d"]);
    drop(d);
}

/// Found in real READMEs: `https://` alone at the end of a line, a URL
/// broken at a hyphen (`fd-` / `find`) or mid-way while it would fit the
/// next line whole.
#[test]
fn urls_move_whole_else_break_at_their_slashes() {
    let md = "The full comparison table is at https://example.org/compare/ for you.";
    let (d, l) = lay(md, 44);
    check(&l, &d);
    assert_eq!(
        lines(&l),
        [
            "The full comparison table is at",
            "https://example.org/compare/ for you."
        ]
    );
    // References list their URL whole too.
    let md = "See the [package](https://crates.io/crates/fd-find) or the comparison at \
              https://beyondgrep.com/feature-comparison/ today.";
    let (d, l) = lay(md, 40);
    check(&l, &d);
    let text = lines(&l);
    assert!(text.iter().all(|t| !t.ends_with("https://")), "{text:?}");
    assert!(text.iter().any(|t| t.ends_with("fd-find")), "{text:?}");
    assert!(text.iter().all(|t| !t.ends_with("fd-")), "{text:?}");
    // A URL wider than the line breaks after its slashes, within the line.
    let (d, l) = lay("https://example.org/a-very-long-path-segment-indeed/x", 24);
    check(&l, &d);
    assert_eq!(
        lines(&l),
        [
            "https://example.org/",
            "a-very-long-path-",
            "segment-indeed/x"
        ]
    );
}

/// Found in review: link text with a URL among other words wraps as words
/// (it became URL pieces that could not break, and one block).
#[test]
fn link_text_with_a_url_among_words_wraps_as_words() {
    let md =
        "See [the manual at https://example.com/manual for details](https://example.com/manual).";
    let (d, l) = lay(md, 30);
    check(&l, &d);
    assert_eq!(
        lines(&l)[..3],
        [
            "See the manual at",
            "https://example.com/manual",
            "for details[1]."
        ]
    );
    let (_, l) = lay(md, 80);
    assert!(
        lines(&l)[0].starts_with("See the manual at https://"),
        "{:?}",
        lines(&l)
    );
}

/// Found in real READMEs: a reference number alone on a line after link
/// text ending in `)` or `.`, and image chips split inside their label.
#[test]
fn reference_numbers_and_chips_stay_whole() {
    let md = "Debian since 2021 [Debian 11 (Bullseye)](https://a.org/x).";
    for width in 40..=46 {
        let (d, l) = lay(md, width);
        check(&l, &d);
        let text = lines(&l);
        assert!(
            text.iter()
                .take_while(|t| !t.is_empty())
                .all(|t| !t.starts_with("[1]")),
            "{width}: {text:?}"
        );
    }
    // A row of badges breaks between them, not inside "Packaging status"
    // (or between CJK characters).
    let md = "[![Build status](b.svg)](https://b.org) [![Packaging status](p.svg)](https://p.org) \
              [![繁體中文](t.png)](https://t.org)";
    // From 25 columns on, the measure (21) holds the widest chip.
    for width in 25..=44 {
        let (d, l) = lay(md, width);
        check(&l, &d);
        let text = lines(&l);
        for chip in [
            "▣ Build status[1]",
            "▣ Packaging status[2]",
            "▣ 繁體中文[3]",
        ] {
            assert!(
                text.iter().any(|t| t.contains(chip)),
                "{chip:?} split at {width}: {text:?}"
            );
        }
    }
    // A chip wider than the line still wraps like words, within the line.
    let (d, l) = lay(
        "[![A very long description of a picture](x.png)](https://x.org) b",
        16,
    );
    check(&l, &d);
    assert!(
        lines(&l)
            .iter()
            .all(|t| crate::text::str_width(t, false) <= 16)
    );
}

#[test]
fn links_showing_their_url_get_no_number() {
    let md = "See [https://example.com/](https://example.com), [me@x.org](mailto:me@x.org) \
              and [docs](https://d.org).";
    let (d, l) = lay(md, 80);
    check(&l, &d);
    assert_eq!(
        lines(&l),
        [
            "See https://example.com/, me@x.org and docs[1].",
            "",
            "[1]: https://d.org"
        ]
    );
    assert!(inline::shows_url(" https://a.b ", "https://a.b/"));
    assert!(!inline::shows_url("", "https://a.b"));
    assert!(!inline::shows_url("a.b", "https://a.b"));
}

/// Found in real READMEs (badge rows in HTML): whitespace at the end of a
/// link stayed in it, so the number came after the space and glued to
/// the next word (`▣ Test [1]▣ Coverage[2]`, `Two [3]and more.`).
#[test]
fn whitespace_at_the_end_of_a_link_leaves_it() {
    let md = "<p>\n<a href=\"https://a.example/ci\">\n  <img src=\"ci.svg\" alt=\"Test\">\n</a>\n\
              <a href=\"https://a.example/cov\">\n  <img src=\"cov.svg\" alt=\"Coverage\">\n</a>\n</p>\n\n\
              [Two ](https://a.example/y) and more.\n";
    let (d, l) = lay(md, 60);
    check(&l, &d);
    let text = lines(&l);
    assert_eq!(text[0], "▣ Test[1] ▣ Coverage[2]", "{text:?}");
    assert_eq!(text[2], "Two[3] and more.", "{text:?}");
    // The space is not part of the link (nor underlined).
    let crate::ir::Block::Para(p) = &d.blocks[1] else {
        panic!("{:#?}", d.blocks)
    };
    let space = p.text.find(' ').unwrap();
    let run = p.runs.iter().find(|r| r.end as usize > space).unwrap();
    assert_eq!(run.link, None, "{:?}", p.runs);
}

// ---------------------------------------------------------------------------
// The pager's layouts: collapsed details, wide blocks
// ---------------------------------------------------------------------------

fn lay_interactive(md: &str, width: u16, opts: &RenderOptions, it: Interactive<'_>) -> Layout {
    layout_with(
        &doc(md),
        width,
        &Theme::test(),
        &plain_caps(),
        opts,
        &PlainHighlighter,
        &NoImages,
        it,
    )
}

/// Pager layouts: what [`check`] says, except that lines of wide blocks
/// (and only those) may be wider than the measure.
fn check_interactive(l: &Layout) {
    for (i, line) in l.lines.iter().enumerate() {
        let text = l.line_text(i);
        assert_eq!(crate::text::str_width(&text, false), usize::from(line.cols));
        if l.wide_at(i).is_none() {
            assert!(line.cols <= l.measure, "line {i} {text:?}");
        }
        if i > 0 {
            assert!(l.lines[i - 1].pos <= line.pos, "line {i}");
        }
    }
    for w in &l.wide {
        assert!(
            w.start < w.end && w.cols > l.measure && w.x0 < l.measure,
            "{w:?}"
        );
    }
}

const DETAILS: &str = "Before.\n\n<details>\n<summary>More <b>here</b></summary>\n\n\
                       ## Inside\n\nHidden text.\n\n<details>\n<summary>Inner</summary>\n\n\
                       Deeper.\n\n</details>\n\n</details>\n\nAfter.\n\n<details open>\n\
                       <summary>Open</summary>\n\nShown.\n\n</details>\n";

#[test]
fn details_collapse_to_their_summary() {
    let opts = RenderOptions::default();
    let collapsed = Interactive {
        hscroll: true,
        details: Some(Folds::default()),
    };
    let l = lay_interactive(DETAILS, 40, &opts, collapsed);
    check_interactive(&l);
    let text = lines(&l);
    assert_eq!(
        text,
        [
            "Before.",
            "",
            "▸ More here (6 lines)",
            "",
            "After.",
            "",
            "▾ Open",
            "  Shown.",
        ],
        "{text:#?}"
    );
    assert_eq!(l.details.len(), 3);
    let (outer, inner, open) = (l.details[0], l.details[1], l.details[2]);
    // The heading (two lines), a gap, the text, a gap, the inner summary.
    assert!(!outer.open && outer.line == 2 && outer.rows == 1 && outer.body_lines == 6);
    assert_eq!(outer.col, 0);
    assert_eq!(inner.line, u32::MAX, "inside a collapsed block");
    assert_eq!(inner.parent, 0);
    assert!(open.open && open.open_attr && open.line == 6);
    // The heading inside is not shown, but known to be in block 0.
    assert_eq!(l.heading_line, [u32::MAX]);
    assert_eq!(l.heading_details, [0]);
    assert_eq!(l.heading_folds(HeadingId(0)), [0]);
    assert_eq!(l.summary_at(2), Some(0));
    assert_eq!(l.summary_at(3), None);
    // Positions after the block are the same as when it is expanded.
    let expanded = lay_interactive(DETAILS, 40, &opts, Interactive::default());
    let after = |l: &Layout| {
        let i = (0..l.len()).find(|&i| l.line_text(i) == "After.").unwrap();
        l.lines[i].pos
    };
    assert_eq!(after(&l), after(&expanded));
    let hidden = expanded.lines[(0..expanded.len())
        .find(|&i| expanded.line_text(i).contains("Hidden"))
        .unwrap()]
    .pos;
    assert_eq!(l.folded_at(hidden), [0]);
    assert_eq!(l.line_at(hidden), 2, "hidden text maps to the summary");
    assert!(outer.holds(hidden));
    // Stream mode shows everything, as before.
    let text = lines(&expanded);
    assert!(text.contains(&"▾ More here".to_owned()), "{text:#?}");
    assert!(text.contains(&"    Deeper.".to_owned()), "{text:#?}");
    assert_eq!(expanded.details[1].line as usize, 8);
}

#[test]
fn toggled_details_open_and_close() {
    let opts = RenderOptions::default();
    // Open the outer one: the inner one stays collapsed.
    let toggled = [0];
    let it = Interactive {
        hscroll: false,
        details: Some(Folds {
            open: false,
            toggled: &toggled,
        }),
    };
    let l = lay_interactive(DETAILS, 40, &opts, it);
    let text = lines(&l);
    assert!(text.contains(&"▾ More here".to_owned()), "{text:#?}");
    assert!(text.contains(&"  ▸ Inner (1 line)".to_owned()), "{text:#?}");
    assert!(!text.iter().any(|t| t.contains("Deeper")));
    assert_eq!(l.folded_chain(1), [1]);
    assert!(l.heading_line[0] != u32::MAX);
    // Expanded by default; toggling `open` blocks closes them.
    let toggled = [2];
    let it = Interactive {
        hscroll: false,
        details: Some(Folds {
            open: true,
            toggled: &toggled,
        }),
    };
    let text = lines(&lay_interactive(DETAILS, 40, &opts, it));
    assert!(text.contains(&"▸ Open (1 line)".to_owned()), "{text:#?}");
    assert!(text.contains(&"    Deeper.".to_owned()), "{text:#?}");
}

#[test]
fn collapsed_details_in_lists_and_quotes() {
    let md = "- item\n\n  <details><summary>In a list</summary>\n\n  hidden\n\n  </details>\n- next\n\n\
              > <details><summary>In a quote</summary>\n>\n> - hidden\n> - more\n>\n> </details>\n";
    let it = Interactive {
        hscroll: false,
        details: Some(Folds::default()),
    };
    let l = lay_interactive(md, 40, &RenderOptions::default(), it);
    check_interactive(&l);
    let text = lines(&l);
    assert!(
        text.contains(&"  ▸ In a list (1 line)".to_owned()),
        "{text:#?}"
    );
    assert!(
        text.contains(&"▎ ▸ In a quote (2 lines)".to_owned()),
        "{text:#?}"
    );
    assert!(!text.iter().any(|t| t.contains("hidden")), "{text:#?}");
    assert_eq!(l.details[1].col, 2, "after the quote bar");
}

#[test]
fn unwrapped_code_is_a_wide_block_in_the_pager() {
    let mut opts = RenderOptions::default();
    opts.code.wrap = false;
    let long = "x".repeat(70);
    let md = format!("- item\n\n  ```\n  short\n  {long}\n  ```\n");
    // Stream mode: cut with ›.
    let (_, l) = lay_with(&md, 40, &plain_caps(), &opts);
    assert!(l.wide.is_empty() && l.narrowed);
    assert!(lines(&l).iter().any(|t| t.contains('›')));
    // The pager: the whole line, in a block recorded as wide.
    let it = Interactive {
        hscroll: true,
        details: None,
    };
    let l = lay_interactive(&md, 40, &opts, it);
    check_interactive(&l);
    assert!(!l.narrowed);
    assert_eq!(l.wide.len(), 1);
    let w = l.wide[0];
    assert_eq!(w.x0, 2, "after the list indentation");
    assert!(lines(&l).iter().any(|t| t.contains(&long)));
    assert_eq!(w.max_shift(l.measure), w.cols - l.measure);
    let row = (0..l.len())
        .find(|&i| l.line_text(i).contains(&long))
        .unwrap();
    assert_eq!(l.shift_at(row, 5), 5);
    assert_eq!(l.shift_at(row, 999), w.max_shift(l.measure));
    assert_eq!(l.shift_at(0, 5), 0, "not in the block");
    // Columns through the window.
    assert_eq!(l.shown_col(row, 1, 5), Some(1), "the prefix stays");
    assert_eq!(l.shown_col(row, 10, 5), Some(5));
    assert_eq!(l.shown_col(row, 4, 5), None, "shifted out");
    assert_eq!(l.shown_col(row, l.measure + 5, 5), None, "beyond the edge");
    assert_eq!(l.layout_col(row, 5, 5), 10);
    assert_eq!(l.layout_col(row, 1, 5), 1);
    // Code that fits is no wide block.
    let l = lay_interactive("```\nshort\n```\n", 40, &opts, it);
    assert!(l.wide.is_empty());
}

#[test]
fn wide_tables_scroll_or_become_records() {
    let head: Vec<String> = (1..=12).map(|i| format!("Column {i}")).collect();
    let md = format!(
        "| {} |\n|{}\n| {} |\n",
        head.join(" | "),
        "---|".repeat(12),
        (1..=12)
            .map(|i| format!("v{i}"))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let it = Interactive {
        hscroll: true,
        details: None,
    };
    let l = lay_interactive(&md, 40, &RenderOptions::default(), it);
    check_interactive(&l);
    assert_eq!(l.wide.len(), 1);
    let w = l.wide[0];
    assert_eq!((w.start, w.end), (0, 5), "borders, header, rule, row");
    assert!(lines(&l)[1].contains("Column 12"));
    // Every row of the grid is as wide as the block.
    for i in w.start..w.end {
        assert_eq!(l.lines[i as usize].cols, w.cols);
    }
    // `overflow = "records"`, and stream mode: records.
    let mut opts = RenderOptions::default();
    opts.tables.overflow = crate::options::TableOverflow::Records;
    let l = lay_interactive(&md, 40, &opts, it);
    assert!(l.wide.is_empty());
    assert!(
        lines(&l).iter().any(|t| t.starts_with("Column 1: v1")),
        "{:#?}",
        lines(&l)
    );
    let (_, l) = lay(&md, 40);
    assert!(l.wide.is_empty() && l.narrowed);
}

#[test]
fn collapsed_details_first_in_a_list_item_keep_the_marker() {
    let md = "- <details><summary>First thing</summary>\n\n  hidden\n\n  </details>\n- second\n";
    let it = Interactive {
        hscroll: false,
        details: Some(Folds::default()),
    };
    let l = lay_interactive(md, 40, &RenderOptions::default(), it);
    check_interactive(&l);
    assert_eq!(lines(&l), ["• ▸ First thing (1 line)", "", "• second"]);
    assert_eq!(l.details[0].col, 2);
    // Expanded: the body under it, the next item after.
    let toggled = [0];
    let it = Interactive {
        hscroll: false,
        details: Some(Folds {
            open: false,
            toggled: &toggled,
        }),
    };
    let l = lay_interactive(md, 40, &RenderOptions::default(), it);
    assert_eq!(lines(&l), ["• ▾ First thing", "    hidden", "", "• second"]);
}

#[test]
fn nested_details_in_a_quote_in_a_list() {
    let md = "- > <details><summary>Outer</summary>\n  >\n  > <details><summary>Inner</summary>\n  >\n  > \
              ## Deep\n  >\n  > text\n  >\n  > </details>\n  >\n  > </details>\n\nAfter.\n";
    let it = Interactive {
        hscroll: false,
        details: Some(Folds::default()),
    };
    let l = lay_interactive(md, 40, &RenderOptions::default(), it);
    check_interactive(&l);
    assert_eq!(lines(&l)[0], "• ▎ ▸ Outer (1 line)", "{:#?}", lines(&l));
    assert_eq!(
        l.heading_folds(HeadingId(0)),
        [0, 1],
        "both hide the heading"
    );
    // Both open: everything shows, positions unchanged.
    let toggled = [0, 1];
    let open = Interactive {
        hscroll: false,
        details: Some(Folds {
            open: false,
            toggled: &toggled,
        }),
    };
    let o = lay_interactive(md, 40, &RenderOptions::default(), open);
    check_interactive(&o);
    assert!(
        lines(&o).iter().any(|t| t.contains("text")),
        "{:#?}",
        lines(&o)
    );
    let last = |l: &Layout| l.lines.last().map(|x| x.pos);
    assert_eq!(last(&l), last(&o));
    assert!(o.heading_folds(HeadingId(0)).is_empty());
}

#[test]
fn thirty_column_tables_scroll() {
    let n = 30;
    let row = |f: &dyn Fn(usize) -> String| {
        format!("| {} |\n", (1..=n).map(f).collect::<Vec<_>>().join(" | "))
    };
    let md = format!(
        "{}|{}\n{}{}",
        row(&|i| format!("Column {i}")),
        "---|".repeat(n),
        row(&|i| format!("{i} 日本 😀")),
        row(&|i| format!("[l{i}](https://example.com/{i})")),
    );
    let it = Interactive {
        hscroll: true,
        details: None,
    };
    let l = lay_interactive(&md, 80, &RenderOptions::default(), it);
    check_interactive(&l);
    assert_eq!(l.wide.len(), 1);
    let w = l.wide[0];
    assert!(w.cols > 300 && w.cols < MAX_WIDE, "{w:?}");
    // Every row and border is as wide as the block: the borders line up
    // whatever the shift.
    for i in w.start..w.end {
        assert_eq!(
            l.lines[i as usize].cols,
            w.cols,
            "{}",
            l.line_text(i as usize)
        );
    }
    // The links, and their numbered references below (no OSC 8 here).
    assert_eq!(l.link_hits.len(), 2 * n);
}

#[test]
fn borderless_wide_tables_with_empty_rows() {
    let mut opts = RenderOptions::default();
    opts.glyphs.table = crate::options::TableBorder::None;
    let cols = 14;
    let md = format!(
        "| {} |\n|{}\n| {} |\n",
        (1..=cols)
            .map(|i| format!("Head {i}"))
            .collect::<Vec<_>>()
            .join(" | "),
        "---|".repeat(cols),
        vec![" "; cols].join(" | "),
    );
    let it = Interactive {
        hscroll: true,
        details: None,
    };
    let l = lay_interactive(&md, 40, &opts, it);
    check_interactive(&l);
    assert_eq!(l.wide.len(), 1, "{}", l.dump());
}

#[test]
fn empty_details_have_no_count() {
    let it = Interactive {
        hscroll: false,
        details: Some(Folds::default()),
    };
    let l = lay_interactive(
        "<details><summary>Nothing</summary>\n</details>\n",
        40,
        &RenderOptions::default(),
        it,
    );
    assert_eq!(lines(&l), ["▸ Nothing"]);
    assert_eq!(l.details[0].body_lines, 0);
}

#[test]
fn links_in_collapsed_details_get_no_reference() {
    // Plain output numbers links; a collapsed body's links are not shown.
    let md = "[seen](https://a.example) and [twice](https://b.example)\n\n<details><summary>S</summary>\n\n\
              [hidden](https://c.example) and [again](https://b.example)\n\n</details>\n\n\
              [after](https://d.example)\n";
    let it = Interactive {
        hscroll: false,
        details: Some(Folds::default()),
    };
    let l = lay_interactive(md, 60, &RenderOptions::default(), it);
    let text = lines(&l).join("\n");
    assert!(text.contains("[1]: https://a.example"), "{text}");
    assert!(text.contains("[2]: https://b.example"), "{text}");
    assert!(text.contains("[3]: https://d.example"), "{text}");
    assert!(!text.contains("c.example"), "{text}");
}
