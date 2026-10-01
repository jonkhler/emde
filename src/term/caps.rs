//! Final capability decision (`decide`): combines the environment snapshot,
//! the optional tmux query and the optional probe into [`super::Caps`].
//!
//! Colour depth, hyperlinks and underline styles arrive already decided in
//! the base [`Caps`] (from `term::color`). [`decide`] adds the terminal
//! identity, `in_tmux` and `over_ssh`, the background colour, the cell size,
//! the graphics path and the block glyphs, and records a [`Reason`] for each.
//! It is pure: all input comes from its arguments.
//!
//! # Graphics for `images = "auto"`
//!
//! A kitty `a=q` OK only shows that the protocol exists, not that Unicode
//! placeholders work: xterm.js (VS Code), WezTerm and Konsole answer OK but
//! have no placeholders. So placeholders also need the terminal's *identity*
//! on an allowlist (kitty ≥ 0.28, Ghostty, iTerm2 ≥ 3.6), taken from
//! XTVERSION outside tmux or `#{client_termtype}` inside it.
//!
//! Inside tmux:
//! 1. kitty placeholders when passthrough is on, the outer terminal answered
//!    the wrapped `a=q` OK, its identity is allowlisted and the client has
//!    `RGB` (the image id travels in a 24-bit colour);
//! 2. tmux's own sixel when tmux's DA1 has `4`, the client has the `sixel`
//!    feature and the client cell size is not `0x0`;
//! 3. blocks. Classic kitty placements and iTerm2 images are never used:
//!    tmux does not move the outer cursor for passthrough bytes, so images
//!    land in the wrong place, and tmux redraws erase them.
//!
//! Outside tmux, in order:
//! 1. kitty placeholders (allowlisted identity and `a=q` OK);
//! 2. iTerm2 OSC 1337 for iTerm2 without kitty, WezTerm, Tabby, Warp, Rio
//!    and mintty (by XTVERSION or `TERM_PROGRAM`; iTerm2 also by
//!    `LC_TERMINAL`, which SSH forwards);
//! 3. classic kitty placements for other terminals that answer `a=q` OK
//!    (VS Code / xterm.js, Konsole, kitty < 0.28);
//! 4. sixel when DA1 has `4` and the cell size is known;
//! 5. blocks.
//!
//! Detection only sends pixels to a terminal on stdout; without colour,
//! block images give way to alt-text boxes ([`Graphics::None`]), and output
//! that takes no escape sequences at all ([`ColorDepth::None`]) gets alt
//! text in every mode.
//!
//! An explicit `images` mode skips detection (also when stdout is not a
//! terminal: the user asked for it) and is only refused, with a reason,
//! where it cannot work: iTerm2 images and classic kitty inside tmux,
//! kitty placeholders in tmux without passthrough or `RGB`, tmux sixel
//! without tmux sixel support, the client's `sixel` feature or a known cell
//! size, and modes the binary was built without.

use super::env::Env;
use super::probe::ProbeReplies;
use super::tmux::TmuxInfo;
use super::{BlockGlyphSet, Caps, ColorDepth, Graphics, Reason, topic};
use crate::color::xterm_rgb;
use crate::options::{BlockGlyphs, ImageMode, ImageOptions};
use crate::style::Rgb;

/// The binary can decode images at all.
const IMAGES_BUILT: bool = cfg!(feature = "images");
/// The binary has the sixel encoder.
const SIXEL_BUILT: bool = cfg!(feature = "sixel");

/// A terminal emulator emde knows by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emulator {
    Kitty,
    Ghostty,
    Iterm2,
    WezTerm,
    Foot,
    XTerm,
    /// The xterm.js library, as its XTVERSION answer names it in every
    /// host (VS Code, Tabby, …); `TERM_PROGRAM` names the host.
    XtermJs,
    VsCode,
    Konsole,
    Tabby,
    Warp,
    Rio,
    Mintty,
    Contour,
    Alacritty,
    AppleTerminal,
    WindowsTerminal,
    Vte,
    Tmux,
    Unknown,
}

impl Emulator {
    /// Names as terminals report them in XTVERSION and `TERM_PROGRAM`.
    const NAMES: &'static [(&'static str, Emulator)] = &[
        ("kitty", Emulator::Kitty),
        ("ghostty", Emulator::Ghostty),
        ("iTerm2", Emulator::Iterm2),
        ("iTerm.app", Emulator::Iterm2),
        ("WezTerm", Emulator::WezTerm),
        ("foot", Emulator::Foot),
        ("XTerm", Emulator::XTerm),
        ("xterm.js", Emulator::XtermJs),
        ("vscode", Emulator::VsCode),
        ("Konsole", Emulator::Konsole),
        ("Tabby", Emulator::Tabby),
        ("WarpTerminal", Emulator::Warp),
        ("rio", Emulator::Rio),
        ("mintty", Emulator::Mintty),
        ("contour", Emulator::Contour),
        ("alacritty", Emulator::Alacritty),
        ("Apple_Terminal", Emulator::AppleTerminal),
        ("tmux", Emulator::Tmux),
    ];

    /// Recognise a reported name, ignoring case.
    pub fn from_name(name: &str) -> Emulator {
        let name = name.trim();
        Emulator::NAMES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map_or(Emulator::Unknown, |&(_, e)| e)
    }

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            Emulator::Kitty => "kitty",
            Emulator::Ghostty => "Ghostty",
            Emulator::Iterm2 => "iTerm2",
            Emulator::WezTerm => "WezTerm",
            Emulator::Foot => "foot",
            Emulator::XTerm => "xterm",
            Emulator::XtermJs => "xterm.js",
            Emulator::VsCode => "VS Code",
            Emulator::Konsole => "Konsole",
            Emulator::Tabby => "Tabby",
            Emulator::Warp => "Warp",
            Emulator::Rio => "Rio",
            Emulator::Mintty => "mintty",
            Emulator::Contour => "Contour",
            Emulator::Alacritty => "Alacritty",
            Emulator::AppleTerminal => "Terminal.app",
            Emulator::WindowsTerminal => "Windows Terminal",
            Emulator::Vte => "VTE",
            Emulator::Tmux => "tmux",
            Emulator::Unknown => "unknown terminal",
        }
    }

    /// Shows iTerm2 inline images (OSC 1337) reliably.
    fn speaks_osc1337(self) -> bool {
        matches!(
            self,
            Emulator::Iterm2
                | Emulator::WezTerm
                | Emulator::Tabby
                | Emulator::Warp
                | Emulator::Rio
                | Emulator::Mintty
        )
    }
}

/// A dotted numeric version (`0.49.1`, `3.6.9`, `20240203`). Versions compare
/// component-wise, with missing components as zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Version(Vec<u32>);

impl Version {
    /// The leading numeric components of `s`: `3.3a` is 3.3, `1.20.2-dev`
    /// is 1.20.2, `20240203-110809-5046fc22` is 20240203, `next-3.5` is empty.
    pub fn parse(s: &str) -> Version {
        let mut parts = Vec::new();
        for component in s.trim().split('.') {
            let end = component
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(component.len());
            let Some(Ok(n)) = component.get(..end).map(str::parse::<u32>) else {
                break;
            };
            parts.push(n);
            if end < component.len() {
                break;
            }
        }
        Version(parts)
    }

    /// The version has at least one component.
    pub fn is_known(&self) -> bool {
        !self.0.is_empty()
    }

    /// Whether this version is at least `min`. An unknown version never is.
    pub fn at_least(&self, min: &[u32]) -> bool {
        if !self.is_known() {
            return false;
        }
        let len = self.0.len().max(min.len());
        for i in 0..len {
            let (have, want) = (
                self.0.get(i).copied().unwrap_or(0),
                min.get(i).copied().unwrap_or(0),
            );
            if have != want {
                return have > want;
            }
        }
        true
    }
}

/// Where an [`Identity`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentitySource {
    /// The terminal's XTVERSION answer.
    Xtversion,
    /// tmux's `#{client_termtype}`: the outer terminal's XTVERSION answer.
    ClientTermtype,
    /// An environment variable (`TERM_PROGRAM`, `LC_TERMINAL`, `TERM`, …).
    Env(&'static str),
}

impl IdentitySource {
    /// Short label for reasons (`XTVERSION`, `client_termtype`, `TERM_PROGRAM`).
    pub fn label(self) -> &'static str {
        match self {
            IdentitySource::Xtversion => "XTVERSION",
            IdentitySource::ClientTermtype => "client_termtype",
            IdentitySource::Env(var) => var,
        }
    }

    /// The identity was reported by the terminal rather than guessed from
    /// the environment.
    pub fn is_reported(self) -> bool {
        !matches!(self, IdentitySource::Env(_))
    }
}

/// Which terminal emde is talking to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The emulator, if recognised.
    pub emulator: Emulator,
    /// Its version, if known.
    pub version: Version,
    /// As reported (`kitty(0.49.1)`, `iTerm2 3.6.9`) or built from the
    /// environment (`VS Code 1.105.0`).
    pub text: String,
    /// Where it came from.
    pub source: IdentitySource,
}

impl Identity {
    /// Parse an XTVERSION-style answer: a name, then a version after a space
    /// or in parentheses (`kitty(0.49.1)`, `ghostty 1.3.1`, `tmux 3.4`).
    pub fn from_xtversion(text: &str, source: IdentitySource) -> Option<Identity> {
        let text = clean(text);
        if text.is_empty() {
            return None;
        }
        let (name, rest) = text.split_once([' ', '(']).unwrap_or((&text, ""));
        Some(Identity {
            emulator: Emulator::from_name(name),
            version: Version::parse(rest.trim().trim_end_matches(')')),
            text: text.clone(),
            source,
        })
    }

