//! Choosing how images are drawn (plan §6): the graphics path from what the
//! terminal revealed, and the block glyph set.
//!
//! [`choose`] is a pure function of a [`Facts`] snapshot that `term::caps`
//! fills from the environment, the tmux query and the probe; its [`Reason`]
//! is what `--doctor` prints. The rules, first match wins:
//!
//! * **Inside tmux:** kitty Unicode placeholders when passthrough is on, the
//!   outer terminal answered the kitty query, its identity supports
//!   placeholders and tmux passes RGB colour (the image id travels in the
//!   24-bit foreground). Otherwise sixel drawn by tmux itself, when tmux
//!   was built with sixel, the client has the `sixel` feature and tmux
//!   knows the client's cell size. Otherwise blocks. Classic kitty
//!   placements and OSC 1337 are never chosen: tmux does not move the outer
//!   cursor for passthrough bytes and erases them on redraw.
//! * **Outside tmux:** placeholders (identity on the list and the kitty
//!   query answered), then OSC 1337 for iTerm2, WezTerm, Tabby, Warp, Rio
//!   and mintty, then classic placements for other terminals that answered
//!   the kitty query (xterm.js/VS Code, Konsole), then sixel when DA1 lists
//!   it and the cell size is known, then blocks.
//!
//! A kitty `a=q` OK only proves the protocol exists: xterm.js, WezTerm and
//! Konsole answer OK without supporting placeholders. That is why
//! placeholders also need the terminal's identity (XTVERSION outside tmux,
//! `#{client_termtype}` inside), see [`Identity::has_placeholders`].
//!
//! Blocks need colour: without at least 16 colours, images are shown as
//! alt text only. Pixels need a terminal: piped output gets blocks.

use std::cmp::Ordering;

use crate::options::{BlockGlyphs, ImageMode};
use crate::term::{BlockGlyphSet, ColorDepth, Graphics, Reason};

/// A terminal's name and version as XTVERSION or tmux's
/// `#{client_termtype}` report them, e.g. `kitty(0.40.1)`, `iTerm2 3.6.9`,
/// `ghostty 1.1.3`, `foot(1.20.2)` or `WezTerm 20240203-110809-5046fc22`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The name, lowercased (`kitty`, `iterm2`, `wezterm`, …).
    pub name: String,
    /// The leading dotted numbers of the version (`[0, 40, 1]`); empty if
    /// none was given.
    pub version: Vec<u32>,
}

impl Identity {
    /// Parse an identity string; `None` if it has no name.
    pub fn parse(s: &str) -> Option<Identity> {
        let s = s.trim();
        let split = s.find(['(', ' ', '/']).unwrap_or(s.len());
        let (name, rest) = s.split_at(split);
        if name.is_empty() {
            return None;
        }
        let rest = rest.trim_start_matches(['(', ' ', '/', 'v']);
        let numeric = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .map_or(rest, |end| &rest[..end]);
        let version = numeric
            .split('.')
            .map_while(|part| part.parse::<u32>().ok())
            .collect();
        Some(Identity {
            name: name.to_ascii_lowercase(),
            version,
        })
    }

    /// Whether the version is at least `min` (missing parts count as 0).
    pub fn at_least(&self, min: &[u32]) -> bool {
        let len = self.version.len().max(min.len());
        let part = |v: &[u32], i: usize| v.get(i).copied().unwrap_or(0);
        for i in 0..len {
            match part(&self.version, i).cmp(&part(min, i)) {
                Ordering::Greater => return true,
                Ordering::Less => return false,
                Ordering::Equal => {}
            }
        }
        true
    }

    fn is(&self, names: &[&str]) -> bool {
        names.contains(&self.name.as_str())
    }

    /// Terminals whose kitty graphics support Unicode placeholders: kitty ≥
    /// 0.28, Ghostty, iTerm2 ≥ 3.6.
    pub fn has_placeholders(&self) -> bool {
        (self.is(&["kitty"]) && self.at_least(&[0, 28]))
            || self.is(&["ghostty"])
            || (self.is(&["iterm2", "iterm.app"]) && self.at_least(&[3, 6]))
    }

