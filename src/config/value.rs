//! Parsing of named setting values (`align = "center"`) and a few special
//! value forms (`max_height = "60%"`, `open = "auto"`).
//!
//! Names are matched case-insensitively with `_` and `-` treated alike, so
//! `unicode_italic` and `Unicode-Italic` both work. Unknown names produce an
//! error that lists the accepted values and suggests the closest one.

use std::fmt::{self, Write as _};
use std::marker::PhantomData;

use serde::Deserializer;
use serde::de::{self, Unexpected, Visitor};

use super::suggest::did_you_mean;
use crate::options::{
    Align, BlockGlyphs, CodeStyle, DisplayMath, FrontMatterMode, H1Style, H2Style, Height,
    HtmlMode, IconSet, ImageMode, InlineMath, TableBorder, When,
};
use crate::style::Underline;
use emde_math::{Bold, Fractions, Letters, ScriptSet};

/// `render.color` is the colour choice of terminal detection, parsed here
/// with the same names as the `--color` flag.
pub use crate::term::color::ColorChoice;

/// A setting whose value is one of a fixed set of names.
pub(crate) trait Named: Copy + PartialEq + 'static {
    /// The documented names (lowercase, `-` separated), in documentation order.
    const NAMES: &'static [(&'static str, Self)];
    /// Further accepted spellings that are not advertised in messages.
    const ALIASES: &'static [(&'static str, Self)] = &[];

    /// Look a value up by name.
    fn from_name(name: &str) -> Option<Self> {
        let key = normalize(name);
        Self::NAMES
            .iter()
            .chain(Self::ALIASES)
            .find(|(n, _)| *n == key)
            .map(|&(_, v)| v)
    }

    /// The documented name of a value.
    #[cfg(test)]
    fn name(self) -> &'static str {
        Self::NAMES
            .iter()
            .find(|(_, v)| *v == self)
            .map_or("?", |(n, _)| n)
    }
}

/// Lowercase, trimmed, `_` → `-`.
#[inline(never)]
fn normalize(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('_', "-")
}

/// Implement [`Named`] from a `"name" => Variant` list.
macro_rules! named_values {
    (
        $ty:ty { $( $name:literal => $val:expr ),* $(,)? }
        $( aliases { $( $alias:literal => $aval:expr ),* $(,)? } )?
    ) => {
        impl Named for $ty {
            const NAMES: &'static [(&'static str, Self)] = &[$(($name, $val)),*];
            $( const ALIASES: &'static [(&'static str, Self)] = &[$(($alias, $aval)),*]; )?
        }
    };
}

/// `theme.background`: which variant of the theme to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BackgroundMode {
    /// From the terminal's background colour (OSC 11, `COLORFGBG`); dark if unknown.
    #[default]
    Auto,
    Dark,
    Light,
}

/// `pager.search_case`: case sensitivity of searches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SearchCase {
    /// Case-sensitive only when the pattern contains an uppercase letter.
    #[default]
    Smart,
    Sensitive,
    Insensitive,
}

/// `pager.open`: how external links are opened.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum OpenCommand {
    /// `open`/`xdg-open` locally; copy the URL over SSH.
    #[default]
    Auto,
    /// Never open links.
    Never,
    /// A command line (split on whitespace) that receives the URL as its last argument.
    Command(String),
}

/// `FromStr` for public setting enums, with the same names and messages as
/// the config file (for command-line flags).
macro_rules! from_str_named {
    ($($ty:ty),*) => {$(
        impl std::str::FromStr for $ty {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, String> {
                <$ty as Named>::from_name(s).ok_or_else(|| unknown_name::<$ty>(s))
            }
        }
    )*};
}

from_str_named!(ColorChoice, BackgroundMode, SearchCase);

