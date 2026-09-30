//! Theme tables as written in theme files and in the config file.
//!
//! * `[palette]`: named colours for both variants, with `[palette.dark]` and
//!   `[palette.light]` sub-tables for one variant only.
//! * `[style.<element>]`: a [`StyleSpec`] per element, with
//!   `[dark.style.<element>]` / `[light.style.<element>]` overrides.
//! * `code` (theme files only): the code theme, one name or `{ dark, light }`.
//!
//! [`ThemePatch`] is the per-variant form every layer is normalised into
//! before layers are combined.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, Unexpected, Visitor};

use super::Variant;
use super::color::ColorSpec;
use super::element::Element;
#[cfg(test)]
use crate::config::layer::Unset;
use crate::config::layer::{Merge, layered};
use crate::config::value::{has_control, named};
use crate::style::Underline;

layered! {
    /// A style as written in a `[style.<element>]` table. Fields left out
    /// inherit from the parent element.
    pub(crate) struct StyleSpec {
        pub(crate) fg: Option<ColorSpec>,
        pub(crate) bg: Option<ColorSpec>,
        /// End colour of a background gradient (bar elements such as `h1`).
        pub(crate) bg_to: Option<ColorSpec>,
        #[serde(deserialize_with = "named")]
        pub(crate) underline: Option<Underline>,
        pub(crate) underline_color: Option<ColorSpec>,
        pub(crate) bold: Option<bool>,
        pub(crate) italic: Option<bool>,
        pub(crate) dim: Option<bool>,
        pub(crate) strikethrough: Option<bool>,
        pub(crate) reverse: Option<bool>,
        pub(crate) overline: Option<bool>,
    }
}

impl StyleSpec {
    /// Every colour field with its key.
    pub(crate) fn colors(&self) -> impl Iterator<Item = (&'static str, &ColorSpec)> {
        [
            ("fg", &self.fg),
            ("bg", &self.bg),
            ("bg_to", &self.bg_to),
            ("underline_color", &self.underline_color),
        ]
        .into_iter()
        .filter_map(|(k, v)| Some((k, v.as_ref()?)))
    }
}

/// `[style.<element>]` tables, keyed by element.
///
/// Merging patches each element field by field. Entries whose name is not an
/// element are skipped while deserialising (and so reported as unknown keys).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StyleTable(pub(crate) BTreeMap<Element, StyleSpec>);

impl Merge for StyleTable {
    fn merge(&mut self, top: Self) {
        for (element, spec) in top.0 {
            self.0.entry(element).or_default().merge(spec);
        }
    }
}

#[cfg(test)]
impl Unset for StyleTable {
    fn unset(&self, _path: &str, _out: &mut Vec<String>) {}
}

struct StyleTableVisitor;

impl<'de> Visitor<'de> for StyleTableVisitor {
    type Value = StyleTable;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a table of element styles")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<StyleTable, A::Error> {
        let mut out = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            match Element::from_name(&key) {
                Some(element) => {
                    out.insert(element, map.next_value::<StyleSpec>()?);
                }
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(StyleTable(out))
    }
}

impl<'de> Deserialize<'de> for StyleTable {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(StyleTableVisitor)
    }
}

layered! {
    /// `[dark]` / `[light]`: settings for one variant.
    pub(crate) struct VariantTable {
        pub(crate) style: StyleTable,
    }
}

/// `[palette]` with optional `[palette.dark]` / `[palette.light]` tables.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PaletteTable {
    /// Entries for both variants.
    pub(crate) both: BTreeMap<String, ColorSpec>,
    /// `[palette.dark]`.
    pub(crate) dark: BTreeMap<String, ColorSpec>,
    /// `[palette.light]`.
    pub(crate) light: BTreeMap<String, ColorSpec>,
}

impl PaletteTable {
    /// Every entry with its key path below `palette`.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (Option<Variant>, &String, &ColorSpec)> {
        let tag = |v: Option<Variant>| move |(k, c)| (v, k, c);
        self.both
            .iter()
            .map(tag(None))
            .chain(self.dark.iter().map(tag(Some(Variant::Dark))))
            .chain(self.light.iter().map(tag(Some(Variant::Light))))
    }

    fn variant(&self, v: Variant) -> &BTreeMap<String, ColorSpec> {
        match v {
            Variant::Dark => &self.dark,
            Variant::Light => &self.light,
        }
    }
}

impl Merge for PaletteTable {
    fn merge(&mut self, top: Self) {
        self.both.extend(top.both);
        self.dark.extend(top.dark);
        self.light.extend(top.light);
    }
}

#[cfg(test)]
impl Unset for PaletteTable {
    fn unset(&self, _path: &str, _out: &mut Vec<String>) {}
}