    /// Guess the terminal from environment variables, most reliable first:
    /// `TERM_PROGRAM`, `LC_TERMINAL` (forwarded by SSH), `TERM`, then
    /// terminal-specific variables. Inside tmux these describe whoever
    /// started the server, so callers only use this outside tmux.
    pub fn from_env(env: &Env) -> Option<Identity> {
        let named = |emulator: Emulator, raw: &str, version: Option<&str>, var: &'static str| {
            let version = version.map(str::trim).filter(|v| !v.is_empty());
            let name = match emulator {
                Emulator::Unknown => raw,
                known => known.name(),
            };
            let text = match version {
                Some(v) => format!("{name} {v}"),
                None => name.to_string(),
            };
            Identity {
                emulator,
                version: Version::parse(version.unwrap_or("")),
                text: clean(&text),
                source: IdentitySource::Env(var),
            }
        };
        if let Some(program) = env
            .non_empty("TERM_PROGRAM")
            .filter(|p| !p.eq_ignore_ascii_case("tmux"))
        {
            let version = env.get("TERM_PROGRAM_VERSION");
            let emulator = Emulator::from_name(program);
            return Some(named(emulator, program, version, "TERM_PROGRAM"));
        }
        if env.get("LC_TERMINAL") == Some("iTerm2") {
            let version = env.get("LC_TERMINAL_VERSION");
            return Some(named(Emulator::Iterm2, "iTerm2", version, "LC_TERMINAL"));
        }
        let by_term = match env.get("TERM").unwrap_or("") {
            "xterm-kitty" => Some(Emulator::Kitty),
            "xterm-ghostty" | "ghostty" => Some(Emulator::Ghostty),
            "foot" | "foot-extra" => Some(Emulator::Foot),
            "wezterm" => Some(Emulator::WezTerm),
            "rio" => Some(Emulator::Rio),
            "alacritty" => Some(Emulator::Alacritty),
            "contour" => Some(Emulator::Contour),
            _ => None,
        };
        if let Some(emulator) = by_term {
            return Some(named(emulator, "", None, "TERM"));
        }
        const HINTS: &[(&str, Emulator)] = &[
            ("KITTY_WINDOW_ID", Emulator::Kitty),
            ("GHOSTTY_RESOURCES_DIR", Emulator::Ghostty),
            ("WEZTERM_EXECUTABLE", Emulator::WezTerm),
            ("KONSOLE_VERSION", Emulator::Konsole),
            ("VSCODE_INJECTION", Emulator::VsCode),
            ("WT_SESSION", Emulator::WindowsTerminal),
            ("VTE_VERSION", Emulator::Vte),
        ];
        HINTS
            .iter()
            .find(|(var, _)| env.is_set(var))
            .map(|&(var, emulator)| named(emulator, "", None, var))
    }

    /// Draws kitty Unicode placeholders: kitty ≥ 0.28, Ghostty, iTerm2 ≥ 3.6.
    pub fn has_placeholders(&self) -> bool {
        match self.emulator {
            Emulator::Kitty => self.version.at_least(&[0, 28]),
            Emulator::Ghostty => true,
            Emulator::Iterm2 => self.version.at_least(&[3, 6]),
            _ => false,
        }
    }

    /// The glyph set `blocks = "auto"` picks: octants where the terminal
    /// draws them itself (kitty ≥ 0.40, Ghostty, foot ≥ 1.20), sextants on
    /// WezTerm (which draws sextants but, in its stable release, not octants).
    fn auto_glyphs(&self) -> Option<BlockGlyphSet> {
        match self.emulator {
            Emulator::Kitty if self.version.at_least(&[0, 40]) => Some(BlockGlyphSet::Octant),
            Emulator::Ghostty => Some(BlockGlyphSet::Octant),
            Emulator::Foot if self.version.at_least(&[1, 20]) => Some(BlockGlyphSet::Octant),
            Emulator::WezTerm => Some(BlockGlyphSet::Sextant),
            _ => None,
        }
    }
}

/// Decide the capabilities `base` leaves open.
///
/// `base` carries the colour, hyperlink and underline decisions (and may
/// preset `size`, `background` or `cell_px`); its reasons are kept and new
/// ones appended. `tmux` is the tmux query result and `probe` the probe
/// replies ([`ProbeOutcome::answers`](super::probe::ProbeOutcome::answers)),
/// when they ran.
pub fn decide(
    env: &Env,
    base: Caps,
    tmux: Option<&TmuxInfo>,
    probe: Option<&ProbeReplies>,
    opts: &ImageOptions,
) -> Caps {
    let facts = Facts::new(env, tmux, probe);
    let mut caps = base;
    let mut reasons = Vec::new();
    let mut note = |topic: &'static str, detail: String| reasons.push(Reason { topic, detail });

    caps.in_tmux = facts.in_tmux;
    note(topic::TMUX, facts.tmux_reason());
    caps.over_ssh = facts.over_ssh.is_some();
    note(topic::SSH, facts.over_ssh.unwrap_or("local").to_string());

    let (terminal, why) = facts.terminal();
    caps.terminal = terminal;
    note(topic::TERMINAL, why);

    let (background, why) = facts.background(caps.background);
    caps.background = background;
    note(topic::BACKGROUND, why);

    let (cell, why) = facts.cell_size(caps.cell_px);
    caps.cell_px = cell;
    note(topic::CELL, why);
    caps.size = caps.size.or_else(|| probe.and_then(|p| p.size_cells));

    let choice = graphics(&facts, &caps, opts);
    caps.graphics = choice.graphics;
    note(topic::GRAPHICS, choice.detail);

    let (glyphs, why) = facts.glyphs(opts.blocks);
    caps.block_glyphs = glyphs;
    note(topic::BLOCKS, why);

    caps.reasons.extend(reasons);
    caps
}

/// `$TERM` names a terminal multiplexer (`tmux*`, `screen*`). `TERM` travels
/// over SSH while `$TMUX` does not, so after `ssh` from a tmux pane (or
/// under GNU screen) a multiplexer still answers every query, and an
/// unwrapped APC would set its pane title or hardstatus line.
pub fn multiplexer_term(env: &Env) -> bool {
    env.get("TERM")
        .is_some_and(|t| t.starts_with("tmux") || t.starts_with("screen"))
}

/// Queries reach a terminal multiplexer rather than the terminal: `$TMUX`
/// is set, `TERM_PROGRAM` is `tmux` (tmux exports it into panes, and it
/// survives an `unset TMUX`), or [`multiplexer_term`].
///
/// The probe then never sends the kitty query unwrapped (it would set the
/// pane title), and environment hints are ignored: they describe the
/// terminal outside, which images cannot reach.
pub fn behind_multiplexer(env: &Env) -> bool {
    env.is_set("TMUX")
        || env
            .get("TERM_PROGRAM")
            .is_some_and(|p| p.eq_ignore_ascii_case("tmux"))
        || multiplexer_term(env)
}

/// Whether kitty placeholders could reach the outer terminal through tmux:
/// passthrough is on, the client has `RGB` (the image id travels in a 24-bit
/// colour) and its `#{client_termtype}` is on the placeholder allowlist.
///
/// Only then can the outer terminal's `a=q` answer change a decision, so the
/// probe only asks it through passthrough in this case (the query would also
/// set the pane title of an outer tmux, and wait for a round trip over SSH).
pub fn tmux_placeholder_candidate(tmux: &TmuxInfo) -> bool {
    tmux.passthrough.enabled()
        && tmux.has_feature("RGB")
        && Identity::from_xtversion(&tmux.client_termtype, IdentitySource::ClientTermtype)
            .is_some_and(|id| id.has_placeholders())
}

/// Config name of a glyph set.
pub fn glyph_name(set: BlockGlyphSet) -> &'static str {
    match set {
        BlockGlyphSet::Half => "half",
        BlockGlyphSet::Quadrant => "quadrant",
        BlockGlyphSet::Sextant => "sextant",
        BlockGlyphSet::Octant => "octant",
    }
}

/// What the decisions draw on, gathered once.
struct Facts<'a> {
    env: &'a Env,
    tmux: Option<&'a TmuxInfo>,
    probe: Option<&'a ProbeReplies>,
    in_tmux: bool,
    /// The variable that shows an SSH session, if any.
    over_ssh: Option<&'static str>,
    /// The identity that may unlock placeholders: XTVERSION outside tmux,
    /// `#{client_termtype}` inside it.
    reported: Option<Identity>,
    /// The identity from environment variables (outside tmux only).
    hinted: Option<Identity>,
}

impl<'a> Facts<'a> {
    fn new(env: &'a Env, tmux: Option<&'a TmuxInfo>, probe: Option<&'a ProbeReplies>) -> Self {
        let xtversion = probe
            .and_then(|p| p.xtversion.as_deref())
            .and_then(|x| Identity::from_xtversion(x, IdentitySource::Xtversion));
        // tmux answers XTVERSION itself, so it also shows tmux when `$TMUX`
        // was dropped from the environment (`sudo`, `env -i`).
        let in_tmux = env.is_set("TMUX")
            || xtversion
                .as_ref()
                .is_some_and(|x| x.emulator == Emulator::Tmux);
        let reported = if in_tmux {
            tmux.and_then(|t| {
                Identity::from_xtversion(&t.client_termtype, IdentitySource::ClientTermtype)
            })
        } else {
            xtversion
        };
        // Behind a multiplexer, environment hints describe the terminal
        // outside it, which images cannot reach.
        let hinted = if in_tmux || behind_multiplexer(env) {
            None
        } else {
            Identity::from_env(env)
        };
        let over_ssh = ["SSH_CONNECTION", "SSH_TTY"]
            .into_iter()
            .find(|v| env.is_set(v));
        Facts {
            env,
            tmux,
            probe,
            in_tmux,
            over_ssh,
            reported,
            hinted,
        }
    }

    /// The best identity: reported, else guessed from the environment.
    fn identity(&self) -> Option<&Identity> {
        self.reported.as_ref().or(self.hinted.as_ref())
    }

    /// The terminal itself answered the kitty query (outside tmux).
    fn kitty_ok(&self) -> bool {
        self.probe.is_some_and(|p| p.kitty_ok)
    }

    fn tmux_reason(&self) -> String {
        match (self.in_tmux, self.tmux) {
            (false, _) => "not in tmux".into(),
            (true, Some(t)) => {
                let cell = match t.cell {
                    Some((w, h)) => format!("{w}x{h}"),
                    None => "0x0".into(),
                };
                format!(
                    "tmux {}, passthrough {}, client cell {cell}",
                    t.version,
                    t.passthrough.as_str()
                )
            }
            (true, None) if self.env.is_set("TMUX") => "$TMUX set, no client info".into(),
            (true, None) => "XTVERSION says tmux, $TMUX unset".into(),
        }
    }

