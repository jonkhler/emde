//! Terminal text styles: colours, attributes and underline styles.
//!
//! Styles are plain `Copy` values. Layout interns the styles it uses into a
//! [`StyleTable`] so spans only carry a 16-bit [`StyleId`]; colours are
//! downsampled to the terminal's colour depth when bytes are emitted.

use std::collections::HashMap;

use bitflags::bitflags;

/// A 24-bit sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parse `#rgb` or `#rrggbb` (the leading `#` is required).
    pub fn parse_hex(s: &str) -> Option<Rgb> {
        let hex = s.strip_prefix('#')?;
        let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        let b = hex.as_bytes();
        match b.len() {
            3 => {
                let (r, g, bl) = (digit(b[0])?, digit(b[1])?, digit(b[2])?);
                Some(Rgb(r * 17, g * 17, bl * 17))
            }
            6 => {
                let byte = |i: usize| Some(digit(b[i])? * 16 + digit(b[i + 1])?);
                Some(Rgb(byte(0)?, byte(2)?, byte(4)?))
            }
            _ => None,
        }
    }
}

/// A terminal colour as a theme specifies it. Downsampled at output time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Color {
    /// The terminal's default foreground or background (SGR 39 / 49).
    #[default]
    Default,
    /// One of the 16 ANSI colours (0–15), so the user's palette applies.
    Ansi(u8),
    /// An xterm 256-colour palette index.
    Indexed(u8),
    /// 24-bit colour.
    Rgb(Rgb),
}

bitflags! {
    /// Text attributes other than underline.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub struct Attrs: u8 {
        const BOLD = 1 << 0;
        const DIM = 1 << 1;
        const ITALIC = 1 << 2;
        const STRIKE = 1 << 3;
        const REVERSE = 1 << 4;
        const OVERLINE = 1 << 5;
    }
}

/// Underline style (SGR 4 / 4:x). Styled variants degrade to `Single` when
/// the terminal cannot render them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// A complete text style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    /// Underline colour (SGR 58); `Default` follows the foreground.
    pub underline_color: Color,
    pub attrs: Attrs,
    pub underline: Underline,
}

impl Style {
    /// The terminal's default style.
    pub const PLAIN: Style = Style {
        fg: Color::Default,
        bg: Color::Default,
        underline_color: Color::Default,
        attrs: Attrs::empty(),
        underline: Underline::None,
    };

    /// A style with only a foreground colour.
    pub const fn fg(fg: Color) -> Style {
        Style { fg, ..Style::PLAIN }
    }

    /// Returns `self` with the given attributes added.
    pub const fn with(mut self, attrs: Attrs) -> Style {
        self.attrs = self.attrs.union(attrs);
        self
    }

    /// Returns `self` with a background colour.
    pub const fn on(mut self, bg: Color) -> Style {
        self.bg = bg;
        self
    }

    /// Returns `self` with an underline style.
    pub const fn underlined(mut self, underline: Underline) -> Style {
        self.underline = underline;
        self
    }

    /// Apply a partial style on top of this one.
    pub fn patch(self, p: &StylePatch) -> Style {
        Style {
            fg: p.fg.unwrap_or(self.fg),
            bg: p.bg.unwrap_or(self.bg),
            underline_color: p.underline_color.unwrap_or(self.underline_color),
            attrs: (self.attrs | p.set) - p.clear,
            underline: p.underline.unwrap_or(self.underline),
        }
    }
}

/// A partial style: `None` fields and unset attributes inherit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StylePatch {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub underline_color: Option<Color>,
    /// Attributes to turn on.
    pub set: Attrs,
    /// Attributes to turn off.
    pub clear: Attrs,
    pub underline: Option<Underline>,
}

/// Handle to an interned [`Style`]. `StyleId(0)` is always [`Style::PLAIN`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct StyleId(pub u16);

/// Interning table for styles used by one layout.
#[derive(Clone, Debug)]
pub struct StyleTable {
    styles: Vec<Style>,
    index: HashMap<Style, StyleId>,
}

impl Default for StyleTable {
    fn default() -> Self {
        Self::new()
    }
}

impl StyleTable {
    /// A table containing only [`Style::PLAIN`] as `StyleId(0)`.
    pub fn new() -> Self {
        let mut index = HashMap::new();
        index.insert(Style::PLAIN, StyleId(0));
        StyleTable {
            styles: vec![Style::PLAIN],
            index,
        }
    }

    /// Intern a style. Saturates at `u16::MAX` distinct styles by falling back
    /// to the plain style (never reached by real documents).
    pub fn intern(&mut self, style: Style) -> StyleId {
        if let Some(&id) = self.index.get(&style) {
            return id;
        }
        let Ok(raw) = u16::try_from(self.styles.len()) else {
            return StyleId(0);
        };
        let id = StyleId(raw);
        self.styles.push(style);
        self.index.insert(style, id);
        id
    }

    /// Look up an interned style.
    pub fn get(&self, id: StyleId) -> &Style {
        self.styles.get(usize::from(id.0)).unwrap_or(&Style::PLAIN)
    }

    /// Number of distinct styles.
    pub fn len(&self) -> usize {
        self.styles.len()
    }

    /// Whether only the plain style is present.
    pub fn is_empty(&self) -> bool {
        self.styles.len() <= 1
    }

    /// All styles, indexed by `StyleId.0`.
    pub fn styles(&self) -> &[Style] {
        &self.styles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex() {
        assert_eq!(Rgb::parse_hex("#89b4fa"), Some(Rgb(0x89, 0xb4, 0xfa)));
        assert_eq!(Rgb::parse_hex("#fff"), Some(Rgb(255, 255, 255)));
        assert_eq!(Rgb::parse_hex("89b4fa"), None);
        assert_eq!(Rgb::parse_hex("#12345"), None);
        assert_eq!(Rgb::parse_hex("#gggggg"), None);
    }

    #[test]
    fn patch_sets_and_clears() {
        let base = Style::fg(Color::Ansi(1)).with(Attrs::BOLD | Attrs::ITALIC);
        let p = StylePatch {
            bg: Some(Color::Indexed(236)),
            set: Attrs::DIM,
            clear: Attrs::ITALIC,
            underline: Some(Underline::Curly),
            ..StylePatch::default()
        };
        let s = base.patch(&p);
        assert_eq!(s.fg, Color::Ansi(1));
        assert_eq!(s.bg, Color::Indexed(236));
        assert_eq!(s.attrs, Attrs::BOLD | Attrs::DIM);
        assert_eq!(s.underline, Underline::Curly);
    }

    #[test]
    fn interning() {
        let mut t = StyleTable::new();
        assert_eq!(t.intern(Style::PLAIN), StyleId(0));
        let a = t.intern(Style::fg(Color::Ansi(2)));
        let b = t.intern(Style::fg(Color::Ansi(2)));
        assert_eq!(a, b);
        assert_eq!(a, StyleId(1));
        assert_eq!(t.get(a).fg, Color::Ansi(2));
        assert_eq!(t.len(), 2);
    }
}