    /// Terminals that speak iTerm2's OSC 1337 inline images (by XTVERSION
    /// or `$TERM_PROGRAM` name).
    pub fn speaks_osc1337(&self) -> bool {
        self.is(&[
            "iterm2",
            "iterm.app",
            "wezterm",
            "tabby",
            "warp",
            "warpterminal",
            "rio",
            "mintty",
        ])
    }

    /// Terminals that draw octant glyphs themselves (not from the font):
    /// kitty ≥ 0.40, Ghostty, foot ≥ 1.20.
    pub fn draws_octants(&self) -> bool {
        (self.is(&["kitty"]) && self.at_least(&[0, 40]))
            || self.is(&["ghostty"])
            || (self.is(&["foot"]) && self.at_least(&[1, 20]))
    }

    /// Terminals that draw sextants but not octants themselves (stable
    /// WezTerm).
    pub fn draws_sextants(&self) -> bool {
        self.is(&["wezterm"])
    }
}

/// What tmux revealed (the `tmux display` query and tmux's own DA1 reply).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TmuxFacts {
    /// `allow-passthrough` is on and the configuration lets emde use it.
    pub passthrough: bool,
    /// `#{client_termfeatures}` has `RGB`.
    pub rgb: bool,
    /// tmux draws sixel itself (its DA1 reply lists attribute 4).
    pub sixel: bool,
    /// `#{client_termfeatures}` has `sixel`.
    pub client_sixel: bool,
    /// `#{client_cell_width}x#{client_cell_height}` is not `0x0`.
    pub client_cell_known: bool,
}

/// Everything the graphics choice depends on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Output is a terminal; pixels are only sent to one.
    pub is_tty: bool,
    /// The colour depth in use; blocks need at least 16 colours.
    pub color: ColorDepth,
    /// The terminal identity: XTVERSION outside tmux, `#{client_termtype}`
    /// inside.
    pub identity: Option<String>,
    /// `$TERM_PROGRAM`, or `$LC_TERMINAL` (which SSH forwards): names OSC
    /// 1337 terminals that may not answer XTVERSION.
    pub term_program: Option<String>,
    /// The kitty `a=q` query was answered OK (through passthrough in tmux).
    pub kitty_ok: bool,
    /// DA1 lists attribute 4, sixel (outside tmux).
    pub sixel: bool,
    /// The terminal reported its cell size in pixels.
    pub cell_px_known: bool,
    /// Present inside tmux.
    pub tmux: Option<TmuxFacts>,
}

/// A graphics decision and why it was made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// The chosen path.
    pub graphics: Graphics,
    /// Topic `images`, e.g. `kitty placeholders (iTerm2 3.6.9)` or
    /// `blocks: kitty ✗ passthrough off · sixel ✗ client cell 0x0`.
    pub reason: Reason,
}

fn decision(graphics: Graphics, detail: String) -> Decision {
    Decision {
        graphics,
        reason: Reason {
            topic: "images",
            detail,
        },
    }
}

/// Blocks, or alt text when there are no colours to draw them with.
fn blocks(f: &Facts, why: &str) -> Decision {
    if f.color >= ColorDepth::Ansi16 {
        decision(Graphics::Blocks, format!("blocks: {why}"))
    } else {
        decision(Graphics::None, format!("alt text: no colours ({why})"))
    }
}

impl Facts {
    fn identity(&self) -> Option<Identity> {
        self.identity.as_deref().and_then(Identity::parse)
    }

    /// The identity, or the `$TERM_PROGRAM` name when it is all we have.
    fn program(&self) -> Option<Identity> {
        self.identity()
            .into_iter()
            .chain(self.term_program.as_deref().and_then(Identity::parse))
            .find(Identity::speaks_osc1337)
    }