    fn terminal(&self) -> (Option<String>, String) {
        match self.identity() {
            Some(id) => (Some(id.text.clone()), id.source.label().to_string()),
            None => {
                let why = if self.in_tmux {
                    "no client_termtype"
                } else if self.probe.is_some() {
                    "no XTVERSION reply"
                } else {
                    "not probed"
                };
                (None, why.to_string())
            }
        }
    }

    /// OSC 11 from the probe, else `COLORFGBG`, else what `preset` says.
    fn background(&self, preset: Option<Rgb>) -> (Option<Rgb>, String) {
        if let Some(bg) = self.probe.and_then(|p| p.background) {
            let via = if self.in_tmux {
                "tmux OSC 11"
            } else {
                "OSC 11"
            };
            return (Some(bg), via.into());
        }
        if let Some(bg) = colorfgbg(self.env) {
            return (Some(bg), "COLORFGBG".into());
        }
        if let Some(bg) = preset {
            return (Some(bg), "preset".into());
        }
        let why = if self.probe.is_some() {
            "no OSC 11 reply, no COLORFGBG"
        } else {
            "not probed, no COLORFGBG"
        };
        (None, why.into())
    }

    /// `CSI 16 t`, else `CSI 14 t` / `CSI 18 t`, else what `preset` says.
    ///
    /// Inside tmux those answers give tmux's window cell: the largest any
    /// attached client reports, or tmux's 16×32 default when none does. So
    /// with the tmux query at hand only the client's own cell counts, and
    /// `0x0` leaves the size unknown.
    fn cell_size(&self, preset: Option<(u16, u16)>) -> (Option<(u16, u16)>, String) {
        if let (true, Some(tmux)) = (self.in_tmux, self.tmux) {
            return match (tmux.cell, preset) {
                (Some(cell), _) => (Some(cell), "tmux client cell".into()),
                (None, Some(cell)) => (Some(cell), "preset".into()),
                (None, None) => (None, "client cell 0x0".into()),
            };
        }
        let via = if self.in_tmux { "tmux " } else { "" };
        if let Some(p) = self.probe {
            if let Some(cell) = p.cell_px {
                return (Some(cell), format!("{via}16t"));
            }
            if let Some(cell) = p.cell_size() {
                return (Some(cell), format!("{via}14t/18t"));
            }
        }
        if let Some(cell) = preset {
            return (Some(cell), "preset".into());
        }
        let why = if self.probe.is_some() {
            "no size reply"
        } else {
            "not probed"
        };
        (None, why.into())
    }

    fn glyphs(&self, wanted: BlockGlyphs) -> (BlockGlyphSet, String) {
        let fixed = match wanted {
            BlockGlyphs::Half => Some(BlockGlyphSet::Half),
            BlockGlyphs::Quadrant => Some(BlockGlyphSet::Quadrant),
            BlockGlyphs::Sextant => Some(BlockGlyphSet::Sextant),
            BlockGlyphs::Octant => Some(BlockGlyphSet::Octant),
            BlockGlyphs::Auto => None,
        };
        if let Some(set) = fixed {
            return (set, format!("blocks = {}", glyph_name(set)));
        }
        match self.identity() {
            Some(id) => match id.auto_glyphs() {
                Some(set) => (set, format!("{} draws {}s", id.text, glyph_name(set))),
                None => (
                    BlockGlyphSet::Half,
                    format!("{} not known to draw sextants or octants", id.text),
                ),
            },
            None => (BlockGlyphSet::Half, "terminal unknown".into()),
        }
    }
}

/// The background from `COLORFGBG` (`fg;bg` or `fg;default;bg`): its last
/// field as one of the 16 ANSI colours (xterm's values).
fn colorfgbg(env: &Env) -> Option<Rgb> {
    let bg = env.non_empty("COLORFGBG")?.rsplit(';').next()?.trim();
    let index: u8 = bg.parse().ok()?;
    (index < 16).then(|| xterm_rgb(index))
}

/// A graphics decision and why.
struct Choice {
    graphics: Graphics,
    detail: String,
}

impl Choice {
    /// `graphics` because of `why`, after the better options in `notes`
    /// were ruled out.
    fn new(graphics: Graphics, why: impl Into<String>, notes: &[String]) -> Choice {
        let mut parts = vec![why.into()];
        parts.extend(notes.iter().cloned());
        parts.retain(|p| !p.is_empty());
        Choice {
            graphics,
            detail: parts.join(" · "),
        }
    }
}

/// Text rendering when no pixel protocol fits; block images need colour.
fn fallback(caps: &Caps, notes: &[String]) -> Choice {
    if caps.color >= ColorDepth::Ansi16 {
        Choice::new(Graphics::Blocks, "", notes)
    } else {
        let mut notes = notes.to_vec();
        notes.push("blocks ✗ no colour".into());
        Choice::new(Graphics::None, "", &notes)
    }
}

fn graphics(facts: &Facts<'_>, caps: &Caps, opts: &ImageOptions) -> Choice {
    let forced = |name: &str, result: Result<(Graphics, String), String>| {
        let requested = format!("images = {name}");
        match result {
            Ok((graphics, why)) => Choice::new(graphics, requested, &[why]),
            Err(note) => fallback(caps, &[requested, note]),
        }
    };
    match opts.mode {
        ImageMode::None => Choice::new(Graphics::None, "images = none", &[]),
        _ if !IMAGES_BUILT => Choice::new(Graphics::None, "built without image support", &[]),
        _ if caps.color == ColorDepth::None => {
            Choice::new(Graphics::None, "no escape sequences", &[])
        }
        ImageMode::Auto => auto(facts, caps, opts),
        ImageMode::Blocks => fallback(caps, &["images = blocks".into()]),
        ImageMode::Kitty => forced("kitty", forced_kitty(facts, opts)),
        ImageMode::Iterm => forced("iterm", forced_iterm(facts)),
        ImageMode::Sixel => forced("sixel", forced_sixel(facts)),
    }
}

fn auto(facts: &Facts<'_>, caps: &Caps, opts: &ImageOptions) -> Choice {
    if !caps.is_tty {
        return fallback(caps, &["pixels ✗ output is not a terminal".into()]);
    }
    if facts.in_tmux {
        auto_in_tmux(facts, caps, opts)
    } else {
        auto_direct(facts, caps)
    }
}

fn auto_in_tmux(facts: &Facts<'_>, caps: &Caps, opts: &ImageOptions) -> Choice {
    let mut notes = Vec::new();
    match tmux_placeholders(facts, opts, true) {
        Ok(why) => return Choice::new(Graphics::KittyPlaceholders, why, &notes),
        Err(note) => notes.push(note),
    }
    match tmux_sixel(facts) {
        Ok(why) => return Choice::new(Graphics::Sixel, why, &notes),
        Err(note) => notes.push(note),
    }
    fallback(caps, &notes)
}

/// kitty placeholders through tmux passthrough. With `check_outer` (auto
/// mode) the outer terminal must also be on the placeholder allowlist and
/// have answered the wrapped `a=q`.
fn tmux_placeholders(
    facts: &Facts<'_>,
    opts: &ImageOptions,
    check_outer: bool,
) -> Result<String, String> {
    let Some(tmux) = facts.tmux else {
        return Err("kitty/iterm ✗ no tmux client info".into());
    };
    if !opts.tmux_passthrough {
        return Err("kitty/iterm ✗ images.tmux_passthrough off".into());
    }
    if !tmux.passthrough.enabled() {
        return Err("kitty/iterm ✗ passthrough off".into());
    }
    let outer = facts.reported.as_ref();
    let name = outer.map_or("the outer terminal", |id| id.text.as_str());
    if check_outer {
        match outer {
            None => return Err("kitty ✗ outer terminal unknown (no client_termtype)".into()),
            Some(id) if !id.has_placeholders() => {
                return Err(format!("kitty ✗ {name} has no Unicode placeholders"));
            }
            Some(_) => {}
        }
    }
    if !tmux.has_feature("RGB") {
        return Err("kitty ✗ client lacks RGB".into());
    }
    let passthrough = tmux.passthrough.as_str();
    if !check_outer {
        return Ok(format!("passthrough {passthrough}"));
    }
    if !facts.probe.is_some_and(|p| p.outer_kitty_ok) {
        return Err(format!("kitty ✗ no a=q reply from {name}"));
    }
    Ok(format!("passthrough {passthrough}, {name} answered a=q"))
}

/// tmux draws the sixel itself, rescaled for its client.
fn tmux_sixel(facts: &Facts<'_>) -> Result<String, String> {
    if !SIXEL_BUILT {
        return Err("sixel ✗ built without sixel".into());
    }
    let Some(tmux) = facts.tmux else {
        return Err("sixel ✗ no tmux client info".into());
    };
    match facts.probe {
        None => return Err("sixel ✗ tmux not probed".into()),
        Some(p) if p.da1.is_none() => return Err("sixel ✗ no DA1 reply from tmux".into()),
        Some(p) if !p.tmux_own_da1_sixel => return Err("sixel ✗ tmux built without sixel".into()),
        Some(_) => {}
    }
    if !tmux.has_feature("sixel") {
        return Err("sixel ✗ client lacks sixel".into());
    }
    let Some((w, h)) = tmux.cell else {
        return Err("sixel ✗ client cell 0x0".into());
    };
    Ok(format!("tmux draws sixel, client cell {w}x{h}"))
}

fn auto_direct(facts: &Facts<'_>, caps: &Caps) -> Choice {
    let mut notes = Vec::new();
    let reported = facts.reported.as_ref();
    // 1. Unicode placeholders: the protocol answer and an allowlisted identity.
    if facts.kitty_ok() {
        match reported {
            Some(id) if id.has_placeholders() => {
                let why = format!("{} answered a=q", id.text);
                return Choice::new(Graphics::KittyPlaceholders, why, &notes);
            }
            Some(id) => notes.push(format!("placeholders ✗ unsupported by {}", id.text)),
            None => notes.push("placeholders ✗ no XTVERSION reply".into()),
        }
    } else if facts.probe.is_some() {
        notes.push("kitty ✗ no a=q reply".into());
    } else {
        notes.push("kitty ✗ not probed".into());
    }
    // 2. iTerm2 inline images.
    if let Some(id) = osc1337_identity(facts) {
        let why = format!("{} speaks OSC 1337 ({})", id.text, id.source.label());
        return Choice::new(Graphics::Iterm, why, &notes);
    }
    // 3. Classic kitty placements.
    if facts.kitty_ok() {
        let who = facts
            .identity()
            .map_or("the terminal", |id| id.text.as_str());
        let why = format!("{who} answered a=q");
        return Choice::new(Graphics::KittyClassic, why, &notes);
    }
    // 4. Sixel.
    match direct_sixel(facts, caps.cell_px) {
        Ok(why) => return Choice::new(Graphics::Sixel, why, &notes),
        Err(note) => notes.push(note),
    }
    fallback(caps, &notes)
}

