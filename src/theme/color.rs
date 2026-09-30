//! Colour values as written in theme and config files, and their resolution.
//!
//! A [`ColorSpec`] is parsed from:
//!
//! * `"#rrggbb"` or `"#rgb"`;
//! * `"default"`, the terminal's own colour;
//! * an integer 0–255 (0–15 are the ANSI colours, 16–255 the xterm palette);
//! * a name: `"base"` (the page background: the terminal's own when known,
//!   else the palette's `base`), `"surface"` (a panel colour derived from
//!   `base`), a palette entry, or an ANSI name (`red`, `bright-red`, …),
//!   looked up in that order;
//! * a tint `"NAME/NN%"` or `"#rrggbb/NN%"`: the colour mixed NN% over the
//!   page background in OKLab (`"yellow/30%"`).
//!
//! Names are resolved per theme variant by [`Resolver`] once the palette and
//! the page background are known; palette entries themselves may be
//! literals, ANSI names or references to other entries ([`resolve_palette`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::de::{self, Deserialize, Deserializer, Unexpected, Visitor};

use crate::color::mix_oklab;
use crate::style::{Color, Rgb};

/// Name of the page background colour.
pub(crate) const BASE: &str = "base";
/// Name of the panel colour derived from the page background.
pub(crate) const SURFACE: &str = "surface";

/// ANSI colour names for indices 0–7 (`bright-` adds 8).
const ANSI: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

/// A colour as written in a theme or config file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ColorSpec {
    /// `default`: the terminal's default colour.
    Default,
    /// A literal 24-bit colour.
    Rgb(Rgb),
    /// One of the 16 ANSI colours (0–15).
    Ansi(u8),
    /// An xterm palette index (16–255).
    Indexed(u8),
    /// A palette, derived or ANSI colour name, resolved later.
    Name(String),
    /// A colour mixed `percent`% over the page background.
    Tint {
        /// The colour being mixed in.
        source: TintSource,
        /// 0–100.
        percent: u8,
    },
}

/// What a tint mixes over the page background.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TintSource {
    /// A literal colour.
    Rgb(Rgb),
    /// A colour name, resolved like any other.
    Name(String),
}

/// Why a colour string could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColorParseError(String);

impl fmt::Display for ColorParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ColorParseError {}

fn parse_error(msg: impl Into<String>) -> ColorParseError {
    ColorParseError(msg.into())
}

/// The ANSI index for a colour name (`red`, `bright-red`, `bright_red`, `grey`).
pub(crate) fn ansi_index(name: &str) -> Option<u8> {
    let n = name.trim().to_ascii_lowercase().replace('_', "-");
    if matches!(n.as_str(), "grey" | "gray") {
        return Some(8);
    }
    let (bright, base) = match n.strip_prefix("bright-") {
        Some(rest) => (8, rest),
        None => (0, n.as_str()),
    };
    let idx = ANSI.iter().position(|&a| a == base)?;
    u8::try_from(idx).ok().map(|i| i + bright)
}

/// All ANSI colour names, for suggestions.
pub(crate) fn ansi_names() -> impl Iterator<Item = String> {
    ANSI.iter()
        .map(|n| (*n).to_owned())
        .chain(ANSI.iter().map(|n| format!("bright-{n}")))
}

/// Colour for an integer 0–255.
fn from_index(i: u8) -> ColorSpec {
    if i < 16 {
        ColorSpec::Ansi(i)
    } else {
        ColorSpec::Indexed(i)
    }
}

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && !s
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '#')
}

impl ColorSpec {
    /// Parse a colour string.
    pub(crate) fn parse(input: &str) -> Result<ColorSpec, ColorParseError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(parse_error("empty colour"));
        }
        if let Some((source, amount)) = s.rsplit_once('/') {
            return parse_tint(source.trim(), amount.trim());
        }
        if s.eq_ignore_ascii_case("default") {
            return Ok(ColorSpec::Default);
        }
        if s.starts_with('#') {
            return Rgb::parse_hex(s).map(ColorSpec::Rgb).ok_or_else(|| {
                parse_error(format!(
                    "invalid colour `{s}`, expected `#rgb` or `#rrggbb`"
                ))
            });
        }
        if s.bytes().all(|b| b.is_ascii_digit()) {
            return s
                .parse::<u8>()
                .map(from_index)
                .map_err(|_| parse_error(format!("colour index {s} is out of range (0-255)")));
        }
        if is_name(s) {
            return Ok(ColorSpec::Name(s.to_owned()));
        }
        Err(parse_error(format!("invalid colour `{s}`")))
    }

    /// The name this colour refers to, if any (directly or as a tint source).
    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            ColorSpec::Name(n)
            | ColorSpec::Tint {
                source: TintSource::Name(n),
                ..
            } => Some(n),
            _ => None,
        }
    }
}

