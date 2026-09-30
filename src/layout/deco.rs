//! Decoration glyphs, resolved once per layout.
//!
//! Every glyph layout draws (bullets, bars, rules, borders, icons, markers)
//! comes from here. Configured glyphs ([`Glyphs`], heading markers) are
//! sanitised and measured; in ASCII mode — `--ascii`, or East Asian
//! Ambiguous characters drawn two columns wide, which would break every box
//! and bar — each glyph falls back to plain ASCII.

use crate::options::{Glyphs, IconSet, RenderOptions, TableBorder};
use crate::text::{sanitize, str_width, strip_soft_hyphens};

/// A decoration string with its display width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Glyph {
    pub(super) text: String,
    pub(super) cols: u16,
}

impl Glyph {
    fn new(text: &str, ambiguous_wide: bool) -> Glyph {
        let text = strip_soft_hyphens(&sanitize(text))
            .replace(['\n', '\t'], " ")
            .to_string();
        let cols = u16::try_from(str_width(&text, ambiguous_wide)).unwrap_or(u16::MAX);
        Glyph { text, cols }
    }

    /// Whether the glyph shows nothing.
    pub(super) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Box-drawing pieces for tables, frames and cards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Borders {
    pub(super) top_left: &'static str,
    pub(super) top_right: &'static str,
    pub(super) bottom_left: &'static str,
    pub(super) bottom_right: &'static str,
    pub(super) horizontal: &'static str,
    pub(super) vertical: &'static str,
    /// `┬`
    pub(super) down: &'static str,
    /// `┴`
    pub(super) up: &'static str,
    /// `├`
    pub(super) right: &'static str,
    /// `┤`
    pub(super) left: &'static str,
    /// `┼`
    pub(super) cross: &'static str,
    /// No borders at all (spaces).
    pub(super) none: bool,
}

impl Borders {
    const fn of(chars: [&'static str; 11]) -> Borders {
        let [tl, tr, bl, br, h, v, down, up, right, left, cross] = chars;
        Borders {
            top_left: tl,
            top_right: tr,
            bottom_left: bl,
            bottom_right: br,
            horizontal: h,
            vertical: v,
            down,
            up,
            right,
            left,
            cross,
            none: false,
        }
    }

    const ROUNDED: Borders = Borders::of(["╭", "╮", "╰", "╯", "─", "│", "┬", "┴", "├", "┤", "┼"]);
    const LIGHT: Borders = Borders::of(["┌", "┐", "└", "┘", "─", "│", "┬", "┴", "├", "┤", "┼"]);
    const HEAVY: Borders = Borders::of(["┏", "┓", "┗", "┛", "━", "┃", "┳", "┻", "┣", "┫", "╋"]);
    const DOUBLE: Borders = Borders::of(["╔", "╗", "╚", "╝", "═", "║", "╦", "╩", "╠", "╣", "╬"]);
    const ASCII: Borders = Borders::of(["+", "+", "+", "+", "-", "|", "+", "+", "+", "+", "+"]);
    const NONE: Borders = Borders {
        none: true,
        ..Borders::of([" "; 11])
    };

    fn table(set: TableBorder, ascii: bool) -> Borders {
        match set {
            TableBorder::None => Borders::NONE,
            _ if ascii => Borders::ASCII,
            TableBorder::Rounded => Borders::ROUNDED,
            TableBorder::Light => Borders::LIGHT,
            TableBorder::Heavy => Borders::HEAVY,
            TableBorder::Double => Borders::DOUBLE,
            TableBorder::Ascii => Borders::ASCII,
        }
    }
}

/// Every decoration glyph of one layout.
#[derive(Clone, Debug)]
pub(super) struct Deco {
    /// ASCII-only decorations.
    pub(super) ascii: bool,
    /// Bullet per list depth (cycled, never empty).
    pub(super) bullets: Vec<Glyph>,
    pub(super) task_open: Glyph,
    pub(super) task_done: Glyph,
    /// Quote bar.
    pub(super) quote: Glyph,
    /// Thematic break glyph (repeated).
    pub(super) rule: Glyph,
    /// Marks wrapped code lines.
    pub(super) wrap: Glyph,
    /// Marks clipped code lines.
    pub(super) clip: &'static str,
    /// Table borders.
    pub(super) table: Borders,
    /// Frames of code blocks, image boxes and front matter cards.
    pub(super) frame: Borders,
    /// Code gutter bar.
    pub(super) gutter: &'static str,
    /// Alert icons: note, tip, important, warning, caution.
    pub(super) icons: [&'static str; 5],
    /// Heading markers per level.
    pub(super) markers: [Glyph; 6],
    /// Rule under an `h1` without colours.
    pub(super) h1_rule: &'static str,
    /// Heavy and light parts of the `h2` rule.
    pub(super) h2_heavy: &'static str,
    pub(super) h2_light: &'static str,
    /// Footnote back-link.
    pub(super) backref: &'static str,
    /// Image chip marker.
    pub(super) chip: &'static str,
    /// `<details>` marker.
    pub(super) details: &'static str,
}

/// ASCII stand-ins for the default bullets.
const ASCII_BULLETS: [&str; 4] = ["*", "-", "+", "-"];

impl Deco {
    pub(super) fn new(opts: &RenderOptions) -> Deco {
        let ascii = opts.ascii || opts.ambiguous_wide;
        let amb = opts.ambiguous_wide;
        let g = &opts.glyphs;
        let pick = |configured: &str, fallback: &str| -> Glyph {
            let glyph = Glyph::new(configured, amb);
            if glyph.is_empty() || (ascii && !glyph.text.is_ascii()) {
                Glyph::new(fallback, amb)
            } else {
                glyph
            }
        };
        let bullets = bullets(g, ascii, amb);
        let icons = match (ascii, opts.glyphs.icons) {
            (true, _) | (_, IconSet::Ascii) => ["(i)", "(*)", "(!)", "/!\\", "(x)"],
            (false, IconSet::Unicode) => ["ⓘ", "✦", "❖", "▲", "⊘"],
            (false, IconSet::Nerd) => ["\u{f05a}", "\u{f0eb}", "\u{f06a}", "\u{f071}", "\u{f05e}"],
        };
        let markers = std::array::from_fn(|i| {
            let configured = opts.heading.markers.get(i).map_or("", String::as_str);
            let glyph = Glyph::new(configured, amb);
            if ascii && !glyph.text.is_ascii() {
                Glyph::new(&ascii_marker(&glyph.text), amb)
            } else {
                glyph
            }
        });
        let alt = |unicode: &'static str, plain: &'static str| if ascii { plain } else { unicode };
        Deco {
            ascii,
            bullets,
            task_open: pick(&g.task[0], "[ ]"),
            task_done: pick(&g.task[1], "[x]"),
            quote: pick(&g.quote, "|"),
            rule: pick(&g.rule, "-"),
            wrap: pick(&g.wrap_marker, ">"),
            clip: alt("›", ">"),
            table: Borders::table(g.table, ascii),
            frame: if ascii {
                Borders::ASCII
            } else {
                Borders::LIGHT
            },
            gutter: alt("▎", "|"),
            icons,
            markers,
            h1_rule: alt("═", "="),
            h2_heavy: alt("━", "="),
            h2_light: alt("─", "-"),
            backref: alt("↑", "^"),
            chip: alt("▣", "[img]"),
            details: alt("▾", "v"),
        }
    }