    fn name(&self) -> String {
        self.identity
            .clone()
            .or_else(|| self.term_program.clone())
            .unwrap_or_else(|| "the terminal".into())
    }

    fn placeholders(&self) -> bool {
        self.kitty_ok && self.identity().is_some_and(|id| id.has_placeholders())
    }
}

/// Decide how images are drawn for the requested `mode`.
pub fn choose(mode: ImageMode, f: &Facts) -> Decision {
    match mode {
        ImageMode::None => decision(Graphics::None, "alt text: images off".into()),
        ImageMode::Blocks => blocks(f, "requested"),
        _ if !f.is_tty => blocks(f, "output is not a terminal"),
        ImageMode::Kitty => forced_kitty(f),
        ImageMode::Iterm => forced_iterm(f),
        ImageMode::Sixel => forced_sixel(f),
        ImageMode::Auto => match f.tmux {
            Some(t) => auto_in_tmux(f, &t),
            None => auto_direct(f),
        },
    }
}

fn auto_in_tmux(f: &Facts, t: &TmuxFacts) -> Decision {
    let name = f.name();
    let kitty: String = if !t.passthrough {
        "passthrough off".into()
    } else if !f.kitty_ok {
        format!("no kitty reply from {name}")
    } else if !f.identity().is_some_and(|id| id.has_placeholders()) {
        format!("no Unicode placeholders in {name}")
    } else if !t.rgb {
        "tmux client lacks RGB".into()
    } else {
        return decision(
            Graphics::KittyPlaceholders,
            format!("kitty placeholders via tmux passthrough ({name})"),
        );
    };
    let sixel = if !t.sixel {
        "tmux built without sixel"
    } else if !t.client_sixel {
        "client lacks the sixel feature"
    } else if !t.client_cell_known {
        "client cell 0x0"
    } else {
        return decision(Graphics::Sixel, "sixel drawn by tmux".into());
    };
    blocks(f, &format!("kitty ✗ {kitty} · sixel ✗ {sixel}"))
}

fn auto_direct(f: &Facts) -> Decision {
    let name = f.name();
    if f.placeholders() {
        return decision(
            Graphics::KittyPlaceholders,
            format!("kitty placeholders ({name})"),
        );
    }
    if let Some(program) = f.program() {
        return decision(Graphics::Iterm, format!("OSC 1337 ({})", program.name));
    }
    if f.kitty_ok {
        return decision(
            Graphics::KittyClassic,
            format!("kitty classic placements ({name} answered the kitty query)"),
        );
    }
    if f.sixel && f.cell_px_known {
        return decision(Graphics::Sixel, format!("sixel ({name})"));
    }
    let sixel = if f.sixel {
        "sixel ✗ cell size unknown"
    } else {
        "no pixel protocol detected"
    };
    blocks(f, sixel)
}

fn forced_kitty(f: &Facts) -> Decision {
    match f.tmux {
        Some(t) if t.passthrough => decision(
            Graphics::KittyPlaceholders,
            "kitty placeholders (requested) via tmux passthrough".into(),
        ),
        Some(_) => blocks(f, "kitty requested, but tmux passthrough is off"),
        None if f.identity().is_some_and(|id| id.has_placeholders()) => decision(
            Graphics::KittyPlaceholders,
            "kitty placeholders (requested)".into(),
        ),
        None => decision(
            Graphics::KittyClassic,
            "kitty classic placements (requested)".into(),
        ),
    }
}

fn forced_iterm(f: &Facts) -> Decision {
    match f.tmux {
        Some(t) if t.passthrough => decision(
            Graphics::Iterm,
            "OSC 1337 (requested) via tmux passthrough; tmux may misplace or erase images".into(),
        ),
        Some(_) => blocks(f, "OSC 1337 requested, but tmux passthrough is off"),
        None => decision(Graphics::Iterm, "OSC 1337 (requested)".into()),
    }
}