impl std::str::FromStr for OpenCommand {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let d = de::value::StrDeserializer::<de::value::Error>::new(s);
        match open_command(d) {
            Ok(Some(cmd)) => Ok(cmd),
            Ok(None) => Err("expected `auto`, `never` or a command".to_owned()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// `render.ambiguous_width`: 1 or 2 columns for East Asian Ambiguous characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AmbiguousWidth {
    Narrow,
    Wide,
}

/// `images.tmux_passthrough`: whether graphics may use tmux passthrough.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TmuxPassthrough {
    /// Only when the user enabled `allow-passthrough` (emde never changes tmux options).
    IfEnabled,
    Never,
}

named_values!(When { "auto" => When::Auto, "always" => When::Always, "never" => When::Never }
    aliases { "true" => When::Always, "false" => When::Never });
named_values!(Align { "center" => Align::Center, "left" => Align::Left }
    aliases { "centre" => Align::Center });
named_values!(H1Style { "bar" => H1Style::Bar, "underline" => H1Style::Underline, "plain" => H1Style::Plain });
named_values!(H2Style { "rule" => H2Style::Rule, "plain" => H2Style::Plain });
named_values!(TableBorder {
    "rounded" => TableBorder::Rounded,
    "light" => TableBorder::Light,
    "heavy" => TableBorder::Heavy,
    "double" => TableBorder::Double,
    "ascii" => TableBorder::Ascii,
    "none" => TableBorder::None,
});
named_values!(IconSet { "unicode" => IconSet::Unicode, "nerd" => IconSet::Nerd, "ascii" => IconSet::Ascii });
named_values!(FrontMatterMode {
    "card" => FrontMatterMode::Card,
    "code" => FrontMatterMode::Code,
    "hide" => FrontMatterMode::Hide,
});
named_values!(HtmlMode { "subset" => HtmlMode::Subset, "strip" => HtmlMode::Strip, "raw" => HtmlMode::Raw });
named_values!(CodeStyle {
    "auto" => CodeStyle::Auto,
    "panel" => CodeStyle::Panel,
    "frame" => CodeStyle::Frame,
    "gutter" => CodeStyle::Gutter,
});
named_values!(InlineMath { "unicode" => InlineMath::Unicode, "ascii" => InlineMath::Ascii, "raw" => InlineMath::Raw });
named_values!(DisplayMath { "2d" => DisplayMath::TwoD, "linear" => DisplayMath::Linear, "raw" => DisplayMath::Raw });
named_values!(ImageMode {
    "auto" => ImageMode::Auto,
    "kitty" => ImageMode::Kitty,
    "iterm" => ImageMode::Iterm,
    "sixel" => ImageMode::Sixel,
    "blocks" => ImageMode::Blocks,
    "none" => ImageMode::None,
} aliases { "iterm2" => ImageMode::Iterm });
named_values!(BlockGlyphs {
    "half" => BlockGlyphs::Half,
    "quadrant" => BlockGlyphs::Quadrant,
    "sextant" => BlockGlyphs::Sextant,
    "octant" => BlockGlyphs::Octant,
    "auto" => BlockGlyphs::Auto,
});
named_values!(Letters {
    "italic" => Letters::Italic,
    "unicode-italic" => Letters::UnicodeItalic,
    "plain" => Letters::Plain,
});
named_values!(ScriptSet { "safe" => ScriptSet::Safe, "full" => ScriptSet::Full });
named_values!(Fractions { "vulgar" => Fractions::Vulgar, "slash" => Fractions::Slash });
named_values!(Bold { "sgr" => Bold::Sgr, "unicode" => Bold::Unicode });
named_values!(Underline {
    "none" => Underline::None,
    "single" => Underline::Single,
    "double" => Underline::Double,
    "curly" => Underline::Curly,
    "dotted" => Underline::Dotted,
    "dashed" => Underline::Dashed,
} aliases { "true" => Underline::Single, "false" => Underline::None });
named_values!(ColorChoice {
    "auto" => ColorChoice::Auto,
    "always" => ColorChoice::Always,
    "never" => ColorChoice::Never,
    "truecolor" => ColorChoice::TrueColor,
    "256" => ColorChoice::Ansi256,
    "16" => ColorChoice::Ansi16,
} aliases { "24bit" => ColorChoice::TrueColor, "true" => ColorChoice::Always, "false" => ColorChoice::Never });
named_values!(BackgroundMode {
    "auto" => BackgroundMode::Auto,
    "dark" => BackgroundMode::Dark,
    "light" => BackgroundMode::Light,
});
named_values!(SearchCase {
    "smart" => SearchCase::Smart,
    "sensitive" => SearchCase::Sensitive,
    "insensitive" => SearchCase::Insensitive,
});
named_values!(AmbiguousWidth { "1" => AmbiguousWidth::Narrow, "2" => AmbiguousWidth::Wide });
named_values!(TmuxPassthrough { "if-enabled" => TmuxPassthrough::IfEnabled, "never" => TmuxPassthrough::Never }
    aliases { "true" => TmuxPassthrough::IfEnabled, "false" => TmuxPassthrough::Never });

/// The documented names of `T`'s values. The messages built from them
/// are compiled once ([`list_of`], [`unknown_among`]), not for every type.
fn names<T: Named>() -> Vec<&'static str> {
    T::NAMES.iter().map(|(n, _)| *n).collect()
}

/// "`a`, `b` or `c`".
fn name_list<T: Named>() -> String {
    list_of(&names::<T>())
}

/// The error message for an unknown name.
pub(crate) fn unknown_name<T: Named>(value: &str) -> String {
    unknown_among(value, &names::<T>())
}