    /// The bullet for a list nested `depth` bullet lists deep.
    pub(super) fn bullet(&self, depth: usize) -> &Glyph {
        let n = self.bullets.len().max(1);
        self.bullets
            .get(depth % n)
            .or_else(|| self.bullets.first())
            .unwrap_or(&self.task_open)
    }
}

/// The configured bullets, or their ASCII stand-ins.
fn bullets(g: &Glyphs, ascii: bool, amb: bool) -> Vec<Glyph> {
    let mut out: Vec<Glyph> = g
        .bullets
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let glyph = Glyph::new(b, amb);
            if glyph.is_empty() || (ascii && !glyph.text.is_ascii()) {
                let fallback = ASCII_BULLETS.get(i % ASCII_BULLETS.len()).unwrap_or(&"*");
                Glyph::new(fallback, amb)
            } else {
                glyph
            }
        })
        .collect();
    if out.is_empty() {
        out = ASCII_BULLETS.iter().map(|b| Glyph::new(b, amb)).collect();
    }
    out
}

/// An ASCII heading marker for a non-ASCII one: bars become `|`, anything
/// else `#`, keeping trailing spaces.
fn ascii_marker(marker: &str) -> String {
    let trimmed = marker.trim_end();
    let spaces = marker.get(trimmed.len()..).unwrap_or("");
    let body: String = trimmed
        .chars()
        .map(|c| match c {
            c if c.is_ascii() => c,
            '▎' | '▍' | '▌' | '│' | '┃' | '▏' | '▐' => '|',
            _ => '#',
        })
        .collect();
    format!("{body}{spaces}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_unicode() {
        let d = Deco::new(&RenderOptions::default());
        assert!(!d.ascii);
        assert_eq!(d.bullet(0).text, "•");
        assert_eq!(d.bullet(1).text, "◦");
        assert_eq!(d.bullet(5).text, "◦", "cycles");
        assert_eq!(d.task_open.text, "☐");
        assert_eq!(d.quote.text, "▎");
        assert_eq!(d.markers[2].text, "▎ ");
        assert_eq!(d.markers[2].cols, 2);
        assert_eq!(d.table.top_left, "╭");
        assert_eq!(d.icons[0], "ⓘ");
    }

    #[test]
    fn ascii_mode_falls_back_everywhere() {
        let opts = RenderOptions {
            ascii: true,
            ..RenderOptions::default()
        };
        let d = Deco::new(&opts);
        assert!(d.ascii);
        for depth in 0..4 {
            assert!(d.bullet(depth).text.is_ascii());
        }
        assert_eq!(d.task_done.text, "[x]");
        assert_eq!(d.quote.text, "|");
        assert_eq!(d.markers[2].text, "| ");
        assert_eq!(d.table.cross, "+");
        assert!(d.icons.iter().all(|i| i.is_ascii()));
        for g in [d.h1_rule, d.h2_heavy, d.h2_light, d.backref, d.chip, d.clip] {
            assert!(g.is_ascii(), "{g}");
        }
    }

    #[test]
    fn ambiguous_width_implies_ascii() {
        let opts = RenderOptions {
            ambiguous_wide: true,
            ..RenderOptions::default()
        };
        assert!(Deco::new(&opts).ascii);
    }

    #[test]
    fn configured_glyphs_are_sanitised() {
        let mut opts = RenderOptions::default();
        opts.glyphs.bullets = vec!["\u{1b}[31m".into(), String::new()];
        opts.glyphs.quote = "┃\u{ad}".into();
        let d = Deco::new(&opts);
        assert_eq!(d.bullet(0).text, "␛[31m");
        assert_eq!(d.bullet(1).text, "-", "empty bullets fall back");
        assert_eq!(d.quote.text, "┃");
        opts.glyphs.bullets.clear();
        assert_eq!(Deco::new(&opts).bullet(0).text, "*");
    }

    #[test]
    fn border_pieces_are_one_column() {
        use crate::text::str_width;
        for set in [
            TableBorder::Rounded,
            TableBorder::Light,
            TableBorder::Heavy,
            TableBorder::Double,
            TableBorder::Ascii,
            TableBorder::None,
        ] {
            for ascii in [false, true] {
                let b = Borders::table(set, ascii);
                for piece in [
                    b.top_left,
                    b.top_right,
                    b.bottom_left,
                    b.bottom_right,
                    b.horizontal,
                    b.vertical,
                    b.down,
                    b.up,
                    b.right,
                    b.left,
                    b.cross,
                ] {
                    assert_eq!(str_width(piece, false), 1, "{set:?} {piece:?}");
                }
            }
        }
        let d = Deco::new(&RenderOptions::default());
        assert_eq!(str_width(d.gutter, false), 1);
    }

    #[test]
    fn table_border_none_is_spaces() {
        let mut opts = RenderOptions::default();
        opts.glyphs.table = TableBorder::None;
        let d = Deco::new(&opts);
        assert!(d.table.none);
        assert_eq!(d.table.vertical, " ");
    }
}