/// The identity that picks OSC 1337: an OSC 1337 terminal by XTVERSION or
/// `TERM_PROGRAM`, or iTerm2 by `LC_TERMINAL`.
///
/// The environment only counts when the terminal did not name itself, or
/// named something emde does not know or the xterm.js library (which OSC
/// 1337 hosts such as Tabby embed). A terminal that did name itself wins
/// over a `TERM_PROGRAM` inherited from the terminal it was started from.
fn osc1337_identity<'f>(facts: &'f Facts<'_>) -> Option<&'f Identity> {
    let env_counts = facts
        .reported
        .as_ref()
        .is_none_or(|id| matches!(id.emulator, Emulator::Unknown | Emulator::XtermJs));
    let hinted = facts.hinted.as_ref().filter(|id| {
        env_counts
            && matches!(
                id.source,
                IdentitySource::Env("TERM_PROGRAM" | "LC_TERMINAL")
            )
    });
    facts
        .reported
        .iter()
        .chain(hinted)
        .find(|id| id.emulator.speaks_osc1337())
}

/// Sixel straight to the terminal: DA1 lists `4` and the cell size (as
/// decided: probe, else preset) is known.
fn direct_sixel(facts: &Facts<'_>, cell: Option<(u16, u16)>) -> Result<String, String> {
    if !SIXEL_BUILT {
        return Err("sixel ✗ built without sixel".into());
    }
    let Some(p) = facts.probe else {
        return Err("sixel ✗ not probed".into());
    };
    if p.da1.is_none() {
        return Err("sixel ✗ no DA1 reply".into());
    }
    if !p.da1_has(4) {
        return Err("sixel ✗ DA1 without 4".into());
    }
    let Some((w, h)) = cell else {
        return Err("sixel ✗ cell size unknown".into());
    };
    Ok(format!("DA1 has 4, cell {w}x{h} px"))
}

/// `images = "kitty"`: placeholders where the terminal has them, classic
/// placements otherwise; inside tmux only placeholders, through passthrough.
fn forced_kitty(facts: &Facts<'_>, opts: &ImageOptions) -> Result<(Graphics, String), String> {
    if facts.in_tmux {
        return tmux_placeholders(facts, opts, false).map(|why| (Graphics::KittyPlaceholders, why));
    }
    Ok(match facts.identity() {
        Some(id) if id.has_placeholders() => (
            Graphics::KittyPlaceholders,
            format!("{} has Unicode placeholders", id.text),
        ),
        _ => (Graphics::KittyClassic, "classic placements".into()),
    })
}

/// `images = "iterm"`: never through tmux.
fn forced_iterm(facts: &Facts<'_>) -> Result<(Graphics, String), String> {
    if facts.in_tmux {
        return Err("iterm ✗ never inside tmux".into());
    }
    Ok((Graphics::Iterm, String::new()))
}

/// `images = "sixel"`: inside tmux only tmux's own sixel works.
fn forced_sixel(facts: &Facts<'_>) -> Result<(Graphics, String), String> {
    if !SIXEL_BUILT {
        return Err("sixel ✗ built without sixel".into());
    }
    if facts.in_tmux {
        return tmux_sixel(facts).map(|why| (Graphics::Sixel, why));
    }
    Ok((Graphics::Sixel, String::new()))
}

