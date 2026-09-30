//! SGR (Select Graphic Rendition) encoding with minimal diffing, and colour
//! downsampling to the terminal's depth.
//!
//! [`Palette::style`] maps a theme style to what the terminal can show:
//! 24-bit colours stay as they are in truecolor, become the nearest xterm
//! index at 256 colours ([`color::to_256`]) and the nearest ANSI colour at
//! 16 ([`color::to_16`]); `Mono` keeps only attributes and `None` nothing
//! at all. Styled underlines (`4:3`, …) and underline colours (`58`) are
//! only kept when the terminal draws them.
//!
//! [`write_transition`] emits the shortest parameter list that turns one
//! (downsampled) style into another: either the differences, or a reset
//! followed by the new style's parameters. SGR `22` turns off *both* bold
//! and dim, so an attribute that stays on is written again after it.

use std::collections::HashMap;

use crate::color;
use crate::style::{Attrs, Color, Rgb, Style, Underline};
use crate::term::ColorDepth;

/// Reset all attributes.
pub const RESET: &[u8] = b"\x1b[0m";

/// Colour downsampling for one terminal, with a memo of mapped colours.
#[derive(Clone, Debug)]
pub struct Palette {
    depth: ColorDepth,
    styled_underline: bool,
    memo: HashMap<Rgb, Color>,
}

impl Palette {
    /// A palette for `depth`; `styled_underline` keeps `4:x` and `58`.
    pub fn new(depth: ColorDepth, styled_underline: bool) -> Palette {
        Palette {
            depth,
            styled_underline,
            memo: HashMap::new(),
        }
    }

    /// The colour depth.
    pub fn depth(&self) -> ColorDepth {
        self.depth
    }

    /// A colour as the terminal can show it.
    pub fn color(&mut self, c: Color) -> Color {
        match (self.depth, c) {
            (ColorDepth::None | ColorDepth::Mono, _) => Color::Default,
            (ColorDepth::TrueColor, c) => c,
            (_, Color::Default) => Color::Default,
            (ColorDepth::Ansi256, Color::Rgb(rgb)) => *self
                .memo
                .entry(rgb)
                .or_insert_with(|| Color::Indexed(color::to_256(rgb))),
            (ColorDepth::Ansi256, c) => c,
            (ColorDepth::Ansi16, Color::Rgb(rgb)) => *self
                .memo
                .entry(rgb)
                .or_insert_with(|| Color::Ansi(color::to_16(rgb))),
            (ColorDepth::Ansi16, Color::Indexed(n)) if n < 16 => Color::Ansi(n),
            (ColorDepth::Ansi16, Color::Indexed(n)) => {
                let rgb = color::xterm_rgb(n);
                *self
                    .memo
                    .entry(rgb)
                    .or_insert_with(|| Color::Ansi(color::to_16(rgb)))
            }
            (ColorDepth::Ansi16, c @ Color::Ansi(_)) => c,
        }
    }

    /// A style as the terminal can show it.
    pub fn style(&mut self, s: &Style) -> Style {
        if self.depth == ColorDepth::None {
            return Style::PLAIN;
        }
        let mut out = *s;
        out.fg = self.color(s.fg);
        out.bg = self.color(s.bg);
        if self.styled_underline {
            out.underline_color = self.color(s.underline_color);
        } else {
            out.underline_color = Color::Default;
            if out.underline != Underline::None {
                out.underline = Underline::Single;
            }
        }
        out
    }
}

/// A small stack buffer for SGR parameters (the longest possible list is
/// well under its size; anything beyond it is dropped, never overflowed).
struct Params {
    buf: [u8; 128],
    len: usize,
}

impl Params {
    fn new() -> Params {
        Params {
            buf: [0; 128],
            len: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, b: u8) {
        if let Some(slot) = self.buf.get_mut(self.len) {
            *slot = b;
            self.len += 1;
        }
    }

    fn extend_from_slice(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.push(b);
        }
    }

    fn as_slice(&self) -> &[u8] {
        self.buf.get(..self.len).unwrap_or(&[])
    }
}