/// A palette entry named `dark` or `light`: a colour, or a variant table.
enum PaletteValue {
    Color(ColorSpec),
    Table(BTreeMap<String, ColorSpec>),
}

struct PaletteValueVisitor;

impl<'de> Visitor<'de> for PaletteValueVisitor {
    type Value = PaletteValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a colour or a table of colours")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<PaletteValue, E> {
        ColorSpec::parse(v)
            .map(PaletteValue::Color)
            .map_err(E::custom)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<PaletteValue, E> {
        let d = de::value::I64Deserializer::<E>::new(v);
        ColorSpec::deserialize(d).map(PaletteValue::Color)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<PaletteValue, E> {
        let d = de::value::U64Deserializer::<E>::new(v);
        ColorSpec::deserialize(d).map(PaletteValue::Color)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<PaletteValue, A::Error> {
        let mut out = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            out.insert(key, map.next_value::<ColorSpec>()?);
        }
        Ok(PaletteValue::Table(out))
    }
}

impl<'de> Deserialize<'de> for PaletteValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(PaletteValueVisitor)
    }
}

struct PaletteVisitor;

impl<'de> Visitor<'de> for PaletteVisitor {
    type Value = PaletteTable;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a table of colours")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<PaletteTable, A::Error> {
        let mut out = PaletteTable::default();
        while let Some(key) = map.next_key::<String>()? {
            let variant = match key.as_str() {
                "dark" => Some(Variant::Dark),
                "light" => Some(Variant::Light),
                _ => None,
            };
            match variant {
                Some(v) => match map.next_value::<PaletteValue>()? {
                    PaletteValue::Color(c) => {
                        out.both.insert(key, c);
                    }
                    PaletteValue::Table(t) => match v {
                        Variant::Dark => out.dark.extend(t),
                        Variant::Light => out.light.extend(t),
                    },
                },
                None => {
                    out.both.insert(key, map.next_value::<ColorSpec>()?);
                }
            }
        }
        Ok(out)
    }
}

impl<'de> Deserialize<'de> for PaletteTable {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(PaletteVisitor)
    }
}

/// A theme file's `code`: one code theme, or one per variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CodeThemeSpec {
    /// The same code theme for both variants.
    Both(String),
    /// `{ dark = "…", light = "…" }`.
    PerVariant {
        dark: Option<String>,
        light: Option<String>,
    },
}

impl CodeThemeSpec {
    /// The code theme for a variant.
    pub(crate) fn get(&self, v: Variant) -> Option<&str> {
        match (self, v) {
            (CodeThemeSpec::Both(s), _) => Some(s),
            (CodeThemeSpec::PerVariant { dark, .. }, Variant::Dark) => dark.as_deref(),
            (CodeThemeSpec::PerVariant { light, .. }, Variant::Light) => light.as_deref(),
        }
    }
}

struct CodeThemeVisitor;

fn code_theme_name<E: de::Error>(v: &str) -> Result<String, E> {
    let t = v.trim();
    if t.is_empty() || has_control(t) {
        return Err(E::invalid_value(
            Unexpected::Str(v),
            &"a code theme name or path",
        ));
    }
    Ok(t.to_owned())
}

impl<'de> Visitor<'de> for CodeThemeVisitor {
    type Value = CodeThemeSpec;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a code theme name, or a table with `dark` and `light`")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<CodeThemeSpec, E> {
        code_theme_name(v).map(CodeThemeSpec::Both)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<CodeThemeSpec, A::Error> {
        let (mut dark, mut light) = (None, None);
        while let Some(key) = map.next_key::<String>()? {
            let slot = match key.as_str() {
                "dark" => &mut dark,
                "light" => &mut light,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                    continue;
                }
            };
            let name: String = map.next_value()?;
            *slot = Some(code_theme_name(&name)?);
        }
        Ok(CodeThemeSpec::PerVariant { dark, light })
    }
}

impl<'de> Deserialize<'de> for CodeThemeSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(CodeThemeVisitor)
    }
}

layered! {
    /// A theme file (`assets/themes/*.toml`, `~/.config/emde/themes/*.toml`).
    pub(crate) struct ThemeFile {
        /// Display name; defaults to the name the theme was looked up by.
        pub(crate) name: Option<String>,
        /// A theme to start from: built-in name, themes-directory name or path.
        pub(crate) inherits: Option<String>,
        /// Code highlighting theme.
        pub(crate) code: Option<CodeThemeSpec>,
        pub(crate) palette: PaletteTable,
        pub(crate) style: StyleTable,
        pub(crate) dark: VariantTable,
        pub(crate) light: VariantTable,
    }
}