/// Trim and drop control characters from reported text.
pub(super) fn clean(s: &str) -> String {
    s.trim().chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::probe::fixtures::*;
    use crate::term::tmux;

    const SSH: (&str, &str) = ("SSH_CONNECTION", "192.0.2.10 50000 192.0.2.20 22");

    /// A decision scenario.
    struct Case<'a> {
        env: Vec<(&'a str, &'a str)>,
        tmux: Option<&'a str>,
        /// Reply bytes and whether the wrapped batch was sent.
        probe: Option<(&'a [u8], bool)>,
        mode: ImageMode,
    }

    impl<'a> Case<'a> {
        fn new(env: &[(&'a str, &'a str)], probe: Option<&'a [u8]>) -> Case<'a> {
            Case {
                env: env.to_vec(),
                tmux: None,
                probe: probe.map(|p| (p, false)),
                mode: ImageMode::Auto,
            }
        }

        fn in_tmux(mut self, line: &'a str, wrapped: bool) -> Case<'a> {
            self.env.push(("TMUX", "/tmp/tmux-1001/default,1,0"));
            self.tmux = Some(line);
            if let Some((bytes, _)) = self.probe {
                self.probe = Some((bytes, wrapped));
            }
            self
        }

        fn mode(mut self, mode: ImageMode) -> Case<'a> {
            self.mode = mode;
            self
        }

        fn decide_with(&self, base: Caps, opts: &ImageOptions) -> Caps {
            let env = Env::from_pairs(&self.env);
            let in_tmux = env.is_set("TMUX");
            let tmux = self.tmux.and_then(tmux::parse);
            let replies = self
                .probe
                .map(|(bytes, wrapped)| replies(bytes, in_tmux, wrapped));
            let opts = ImageOptions {
                mode: self.mode,
                ..opts.clone()
            };
            decide(&env, base, tmux.as_ref(), replies.as_ref(), &opts)
        }

        fn decide(&self) -> Caps {
            self.decide_with(Caps::full(), &ImageOptions::default())
        }
    }

    fn reason<'c>(caps: &'c Caps, topic: &str) -> &'c str {
        caps.reasons
            .iter()
            .rev()
            .find(|r| r.topic == topic)
            .map_or("", |r| r.detail.as_str())
    }

    const ITERM_SSH: &[(&str, &str)] = &[
        ("TERM", "xterm-256color"),
        ("LC_TERMINAL", "iTerm2"),
        ("LC_TERMINAL_VERSION", "3.6.9"),
        SSH,
    ];
    #[cfg(not(feature = "images"))]
    #[test]
    fn without_image_support_images_are_alt_text() {
        for mode in [ImageMode::Auto, ImageMode::Kitty, ImageMode::Blocks] {
            let caps = Case::new(&[("TERM", "xterm-kitty")], Some(KITTY))
                .mode(mode)
                .decide();
            assert_eq!(caps.graphics, Graphics::None);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "built without image support"
            );
        }
        let caps = Case::new(&[], None).mode(ImageMode::None).decide();
        assert_eq!(reason(&caps, topic::GRAPHICS), "images = none");
    }

    // --- glyphs, background, cell size -----------------------------------

    fn glyphs_for(
        env: &[(&str, &str)],
        probe: Option<&[u8]>,
        wanted: BlockGlyphs,
    ) -> (BlockGlyphSet, String) {
        let opts = ImageOptions {
            blocks: wanted,
            ..ImageOptions::default()
        };
        let caps = Case::new(env, probe).decide_with(Caps::full(), &opts);
        (caps.block_glyphs, reason(&caps, topic::BLOCKS).to_string())
    }

    #[test]
    fn automatic_block_glyphs() {
        let auto = |env: &[(&str, &str)], probe: Option<&[u8]>| {
            glyphs_for(env, probe, BlockGlyphs::Auto).0
        };
        assert_eq!(auto(&[], Some(KITTY)), BlockGlyphSet::Octant);
        let old_kitty = b"\x1bP>|kitty(0.39.1)\x1b\\\x1b[?62;c";
        assert_eq!(auto(&[], Some(old_kitty)), BlockGlyphSet::Half);
        assert_eq!(auto(&[], Some(GHOSTTY)), BlockGlyphSet::Octant);
        assert_eq!(
            auto(&[("TERM_PROGRAM", "ghostty")], None),
            BlockGlyphSet::Octant
        );
        assert_eq!(auto(&[], Some(FOOT)), BlockGlyphSet::Octant);
        let old_foot = b"\x1bP>|foot(1.19.0)\x1b\\\x1b[?62;4c";
        assert_eq!(auto(&[], Some(old_foot)), BlockGlyphSet::Half);
        assert_eq!(auto(&[], Some(WEZTERM)), BlockGlyphSet::Sextant);
        assert_eq!(auto(&[], Some(ITERM2)), BlockGlyphSet::Half);
        assert_eq!(auto(&[], Some(XTERM)), BlockGlyphSet::Half);
        assert_eq!(auto(&[], None), BlockGlyphSet::Half);
        // TERM says kitty, but the version is unknown.
        assert_eq!(auto(&[("TERM", "xterm-kitty")], None), BlockGlyphSet::Half);
        let (set, why) = glyphs_for(&[], Some(KITTY), BlockGlyphs::Auto);
        assert_eq!(
            (set, why.as_str()),
            (BlockGlyphSet::Octant, "kitty(0.49.1) draws octants")
        );
        // Inside tmux, the client's identity counts.
        let kitty_client = "3.4|kitty(0.49.1)|xterm-kitty|256,RGB|10x21|off|external";
        let opts = ImageOptions {
            blocks: BlockGlyphs::Auto,
            ..ImageOptions::default()
        };
        let caps = Case::new(&[], Some(TMUX_LOCAL))
            .in_tmux(kitty_client, false)
            .decide_with(Caps::full(), &opts);
        assert_eq!(caps.block_glyphs, BlockGlyphSet::Octant);
    }

    #[test]
    fn fixed_block_glyphs() {
        for (wanted, set) in [
            (BlockGlyphs::Half, BlockGlyphSet::Half),
            (BlockGlyphs::Quadrant, BlockGlyphSet::Quadrant),
            (BlockGlyphs::Sextant, BlockGlyphSet::Sextant),
            (BlockGlyphs::Octant, BlockGlyphSet::Octant),
        ] {
            let (got, why) = glyphs_for(&[], Some(ITERM2), wanted);
            assert_eq!(got, set);
            assert_eq!(why, format!("blocks = {}", glyph_name(set)));
        }
    }

    #[test]
    fn background_sources() {
        let caps = Case::new(&[], Some(KITTY)).decide();
        assert_eq!(caps.background, Some(Rgb(0x1e, 0x1e, 0x2e)));
        assert_eq!(reason(&caps, topic::BACKGROUND), "OSC 11");
        // OSC 11 wins over COLORFGBG.
        let caps = Case::new(&[("COLORFGBG", "0;15")], Some(KITTY)).decide();
        assert_eq!(caps.background, Some(Rgb(0x1e, 0x1e, 0x2e)));
        for (value, expected) in [
            ("15;0", Some(xterm_rgb(0))),
            ("0;15", Some(xterm_rgb(15))),
            ("15;default;0", Some(xterm_rgb(0))),
            (" 7 ; 8 ", Some(xterm_rgb(8))),
            ("default;default", None),
            ("0;99", None),
            ("", None),
        ] {
            let caps = Case::new(&[("COLORFGBG", value)], Some(TMUX_LOCAL)).decide();
            assert_eq!(caps.background, expected, "{value:?}");
            if expected.is_some() {
                assert_eq!(reason(&caps, topic::BACKGROUND), "COLORFGBG");
            }
        }
        let caps = Case::new(&[], Some(TMUX_LOCAL)).decide();
        assert_eq!(
            reason(&caps, topic::BACKGROUND),
            "no OSC 11 reply, no COLORFGBG"
        );
        let caps = Case::new(&[], None).decide();
        assert_eq!(reason(&caps, topic::BACKGROUND), "not probed, no COLORFGBG");
        // A preset in the base survives when nothing better is known.
        let base = Caps {
            background: Some(Rgb(1, 2, 3)),
            ..Caps::full()
        };
        let caps = Case::new(&[], None).decide_with(base, &ImageOptions::default());
        assert_eq!(caps.background, Some(Rgb(1, 2, 3)));
        assert_eq!(reason(&caps, topic::BACKGROUND), "preset");
    }

    #[test]
    fn cell_size_sources() {
        let caps = Case::new(&[], Some(KITTY)).decide();
        assert_eq!(
            (caps.cell_px, reason(&caps, topic::CELL)),
            (Some((17, 36)), "16t")
        );
        let caps = Case::new(&[], Some(ITERM2)).decide();
        assert_eq!(
            (caps.cell_px, reason(&caps, topic::CELL)),
            (Some((8, 16)), "14t/18t")
        );
        let caps = Case::new(&[], Some(VSCODE_PLAIN)).decide();
        assert_eq!(
            (caps.cell_px, reason(&caps, topic::CELL)),
            (None, "no size reply")
        );
        let caps = Case::new(&[], None).decide();
        assert_eq!(
            (caps.cell_px, reason(&caps, topic::CELL)),
            (None, "not probed")
        );
        let base = Caps {
            cell_px: Some((9, 18)),
            size: Some((100, 30)),
            ..Caps::full()
        };
        let caps = Case::new(&[], Some(KITTY)).decide_with(base.clone(), &ImageOptions::default());
        assert_eq!(caps.cell_px, Some((17, 36)), "the probe beats a preset");
        assert_eq!(
            caps.size,
            Some((100, 30)),
            "the caller's size beats CSI 18 t"
        );
        let caps = Case::new(&[], None).decide_with(base, &ImageOptions::default());
        assert_eq!(
            (caps.cell_px, reason(&caps, topic::CELL)),
            (Some((9, 18)), "preset")
        );
    }

    #[test]
    fn cell_size_inside_tmux() {
        use crate::term::tmux::fixtures as tmux_lines;
        let cell = |caps: &Caps| (caps.cell_px, reason(caps, topic::CELL).to_string());
        // tmux answers 16t with its window cell (16x32 here); the client's
        // own cell is what the outer terminal draws with.
        let caps = Case::new(&[], Some(TMUX_LOCAL))
            .in_tmux(tmux_lines::FOOT_CLIENT, false)
            .decide();
        assert_eq!(cell(&caps), (Some((10, 20)), "tmux client cell".into()));
        // A client cell of 0x0 leaves the size unknown: 16x32 is tmux's default.
        let caps = Case::new(&[], Some(TMUX_LOCAL))
            .in_tmux(tmux_lines::USER_SESSION, false)
            .decide();
        assert_eq!(cell(&caps), (None, "client cell 0x0".into()));
        // A preset fills the gap, but never beats the client's cell.
        let base = Caps {
            cell_px: Some((9, 18)),
            ..Caps::full()
        };
        let caps = Case::new(&[], Some(TMUX_LOCAL))
            .in_tmux(tmux_lines::USER_SESSION, false)
            .decide_with(base.clone(), &ImageOptions::default());
        assert_eq!(cell(&caps), (Some((9, 18)), "preset".into()));
        let caps = Case::new(&[], Some(TMUX_LOCAL))
            .in_tmux(tmux_lines::FOOT_CLIENT, false)
            .decide_with(base, &ImageOptions::default());
        assert_eq!(cell(&caps), (Some((10, 20)), "tmux client cell".into()));
        // Without the tmux query, tmux's answer is all there is.
        let caps = Case::new(&[("TERM", "tmux-256color")], Some(TMUX_LOCAL)).decide();
        assert!(caps.in_tmux);
        assert_eq!(cell(&caps), (Some((16, 32)), "tmux 16t".into()));
        let mut no_query = Case::new(&[], Some(TMUX_LOCAL)).in_tmux(tmux_lines::FOOT_CLIENT, false);
        no_query.tmux = None;
        assert_eq!(
            cell(&no_query.decide()),
            (Some((16, 32)), "tmux 16t".into())
        );
    }

    #[test]
    fn multiplexers_are_recognised() {
        let behind = |pairs: &[(&str, &str)]| behind_multiplexer(&Env::from_pairs(pairs));
        assert!(behind(&[("TMUX", "/tmp/tmux-1001/default,1,0")]));
        assert!(behind(&[
            ("TERM_PROGRAM", "tmux"),
            ("TERM", "xterm-256color")
        ]));
        assert!(behind(&[("TERM", "tmux-256color")]));
        assert!(behind(&[("TERM", "screen.xterm-256color")]));
        assert!(!behind(&[
            ("TERM", "xterm-kitty"),
            ("TERM_PROGRAM", "vscode")
        ]));
        assert!(!behind(&[("TMUX", "")]));
        assert!(!behind(&[]));
    }

    #[test]
    fn every_decision_has_a_reason_and_base_reasons_are_kept() {
        let base = Caps {
            reasons: vec![Reason {
                topic: topic::COLOR,
                detail: "in tmux".into(),
            }],
            ..Caps::full()
        };
        let caps = Case::new(ITERM_SSH, Some(ITERM2)).decide_with(base, &ImageOptions::default());
        assert_eq!(
            caps.reasons.first().map(|r| r.detail.as_str()),
            Some("in tmux")
        );
        for topic in [
            topic::TMUX,
            topic::SSH,
            topic::TERMINAL,
            topic::BACKGROUND,
            topic::CELL,
            topic::GRAPHICS,
            topic::BLOCKS,
        ] {
            assert_eq!(
                caps.reasons.iter().filter(|r| r.topic == topic).count(),
                1,
                "{topic}"
            );
            assert!(!reason(&caps, topic).is_empty(), "{topic}");
        }
        // Colour decisions from the base are left alone.
        assert_eq!(caps.color, ColorDepth::TrueColor);
        assert!(caps.hyperlinks && caps.styled_underline);
    }

    // --- identities ----------------------------------------------------------

    #[test]
    fn versions() {
        let v = Version::parse;
        assert_eq!(v("0.49.1"), Version(vec![0, 49, 1]));
        assert_eq!(v("3.3a"), Version(vec![3, 3]));
        assert_eq!(v("1.20.2-dev"), Version(vec![1, 20, 2]));
        assert_eq!(v("20240203-110809-5046fc22"), Version(vec![20240203]));
        assert_eq!(v("next-3.5"), Version(vec![]));
        assert_eq!(v(""), Version(vec![]));
        assert_eq!(v("99999999999"), Version(vec![]));
        assert!(v("3.6.9").at_least(&[3, 6]));
        assert!(v("3.6").at_least(&[3, 6, 0]));
        assert!(v("0.28").at_least(&[0, 28]));
        assert!(!v("0.27.1").at_least(&[0, 28]));
        assert!(!v("3.5.14").at_least(&[3, 6]));
        assert!(v("4").at_least(&[3, 6]));
        assert!(!v("").at_least(&[]));
        assert!(!v("").is_known());
    }

    #[test]
    fn xtversion_identities() {
        let id = |s: &str| Identity::from_xtversion(s, IdentitySource::Xtversion).unwrap();
        for (text, emulator, placeholders) in [
            ("kitty(0.49.1)", Emulator::Kitty, true),
            ("kitty(0.28.0)", Emulator::Kitty, true),
            ("kitty(0.27.1)", Emulator::Kitty, false),
            ("ghostty 1.3.1", Emulator::Ghostty, true),
            ("iTerm2 3.6.9", Emulator::Iterm2, true),
            ("iTerm2 3.5.14", Emulator::Iterm2, false),
            ("WezTerm 20240203-110809-5046fc22", Emulator::WezTerm, false),
            ("foot(1.20.2)", Emulator::Foot, false),
            ("XTerm(390)", Emulator::XTerm, false),
            ("xterm.js(6.0.0)", Emulator::XtermJs, false),
            ("tmux 3.4", Emulator::Tmux, false),
            ("Konsole 24.02.1", Emulator::Konsole, false),
            ("Something 1.0", Emulator::Unknown, false),
        ] {
            let identity = id(text);
            assert_eq!(identity.emulator, emulator, "{text}");
            assert_eq!(identity.has_placeholders(), placeholders, "{text}");
            assert_eq!(identity.text, text);
        }
        assert_eq!(id("kitty(0.49.1)").version, Version(vec![0, 49, 1]));
        assert_eq!(
            Identity::from_xtversion("  ", IdentitySource::Xtversion),
            None
        );
        assert_eq!(id("\x1bkitty(0.49.1)").text, "kitty(0.49.1)");
    }

    #[test]
    fn environment_identities() {
        let id = |pairs: &[(&str, &str)]| Identity::from_env(&Env::from_pairs(pairs));
        let text = |pairs: &[(&str, &str)]| id(pairs).map(|i| (i.text, i.source.label()));
        assert_eq!(
            text(&[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "3.6.9")
            ]),
            Some(("iTerm2 3.6.9".into(), "TERM_PROGRAM"))
        );
        assert_eq!(
            text(&[("LC_TERMINAL", "iTerm2"), ("LC_TERMINAL_VERSION", "3.6.9")]),
            Some(("iTerm2 3.6.9".into(), "LC_TERMINAL"))
        );
        assert_eq!(
            text(&[("TERM_PROGRAM", "tmux"), ("LC_TERMINAL", "iTerm2")]),
            Some(("iTerm2".into(), "LC_TERMINAL"))
        );
        assert_eq!(
            text(&[("TERM_PROGRAM", "Hyper"), ("TERM_PROGRAM_VERSION", "3.4.1")]),
            Some(("Hyper 3.4.1".into(), "TERM_PROGRAM"))
        );
        assert_eq!(
            text(&[("TERM", "xterm-kitty")]),
            Some(("kitty".into(), "TERM"))
        );
        assert_eq!(text(&[("TERM", "foot")]), Some(("foot".into(), "TERM")));
        assert_eq!(
            text(&[("TERM", "xterm-256color"), ("KONSOLE_VERSION", "240801")]),
            Some(("Konsole".into(), "KONSOLE_VERSION"))
        );
        assert_eq!(
            text(&[("WT_SESSION", "abc")]),
            Some(("Windows Terminal".into(), "WT_SESSION"))
        );
        assert_eq!(text(&[("TERM", "xterm-256color")]), None);
        assert_eq!(
            id(&[("TERM_PROGRAM", "WezTerm")]).unwrap().emulator,
            Emulator::WezTerm
        );
        assert_eq!(
            id(&[("TERM_PROGRAM", "vscode")]).unwrap().emulator,
            Emulator::VsCode
        );
    }

    /// Graphics decisions (they need the `images` feature).
    #[cfg(feature = "images")]
    mod graphics {
        use super::*;
        use crate::term::tmux::fixtures as tmux_lines;

        /// The user's environment inside tmux (iTerm2 3.6.9 → SSH → tmux 3.4).
        const USER_ENV: &[(&str, &str)] = &[
            ("TERM", "tmux-256color"),
            ("TERM_PROGRAM", "tmux"),
            ("TERM_PROGRAM_VERSION", "3.4"),
            ("TMUX", "/tmp/tmux-1001/default,443340,2"),
            ("LC_TERMINAL", "iTerm2"),
            ("LC_TERMINAL_VERSION", "3.6.9"),
            SSH,
        ];

        const VSCODE: &[(&str, &str)] = &[
            ("TERM", "xterm-256color"),
            ("TERM_PROGRAM", "vscode"),
            ("TERM_PROGRAM_VERSION", "1.105.0"),
            ("VSCODE_INJECTION", "1"),
        ];

        fn without_kitty(replies: &[u8]) -> Vec<u8> {
            let ok = b"\x1b_Gi=31;OK\x1b\\";
            let pos = replies.windows(ok.len()).position(|w| w == ok).unwrap();
            [&replies[..pos], &replies[pos + ok.len()..]].concat()
        }

        /// Sixel where the build has it, else blocks.
        fn sixel_or_blocks() -> Graphics {
            if SIXEL_BUILT {
                Graphics::Sixel
            } else {
                Graphics::Blocks
            }
        }

        // --- the plan's environment table ------------------------------------

        #[test]
        fn iterm2_over_ssh() {
            let with_kitty = Case::new(ITERM_SSH, Some(ITERM2));
            let caps = with_kitty.decide();
            assert_eq!(caps.graphics, Graphics::KittyPlaceholders);
            assert_eq!(reason(&caps, topic::GRAPHICS), "iTerm2 3.6.9 answered a=q");
            assert_eq!(caps.terminal.as_deref(), Some("iTerm2 3.6.9"));
            assert!(caps.over_ssh && !caps.in_tmux);
            // kitty graphics switched off in iTerm2: no a=q answer.
            let no_kitty = without_kitty(ITERM2);
            let caps = Case::new(ITERM_SSH, Some(&no_kitty)).decide();
            assert_eq!(caps.graphics, Graphics::Iterm);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "iTerm2 3.6.9 speaks OSC 1337 (XTVERSION) · kitty ✗ no a=q reply"
            );
            // No probe (timed out or skipped): LC_TERMINAL still says iTerm2.
            let caps = Case::new(ITERM_SSH, None).decide();
            assert_eq!(caps.graphics, Graphics::Iterm);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "iTerm2 3.6.9 speaks OSC 1337 (LC_TERMINAL) · kitty ✗ not probed"
            );
        }

        #[test]
        fn vscode_terminal() {
            let caps = Case::new(VSCODE, Some(VSCODE_IMAGES)).decide();
            assert_eq!(caps.graphics, Graphics::KittyClassic);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "xterm.js(6.0.0) answered a=q · placeholders ✗ unsupported by xterm.js(6.0.0)"
            );
            let caps = Case::new(VSCODE, Some(VSCODE_PLAIN)).decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            let sixel = if SIXEL_BUILT {
                "sixel ✗ DA1 without 4"
            } else {
                "sixel ✗ built without sixel"
            };
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                format!("kitty ✗ no a=q reply · {sixel}")
            );
            // Without XTVERSION the environment names it.
            let quiet = VSCODE_PLAIN
                .strip_prefix(b"\x1bP>|xterm.js(6.0.0)\x1b\\")
                .unwrap();
            let caps = Case::new(VSCODE, Some(quiet)).decide();
            assert_eq!(caps.terminal.as_deref(), Some("VS Code 1.105.0"));
            assert_eq!(reason(&caps, topic::TERMINAL), "TERM_PROGRAM");
        }

        #[test]
        fn tmux_in_iterm2_with_passthrough() {
            let caps = Case::new(&[SSH], Some(ITERM2_VIA_TMUX))
                .in_tmux(tmux_lines::USER_SESSION_PASSTHROUGH, true)
                .decide();
            assert_eq!(caps.graphics, Graphics::KittyPlaceholders);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "passthrough on, iTerm2 3.6.9 answered a=q"
            );
            assert_eq!(caps.terminal.as_deref(), Some("iTerm2 3.6.9"));
            assert_eq!(reason(&caps, topic::TERMINAL), "client_termtype");
            assert!(caps.in_tmux && caps.over_ssh);
        }

        #[test]
        fn users_session_gets_blocks() {
            let case = Case {
                env: USER_ENV.to_vec(),
                tmux: Some(tmux_lines::USER_SESSION),
                probe: Some((USER_SESSION, false)),
                mode: ImageMode::Auto,
            };
            let caps = case.decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            let expected = if SIXEL_BUILT {
                "kitty/iterm ✗ passthrough off · sixel ✗ client cell 0x0"
            } else {
                "kitty/iterm ✗ passthrough off · sixel ✗ built without sixel"
            };
            assert_eq!(reason(&caps, topic::GRAPHICS), expected);
            assert_eq!(caps.terminal.as_deref(), Some("iTerm2 3.6.9"));
            assert_eq!(caps.background, Some(Rgb(0x1e, 0x1e, 0x2e)));
            assert_eq!(reason(&caps, topic::BACKGROUND), "tmux OSC 11");
            // tmux answered 16t with its 16x32 default; the client cell is 0x0.
            assert_eq!(caps.cell_px, None);
            assert_eq!(reason(&caps, topic::CELL), "client cell 0x0");
            assert_eq!(caps.size, Some((214, 54)));
            assert_eq!(
                reason(&caps, topic::TMUX),
                "tmux 3.4, passthrough off, client cell 0x0"
            );
            assert_eq!(reason(&caps, topic::SSH), "SSH_CONNECTION");
        }

        #[test]
        fn tmux_in_vscode_gets_blocks() {
            // The outer terminal answers a=q through passthrough, but xterm.js
            // has no Unicode placeholders.
            let replies = [USER_SESSION, b"\x1b_Gi=31;OK\x1b\\\x1b[0n"].concat();
            let caps = Case::new(&[], Some(&replies))
                .in_tmux(tmux_lines::VSCODE_CLIENT, true)
                .decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert!(
                reason(&caps, topic::GRAPHICS)
                    .starts_with("kitty ✗ xterm.js(6.0.0) has no Unicode placeholders · ")
            );
            // Without an XTVERSION answer tmux has no client_termtype.
            let unknown = "3.4||xterm-256color|256,RGB,hyperlinks|0x0|on|external";
            let caps = Case::new(&[], Some(&replies))
                .in_tmux(unknown, true)
                .decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert!(reason(&caps, topic::GRAPHICS).starts_with("kitty ✗ outer terminal unknown"));
            assert_eq!(caps.terminal, None);
        }

        #[test]
        fn local_terminals() {
            let kitty = Case::new(
                &[("TERM", "xterm-kitty"), ("KITTY_WINDOW_ID", "1")],
                Some(KITTY),
            );
            assert_eq!(kitty.decide().graphics, Graphics::KittyPlaceholders);
            let ghostty_env = [
                ("TERM", "xterm-ghostty"),
                ("TERM_PROGRAM", "ghostty"),
                ("TERM_PROGRAM_VERSION", "1.3.1"),
            ];
            assert_eq!(
                Case::new(&ghostty_env, Some(GHOSTTY)).decide().graphics,
                Graphics::KittyPlaceholders
            );
            let wezterm_env = [
                ("TERM", "xterm-256color"),
                ("TERM_PROGRAM", "WezTerm"),
                ("TERM_PROGRAM_VERSION", "20240203-110809-5046fc22"),
            ];
            let caps = Case::new(&wezterm_env, Some(WEZTERM)).decide();
            assert_eq!(caps.graphics, Graphics::Iterm);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "WezTerm 20240203-110809-5046fc22 speaks OSC 1337 (XTVERSION) · placeholders ✗ \
                 unsupported by WezTerm 20240203-110809-5046fc22"
            );
            let iterm_env = [
                ("TERM", "xterm-256color"),
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "3.6.9"),
                ("LC_TERMINAL", "iTerm2"),
                ("LC_TERMINAL_VERSION", "3.6.9"),
            ];
            assert_eq!(
                Case::new(&iterm_env, Some(ITERM2)).decide().graphics,
                Graphics::KittyPlaceholders
            );
            let no_kitty = without_kitty(ITERM2);
            assert_eq!(
                Case::new(&iterm_env, Some(&no_kitty)).decide().graphics,
                Graphics::Iterm
            );
        }

        #[test]
        fn sixel_terminals() {
            let foot = Case::new(&[("TERM", "foot")], Some(FOOT)).decide();
            assert_eq!(foot.graphics, sixel_or_blocks());
            if SIXEL_BUILT {
                assert_eq!(
                    reason(&foot, topic::GRAPHICS),
                    "DA1 has 4, cell 10x20 px · kitty ✗ no a=q reply"
                );
            }
            let xterm = Case::new(&[("TERM", "xterm-256color")], Some(XTERM)).decide();
            assert_eq!(xterm.graphics, Graphics::Blocks);
            let vt340 = Case::new(&[("TERM", "xterm-256color")], Some(XTERM_VT340));
            assert_eq!(vt340.decide().graphics, sixel_or_blocks());
            // Sixel needs the cell size.
            let no_size = b"\x1bP>|XTerm(390)\x1b\\\x1b[?63;1;2;4c";
            let caps = Case::new(&[("TERM", "xterm")], Some(no_size)).decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            if SIXEL_BUILT {
                assert!(reason(&caps, topic::GRAPHICS).ends_with("sixel ✗ cell size unknown"));
            }
        }

        #[test]
        fn sixel_uses_a_preset_cell_size() {
            // DA1 lists sixel, but the terminal answers no window reports.
            let quiet = b"\x1bP>|XTerm(390)\x1b\\\x1b[?63;1;2;4c";
            let base = Caps {
                cell_px: Some((9, 17)),
                ..Caps::full()
            };
            let caps = Case::new(&[("TERM", "xterm")], Some(quiet))
                .decide_with(base, &ImageOptions::default());
            assert_eq!(caps.graphics, sixel_or_blocks());
            if SIXEL_BUILT {
                assert_eq!(
                    reason(&caps, topic::GRAPHICS),
                    "DA1 has 4, cell 9x17 px · kitty ✗ no a=q reply"
                );
            }
        }

        #[test]
        fn a_reported_terminal_beats_stale_environment_hints() {
            // foot started from a WezTerm shell inherits TERM_PROGRAM=WezTerm,
            // but foot has no OSC 1337: its own XTVERSION answer counts.
            let caps = Case::new(&[("TERM_PROGRAM", "WezTerm")], Some(FOOT)).decide();
            assert_eq!(caps.graphics, sixel_or_blocks());
            assert_eq!(caps.terminal.as_deref(), Some("foot(1.20.2)"));
            // kitty < 0.28 under a stale iTerm2 hint: classic kitty, not OSC 1337.
            let old_kitty = b"\x1b_Gi=31;OK\x1b\\\x1bP>|kitty(0.27.1)\x1b\\\x1b[?62;c";
            let env = [("LC_TERMINAL", "iTerm2"), ("TERM_PROGRAM", "iTerm.app")];
            let caps = Case::new(&env, Some(old_kitty)).decide();
            assert_eq!(caps.graphics, Graphics::KittyClassic);
            // An unknown name leaves the environment in charge.
            let unknown = b"\x1bP>|SomeTerm 2.0\x1b\\\x1b[?62;c";
            let caps = Case::new(&[("TERM_PROGRAM", "WezTerm")], Some(unknown)).decide();
            assert_eq!(caps.graphics, Graphics::Iterm);
        }

        #[test]
        fn term_program_tmux_hides_environment_hints() {
            // `unset TMUX` inside tmux: TERM_PROGRAM still says tmux.
            let env = [
                ("TERM", "xterm-256color"),
                ("TERM_PROGRAM", "tmux"),
                ("LC_TERMINAL", "iTerm2"),
                ("LC_TERMINAL_VERSION", "3.6.9"),
            ];
            let caps = Case::new(&env, None).decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert_eq!(caps.terminal, None);
        }

        #[test]
        fn tmux_native_sixel() {
            let case = Case::new(&[("TERM", "tmux-256color")], Some(TMUX_LOCAL))
                .in_tmux(tmux_lines::FOOT_CLIENT, false);
            let caps = case.decide();
            assert_eq!(caps.graphics, sixel_or_blocks());
            if SIXEL_BUILT {
                assert_eq!(
                    reason(&caps, topic::GRAPHICS),
                    "tmux draws sixel, client cell 10x20 · kitty/iterm ✗ passthrough off"
                );
            }
            // tmux built without sixel answers `?1;2c`.
            let plain_tmux = b"\x1bP>|tmux 3.4\x1b\\\x1b[6;20;10t\x1b[?1;2c";
            let caps = Case::new(&[], Some(plain_tmux))
                .in_tmux(tmux_lines::FOOT_CLIENT, false)
                .decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            if SIXEL_BUILT {
                assert!(
                    reason(&caps, topic::GRAPHICS).ends_with("sixel ✗ tmux built without sixel")
                );
            }
            // A client without the sixel feature.
            let no_feature = "3.4|kitty(0.49.1)|xterm-kitty|256,RGB|10x21|off|external";
            let caps = Case::new(&[], Some(TMUX_LOCAL))
                .in_tmux(no_feature, false)
                .decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            if SIXEL_BUILT {
                assert!(reason(&caps, topic::GRAPHICS).ends_with("sixel ✗ client lacks sixel"));
            }
        }

        #[test]
        fn classic_kitty_elsewhere() {
            // Konsole answers a=q but has no placeholders and no XTVERSION.
            let konsole = b"\x1b_Gi=31;OK\x1b\\\x1b]11;rgb:2323/2626/2727\x1b\\\x1b[?62;1;4c";
            let env = [("TERM", "xterm-256color"), ("KONSOLE_VERSION", "240801")];
            let caps = Case::new(&env, Some(konsole)).decide();
            assert_eq!(caps.graphics, Graphics::KittyClassic);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "Konsole answered a=q · placeholders ✗ no XTVERSION reply"
            );
            // kitty before 0.28 had no placeholders.
            let old = b"\x1b_Gi=31;OK\x1b\\\x1bP>|kitty(0.27.1)\x1b\\\x1b[?62;c";
            let caps = Case::new(&[("TERM", "xterm-kitty")], Some(old)).decide();
            assert_eq!(caps.graphics, Graphics::KittyClassic);
        }

        #[test]
        fn placeholders_are_never_chosen_on_env_alone() {
            // TERM says kitty, but without an XTVERSION answer there is no
            // version to check.
            let quiet = b"\x1b_Gi=31;OK\x1b\\\x1b[?62;c";
            let caps = Case::new(&[("TERM", "xterm-kitty")], Some(quiet)).decide();
            assert_eq!(caps.graphics, Graphics::KittyClassic);
        }

        #[test]
        fn inside_tmux_never_classic_kitty_or_iterm() {
            // Even with every signal for iTerm2 in the environment.
            for tmux_line in [
                tmux_lines::USER_SESSION,
                tmux_lines::USER_SESSION_PASSTHROUGH,
                tmux_lines::VSCODE_CLIENT,
            ] {
                for probe in [
                    None,
                    Some((USER_SESSION, false)),
                    Some((ITERM2_VIA_TMUX, true)),
                ] {
                    let case = Case {
                        env: USER_ENV.to_vec(),
                        tmux: Some(tmux_line),
                        probe,
                        mode: ImageMode::Auto,
                    };
                    let g = case.decide().graphics;
                    assert!(
                        !matches!(g, Graphics::KittyClassic | Graphics::Iterm),
                        "{tmux_line} {probe:?}: {g:?}"
                    );
                }
            }
        }

        #[test]
        fn tmux_without_query_or_probe() {
            let case = Case {
                env: USER_ENV.to_vec(),
                tmux: None,
                probe: None,
                mode: ImageMode::Auto,
            };
            let caps = case.decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert!(caps.in_tmux);
            assert_eq!(reason(&caps, topic::TMUX), "$TMUX set, no client info");
            assert_eq!(caps.terminal, None);
            // `$TMUX` dropped, but tmux answers XTVERSION.
            let caps = Case::new(&[("TERM", "tmux-256color")], Some(TMUX_LOCAL)).decide();
            assert!(caps.in_tmux);
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert_eq!(
                reason(&caps, topic::TMUX),
                "XTVERSION says tmux, $TMUX unset"
            );
        }

        #[test]
        fn multiplexer_terms_hide_environment_hints() {
            // `ssh` from a tmux pane: TERM survives, TMUX does not; tmux answers.
            let env = [("TERM", "tmux-256color"), ("LC_TERMINAL", "iTerm2"), SSH];
            let caps = Case::new(&env, Some(TMUX_LOCAL)).decide();
            assert!(caps.in_tmux);
            assert_eq!(caps.graphics, Graphics::Blocks);
            // Not probed: still no OSC 1337 on the strength of LC_TERMINAL.
            let caps = Case::new(&env, None).decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert_eq!(caps.terminal, None);
            // GNU screen answers DA1 without sixel and no XTVERSION.
            let screen = [("TERM", "screen-256color"), ("TERM_PROGRAM", "iTerm.app")];
            let caps = Case::new(&screen, Some(b"\x1b[?1;2c")).decide();
            assert!(!caps.in_tmux);
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert!(multiplexer_term(&Env::from_pairs(&[("TERM", "screen")])));
            assert!(!multiplexer_term(&Env::from_pairs(&[(
                "TERM",
                "xterm-kitty"
            )])));
            assert!(!multiplexer_term(&Env::default()));
        }

        #[test]
        fn passthrough_needs_permission_and_rgb() {
            let case = Case::new(&[SSH], Some(ITERM2_VIA_TMUX))
                .in_tmux(tmux_lines::USER_SESSION_PASSTHROUGH, true);
            let opts = ImageOptions {
                tmux_passthrough: false,
                ..ImageOptions::default()
            };
            let caps = case.decide_with(Caps::full(), &opts);
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert!(
                reason(&caps, topic::GRAPHICS)
                    .starts_with("kitty/iterm ✗ images.tmux_passthrough off")
            );
            let no_rgb = "3.4|iTerm2 3.6.9|xterm-256color|256,hyperlinks,sixel|0x0|on|external";
            let caps = Case::new(&[SSH], Some(ITERM2_VIA_TMUX))
                .in_tmux(no_rgb, true)
                .decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert!(reason(&caps, topic::GRAPHICS).starts_with("kitty ✗ client lacks RGB"));
            // No answer through passthrough (e.g. the pane is hidden with `on`).
            let caps = Case::new(&[SSH], Some(USER_SESSION))
                .in_tmux(tmux_lines::USER_SESSION_PASSTHROUGH, true)
                .decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert!(
                reason(&caps, topic::GRAPHICS)
                    .starts_with("kitty ✗ no a=q reply from iTerm2 3.6.9")
            );
        }

        #[test]
        fn output_and_colour_limits() {
            let case = Case::new(&[("TERM", "xterm-kitty")], Some(KITTY));
            let piped = Caps {
                is_tty: false,
                ..Caps::full()
            };
            let caps = case.decide_with(piped, &ImageOptions::default());
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "pixels ✗ output is not a terminal"
            );
            let plain = case.decide_with(Caps::plain(), &ImageOptions::default());
            assert_eq!(plain.graphics, Graphics::None);
            assert_eq!(reason(&plain, topic::GRAPHICS), "no escape sequences");
            let mono = Caps {
                color: ColorDepth::Mono,
                ..Caps::full()
            };
            let caps = Case::new(&[("TERM", "xterm")], Some(XTERM))
                .decide_with(mono, &ImageOptions::default());
            assert_eq!(caps.graphics, Graphics::None);
            assert!(reason(&caps, topic::GRAPHICS).ends_with("blocks ✗ no colour"));
            let sixteen = Caps {
                color: ColorDepth::Ansi16,
                ..Caps::full()
            };
            let caps = Case::new(&[("TERM", "xterm")], Some(XTERM))
                .decide_with(sixteen, &ImageOptions::default());
            assert_eq!(caps.graphics, Graphics::Blocks);
        }

        // --- explicit modes ----------------------------------------------------

        fn users_session(mode: ImageMode) -> Caps {
            Case {
                env: USER_ENV.to_vec(),
                tmux: Some(tmux_lines::USER_SESSION),
                probe: Some((USER_SESSION, false)),
                mode,
            }
            .decide()
        }

        #[test]
        fn explicit_modes_that_cannot_work_fall_back() {
            let caps = users_session(ImageMode::Kitty);
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "images = kitty · kitty/iterm ✗ passthrough off"
            );
            let caps = users_session(ImageMode::Iterm);
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "images = iterm · iterm ✗ never inside tmux"
            );
            let caps = users_session(ImageMode::Sixel);
            assert_eq!(caps.graphics, Graphics::Blocks);
            let expected = if SIXEL_BUILT {
                "images = sixel · sixel ✗ client cell 0x0"
            } else {
                "images = sixel · sixel ✗ built without sixel"
            };
            assert_eq!(reason(&caps, topic::GRAPHICS), expected);
        }

        #[test]
        fn explicit_modes_are_honoured() {
            // The documented opt-in for VS Code when detection says no.
            let caps = Case::new(VSCODE, Some(VSCODE_PLAIN))
                .mode(ImageMode::Kitty)
                .decide();
            assert_eq!(caps.graphics, Graphics::KittyClassic);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "images = kitty · classic placements"
            );
            let caps = Case::new(VSCODE, Some(VSCODE_PLAIN))
                .mode(ImageMode::Iterm)
                .decide();
            assert_eq!(caps.graphics, Graphics::Iterm);
            assert_eq!(reason(&caps, topic::GRAPHICS), "images = iterm");
            let kitty = Case::new(&[("TERM", "xterm-kitty")], Some(KITTY));
            assert_eq!(
                kitty.mode(ImageMode::Kitty).decide().graphics,
                Graphics::KittyPlaceholders
            );
            let kitty = Case::new(&[("TERM", "xterm-kitty")], Some(KITTY));
            assert_eq!(
                kitty.mode(ImageMode::Iterm).decide().graphics,
                Graphics::Iterm
            );
            let wezterm = Case::new(&[("TERM_PROGRAM", "WezTerm")], Some(WEZTERM));
            assert_eq!(
                wezterm.mode(ImageMode::Sixel).decide().graphics,
                sixel_or_blocks()
            );
            // Through tmux, `kitty` skips the outer-terminal checks.
            let caps = Case::new(&[SSH], Some(USER_SESSION))
                .in_tmux(tmux_lines::USER_SESSION_PASSTHROUGH, false)
                .mode(ImageMode::Kitty)
                .decide();
            assert_eq!(caps.graphics, Graphics::KittyPlaceholders);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "images = kitty · passthrough on"
            );
        }

        #[test]
        fn blocks_and_none_modes() {
            let kitty = || Case::new(&[("TERM", "xterm-kitty")], Some(KITTY));
            let caps = kitty().mode(ImageMode::Blocks).decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert_eq!(reason(&caps, topic::GRAPHICS), "images = blocks");
            let caps = kitty().mode(ImageMode::None).decide();
            assert_eq!(caps.graphics, Graphics::None);
            assert_eq!(reason(&caps, topic::GRAPHICS), "images = none");
            let mono = Caps {
                color: ColorDepth::Mono,
                ..Caps::full()
            };
            let caps = kitty()
                .mode(ImageMode::Blocks)
                .decide_with(mono, &ImageOptions::default());
            assert_eq!(caps.graphics, Graphics::None);
            // Explicit pixel modes still respect "no escape sequences".
            let caps = kitty()
                .mode(ImageMode::Kitty)
                .decide_with(Caps::plain(), &ImageOptions::default());
            assert_eq!(caps.graphics, Graphics::None);
        }

        #[test]
        fn no_signal_at_all_gives_blocks() {
            let caps = Case::new(&[], None).decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            let sixel = if SIXEL_BUILT {
                "sixel ✗ not probed"
            } else {
                "sixel ✗ built without sixel"
            };
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                format!("kitty ✗ not probed · {sixel}")
            );
        }

        #[test]
        fn no_escape_sequences_means_alt_text_in_every_mode() {
            // `TERM=dumb` on a terminal that still answered everything: even
            // a forced pixel mode must not write escape sequences.
            let dumb = Caps {
                color: ColorDepth::None,
                ..Caps::full()
            };
            for probe in [KITTY, XTERM_VT340] {
                for mode in [
                    ImageMode::Auto,
                    ImageMode::Kitty,
                    ImageMode::Iterm,
                    ImageMode::Sixel,
                    ImageMode::Blocks,
                    ImageMode::None,
                ] {
                    let caps = Case::new(&[("TERM", "dumb")], Some(probe))
                        .mode(mode)
                        .decide_with(dumb.clone(), &ImageOptions::default());
                    assert_eq!(caps.graphics, Graphics::None, "{mode:?}");
                }
            }
        }

        #[test]
        fn attributes_only_output_still_gets_pixels() {
            // NO_COLOR: images are content, not decoration; only block images
            // need colours.
            let mono = Caps {
                color: ColorDepth::Mono,
                ..Caps::full()
            };
            let caps = Case::new(&[("TERM", "xterm-kitty")], Some(KITTY))
                .decide_with(mono, &ImageOptions::default());
            assert_eq!(caps.graphics, Graphics::KittyPlaceholders);
        }

        #[test]
        fn forced_kitty_trusts_an_identity_from_the_environment() {
            // Ghostty always has placeholders, so no version is needed; kitty
            // without a reported version gets classic placements.
            let ghostty = [
                ("TERM_PROGRAM", "ghostty"),
                ("TERM_PROGRAM_VERSION", "1.3.1"),
            ];
            let caps = Case::new(&ghostty, None).mode(ImageMode::Kitty).decide();
            assert_eq!(caps.graphics, Graphics::KittyPlaceholders);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "images = kitty · Ghostty 1.3.1 has Unicode placeholders"
            );
            let kitty = [("TERM", "xterm-kitty")];
            let caps = Case::new(&kitty, None).mode(ImageMode::Kitty).decide();
            assert_eq!(caps.graphics, Graphics::KittyClassic);
        }

        #[test]
        fn explicit_modes_are_honoured_when_piped() {
            // Automatic detection never sends pixels into a pipe, but an
            // explicit request is the user's call (`emde --images kitty
            // --color=always x.md > saved`), as long as escapes are allowed.
            let piped = Caps {
                is_tty: false,
                ..Caps::full()
            };
            let iterm = [
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "3.6.9"),
            ];
            let caps = Case::new(&iterm, None).decide_with(piped.clone(), &ImageOptions::default());
            assert_eq!(caps.graphics, Graphics::Blocks);
            let caps = Case::new(&iterm, None)
                .mode(ImageMode::Kitty)
                .decide_with(piped, &ImageOptions::default());
            assert_eq!(caps.graphics, Graphics::KittyPlaceholders);
        }

        #[test]
        fn iterm_images_never_go_through_tmux() {
            // Even with passthrough on: tmux neither moves the outer cursor
            // for them nor redraws them.
            let caps = Case::new(&[SSH], Some(ITERM2_VIA_TMUX))
                .in_tmux(tmux_lines::USER_SESSION_PASSTHROUGH, true)
                .mode(ImageMode::Iterm)
                .decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            assert_eq!(
                reason(&caps, topic::GRAPHICS),
                "images = iterm · iterm ✗ never inside tmux"
            );
        }

        #[test]
        fn osc1337_needs_a_reported_name_or_term_program() {
            // TERM=wezterm alone is not enough for OSC 1337 (it is for glyphs).
            let caps = Case::new(&[("TERM", "wezterm")], Some(VSCODE_PLAIN)).decide();
            assert_eq!(caps.graphics, Graphics::Blocks);
            let caps = Case::new(&[("TERM_PROGRAM", "WarpTerminal")], None).decide();
            assert_eq!(caps.graphics, Graphics::Iterm);
            let caps = Case::new(&[("TERM_PROGRAM", "Tabby")], Some(VSCODE_PLAIN)).decide();
            assert_eq!(
                caps.graphics,
                Graphics::Iterm,
                "Tabby answers XTVERSION as xterm.js"
            );
        }
    }
}
