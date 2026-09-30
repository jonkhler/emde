//! Option-only configuration layers.
//!
//! Every source of settings (the embedded defaults, the user file, each
//! `--set`, the command-line flags) deserialises into the same [`ConfigLayer`]
//! shape, in which every value is optional. Layers are combined with
//! [`Merge`], higher layers winning field by field, and only the merged result
//! is turned into the resolved option structs. Because no layer type ever
//! needs a `Default` that parses TOML, the serde `default` recursion trap
//! cannot occur.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::value::{
    AmbiguousWidth, BackgroundMode, ColorChoice, OpenCommand, SearchCase, TmuxPassthrough, height,
    named, open_command,
};
use crate::options::{
    Align, BlockGlyphs, CodeStyle, DisplayMath, FrontMatterMode, H1Style, H2Style, Height,
    HtmlMode, IconSet, ImageMode, InlineMath, TableBorder, When,
};
use crate::theme::spec::{PaletteTable, StyleTable, VariantTable};
use emde_math::{Bold, Fractions, Letters, ScriptSet};

/// Combining a lower-priority value with a higher-priority one.
pub(crate) trait Merge {
    /// Merge `top`, which has priority, into `self`.
    fn merge(&mut self, top: Self);
}

/// A single setting: a set value replaces the lower layer's.
impl<T> Merge for Option<T> {
    fn merge(&mut self, top: Self) {
        if top.is_some() {
            *self = top;
        }
    }
}

/// Reporting settings that no layer provided (used to check that the
/// embedded defaults are complete).
#[cfg(test)]
pub(crate) trait Unset {
    /// Push the dotted path of every unset value below `path`.
    fn unset(&self, path: &str, out: &mut Vec<String>);
}

#[cfg(test)]
impl<T> Unset for Option<T> {
    fn unset(&self, path: &str, out: &mut Vec<String>) {
        if self.is_none() {
            out.push(path.to_owned());
        }
    }
}