/// Theme data of one layer (a theme file, the config file, a `--set`),
/// split per variant.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ThemePatch {
    /// Palette entries, indexed by [`Variant::index`].
    pub(crate) palette: [BTreeMap<String, ColorSpec>; 2],
    /// Element styles, indexed by [`Variant::index`].
    pub(crate) styles: [BTreeMap<Element, StyleSpec>; 2],
    /// Code theme, indexed by [`Variant::index`].
    pub(crate) code: [Option<String>; 2],
}

impl ThemePatch {
    /// Normalise one layer's tables. Within a layer, variant-specific
    /// palette entries replace shared ones, and `[dark.style.X]` patches
    /// `[style.X]` field by field.
    pub(crate) fn from_tables(
        palette: &PaletteTable,
        style: &StyleTable,
        dark: &StyleTable,
        light: &StyleTable,
        code: Option<&CodeThemeSpec>,
    ) -> ThemePatch {
        let mut patch = ThemePatch::default();
        for v in Variant::ALL {
            let pal = &mut patch.palette[v.index()];
            pal.extend(palette.both.iter().map(|(k, c)| (k.clone(), c.clone())));
            pal.extend(
                palette
                    .variant(v)
                    .iter()
                    .map(|(k, c)| (k.clone(), c.clone())),
            );
            let styles = &mut patch.styles[v.index()];
            styles.extend(style.0.iter().map(|(e, s)| (*e, s.clone())));
            let own = match v {
                Variant::Dark => dark,
                Variant::Light => light,
            };
            for (e, s) in &own.0 {
                styles.entry(*e).or_default().merge(s.clone());
            }
            patch.code[v.index()] = code.and_then(|c| c.get(v)).map(str::to_owned);
        }
        patch
    }

    /// Normalise a theme file.
    pub(crate) fn from_theme_file(file: &ThemeFile) -> ThemePatch {
        ThemePatch::from_tables(
            &file.palette,
            &file.style,
            &file.dark.style,
            &file.light.style,
            file.code.as_ref(),
        )
    }

    /// Put `top` over `self` the way a theme sits over the theme it
    /// inherits, and the config over the theme: palette entries merge per
    /// key, and each element style in `top` *replaces* ours.
    pub(crate) fn overlay(&mut self, top: ThemePatch) {
        let [pd, pl] = top.palette;
        let [sd, sl] = top.styles;
        for (i, (pal, styles)) in [(pd, sd), (pl, sl)].into_iter().enumerate() {
            if let Some(p) = self.palette.get_mut(i) {
                p.extend(pal);
            }
            if let Some(s) = self.styles.get_mut(i) {
                s.extend(styles);
            }
        }
        for (mine, theirs) in self.code.iter_mut().zip(top.code) {
            mine.merge(theirs);
        }
    }

    /// Put `top` over `self` the way one config layer sits over another:
    /// palette entries merge per key and element styles merge field by field.
    pub(crate) fn patch(&mut self, top: ThemePatch) {
        let [pd, pl] = top.palette;
        let [sd, sl] = top.styles;
        for (i, (pal, styles)) in [(pd, sd), (pl, sl)].into_iter().enumerate() {
            if let Some(p) = self.palette.get_mut(i) {
                p.extend(pal);
            }
            if let Some(s) = self.styles.get_mut(i) {
                for (e, spec) in styles {
                    s.entry(e).or_default().merge(spec);
                }
            }
        }
        for (mine, theirs) in self.code.iter_mut().zip(top.code) {
            mine.merge(theirs);
        }
    }

    /// Whether the patch changes nothing.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.palette.iter().all(BTreeMap::is_empty)
            && self.styles.iter().all(BTreeMap::is_empty)
            && self.code.iter().all(Option::is_none)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Rgb;

    fn file(src: &str) -> ThemeFile {
        toml::from_str(src).unwrap()
    }

    #[test]
    fn style_specs() {
        let f = file(
            "[style.h1]\nfg = \"accent\"\nbold = true\nunderline = \"curly\"\n\
             [style.link]\nunderline = true\n[style.nope]\nfg = \"red\"",
        );
        let h1 = &f.style.0[&Element::H1];
        assert_eq!(h1.fg, Some(ColorSpec::Name("accent".into())));
        assert_eq!(h1.bold, Some(true));
        assert_eq!(h1.underline, Some(Underline::Curly));
        assert_eq!(f.style.0[&Element::Link].underline, Some(Underline::Single));
        assert_eq!(f.style.0.len(), 2, "unknown elements are skipped");
        assert_eq!(h1.colors().map(|(k, _)| k).collect::<Vec<_>>(), vec!["fg"]);
    }

