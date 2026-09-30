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