fn parse_tint(source: &str, amount: &str) -> Result<ColorSpec, ColorParseError> {
    let bad = || {
        parse_error(format!(
            "invalid tint `{source}/{amount}`, expected `COLOUR/NN%` with NN from 0 to 100"
        ))
    };
    let pct = amount.strip_suffix('%').ok_or_else(bad)?.trim();
    let percent = pct
        .parse::<u8>()
        .ok()
        .filter(|p| *p <= 100)
        .ok_or_else(bad)?;
    let source = if source.starts_with('#') {
        TintSource::Rgb(Rgb::parse_hex(source).ok_or_else(bad)?)
    } else if is_name(source) && !source.eq_ignore_ascii_case("default") {
        TintSource::Name(source.to_owned())
    } else {
        return Err(bad());
    };
    Ok(ColorSpec::Tint { source, percent })
}

impl fmt::Display for ColorSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn hex(f: &mut fmt::Formatter<'_>, c: Rgb) -> fmt::Result {
            write!(f, "#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
        }
        match self {
            ColorSpec::Default => f.write_str("default"),
            ColorSpec::Rgb(c) => hex(f, *c),
            ColorSpec::Ansi(i) | ColorSpec::Indexed(i) => write!(f, "{i}"),
            ColorSpec::Name(n) => f.write_str(n),
            ColorSpec::Tint { source, percent } => {
                match source {
                    TintSource::Rgb(c) => hex(f, *c)?,
                    TintSource::Name(n) => f.write_str(n)?,
                }
                write!(f, "/{percent}%")
            }
        }
    }
}

struct ColorVisitor;

impl Visitor<'_> for ColorVisitor {
    type Value = ColorSpec;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a colour: \"#rrggbb\", \"#rgb\", a colour name, 0-255 or \"default\"")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<ColorSpec, E> {
        ColorSpec::parse(v).map_err(E::custom)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<ColorSpec, E> {
        u8::try_from(v)
            .map(from_index)
            .map_err(|_| E::invalid_value(Unexpected::Signed(v), &"a colour index from 0 to 255"))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<ColorSpec, E> {
        u8::try_from(v)
            .map(from_index)
            .map_err(|_| E::invalid_value(Unexpected::Unsigned(v), &"a colour index from 0 to 255"))
    }
}

impl<'de> Deserialize<'de> for ColorSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ColorVisitor)
    }
}

/// A colour name that could not be resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnknownColor(pub(crate) String);

/// Resolves colour specs for one theme variant.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Resolver<'a> {
    /// The variant's resolved palette.
    pub(crate) palette: &'a BTreeMap<String, Color>,
    /// Page background.
    pub(crate) base: Rgb,
    /// Panel colour derived from `base`.
    pub(crate) surface: Rgb,
}

impl Resolver<'_> {
    /// Resolve a spec to a terminal colour.
    pub(crate) fn resolve(&self, spec: &ColorSpec) -> Result<Color, UnknownColor> {
        Ok(match spec {
            ColorSpec::Default => Color::Default,
            ColorSpec::Rgb(c) => Color::Rgb(*c),
            ColorSpec::Ansi(i) => Color::Ansi(*i),
            ColorSpec::Indexed(i) => Color::Indexed(*i),
            ColorSpec::Name(n) => self.name(n)?,
            ColorSpec::Tint { source, percent } => {
                let over = match source {
                    TintSource::Rgb(c) => Color::Rgb(*c),
                    TintSource::Name(n) => self.name(n)?,
                };
                match over {
                    Color::Rgb(c) => {
                        Color::Rgb(mix_oklab(self.base, c, f32::from(*percent) / 100.0))
                    }
                    // Palette indices cannot be mixed; use the colour itself.
                    other => other,
                }
            }
        })
    }

    fn name(&self, name: &str) -> Result<Color, UnknownColor> {
        if name == BASE {
            return Ok(Color::Rgb(self.base));
        }
        if name == SURFACE {
            return Ok(Color::Rgb(self.surface));
        }
        if let Some(&c) = self.palette.get(name) {
            return Ok(c);
        }
        ansi_index(name)
            .map(Color::Ansi)
            .ok_or_else(|| UnknownColor(name.to_owned()))
    }
}