/// "`a`, `b` or `c`" for `names`.
#[inline(never)]
fn list_of(names: &[&str]) -> String {
    let mut out = String::new();
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push_str(if i + 1 == names.len() { " or " } else { ", " });
        }
        let _ = write!(out, "`{name}`");
    }
    out
}

/// The error message for `value`, which is none of `names`.
#[inline(never)]
fn unknown_among(value: &str, names: &[&str]) -> String {
    let mut msg = format!("invalid value `{value}`, expected {}", list_of(names));
    if let Some(s) = did_you_mean(&normalize(value), names.iter().copied()) {
        let _ = write!(msg, " (did you mean `{s}`?)");
    }
    msg
}

struct NamedVisitor<T>(PhantomData<T>);

impl<'de, T: Named> Visitor<'de> for NamedVisitor<T> {
    type Value = T;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&name_list::<T>())
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<T, E> {
        T::from_name(v).ok_or_else(|| E::custom(unknown_name::<T>(v)))
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<T, E> {
        T::from_name(if v { "true" } else { "false" })
            .ok_or_else(|| E::invalid_type(Unexpected::Bool(v), &self))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<T, E> {
        T::from_name(&v.to_string()).ok_or_else(|| E::invalid_type(Unexpected::Signed(v), &self))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<T, E> {
        T::from_name(&v.to_string()).ok_or_else(|| E::invalid_type(Unexpected::Unsigned(v), &self))
    }
}

/// `deserialize_with` helper for `Option<T>` fields holding a [`Named`] value.
pub(crate) fn named<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Named,
{
    deserializer
        .deserialize_any(NamedVisitor(PhantomData))
        .map(Some)
}

/// Whether a string contains characters that must never reach the terminal.
pub(crate) fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

struct HeightVisitor;

impl HeightVisitor {
    fn rows<E: de::Error>(v: u64) -> Result<Height, E> {
        match u16::try_from(v) {
            Ok(rows) if rows > 0 => Ok(Height::Rows(rows)),
            _ => Err(E::custom(format!(
                "invalid height {v}, expected 1 to {} rows",
                u16::MAX
            ))),
        }
    }
}

impl Visitor<'_> for HeightVisitor {
    type Value = Height;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a number of rows (20) or a share of the screen (\"60%\")")
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Height, E> {
        match u64::try_from(v) {
            Ok(v) => Self::rows(v),
            Err(_) => Err(E::invalid_value(Unexpected::Signed(v), &self)),
        }
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Height, E> {
        Self::rows(v)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Height, E> {
        let t = v.trim();
        if let Some(pct) = t.strip_suffix('%') {
            return match pct.trim().parse::<u8>() {
                Ok(p) if (1..=100).contains(&p) => Ok(Height::Percent(p)),
                _ => Err(E::invalid_value(
                    Unexpected::Str(v),
                    &"a percentage from 1% to 100%",
                )),
            };
        }
        match t.parse::<u64>() {
            Ok(rows) => Self::rows(rows),
            Err(_) => Err(E::invalid_value(Unexpected::Str(v), &self)),
        }
    }
}

/// `deserialize_with` helper for `images.max_height`.
pub(crate) fn height<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Height>, D::Error> {
    deserializer.deserialize_any(HeightVisitor).map(Some)
}

struct OpenVisitor;

impl Visitor<'_> for OpenVisitor {
    type Value = OpenCommand;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("`auto`, `never` or a command")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<OpenCommand, E> {
        let t = v.trim();
        if t.is_empty() || has_control(t) {
            return Err(E::invalid_value(Unexpected::Str(v), &self));
        }
        Ok(match normalize(t).as_str() {
            "auto" => OpenCommand::Auto,
            "never" => OpenCommand::Never,
            _ => OpenCommand::Command(t.to_owned()),
        })
    }
}

/// `deserialize_with` helper for `pager.open`.
pub(crate) fn open_command<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<OpenCommand>, D::Error> {
    deserializer.deserialize_any(OpenVisitor).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Probe {
        #[serde(default, deserialize_with = "named")]
        align: Option<Align>,
        #[serde(default, deserialize_with = "named")]
        width: Option<AmbiguousWidth>,
        #[serde(default, deserialize_with = "named")]
        color: Option<ColorChoice>,
        #[serde(default, deserialize_with = "height")]
        height: Option<Height>,
        #[serde(default, deserialize_with = "open_command")]
        open: Option<OpenCommand>,
    }

    fn probe(src: &str) -> Result<Probe, String> {
        toml::from_str(src).map_err(|e| e.message().to_owned())
    }

    #[test]
    fn names_are_normalised() {
        assert_eq!(
            Letters::from_name("Unicode_Italic"),
            Some(Letters::UnicodeItalic)
        );
        assert_eq!(Letters::from_name(" italic "), Some(Letters::Italic));
        assert_eq!(DisplayMath::from_name("2D"), Some(DisplayMath::TwoD));
        assert_eq!(When::from_name("true"), Some(When::Always));
        assert_eq!(Align::from_name("centre"), Some(Align::Center));
        assert_eq!(Align::from_name("middle"), None);
        assert_eq!(Underline::Curly.name(), "curly");
        assert_eq!(ColorChoice::Ansi256.name(), "256");
    }

    #[test]
    fn documented_names_are_canonical() {
        fn check<T: Named + fmt::Debug>() {
            for (name, value) in T::NAMES {
                assert_eq!(normalize(name), *name);
                assert_eq!(T::from_name(name), Some(*value));
                assert_eq!(value.name(), *name);
            }
        }
        check::<When>();
        check::<Align>();
        check::<H1Style>();
        check::<H2Style>();
        check::<TableBorder>();
        check::<IconSet>();
        check::<FrontMatterMode>();
        check::<HtmlMode>();
        check::<CodeStyle>();
        check::<InlineMath>();
        check::<DisplayMath>();
        check::<ImageMode>();
        check::<BlockGlyphs>();
        check::<Letters>();
        check::<ScriptSet>();
        check::<Fractions>();
        check::<Bold>();
        check::<Underline>();
        check::<ColorChoice>();
        check::<BackgroundMode>();
        check::<SearchCase>();
        check::<AmbiguousWidth>();
        check::<TmuxPassthrough>();
    }

    #[test]
    fn deserialises_strings_integers_and_bools() {
        let p = probe("align = \"left\"\nwidth = 2\ncolor = 256").unwrap();
        assert_eq!(p.align, Some(Align::Left));
        assert_eq!(p.width, Some(AmbiguousWidth::Wide));
        assert_eq!(p.color, Some(ColorChoice::Ansi256));
        let p = probe("color = \"16\"\nwidth = \"1\"").unwrap();
        assert_eq!(p.color, Some(ColorChoice::Ansi16));
        assert_eq!(p.width, Some(AmbiguousWidth::Narrow));
        assert!(probe("").unwrap().align.is_none());
    }

    #[test]
    fn unknown_names_list_choices_and_suggest() {
        let err = probe("align = \"centr\"").unwrap_err();
        assert_eq!(
            err,
            "invalid value `centr`, expected `center` or `left` (did you mean `center`?)"
        );
        let err = probe("width = 3").unwrap_err();
        assert!(err.contains("expected `1` or `2`"), "{err}");
        let err = probe("align = []").unwrap_err();
        assert!(err.contains("expected `center` or `left`"), "{err}");
    }

    #[test]
    fn heights() {
        assert_eq!(probe("height = 20").unwrap().height, Some(Height::Rows(20)));
        assert_eq!(
            probe("height = \"60%\"").unwrap().height,
            Some(Height::Percent(60))
        );
        assert_eq!(
            probe("height = \"12\"").unwrap().height,
            Some(Height::Rows(12))
        );
        for bad in ["0", "-3", "70000", "\"0%\"", "\"101%\"", "\"tall\"", "1.5"] {
            assert!(probe(&format!("height = {bad}")).is_err(), "{bad}");
        }
    }

    #[test]
    fn from_str_for_flags() {
        assert_eq!("256".parse::<ColorChoice>(), Ok(ColorChoice::Ansi256));
        assert_eq!(
            "TrueColor".parse::<ColorChoice>(),
            Ok(ColorChoice::TrueColor)
        );
        assert_eq!("light".parse::<BackgroundMode>(), Ok(BackgroundMode::Light));
        assert_eq!("smart".parse::<SearchCase>(), Ok(SearchCase::Smart));
        assert_eq!(
            "ligth".parse::<BackgroundMode>(),
            Err(
                "invalid value `ligth`, expected `auto`, `dark` or `light` (did you mean `light`?)"
                    .into()
            )
        );
        assert_eq!("never".parse::<OpenCommand>(), Ok(OpenCommand::Never));
        assert_eq!(
            "xdg-open".parse::<OpenCommand>(),
            Ok(OpenCommand::Command("xdg-open".into()))
        );
        assert!("".parse::<OpenCommand>().is_err());
    }

    #[test]
    fn open_commands() {
        assert_eq!(
            probe("open = \"auto\"").unwrap().open,
            Some(OpenCommand::Auto)
        );
        assert_eq!(
            probe("open = \"NEVER\"").unwrap().open,
            Some(OpenCommand::Never)
        );
        assert_eq!(
            probe("open = \" firefox --new-tab \"").unwrap().open,
            Some(OpenCommand::Command("firefox --new-tab".into()))
        );
        assert!(probe("open = \"\"").is_err());
        assert!(probe("open = \"a\\u001bb\"").is_err());
        assert!(probe("open = 3").is_err());
    }
}