/// `prefix.key`, or `key` at the top level.
#[cfg(test)]
pub(crate) fn join_key(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Declare an Option-only layer struct.
///
/// Every field gets `#[serde(default)]` (so a missing key is `None` or an
/// empty table) and the struct gets [`Merge`] (field by field), `Unset` (in
/// tests) and a `KEYS` constant listing its TOML keys in declaration order.
/// Field types must implement `Default` and `Merge` (and `Unset`): plain
/// settings are `Option<T>`, nested tables are other layer structs.
macro_rules! layered {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $( $(#[$fmeta:meta])* $fvis:vis $field:ident : $ty:ty ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
        #[serde(expecting = "a table")]
        $vis struct $name {
            $( $(#[$fmeta])* #[serde(default)] $fvis $field: $ty, )*
        }

        impl $crate::config::layer::Merge for $name {
            fn merge(&mut self, top: Self) {
                $( $crate::config::layer::Merge::merge(&mut self.$field, top.$field); )*
            }
        }

        #[cfg(test)]
        impl $crate::config::layer::Unset for $name {
            fn unset(&self, path: &str, out: &mut Vec<String>) {
                $(
                    $crate::config::layer::Unset::unset(
                        &self.$field,
                        &$crate::config::layer::join_key(path, stringify!($field)),
                        out,
                    );
                )*
            }
        }

        impl $name {
            /// The TOML keys of this table, in documentation order.
            pub(crate) const KEYS: &'static [&'static str] = &[$(stringify!($field)),*];
        }
    };
}

pub(crate) use layered;

layered! {
    /// `[theme]`: which theme to use and how.
    pub(crate) struct ThemeLayer {
        /// Built-in name, name in the themes directory, or a path.
        pub(crate) name: Option<String>,
        #[serde(deserialize_with = "named")]
        pub(crate) background: Option<BackgroundMode>,
        /// `auto`, a two-face theme name or a `.tmTheme` path.
        pub(crate) code: Option<String>,
    }
}

layered! {
    /// `[render]`: geometry and output switches.
    pub(crate) struct RenderLayer {
        /// Total width; 0 means the terminal width.
        pub(crate) width: Option<u16>,
        pub(crate) max_width: Option<u16>,
        pub(crate) margin: Option<u16>,
        #[serde(deserialize_with = "named")]
        pub(crate) align: Option<Align>,
        #[serde(deserialize_with = "named")]
        pub(crate) images: Option<ImageMode>,
        #[serde(deserialize_with = "named")]
        pub(crate) math: Option<InlineMath>,
        #[serde(deserialize_with = "named")]
        pub(crate) color: Option<ColorChoice>,
        #[serde(deserialize_with = "named")]
        pub(crate) hyperlinks: Option<When>,
        #[serde(deserialize_with = "named")]
        pub(crate) link_refs: Option<When>,
        pub(crate) ascii: Option<bool>,
        #[serde(deserialize_with = "named")]
        pub(crate) ambiguous_width: Option<AmbiguousWidth>,
        #[serde(deserialize_with = "named")]
        pub(crate) gradients: Option<When>,
        #[serde(deserialize_with = "named")]
        pub(crate) front_matter: Option<FrontMatterMode>,
        #[serde(deserialize_with = "named")]
        pub(crate) html: Option<HtmlMode>,
    }
}

layered! {
    /// `[heading]`: heading presentation.
    pub(crate) struct HeadingLayer {
        #[serde(deserialize_with = "named")]
        pub(crate) h1: Option<H1Style>,
        #[serde(deserialize_with = "named")]
        pub(crate) h2: Option<H2Style>,
        pub(crate) markers: Option<[String; 6]>,
        pub(crate) numbers: Option<bool>,
    }
}

/// `[code.aliases]`: extra fence-token aliases, merged key by key.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(transparent)]
pub(crate) struct AliasTable(pub(crate) BTreeMap<String, String>);

impl Merge for AliasTable {
    fn merge(&mut self, top: Self) {
        self.0.extend(top.0);
    }
}

#[cfg(test)]
impl Unset for AliasTable {
    fn unset(&self, _path: &str, _out: &mut Vec<String>) {}
}

layered! {
    /// `[code]`: code blocks.
    pub(crate) struct CodeLayer {
        pub(crate) wrap: Option<bool>,
        pub(crate) line_numbers: Option<bool>,
        pub(crate) tab_width: Option<u8>,
        pub(crate) label: Option<bool>,
        pub(crate) max_highlight_bytes: Option<usize>,
        #[serde(deserialize_with = "named")]
        pub(crate) style: Option<CodeStyle>,
        pub(crate) aliases: AliasTable,
    }
}

layered! {
    /// `[tables]`
    pub(crate) struct TablesLayer {
        pub(crate) zebra: Option<bool>,
    }
}

layered! {
    /// `[math]`: math rendering (inline math is `render.math`).
    pub(crate) struct MathLayer {
        #[serde(deserialize_with = "named")]
        pub(crate) display: Option<DisplayMath>,
        #[serde(deserialize_with = "named")]
        pub(crate) letters: Option<Letters>,
        #[serde(deserialize_with = "named")]
        pub(crate) fractions: Option<Fractions>,
        #[serde(deserialize_with = "named")]
        pub(crate) scripts: Option<ScriptSet>,
        #[serde(deserialize_with = "named")]
        pub(crate) bold: Option<Bold>,
        pub(crate) max_height: Option<u16>,
        pub(crate) tex_delimiters: Option<bool>,
    }
}

layered! {
    /// `[images]` (the image mode itself is `render.images`).
    pub(crate) struct ImagesLayer {
        #[serde(deserialize_with = "named")]
        pub(crate) blocks: Option<BlockGlyphs>,
        #[serde(deserialize_with = "height")]
        pub(crate) max_height: Option<Height>,
        pub(crate) remote: Option<bool>,
        #[serde(deserialize_with = "named")]
        pub(crate) tmux_passthrough: Option<TmuxPassthrough>,
        pub(crate) max_pixels: Option<u64>,
    }
}

layered! {
    /// `[markdown]`: dialect switches.
    pub(crate) struct MarkdownLayer {
        pub(crate) math: Option<bool>,
        pub(crate) linkify: Option<bool>,
        pub(crate) definition_lists: Option<bool>,
        pub(crate) smart_punctuation: Option<bool>,
    }
}

layered! {
    /// `[glyphs]`: decoration characters.
    pub(crate) struct GlyphsLayer {
        pub(crate) bullets: Option<Vec<String>>,
        pub(crate) task: Option<[String; 2]>,
        pub(crate) quote: Option<String>,
        pub(crate) rule: Option<String>,
        #[serde(deserialize_with = "named")]
        pub(crate) table: Option<TableBorder>,
        pub(crate) wrap_marker: Option<String>,
        #[serde(deserialize_with = "named")]
        pub(crate) icons: Option<IconSet>,
    }
}

layered! {
    /// `[pager]`: the built-in pager.
    pub(crate) struct PagerLayer {
        #[serde(deserialize_with = "named")]
        pub(crate) enabled: Option<When>,
        pub(crate) mouse: Option<bool>,
        pub(crate) watch: Option<bool>,
        pub(crate) scroll_lines: Option<u16>,
        #[serde(deserialize_with = "named")]
        pub(crate) search_case: Option<SearchCase>,
        #[serde(deserialize_with = "open_command")]
        pub(crate) open: Option<OpenCommand>,
    }
}

layered! {
    /// `[terminal]`: terminal detection.
    pub(crate) struct TerminalLayer {
        pub(crate) probe: Option<bool>,
        pub(crate) probe_timeout_ms: Option<u32>,
    }
}

layered! {
    /// A whole configuration document: the embedded defaults, the user file,
    /// one `--set`, or the command-line flags.
    pub(crate) struct ConfigLayer {
        pub(crate) theme: ThemeLayer,
        /// Colours merged per key over the theme's palette.
        pub(crate) palette: PaletteTable,
        /// Element styles; each replaces the theme's style for that element.
        pub(crate) style: StyleTable,
        /// Styles for the dark variant only.
        pub(crate) dark: VariantTable,
        /// Styles for the light variant only.
        pub(crate) light: VariantTable,
        pub(crate) render: RenderLayer,
        pub(crate) heading: HeadingLayer,
        pub(crate) code: CodeLayer,
        pub(crate) tables: TablesLayer,
        pub(crate) math: MathLayer,
        pub(crate) images: ImagesLayer,
        pub(crate) markdown: MarkdownLayer,
        pub(crate) glyphs: GlyphsLayer,
        pub(crate) pager: PagerLayer,
        pub(crate) terminal: TerminalLayer,
    }
}

/// Top-level tables that hold theme data rather than settings.
pub(crate) const THEME_TABLES: &[&str] = &["palette", "style", "dark", "light"];

/// The keys of a settings table of [`ConfigLayer`] (`None` for tables whose
/// keys are free-form, such as `palette` and `code.aliases`).
pub(crate) fn section_keys(section: &str) -> Option<&'static [&'static str]> {
    Some(match section {
        "theme" => ThemeLayer::KEYS,
        "render" => RenderLayer::KEYS,
        "heading" => HeadingLayer::KEYS,
        "code" => CodeLayer::KEYS,
        "tables" => TablesLayer::KEYS,
        "math" => MathLayer::KEYS,
        "images" => ImagesLayer::KEYS,
        "markdown" => MarkdownLayer::KEYS,
        "glyphs" => GlyphsLayer::KEYS,
        "pager" => PagerLayer::KEYS,
        "terminal" => TerminalLayer::KEYS,
        _ => return None,
    })
}

/// Settings tables of [`ConfigLayer`] (everything except the theme tables).
pub(crate) fn settings_sections() -> impl Iterator<Item = &'static str> {
    ConfigLayer::KEYS
        .iter()
        .copied()
        .filter(|k| !THEME_TABLES.contains(k))
}

impl ConfigLayer {
    /// Dotted paths of all settings no value was given for.
    #[cfg(test)]
    pub(crate) fn unset_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.unset("", &mut out);
        out
    }

    /// Whether the layer has palette or style tables.
    pub(crate) fn has_theme_tables(&self) -> bool {
        let p = &self.palette;
        !(p.both.is_empty()
            && p.dark.is_empty()
            && p.light.is_empty()
            && self.style.0.is_empty()
            && self.dark.style.0.is_empty()
            && self.light.style.0.is_empty())
    }

    /// Move the theme tables out, leaving only settings.
    pub(crate) fn take_theme_tables(
        &mut self,
    ) -> (PaletteTable, StyleTable, StyleTable, StyleTable) {
        (
            std::mem::take(&mut self.palette),
            std::mem::take(&mut self.style),
            std::mem::take(&mut self.dark.style),
            std::mem::take(&mut self.light.style),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_merge_prefers_top() {
        let mut a = Some(1);
        a.merge(None);
        assert_eq!(a, Some(1));
        a.merge(Some(2));
        assert_eq!(a, Some(2));
        let mut b: Option<u8> = None;
        b.merge(Some(3));
        assert_eq!(b, Some(3));
    }

    #[test]
    fn layers_merge_field_by_field() {
        let mut low = RenderLayer {
            max_width: Some(100),
            margin: Some(2),
            ..RenderLayer::default()
        };
        low.merge(RenderLayer {
            margin: Some(4),
            ..RenderLayer::default()
        });
        assert_eq!(low.max_width, Some(100));
        assert_eq!(low.margin, Some(4));
    }

    #[test]
    fn aliases_merge_per_key() {
        let mut low = AliasTable([("a".into(), "x".into())].into());
        low.merge(AliasTable(
            [("b".into(), "y".into()), ("a".into(), "z".into())].into(),
        ));
        assert_eq!(low.0.get("a").map(String::as_str), Some("z"));
        assert_eq!(low.0.get("b").map(String::as_str), Some("y"));
    }

    #[test]
    fn keys_and_unset_paths() {
        assert_eq!(TablesLayer::KEYS, &["zebra"]);
        let layer = ConfigLayer::default();
        let unset = layer.unset_paths();
        assert!(unset.contains(&"render.max_width".to_owned()));
        assert!(unset.contains(&"terminal.probe_timeout_ms".to_owned()));
        // Free-form tables are never "unset".
        assert!(!unset.iter().any(|p| p.starts_with("palette")));
        assert!(!unset.iter().any(|p| p.starts_with("code.aliases")));
    }

    #[test]
    fn every_settings_section_has_keys() {
        for section in settings_sections() {
            assert!(section_keys(section).is_some(), "{section}");
        }
        assert!(section_keys("palette").is_none());
    }
}