/// Append one SGR parameter (`;`-separated).
fn param(buf: &mut Params, p: &[u8]) {
    if !buf.is_empty() {
        buf.push(b';');
    }
    buf.extend_from_slice(p);
}

/// Append a decimal number.
fn num(buf: &mut Params, n: u8) {
    if n >= 100 {
        buf.push(b'0' + n / 100);
    }
    if n >= 10 {
        buf.push(b'0' + n / 10 % 10);
    }
    buf.push(b'0' + n % 10);
}

/// Foreground (`base` 30) or background (`base` 40) colour parameters.
fn color_param(buf: &mut Params, c: Color, base: u8) {
    if !buf.is_empty() {
        buf.push(b';');
    }
    match c {
        Color::Default => num(buf, base + 9),
        Color::Ansi(n) if n < 8 => num(buf, base + n),
        Color::Ansi(n) if n < 16 => num(buf, base + 60 + n - 8),
        Color::Ansi(n) | Color::Indexed(n) => {
            num(buf, base + 8);
            buf.extend_from_slice(b";5;");
            num(buf, n);
        }
        Color::Rgb(Rgb(r, g, b)) => {
            num(buf, base + 8);
            buf.extend_from_slice(b";2;");
            num(buf, r);
            buf.push(b';');
            num(buf, g);
            buf.push(b';');
            num(buf, b);
        }
    }
}

/// Underline colour parameters (`58:…`, `59`).
fn underline_color_param(buf: &mut Params, c: Color) {
    if !buf.is_empty() {
        buf.push(b';');
    }
    match c {
        Color::Default => buf.extend_from_slice(b"59"),
        Color::Ansi(n) | Color::Indexed(n) => {
            buf.extend_from_slice(b"58:5:");
            num(buf, n);
        }
        Color::Rgb(Rgb(r, g, b)) => {
            buf.extend_from_slice(b"58:2::");
            num(buf, r);
            buf.push(b':');
            num(buf, g);
            buf.push(b':');
            num(buf, b);
        }
    }
}

fn underline_param(buf: &mut Params, u: Underline) {
    let p: &[u8] = match u {
        Underline::None => b"24",
        Underline::Single => b"4",
        Underline::Double => b"4:2",
        Underline::Curly => b"4:3",
        Underline::Dotted => b"4:4",
        Underline::Dashed => b"4:5",
    };
    param(buf, p);
}

/// Attribute on/off codes, in emission order.
const ATTRS: [(Attrs, &[u8], &[u8]); 4] = [
    (Attrs::ITALIC, b"3", b"23"),
    (Attrs::STRIKE, b"9", b"29"),
    (Attrs::REVERSE, b"7", b"27"),
    (Attrs::OVERLINE, b"53", b"55"),
];

/// Parameters that turn on everything `s` has (from a reset state).
fn full_params(s: &Style, buf: &mut Params) {
    if s.attrs.contains(Attrs::BOLD) {
        param(buf, b"1");
    }
    if s.attrs.contains(Attrs::DIM) {
        param(buf, b"2");
    }
    for (a, on, _) in ATTRS {
        if s.attrs.contains(a) {
            param(buf, on);
        }
    }
    if s.underline != Underline::None {
        underline_param(buf, s.underline);
    }
    if s.fg != Color::Default {
        color_param(buf, s.fg, 30);
    }
    if s.bg != Color::Default {
        color_param(buf, s.bg, 40);
    }
    if s.underline_color != Color::Default {
        underline_color_param(buf, s.underline_color);
    }
}

