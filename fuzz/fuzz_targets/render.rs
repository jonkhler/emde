//! The whole stream pipeline: bytes → `Source` (lossy UTF-8, sanitised) →
//! parse → layout at two widths → plain and styled output, the styled
//! output once with figures as boxes and once with block-glyph images.
//!
//! Input: four option bytes, then the document.
//!
//! | byte | meaning |
//! |---|---|
//! | 0 | the width, 1–200 |
//! | 1 | bit 0 ASCII decorations, 1 wide ambiguous characters, 2 no code wrapping, 3 line numbers, 4 no `max_width`, 5–7 the terminal |
//! | 2 | bits 0–1 display math, 2–3 inline math, 4–5 HTML mode, 6–7 link references |
//! | 3 | the second width, 1–200 |
//!
//! Checked:
//! * every line of the layout is as wide as it says, and fits the width;
//! * no rendered line is wider than the width (plain or styled);
//! * rendered bytes hold only emde's own escapes (SGR, OSC 8), each OSC 8
//!   link closed on its line with a URI that is encoded, at most 2 KiB and
//!   of no scheme that runs code, and no control characters: nothing the
//!   document contains reaches the terminal as an escape;
//! * line positions never decrease and the blocks' line ranges tile the
//!   layout; link hit boxes, heading lines and figure boxes point at lines
//!   that exist and agree with them; `line_at` finds each line's position;
//! * laying out at another width and back gives the same layout.

#![no_main]

use std::sync::OnceLock;

use emde::config::{self, LoadOptions};
use emde::gfx::raster::RasterCell;
use emde::highlight::{CodeColors, Highlighter, HlBlock, HlSpan, LangId};
use emde::ir::{Document, ImageId};
use emde::layout::{ImageSizer, Layout, LineKind, Placement, layout};
use emde::options::{DisplayMath, HtmlMode, InlineMath, RenderOptions, When};
use emde::parse::{ParseOptions, parse_source};
use emde::render::{Emitter, ImageRows, RenderConfig, RowContent, plain_text, to_bytes};
use emde::source::{Origin, Source};
use emde::style::{Attrs, Color, Rgb, Style};
use emde::term::{Caps, ColorDepth};
use emde::text::str_width;
use emde::theme::Theme;
use emde_fuzz::{check_escapes, header};
use libfuzzer_sys::fuzz_target;

fuzz_target!(init: emde_fuzz::init(), |data: &[u8]| {
    let ([w1, flags, modes, w2], body) = header::<4>(data);
    let opts = options(flags, modes);
    let caps = terminal(flags >> 5);
    let theme = theme(caps.color);
    let source = Source::from_bytes(body.to_vec(), Origin::Memory);
    let doc = parse_source(&source, &ParseOptions::from(&opts));
    let w1 = width(w1);
    let first = check_width(&doc, w1, &theme, &caps, &opts);
    let _ = check_width(&doc, width(w2), &theme, &caps, &opts);
    let again = lay_out(&doc, w1, &theme, &caps, &opts);
    assert_eq!(first.lines, again.lines, "layout depends on an earlier width");
    assert_eq!(first.spans, again.spans);
    assert_eq!(first.text, again.text);
});

/// 1–200 columns.
fn width(b: u8) -> u16 {
    1 + u16::from(b) % 200
}

/// Render options from the option bytes.
fn options(flags: u8, modes: u8) -> RenderOptions {
    let bit = |n: u8| flags & (1 << n) != 0;
    let mut opts = RenderOptions {
        ascii: bit(0),
        ambiguous_wide: bit(1),
        ..RenderOptions::default()
    };
    opts.code.wrap = !bit(2);
    opts.code.line_numbers = bit(3);
    if bit(4) {
        opts.max_width = 0;
    }
    opts.math.display =
        [DisplayMath::TwoD, DisplayMath::Linear, DisplayMath::Raw][usize::from(modes & 3) % 3];
    opts.math.inline = [InlineMath::Unicode, InlineMath::Ascii, InlineMath::Raw]
        [usize::from((modes >> 2) & 3) % 3];
    opts.html =
        [HtmlMode::Subset, HtmlMode::Strip, HtmlMode::Raw][usize::from((modes >> 4) & 3) % 3];
    opts.link_refs = [When::Auto, When::Always, When::Never][usize::from(modes >> 6) % 3];
    opts.math.opts.ambiguous_wide = opts.ambiguous_wide;
    opts
}

/// One of eight terminals.
fn terminal(n: u8) -> Caps {
    let full = Caps::full();
    match n % 8 {
        0 => Caps::plain(),
        1 => full,
        2 => Caps {
            color: ColorDepth::Ansi256,
            ..full
        },
        3 => Caps {
            color: ColorDepth::Ansi16,
            hyperlinks: false,
            styled_underline: false,
            ..full
        },
        4 => Caps {
            color: ColorDepth::Mono,
            ..full
        },
        5 => Caps {
            is_tty: false,
            ..full
        },
        6 => Caps {
            background: Some(Rgb(0xef, 0xf1, 0xf5)),
            ..full
        },
        _ => Caps {
            color: ColorDepth::None,
            hyperlinks: false,
            ..Caps::plain()
        },
    }
}