fn forced_sixel(f: &Facts) -> Decision {
    match f.tmux {
        Some(t) if !(t.sixel && t.client_sixel && t.client_cell_known) => decision(
            Graphics::Sixel,
            "sixel (requested); tmux may show a placeholder box instead".into(),
        ),
        Some(_) => decision(Graphics::Sixel, "sixel (requested) drawn by tmux".into()),
        None => decision(Graphics::Sixel, "sixel (requested)".into()),
    }
}

/// The block glyph set for the configured choice. `Auto` picks octants on
/// terminals that draw them themselves (kitty ≥ 0.40, Ghostty, foot ≥ 1.20),
/// sextants on WezTerm and half blocks everywhere else, since fonts often
/// lack the newer glyphs.
pub fn block_glyphs(requested: BlockGlyphs, identity: Option<&str>) -> BlockGlyphSet {
    match requested {
        BlockGlyphs::Half => BlockGlyphSet::Half,
        BlockGlyphs::Quadrant => BlockGlyphSet::Quadrant,
        BlockGlyphs::Sextant => BlockGlyphSet::Sextant,
        BlockGlyphs::Octant => BlockGlyphSet::Octant,
        BlockGlyphs::Auto => match identity.and_then(Identity::parse) {
            Some(id) if id.draws_octants() => BlockGlyphSet::Octant,
            Some(id) if id.draws_sextants() => BlockGlyphSet::Sextant,
            _ => BlockGlyphSet::Half,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tty() -> Facts {
        Facts {
            is_tty: true,
            color: ColorDepth::TrueColor,
            ..Facts::default()
        }
    }

    fn id(s: &str) -> Identity {
        Identity::parse(s).unwrap()
    }

    #[test]
    fn identities() {
        assert_eq!(
            id("kitty(0.40.1)"),
            Identity {
                name: "kitty".into(),
                version: vec![0, 40, 1]
            }
        );
        assert_eq!(id("iTerm2 3.6.9").name, "iterm2");
        assert_eq!(id("iTerm2 3.6.9").version, [3, 6, 9]);
        assert_eq!(id("WezTerm 20240203-110809-5046fc22").version, [20240203]);
        assert_eq!(id("ghostty 1.1.3").version, [1, 1, 3]);
        assert_eq!(id("foot(1.20.2)").version, [1, 20, 2]);
        assert_eq!(id("tmux 3.4").name, "tmux");
        assert_eq!(id("tmux next-3.5").version, Vec::<u32>::new());
        assert_eq!(id("iTerm.app").version, Vec::<u32>::new());
        assert_eq!(Identity::parse("  "), None);
        assert_eq!(Identity::parse("(1.2)"), None);
        assert!(id("kitty(0.28.0)").at_least(&[0, 28]));
        assert!(!id("kitty(0.27.9)").at_least(&[0, 28]));
        assert!(id("kitty(1)").at_least(&[0, 28]));
        assert!(!id("kitty").at_least(&[0, 28]));
    }

    #[test]
    fn placeholder_list() {
        assert!(id("kitty(0.28.1)").has_placeholders());
        assert!(!id("kitty(0.27.0)").has_placeholders());
        assert!(id("ghostty 1.0.0").has_placeholders());
        assert!(id("iTerm2 3.6.9").has_placeholders());
        assert!(!id("iTerm2 3.5.14").has_placeholders());
        assert!(!id("WezTerm 20240203-110809-5046fc22").has_placeholders());
        assert!(!id("xterm.js(5.5.0)").has_placeholders());
        assert!(!id("Konsole 24.02.1").has_placeholders());
    }

    #[test]
    fn this_users_tmux_gets_blocks_with_the_passthrough_hint() {
        let f = Facts {
            identity: Some("iTerm2 3.6.9".into()),
            tmux: Some(TmuxFacts {
                passthrough: false,
                rgb: true,
                sixel: true,
                client_sixel: true,
                client_cell_known: false,
            }),
            ..tty()
        };
        let d = choose(ImageMode::Auto, &f);
        assert_eq!(d.graphics, Graphics::Blocks);
        assert_eq!(
            d.reason.detail,
            "blocks: kitty ✗ passthrough off · sixel ✗ client cell 0x0"
        );
        assert_eq!(d.reason.topic, "images");
    }

    #[test]
    fn tmux_with_passthrough_and_iterm2_gets_placeholders() {
        let mut f = Facts {
            identity: Some("iTerm2 3.6.9".into()),
            kitty_ok: true,
            tmux: Some(TmuxFacts {
                passthrough: true,
                rgb: true,
                ..TmuxFacts::default()
            }),
            ..tty()
        };
        assert_eq!(
            choose(ImageMode::Auto, &f).graphics,
            Graphics::KittyPlaceholders
        );
        // Without RGB the id in the foreground colour would be mangled.
        f.tmux = f.tmux.map(|t| TmuxFacts { rgb: false, ..t });
        assert_eq!(choose(ImageMode::Auto, &f).graphics, Graphics::Blocks);
        // VS Code outside tmux: kitty OK but no placeholders → never classic
        // inside tmux.
        f.identity = Some("xterm.js(5.5.0)".into());
        f.tmux = f.tmux.map(|t| TmuxFacts { rgb: true, ..t });
        let d = choose(ImageMode::Auto, &f);
        assert_eq!(d.graphics, Graphics::Blocks);
        assert!(d.reason.detail.contains("no Unicode placeholders"), "{d:?}");
    }

    #[test]
    fn tmux_native_sixel() {
        let f = Facts {
            tmux: Some(TmuxFacts {
                sixel: true,
                client_sixel: true,
                client_cell_known: true,
                ..TmuxFacts::default()
            }),
            ..tty()
        };
        assert_eq!(choose(ImageMode::Auto, &f).graphics, Graphics::Sixel);
    }

    #[test]
    fn direct_preference_order() {
        let kitty = Facts {
            identity: Some("kitty(0.40.1)".into()),
            kitty_ok: true,
            ..tty()
        };
        assert_eq!(
            choose(ImageMode::Auto, &kitty).graphics,
            Graphics::KittyPlaceholders
        );
        // iTerm2 over SSH without kitty support: OSC 1337 from LC_TERMINAL.
        let iterm = Facts {
            term_program: Some("iTerm2".into()),
            ..tty()
        };
        let d = choose(ImageMode::Auto, &iterm);
        assert_eq!(d.graphics, Graphics::Iterm);
        assert_eq!(d.reason.detail, "OSC 1337 (iterm2)");
        // WezTerm answers the kitty query but prefers OSC 1337.
        let wezterm = Facts {
            identity: Some("WezTerm 20240203-110809-5046fc22".into()),
            kitty_ok: true,
            ..tty()
        };
        assert_eq!(choose(ImageMode::Auto, &wezterm).graphics, Graphics::Iterm);
        // VS Code with images enabled answers the kitty query.
        let vscode = Facts {
            identity: Some("xterm.js(5.5.0)".into()),
            term_program: Some("vscode".into()),
            kitty_ok: true,
            ..tty()
        };
        assert_eq!(
            choose(ImageMode::Auto, &vscode).graphics,
            Graphics::KittyClassic
        );
        let foot = Facts {
            identity: Some("foot(1.20.2)".into()),
            sixel: true,
            cell_px_known: true,
            ..tty()
        };
        assert_eq!(choose(ImageMode::Auto, &foot).graphics, Graphics::Sixel);
        let no_size = Facts {
            cell_px_known: false,
            ..foot
        };
        let d = choose(ImageMode::Auto, &no_size);
        assert_eq!(d.graphics, Graphics::Blocks);
        assert_eq!(d.reason.detail, "blocks: sixel ✗ cell size unknown");
        assert_eq!(choose(ImageMode::Auto, &tty()).graphics, Graphics::Blocks);
    }

    #[test]
    fn colour_and_tty_gates() {
        let mono = Facts {
            color: ColorDepth::Mono,
            ..tty()
        };
        assert_eq!(choose(ImageMode::Auto, &mono).graphics, Graphics::None);
        assert_eq!(choose(ImageMode::Blocks, &mono).graphics, Graphics::None);
        let piped = Facts {
            is_tty: false,
            identity: Some("kitty(0.40.1)".into()),
            kitty_ok: true,
            ..tty()
        };
        let d = choose(ImageMode::Kitty, &piped);
        assert_eq!(d.graphics, Graphics::Blocks);
        assert_eq!(d.reason.detail, "blocks: output is not a terminal");
        assert_eq!(choose(ImageMode::None, &tty()).graphics, Graphics::None);
    }

    #[test]
    fn forced_modes() {
        let plain = tty();
        assert_eq!(
            choose(ImageMode::Kitty, &plain).graphics,
            Graphics::KittyClassic
        );
        let ghostty = Facts {
            identity: Some("ghostty 1.1.3".into()),
            ..tty()
        };
        assert_eq!(
            choose(ImageMode::Kitty, &ghostty).graphics,
            Graphics::KittyPlaceholders
        );
        assert_eq!(choose(ImageMode::Iterm, &plain).graphics, Graphics::Iterm);
        assert_eq!(choose(ImageMode::Sixel, &plain).graphics, Graphics::Sixel);
        let tmux_off = Facts {
            tmux: Some(TmuxFacts::default()),
            ..tty()
        };
        assert_eq!(
            choose(ImageMode::Kitty, &tmux_off).graphics,
            Graphics::Blocks
        );
        assert_eq!(
            choose(ImageMode::Iterm, &tmux_off).graphics,
            Graphics::Blocks
        );
        assert_eq!(
            choose(ImageMode::Sixel, &tmux_off).graphics,
            Graphics::Sixel
        );
        let tmux_on = Facts {
            tmux: Some(TmuxFacts {
                passthrough: true,
                ..TmuxFacts::default()
            }),
            ..tty()
        };
        assert_eq!(
            choose(ImageMode::Kitty, &tmux_on).graphics,
            Graphics::KittyPlaceholders
        );
        assert_eq!(choose(ImageMode::Iterm, &tmux_on).graphics, Graphics::Iterm);
    }

    #[test]
    fn glyph_sets() {
        use BlockGlyphs as B;
        assert_eq!(
            block_glyphs(B::Half, Some("kitty(0.40.1)")),
            BlockGlyphSet::Half
        );
        assert_eq!(block_glyphs(B::Octant, None), BlockGlyphSet::Octant);
        assert_eq!(block_glyphs(B::Quadrant, None), BlockGlyphSet::Quadrant);
        assert_eq!(block_glyphs(B::Sextant, None), BlockGlyphSet::Sextant);
        assert_eq!(
            block_glyphs(B::Auto, Some("kitty(0.40.1)")),
            BlockGlyphSet::Octant
        );
        assert_eq!(
            block_glyphs(B::Auto, Some("kitty(0.39.0)")),
            BlockGlyphSet::Half
        );
        assert_eq!(
            block_glyphs(B::Auto, Some("ghostty 1.1.3")),
            BlockGlyphSet::Octant
        );
        assert_eq!(
            block_glyphs(B::Auto, Some("foot(1.20.2)")),
            BlockGlyphSet::Octant
        );
        assert_eq!(
            block_glyphs(B::Auto, Some("foot(1.19.0)")),
            BlockGlyphSet::Half
        );
        assert_eq!(
            block_glyphs(B::Auto, Some("WezTerm 20240203-110809-5046fc22")),
            BlockGlyphSet::Sextant
        );
        assert_eq!(
            block_glyphs(B::Auto, Some("iTerm2 3.6.9")),
            BlockGlyphSet::Half
        );
        assert_eq!(block_glyphs(B::Auto, None), BlockGlyphSet::Half);
    }
}