/// A problem found while resolving a palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaletteProblem {
    /// An entry names a colour that is neither a palette entry nor an ANSI colour.
    Unknown {
        /// The entry.
        key: String,
        /// The unknown name.
        name: String,
    },
    /// Entries refer to each other in a loop.
    Cycle(Vec<String>),
    /// A chain of references is longer than [`MAX_PALETTE_DEPTH`].
    TooDeep(String),
    /// A tint was used as a palette value (tints need the page background).
    Tint(String),
}

impl fmt::Display for PaletteProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaletteProblem::Unknown { key, name } => {
                write!(
                    f,
                    "palette colour `{key}` refers to unknown colour `{name}`"
                )
            }
            PaletteProblem::Cycle(keys) => {
                write!(
                    f,
                    "palette colours refer to each other in a loop: {}",
                    keys.join(" → ")
                )
            }
            PaletteProblem::TooDeep(key) => write!(
                f,
                "palette colour `{key}` is a chain of more than {MAX_PALETTE_DEPTH} references"
            ),
            PaletteProblem::Tint(key) => write!(
                f,
                "palette colour `{key}` is a tint; tints can only be used in styles"
            ),
        }
    }
}

/// Maximum length of a chain of palette references.
const MAX_PALETTE_DEPTH: usize = 16;

/// Resolve palette entries to colours.
///
/// An entry may be a literal, an ANSI name, or the name of another entry.
/// A name equal to the entry's own key is an ANSI name, so `red = "red"`
/// means ANSI red. Unresolvable entries are left out and reported.
pub(crate) fn resolve_palette(
    entries: &BTreeMap<String, ColorSpec>,
) -> (BTreeMap<String, Color>, Vec<PaletteProblem>) {
    let mut done: BTreeMap<String, Option<Color>> = BTreeMap::new();
    let mut problems = Vec::new();
    for key in entries.keys() {
        let mut stack = Vec::new();
        resolve_entry(key, entries, &mut done, &mut stack, &mut problems);
    }
    let resolved = done
        .into_iter()
        .filter_map(|(k, v)| Some((k, v?)))
        .collect();
    (resolved, problems)
}

fn resolve_entry(
    key: &str,
    entries: &BTreeMap<String, ColorSpec>,
    done: &mut BTreeMap<String, Option<Color>>,
    stack: &mut Vec<String>,
    problems: &mut Vec<PaletteProblem>,
) -> Option<Color> {
    if let Some(&c) = done.get(key) {
        return c;
    }
    if let Some(pos) = stack.iter().position(|k| k == key) {
        let mut cycle: Vec<String> = stack.get(pos..).unwrap_or_default().to_vec();
        cycle.push(key.to_owned());
        problems.push(PaletteProblem::Cycle(cycle));
        return None;
    }
    if stack.len() >= MAX_PALETTE_DEPTH {
        let first = stack.first().map_or(key, String::as_str);
        problems.push(PaletteProblem::TooDeep(first.to_owned()));
        return None;
    }
    let spec = entries.get(key)?;
    stack.push(key.to_owned());
    let color = match spec {
        ColorSpec::Default => Some(Color::Default),
        ColorSpec::Rgb(c) => Some(Color::Rgb(*c)),
        ColorSpec::Ansi(i) => Some(Color::Ansi(*i)),
        ColorSpec::Indexed(i) => Some(Color::Indexed(*i)),
        ColorSpec::Tint { .. } => {
            problems.push(PaletteProblem::Tint(key.to_owned()));
            None
        }
        ColorSpec::Name(name) => {
            if name != key && entries.contains_key(name.as_str()) {
                resolve_entry(name, entries, done, stack, problems)
            } else if let Some(i) = ansi_index(name) {
                Some(Color::Ansi(i))
            } else {
                problems.push(PaletteProblem::Unknown {
                    key: key.to_owned(),
                    name: name.clone(),
                });
                None
            }
        }
    };
    stack.pop();
    done.insert(key.to_owned(), color);
    color
}

/// Names a style colour may use given the palette keys (for checks and
/// suggestions): `base`, `surface`, palette keys and ANSI names.
pub(crate) fn known_names(palette_keys: &BTreeSet<String>) -> Vec<String> {
    let mut names: Vec<String> = vec![BASE.to_owned(), SURFACE.to_owned()];
    names.extend(palette_keys.iter().cloned());
    names.extend(ansi_names());
    names
}

