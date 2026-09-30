//! Terminal capabilities: what the output terminal can show.
//!
//! [`Caps`] is decided once per run (environment, one optional `tmux display`
//! query, one optional `/dev/tty` probe) and consumed by rendering, the pager
//! and the graphics code. Every decision records a [`Reason`] for `--doctor`.

pub mod caps;
pub mod color;
pub mod env;
pub mod probe;
pub mod tmux;

use crate::style::Rgb;

/// How many colours (if any) the output supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ColorDepth {
    /// Not a terminal / `TERM=dumb`: emit no escape sequences at all.
    None,
    /// `NO_COLOR`: attributes (bold, italic, …) but no colours.
    Mono,
    /// The 16 ANSI colours.
    Ansi16,
    /// xterm 256 colours.
    Ansi256,
    /// 24-bit colour.
    #[default]
    TrueColor,
}

/// The graphics path chosen for images.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Graphics {
    /// Images are shown as an alt-text box only.
    None,
    /// Text rendering with block glyphs (works everywhere with colour).
    #[default]
    Blocks,
    /// kitty graphics with Unicode placeholders (kitty, Ghostty, iTerm2 ≥ 3.6).
    KittyPlaceholders,
    /// kitty graphics with classic placements (never inside tmux).
    KittyClassic,
    /// iTerm2 inline images (OSC 1337).
    Iterm,
    /// DEC sixel.
    Sixel,
}

/// Glyph set for text-mode images.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlockGlyphSet {
    #[default]
    Half,
    Quadrant,
    Sextant,
    Octant,
}

/// Why a capability was decided the way it was (shown by `--doctor`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reason {
    pub topic: &'static str,
    pub detail: String,
}

/// Decided terminal capabilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caps {
    /// Output is a terminal (else stream mode without escapes unless forced).
    pub is_tty: bool,
    pub color: ColorDepth,
    /// OSC 8 hyperlinks.
    pub hyperlinks: bool,
    /// Styled/coloured underlines (SGR 4:x, 58).
    pub styled_underline: bool,
    /// Synchronized output (mode 2026); harmless where unsupported.
    pub sync: bool,
    /// Terminal background colour, if known (OSC 11 or `COLORFGBG`).
    pub background: Option<Rgb>,
    pub graphics: Graphics,
    pub block_glyphs: BlockGlyphSet,
    /// Cell size in pixels, if known.
    pub cell_px: Option<(u16, u16)>,
    /// Terminal size in cells.
    pub size: Option<(u16, u16)>,
    pub in_tmux: bool,
    pub over_ssh: bool,
    /// Terminal identity (XTVERSION or `#{client_termtype}`), e.g. `iTerm2 3.6.9`.
    pub terminal: Option<String>,
    pub reasons: Vec<Reason>,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            is_tty: false,
            color: ColorDepth::None,
            hyperlinks: false,
            styled_underline: false,
            sync: false,
            background: None,
            graphics: Graphics::None,
            block_glyphs: BlockGlyphSet::Half,
            cell_px: None,
            size: None,
            in_tmux: false,
            over_ssh: false,
            terminal: None,
            reasons: Vec::new(),
        }
    }
}

impl Caps {
    /// Plain output: no escape sequences at all (pipes, `TERM=dumb`).
    pub fn plain() -> Caps {
        Caps::default()
    }

    /// A fully capable truecolor terminal (tests and `--color=always`).
    pub fn full() -> Caps {
        Caps {
            is_tty: true,
            color: ColorDepth::TrueColor,
            hyperlinks: true,
            styled_underline: true,
            sync: true,
            graphics: Graphics::Blocks,
            ..Caps::default()
        }
    }
}