    #[test]
    fn palettes_split_per_variant() {
        let f = file(
            "[palette]\naccent = \"#89b4fa\"\ndark = \"#000\"\n\
             [palette.light]\naccent = \"#1e66f5\"\n",
        );
        assert_eq!(
            f.palette.both.len(),
            2,
            "`dark = colour` is a colour named dark"
        );
        assert_eq!(f.palette.light.len(), 1);
        let p = ThemePatch::from_theme_file(&f);
        assert_eq!(
            p.palette[Variant::Dark.index()]["accent"],
            ColorSpec::Rgb(Rgb(0x89, 0xb4, 0xfa))
        );
        assert_eq!(
            p.palette[Variant::Light.index()]["accent"],
            ColorSpec::Rgb(Rgb(0x1e, 0x66, 0xf5))
        );
        assert_eq!(f.palette.entries().count(), 3);
    }

    #[test]
    fn palette_type_errors() {
        assert!(toml::from_str::<ThemeFile>("[palette]\naccent = true").is_err());
        assert!(toml::from_str::<ThemeFile>("[palette.other]\na = \"#fff\"").is_err());
        assert!(toml::from_str::<ThemeFile>("[palette.dark]\na = \"#ff\"").is_err());
        assert!(toml::from_str::<ThemeFile>("[palette]\ndark = 7").is_ok());
    }

    #[test]
    fn code_themes() {
        let both = file("code = \"Nord\"");
        assert_eq!(both.code, Some(CodeThemeSpec::Both("Nord".into())));
        let per = file("code = { dark = \"OneHalfDark\", light = \"OneHalfLight\" }");
        let spec = per.code.unwrap();
        assert_eq!(spec.get(Variant::Dark), Some("OneHalfDark"));
        assert_eq!(spec.get(Variant::Light), Some("OneHalfLight"));
        let half = file("code = { light = \"GitHub\" }").code.unwrap();
        assert_eq!(half.get(Variant::Dark), None);
        assert!(toml::from_str::<ThemeFile>("code = \"\"").is_err());
        assert!(toml::from_str::<ThemeFile>("code = 3").is_err());
    }

    #[test]
    fn variant_styles_patch_shared_ones() {
        let f = file(
            "[style.h1]\nfg = \"red\"\nbold = true\n[dark.style.h1]\nfg = \"blue\"\n\
             [light.style.h2]\nitalic = true",
        );
        let p = ThemePatch::from_theme_file(&f);
        let dark_h1 = &p.styles[Variant::Dark.index()][&Element::H1];
        assert_eq!(dark_h1.fg, Some(ColorSpec::Name("blue".into())));
        assert_eq!(dark_h1.bold, Some(true));
        let light_h1 = &p.styles[Variant::Light.index()][&Element::H1];
        assert_eq!(light_h1.fg, Some(ColorSpec::Name("red".into())));
        assert!(!p.styles[Variant::Dark.index()].contains_key(&Element::H2));
        assert!(p.styles[Variant::Light.index()].contains_key(&Element::H2));
    }

    #[test]
    fn overlay_replaces_and_patch_merges() {
        let base = ThemePatch::from_theme_file(&file(
            "code = \"Nord\"\n[palette]\na = \"#111\"\nb = \"#222\"\n\
             [style.h1]\nbg = \"a\"\nbold = true",
        ));
        let top =
            ThemePatch::from_theme_file(&file("[palette]\nb = \"#333\"\n[style.h1]\nfg = \"b\""));
        let d = Variant::Dark.index();

        let mut over = base.clone();
        over.overlay(top.clone());
        assert_eq!(over.palette[d]["a"], ColorSpec::Rgb(Rgb(0x11, 0x11, 0x11)));
        assert_eq!(over.palette[d]["b"], ColorSpec::Rgb(Rgb(0x33, 0x33, 0x33)));
        let h1 = &over.styles[d][&Element::H1];
        assert_eq!(h1.bg, None, "overlay replaces the whole element");
        assert_eq!(h1.bold, None);
        assert_eq!(h1.fg, Some(ColorSpec::Name("b".into())));
        assert_eq!(over.code[d].as_deref(), Some("Nord"));

        let mut patched = base;
        patched.patch(top);
        let h1 = &patched.styles[d][&Element::H1];
        assert_eq!(
            h1.bg,
            Some(ColorSpec::Name("a".into())),
            "patch merges fields"
        );
        assert_eq!(h1.bold, Some(true));
        assert_eq!(h1.fg, Some(ColorSpec::Name("b".into())));
        assert!(!patched.is_empty());
        assert!(ThemePatch::default().is_empty());
    }
}