/// The theme emde would use at a colour depth (built once per depth).
fn theme(depth: ColorDepth) -> Theme {
    static THEMES: OnceLock<Vec<(ColorDepth, Theme)>> = OnceLock::new();
    let themes = THEMES.get_or_init(|| {
        let config = config::load(&LoadOptions::default()).config;
        [
            ColorDepth::None,
            ColorDepth::Mono,
            ColorDepth::Ansi16,
            ColorDepth::Ansi256,
            ColorDepth::TrueColor,
        ]
        .into_iter()
        .map(|d| (d, config::build_theme(&config, None, d)))
        .collect()
    });
    themes
        .iter()
        .find(|(d, _)| *d == depth)
        .map_or_else(Theme::test, |(_, t)| t.clone())
}

/// Lay out with the fuzz highlighter and image sizer.
fn lay_out(doc: &Document, width: u16, theme: &Theme, caps: &Caps, opts: &RenderOptions) -> Layout {
    layout(doc, width, theme, caps, opts, &FuzzHighlighter, &FuzzImages)
}

/// Lay out and render at `width`, checking every invariant.
fn check_width(
    doc: &Document,
    width: u16,
    theme: &Theme,
    caps: &Caps,
    opts: &RenderOptions,
) -> Layout {
    let amb = opts.ambiguous_wide;
    let l = lay_out(doc, width, theme, caps, opts);
    check_layout(&l, doc, amb);
    let fits = |line: &str| {
        let w = str_width(line, amb);
        assert!(w <= usize::from(width), "{w} columns > {width}: {line:?}");
    };
    let plain = plain_text(doc, &l);
    for line in plain.lines() {
        let visible = check_escapes(line).unwrap_or_else(|e| panic!("plain output: {e}"));
        assert_eq!(visible, line, "an escape in plain output");
        fits(line);
    }
    let cfg = RenderConfig {
        link_id_prefix: "f".into(),
        ..RenderConfig::from_caps(caps)
    };
    let mut outputs = vec![to_bytes(doc, &l, &cfg)];
    // emde draws no block-glyph images where ambiguous characters are wide
    // (`term::caps::refuse_wide_blocks`).
    if !amb {
        let mut with_images = Vec::new();
        Emitter::new(doc, &l, &cfg)
            .with_images(&FuzzRows::new())
            .write_all(&mut with_images);
        outputs.push(with_images);
    }
    for bytes in outputs {
        let text = std::str::from_utf8(&bytes).expect("rendered bytes are UTF-8");
        for line in text.split('\n') {
            let visible = check_escapes(line).unwrap_or_else(|e| panic!("styled output: {e}"));
            fits(&visible);
        }
    }
    l
}

/// The layout's own invariants.
fn check_layout(l: &Layout, doc: &Document, amb: bool) {
    assert!(
        !l.text
            .contains(['\u{ad}', '\u{1b}', '\u{9b}', '\n', '\t', '\r', '\0']),
        "unsafe text in the arena"
    );
    for (i, line) in l.lines.iter().enumerate() {
        let text = l.line_text(i);
        assert_eq!(
            str_width(&text, amb),
            usize::from(line.cols),
            "line {i}: {text:?}"
        );
        assert!(
            usize::from(l.indent) + usize::from(line.cols) <= usize::from(l.width),
            "line {i} is wider than the layout: {text:?}"
        );
        if i > 0 {
            assert!(
                l.lines[i - 1].pos <= line.pos,
                "position decreases at line {i}"
            );
        }
    }
    let mut next = 0;
    for r in &l.block_lines {
        assert_eq!(r.start, next, "block line ranges do not tile");
        next = r.end;
    }
    assert_eq!(
        next as usize,
        l.lines.len(),
        "block line ranges do not cover the layout"
    );
    assert_eq!(l.block_lines.len(), doc.blocks.len());
    check_side_tables(l, doc);
}

/// The tables the pager looks things up in point at lines that exist and
/// agree with them.
fn check_side_tables(l: &Layout, doc: &Document) {
    for (i, line) in l.lines.iter().enumerate() {
        assert!(line.cols <= l.measure, "a line wider than the measure");
        // Re-anchoring after a resize finds the first line of a position.
        let first = l.line_at(line.pos);
        assert!(
            first <= i && l.lines[first].pos == line.pos,
            "line_at({:?}) is {first}, not a line at or before {i} with that position",
            line.pos
        );
        if let LineKind::Code { block, .. } = line.kind {
            assert!((block as usize) < l.code.len(), "a code line of no block");
        }
    }
    for hit in &l.link_hits {
        let line = l
            .lines
            .get(hit.line as usize)
            .expect("a link hit on a missing line");
        assert!(
            hit.cols.start <= hit.cols.end && hit.cols.end <= line.cols,
            "{hit:?} is outside its line of {} columns",
            line.cols
        );
        assert!(hit.link.index() < doc.links.len(), "{hit:?}: no such link");
    }
    assert_eq!(l.heading_line.len(), doc.headings.len());
    for &first in &l.heading_line {
        assert!(first == u32::MAX || (first as usize) < l.lines.len());
    }
    for (index, place) in l.images.iter().enumerate() {
        assert!(
            u32::from(place.col) + u32::from(place.cols) <= u32::from(l.measure),
            "{place:?} is too wide"
        );
        for row in 0..place.rows {
            let line = l
                .lines
                .get(place.line as usize + usize::from(row))
                .expect("a figure row on a missing line");
            assert_eq!(
                line.kind,
                LineKind::Image {
                    placement: index as u32,
                    row
                },
                "{place:?}"
            );
        }
    }
}