/// Whether a style colour name resolves given the palette keys.
pub(crate) fn name_resolves(name: &str, palette_keys: &BTreeSet<String>) -> bool {
    name == BASE || name == SURFACE || palette_keys.contains(name) || ansi_index(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ColorSpec {
        ColorSpec::parse(s).unwrap()
    }

    #[test]
    fn parses_literals() {
        assert_eq!(p("#89b4fa"), ColorSpec::Rgb(Rgb(0x89, 0xb4, 0xfa)));
        assert_eq!(p(" #fff "), ColorSpec::Rgb(Rgb(255, 255, 255)));
        assert_eq!(p("default"), ColorSpec::Default);
        assert_eq!(p("Default"), ColorSpec::Default);
        assert_eq!(p("0"), ColorSpec::Ansi(0));
        assert_eq!(p("15"), ColorSpec::Ansi(15));
        assert_eq!(p("16"), ColorSpec::Indexed(16));
        assert_eq!(p("255"), ColorSpec::Indexed(255));
        assert_eq!(p("accent"), ColorSpec::Name("accent".into()));
        assert_eq!(p("bright-red"), ColorSpec::Name("bright-red".into()));
    }

    #[test]
    fn parses_tints() {
        assert_eq!(
            p("yellow/30%"),
            ColorSpec::Tint {
                source: TintSource::Name("yellow".into()),
                percent: 30
            }
        );
        assert_eq!(
            p("#313244 / 50 %"),
            ColorSpec::Tint {
                source: TintSource::Rgb(Rgb(0x31, 0x32, 0x44)),
                percent: 50
            }
        );
        assert_eq!(p("red/0%").to_string(), "red/0%");
        assert_eq!(p("red/100%").to_string(), "red/100%");
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "  ",
            "#12345",
            "#gggggg",
            "256",
            "999",
            "red/30",
            "red/101%",
            "red/x%",
            "/30%",
            "#12/30%",
            "default/30%",
            "a b",
            "a\u{1b}b",
        ] {
            assert!(ColorSpec::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn display_roundtrips() {
        for s in [
            "#89b4fa",
            "default",
            "7",
            "200",
            "accent",
            "yellow/30%",
            "#313244/50%",
        ] {
            assert_eq!(p(s).to_string(), s);
            assert_eq!(p(&p(s).to_string()), p(s));
        }
    }

    #[test]
    fn deserialises_strings_and_integers() {
        #[derive(Debug, serde::Deserialize)]
        struct T {
            a: ColorSpec,
            b: ColorSpec,
        }
        let t: T = toml::from_str("a = \"#fff\"\nb = 196").unwrap();
        assert_eq!(t.a, ColorSpec::Rgb(Rgb(255, 255, 255)));
        assert_eq!(t.b, ColorSpec::Indexed(196));
        assert!(toml::from_str::<T>("a = 256\nb = 1").is_err());
        assert!(toml::from_str::<T>("a = -1\nb = 1").is_err());
        assert!(toml::from_str::<T>("a = true\nb = 1").is_err());
        let err = toml::from_str::<T>("a = \"#12\"\nb = 1").unwrap_err();
        assert!(
            err.message().contains("expected `#rgb` or `#rrggbb`"),
            "{err}"
        );
    }

    #[test]
    fn ansi_names() {
        assert_eq!(ansi_index("black"), Some(0));
        assert_eq!(ansi_index("white"), Some(7));
        assert_eq!(ansi_index("bright-black"), Some(8));
        assert_eq!(ansi_index("Bright_White"), Some(15));
        assert_eq!(ansi_index("grey"), Some(8));
        assert_eq!(ansi_index("purple"), None);
        assert_eq!(super::ansi_names().count(), 16);
    }

    fn palette(pairs: &[(&str, &str)]) -> BTreeMap<String, ColorSpec> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), p(v))).collect()
    }

    #[test]
    fn palette_references() {
        let (pal, problems) = resolve_palette(&palette(&[
            ("blue", "blue"),
            ("accent", "blue"),
            ("link", "accent"),
            ("text", "#cdd6f4"),
            ("dim", "default"),
            ("hi", "200"),
        ]));
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            pal["blue"],
            Color::Ansi(4),
            "a self-reference is the ANSI name"
        );
        assert_eq!(pal["accent"], Color::Ansi(4));
        assert_eq!(pal["link"], Color::Ansi(4));
        assert_eq!(pal["text"], Color::Rgb(Rgb(0xcd, 0xd6, 0xf4)));
        assert_eq!(pal["dim"], Color::Default);
        assert_eq!(pal["hi"], Color::Indexed(200));
    }

    #[test]
    fn palette_problems() {
        let (pal, problems) = resolve_palette(&palette(&[
            ("a", "b"),
            ("b", "a"),
            ("c", "nope"),
            ("d", "red/20%"),
            ("e", "#000"),
        ]));
        assert_eq!(pal.len(), 1);
        assert_eq!(pal["e"], Color::Rgb(Rgb(0, 0, 0)));
        assert!(problems.contains(&PaletteProblem::Cycle(vec![
            "a".into(),
            "b".into(),
            "a".into()
        ])));
        assert!(problems.contains(&PaletteProblem::Unknown {
            key: "c".into(),
            name: "nope".into()
        }));
        assert!(problems.contains(&PaletteProblem::Tint("d".into())));
        assert_eq!(
            PaletteProblem::Cycle(vec!["a".into(), "b".into(), "a".into()]).to_string(),
            "palette colours refer to each other in a loop: a → b → a"
        );
    }

    #[test]
    fn long_reference_chains_are_cut() {
        let mut entries = BTreeMap::new();
        for i in 0..40 {
            entries.insert(
                format!("c{i:02}"),
                ColorSpec::Name(format!("c{:02}", i + 1)),
            );
        }
        entries.insert("c40".into(), ColorSpec::Rgb(Rgb(1, 2, 3)));
        let (pal, problems) = resolve_palette(&entries);
        // Entries close enough to the literal resolve; the rest are reported.
        assert_eq!(pal.get("c40"), Some(&Color::Rgb(Rgb(1, 2, 3))));
        assert_eq!(pal.get("c39"), Some(&Color::Rgb(Rgb(1, 2, 3))));
        assert!(problems.contains(&PaletteProblem::TooDeep("c00".into())));
    }

    #[test]
    fn resolver_order_and_tints() {
        let pal: BTreeMap<String, Color> = [
            ("yellow".to_owned(), Color::Rgb(Rgb(0xf9, 0xe2, 0xaf))),
            ("surface".to_owned(), Color::Rgb(Rgb(0x31, 0x32, 0x44))),
            ("ansi".to_owned(), Color::Ansi(3)),
        ]
        .into();
        let r = Resolver {
            palette: &pal,
            base: Rgb(0x1e, 0x1e, 0x2e),
            surface: Rgb(0x30, 0x30, 0x40),
        };
        // Derived names win over palette entries of the same name.
        assert_eq!(
            r.resolve(&p("surface")),
            Ok(Color::Rgb(Rgb(0x30, 0x30, 0x40)))
        );
        assert_eq!(r.resolve(&p("base")), Ok(Color::Rgb(Rgb(0x1e, 0x1e, 0x2e))));
        // Palette entries win over ANSI names; ANSI names are the fallback.
        assert_eq!(
            r.resolve(&p("yellow")),
            Ok(Color::Rgb(Rgb(0xf9, 0xe2, 0xaf)))
        );
        assert_eq!(r.resolve(&p("red")), Ok(Color::Ansi(1)));
        assert_eq!(r.resolve(&p("nope")), Err(UnknownColor("nope".into())));
        // Tints mix over the base in OKLab.
        let tint = r.resolve(&p("yellow/30%")).unwrap();
        assert_eq!(
            tint,
            Color::Rgb(mix_oklab(Rgb(0x1e, 0x1e, 0x2e), Rgb(0xf9, 0xe2, 0xaf), 0.3))
        );
        assert_eq!(
            r.resolve(&p("yellow/0%")),
            Ok(Color::Rgb(Rgb(0x1e, 0x1e, 0x2e)))
        );
        assert_eq!(
            r.resolve(&p("#f9e2af/100%")),
            Ok(Color::Rgb(Rgb(0xf9, 0xe2, 0xaf)))
        );
        // Palette indices cannot be mixed.
        assert_eq!(r.resolve(&p("ansi/50%")), Ok(Color::Ansi(3)));
        assert_eq!(r.resolve(&p("nope/50%")), Err(UnknownColor("nope".into())));
    }

    #[test]
    fn name_checks() {
        let keys: BTreeSet<String> = ["accent".to_owned()].into();
        assert!(name_resolves("accent", &keys));
        assert!(name_resolves("base", &keys));
        assert!(name_resolves("bright-cyan", &keys));
        assert!(!name_resolves("acent", &keys));
        assert!(known_names(&keys).contains(&"accent".to_owned()));
    }
}