/// Parameters that turn `from` into `to`.
fn diff_params(from: &Style, to: &Style, buf: &mut Params) {
    let off = from.attrs - to.attrs;
    let on = to.attrs - from.attrs;
    if off.intersects(Attrs::BOLD | Attrs::DIM) {
        // 22 clears both bold and dim: write back whichever stays on.
        param(buf, b"22");
        if to.attrs.contains(Attrs::BOLD) {
            param(buf, b"1");
        }
        if to.attrs.contains(Attrs::DIM) {
            param(buf, b"2");
        }
    } else {
        if on.contains(Attrs::BOLD) {
            param(buf, b"1");
        }
        if on.contains(Attrs::DIM) {
            param(buf, b"2");
        }
    }
    for (a, on_code, off_code) in ATTRS {
        if on.contains(a) {
            param(buf, on_code);
        } else if off.contains(a) {
            param(buf, off_code);
        }
    }
    if from.underline != to.underline {
        underline_param(buf, to.underline);
    }
    if from.fg != to.fg {
        color_param(buf, to.fg, 30);
    }
    if from.bg != to.bg {
        color_param(buf, to.bg, 40);
    }
    if from.underline_color != to.underline_color {
        underline_color_param(buf, to.underline_color);
    }
}

/// Append the escape sequence that changes the terminal from style `from`
/// to style `to` (both already downsampled), if any.
pub fn write_transition(from: &Style, to: &Style, out: &mut Vec<u8>) {
    if from == to {
        return;
    }
    if *to == Style::PLAIN {
        out.extend_from_slice(RESET);
        return;
    }
    let mut diff = Params::new();
    diff_params(from, to, &mut diff);
    // A change that only turns things on is never longer than a reset
    // followed by the whole new style; otherwise compare the two.
    let only_on = (from.attrs - to.attrs).is_empty()
        && (from.underline == to.underline || to.underline != Underline::None)
        && (from.fg == to.fg || to.fg != Color::Default)
        && (from.bg == to.bg || to.bg != Color::Default)
        && (from.underline_color == to.underline_color || to.underline_color != Color::Default);
    out.extend_from_slice(b"\x1b[");
    if only_on {
        out.extend_from_slice(diff.as_slice());
    } else {
        let mut full = Params::new();
        full.push(b'0');
        full_params(to, &mut full);
        let params = if diff.len <= full.len { &diff } else { &full };
        out.extend_from_slice(params.as_slice());
    }
    out.push(b'm');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sgr(from: Style, to: Style) -> String {
        let mut out = Vec::new();
        write_transition(&from, &to, &mut out);
        String::from_utf8(out).unwrap().replace('\x1b', "␛")
    }

    const BLUE: Rgb = Rgb(0x89, 0xb4, 0xfa);

    #[test]
    fn users_h1_example() {
        // [style.h1] fg = "accent", bold = true, underline = "curly"
        let h1 = Style::fg(Color::Rgb(BLUE))
            .with(Attrs::BOLD)
            .underlined(Underline::Curly);
        assert_eq!(sgr(Style::PLAIN, h1), "␛[1;4:3;38;2;137;180;250m");
        let mut plain_ul = Palette::new(ColorDepth::TrueColor, false);
        let s = plain_ul.style(&h1);
        assert_eq!(sgr(Style::PLAIN, s), "␛[1;4;38;2;137;180;250m");
    }

    #[test]
    fn nothing_for_equal_styles_and_reset_for_plain() {
        let s = Style::fg(Color::Ansi(1));
        assert_eq!(sgr(s, s), "");
        assert_eq!(sgr(s, Style::PLAIN), "␛[0m");
    }

    #[test]
    fn turning_off_bold_keeps_dim() {
        let red = Style::fg(Color::Ansi(1));
        let both = red.with(Attrs::BOLD | Attrs::DIM);
        let dim = red.with(Attrs::DIM);
        let bold = red.with(Attrs::BOLD);
        assert_eq!(sgr(both, dim), "␛[22;2m");
        assert_eq!(sgr(both, bold), "␛[22;1m");
        assert_eq!(sgr(bold, both), "␛[2m");
        assert_eq!(sgr(dim, bold), "␛[22;1m");
        // Without other attributes a reset is as short.
        let plain_both = Style::PLAIN.with(Attrs::BOLD | Attrs::DIM);
        assert_eq!(sgr(plain_both, Style::PLAIN.with(Attrs::DIM)), "␛[0;2m");
    }

    #[test]
    fn minimal_differences() {
        let a = Style::fg(Color::Ansi(1)).with(Attrs::ITALIC);
        let b = Style::fg(Color::Ansi(2)).with(Attrs::ITALIC);
        assert_eq!(sgr(a, b), "␛[32m");
        let c = b.on(Color::Indexed(236));
        assert_eq!(sgr(b, c), "␛[48;5;236m");
        let d = Style::fg(Color::Ansi(9));
        assert_eq!(sgr(c, d), "␛[0;91m", "a reset is shorter");
        let e = Style::PLAIN.with(Attrs::STRIKE | Attrs::REVERSE | Attrs::OVERLINE);
        assert_eq!(sgr(Style::PLAIN, e), "␛[9;7;53m");
        let f = Style::PLAIN.with(Attrs::REVERSE);
        assert_eq!(sgr(e, f), "␛[0;7m");
        let g = f.on(Color::Ansi(4));
        assert_eq!(sgr(e.on(Color::Ansi(4)), g), "␛[29;55m");
        let u = Style::PLAIN.underlined(Underline::Dotted);
        assert_eq!(sgr(u, Style::PLAIN.underlined(Underline::Dashed)), "␛[4:5m");
        assert_eq!(
            sgr(u.with(Attrs::BOLD), Style::PLAIN.with(Attrs::BOLD)),
            "␛[24m"
        );
    }

    #[test]
    fn colour_codes() {
        for (c, want) in [
            (Color::Ansi(0), "30"),
            (Color::Ansi(7), "37"),
            (Color::Ansi(8), "90"),
            (Color::Ansi(15), "97"),
            (Color::Indexed(196), "38;5;196"),
            (Color::Rgb(Rgb(1, 22, 255)), "38;2;1;22;255"),
            (Color::Default, "39"),
        ] {
            let mut out = Params::new();
            color_param(&mut out, c, 30);
            assert_eq!(String::from_utf8_lossy(out.as_slice()), want, "{c:?}");
        }
        let mut out = Params::new();
        color_param(&mut out, Color::Ansi(12), 40);
        assert_eq!(out.as_slice(), b"104");
        out = Params::new();
        underline_color_param(&mut out, Color::Rgb(BLUE));
        assert_eq!(out.as_slice(), b"58:2::137:180:250");
        out = Params::new();
        underline_color_param(&mut out, Color::Indexed(5));
        assert_eq!(out.as_slice(), b"58:5:5");
        assert!(Params::new().is_empty());
    }

    #[test]
    fn downsampling() {
        let mut p = Palette::new(ColorDepth::Ansi256, true);
        assert_eq!(p.color(Color::Rgb(Rgb(255, 0, 0))), Color::Indexed(196));
        assert_eq!(p.color(Color::Ansi(3)), Color::Ansi(3));
        let mut p = Palette::new(ColorDepth::Ansi16, true);
        assert_eq!(p.color(Color::Rgb(Rgb(250, 10, 10))), Color::Ansi(9));
        assert_eq!(p.color(Color::Indexed(4)), Color::Ansi(4));
        assert_eq!(p.color(Color::Indexed(231)), Color::Ansi(15));
        let mut p = Palette::new(ColorDepth::Mono, true);
        let s = Style::fg(Color::Rgb(BLUE))
            .with(Attrs::BOLD)
            .underlined(Underline::Curly);
        let m = p.style(&s);
        assert_eq!(m.fg, Color::Default);
        assert_eq!(m.attrs, Attrs::BOLD);
        assert_eq!(m.underline, Underline::Curly);
        let mut p = Palette::new(ColorDepth::None, true);
        assert_eq!(p.style(&s), Style::PLAIN);
    }

    #[test]
    fn underline_colour_needs_styled_underlines() {
        let s = Style {
            underline: Underline::Curly,
            underline_color: Color::Rgb(BLUE),
            ..Style::PLAIN
        };
        let mut styled = Palette::new(ColorDepth::TrueColor, true);
        assert_eq!(
            sgr(Style::PLAIN, styled.style(&s)),
            "␛[4:3;58:2::137:180:250m"
        );
        let mut plain = Palette::new(ColorDepth::TrueColor, false);
        assert_eq!(sgr(Style::PLAIN, plain.style(&s)), "␛[4m");
    }
}