/// A deterministic highlighter: keywords, strings, numbers and comments in
/// a few languages, so the highlight overlay is exercised on any code.
struct FuzzHighlighter;

impl Highlighter for FuzzHighlighter {
    fn resolve(&self, token: &str) -> Option<LangId> {
        match token {
            "rust" | "rs" => Some(LangId(1)),
            "python" | "py" => Some(LangId(2)),
            "sh" | "bash" | "console" => Some(LangId(3)),
            _ => None,
        }
    }

    fn highlight(&self, lang: LangId, code: &str) -> HlBlock {
        HlBlock {
            lines: code.split('\n').map(|l| spans(lang, l)).collect(),
        }
    }

    fn colors(&self) -> CodeColors {
        CodeColors::default()
    }

    fn language_name(&self, lang: LangId) -> String {
        ["", "Rust", "Python", "Shell"][(lang.0 as usize).min(3)].to_owned()
    }
}

/// Highlight runs of one line: char-boundary ends, increasing, covering at
/// most the line.
fn spans(lang: LangId, line: &str) -> Vec<HlSpan> {
    let comment = if lang.0 == 1 { "//" } else { "#" };
    let style = |rgb: (u8, u8, u8), attrs: Attrs| {
        Style::fg(Color::Rgb(Rgb(rgb.0, rgb.1, rgb.2))).with(attrs)
    };
    let mut out: Vec<HlSpan> = Vec::new();
    let mut push = |end: usize, s: Style| {
        out.push(HlSpan {
            end: end as u32,
            style: s,
        })
    };
    let mut i = 0;
    while i < line.len() {
        let rest = &line[i..];
        let c = rest.chars().next().unwrap_or(' ');
        let end = if rest.starts_with(comment) {
            push(line.len(), style((0x6c, 0x70, 0x86), Attrs::ITALIC));
            break;
        } else if c == '"' {
            let end = rest[1..].find('"').map_or(line.len(), |j| i + j + 2);
            push(end, style((0xa6, 0xe3, 0xa1), Attrs::empty()));
            end
        } else if c.is_ascii_digit() {
            let end = rest
                .find(|ch: char| !ch.is_ascii_digit())
                .map_or(line.len(), |j| i + j);
            push(end, style((0xfa, 0xb3, 0x87), Attrs::BOLD));
            end
        } else {
            let end = i + c.len_utf8();
            push(end, Style::PLAIN);
            end
        };
        i = end;
    }
    out
}

/// Figure sizes from the image's index, within what layout allows: the
/// largest box, a thin one, a small one (perhaps empty), or none.
struct FuzzImages;

impl ImageSizer for FuzzImages {
    fn cells(&self, image: ImageId, max_cols: u16, max_rows: u16) -> Option<(u16, u16)> {
        let n = image.0;
        let small = |m: u32, max: u16| u16::try_from(n % m).unwrap_or(0).min(max);
        match n % 4 {
            0 => None,
            1 => Some((max_cols, max_rows)),
            2 => Some((small(7, max_cols), small(5, max_rows))),
            _ => Some((max_cols.min(1), max_rows)),
        }
    }
}

/// Image rows drawn as block glyphs, as stream mode shows text images: one
/// cell per column of the box, for every other figure.
struct FuzzRows {
    cells: Vec<RasterCell>,
}

impl FuzzRows {
    fn new() -> FuzzRows {
        let glyphs = ['▀', '▄', '█', ' ', '▌', '🬀'];
        let cells = (0..=u8::MAX)
            .map(|i| RasterCell {
                ch: glyphs[usize::from(i) % glyphs.len()],
                fg: Color::Rgb(Rgb(i, 255 - i, 128)),
                bg: match i % 3 {
                    0 => Color::Default,
                    1 => Color::Ansi(i % 16),
                    _ => Color::Indexed(i),
                },
            })
            .collect();
        FuzzRows { cells }
    }
}

impl ImageRows for FuzzRows {
    fn row(&self, placement: &Placement, row: u16) -> Option<RowContent<'_>> {
        if placement.image.0 % 2 == 1 || row >= placement.rows {
            return None;
        }
        self.cells
            .get(..usize::from(placement.cols))
            .map(RowContent::Cells)
    }
}
