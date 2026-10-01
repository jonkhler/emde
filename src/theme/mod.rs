//! Themes: one resolved [`Style`] per [`Element`], plus the palette and the
//! code-highlighting theme name.
//!
//! The resolved [`Theme`] is the contract between configuration (which builds
//! it from theme files, `[palette]` and `[style.*]` overrides) and layout
//! (which only calls [`Theme::style`] / [`Theme::gradient`]).
//!
//! Theme files are TOML (`assets/themes/*.toml` for the built-ins,
//! `~/.config/emde/themes/NAME.toml` for the user's):
//!
//! * `color`: colour values (`#rrggbb`, names, tints) and palette resolution;
//! * `spec`: the `[palette]`, `[style.<element>]` and `code` tables;
//! * `chain`: lookup and `inherits` chains;
//! * `build`: resolving theme data into a [`Theme`] for one variant;
//! * [`builtin`]: the embedded themes: `emde`, `ansi` and `mono`, and the
//!   palettes built on `emde` (`nord`, `gruvbox`, …).

pub(crate) mod build;
pub mod builtin;
pub(crate) mod chain;
pub(crate) mod color;
pub mod element;
pub(crate) mod spec;

use std::collections::BTreeMap;

pub use chain::{MAX_DEPTH, ThemeInfo, list_themes};
pub use element::Element;

use crate::color::{adjust_lightness, is_dark, mix_oklab};
use crate::style::{Attrs, Color, Rgb, Style, StylePatch, Underline};

/// Light or dark variant of a theme (chosen from the terminal background).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Variant {
    #[default]
    Dark,
    Light,
}

impl Variant {
    /// Both variants, in [`Variant::index`] order.
    pub const ALL: [Variant; 2] = [Variant::Dark, Variant::Light];

    /// Index into per-variant tables (`Dark` = 0, `Light` = 1).
    pub const fn index(self) -> usize {
        match self {
            Variant::Dark => 0,
            Variant::Light => 1,
        }
    }

    /// The variant suited to a terminal background colour.
    pub fn for_background(bg: Rgb) -> Variant {
        if is_dark(bg) {
            Variant::Dark
        } else {
            Variant::Light
        }
    }
}

/// A fully resolved theme.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// Theme name (for `--doctor` and diagnostics).
    pub name: String,
    pub variant: Variant,
    /// Named palette colours, after config overrides.
    pub palette: BTreeMap<String, Rgb>,
    /// Page background assumed for colour mixing (the terminal background if known).
    pub base: Rgb,
    /// Panel background (code blocks, table headers, key caps).
    pub surface: Rgb,
    /// syntect/two-face theme name or `.tmTheme` path for code blocks.
    pub code_theme: String,
    styles: Vec<Style>,
    gradients: Vec<Option<(Color, Color)>>,
}

impl Theme {
    /// Build a theme from per-element patches. Unset elements inherit from
    /// their parent chain; `Text` starts from [`Style::PLAIN`].
    #[allow(clippy::too_many_arguments)]
    pub fn from_patches(
        name: impl Into<String>,
        variant: Variant,
        palette: BTreeMap<String, Rgb>,
        base: Rgb,
        surface: Rgb,
        code_theme: impl Into<String>,
        patches: &[(Element, StylePatch)],
        gradients: &[(Element, Color, Color)],
    ) -> Theme {
        let mut own: Vec<Option<StylePatch>> = vec![None; Element::COUNT];
        for (e, p) in patches {
            own[e.index()] = Some(*p);
        }
        let mut resolved: Vec<Option<Style>> = vec![None; Element::COUNT];
        fn resolve(e: Element, own: &[Option<StylePatch>], out: &mut [Option<Style>]) -> Style {
            if let Some(s) = out[e.index()] {
                return s;
            }
            let parent = e.parent().map_or(Style::PLAIN, |p| resolve(p, own, out));
            let s = match &own[e.index()] {
                Some(p) => parent.patch(p),
                None => parent,
            };
            out[e.index()] = Some(s);
            s
        }
        for &e in Element::ALL {
            resolve(e, &own, &mut resolved);
        }
        let mut grads = vec![None; Element::COUNT];
        for &(e, from, to) in gradients {
            grads[e.index()] = Some((from, to));
        }
        Theme {
            name: name.into(),
            variant,
            palette,
            base,
            surface,
            code_theme: code_theme.into(),
            styles: resolved
                .into_iter()
                .map(Option::unwrap_or_default)
                .collect(),
            gradients: grads,
        }
    }

    /// The resolved style of an element.
    pub fn style(&self, e: Element) -> Style {
        self.styles[e.index()]
    }

    /// Background gradient (from, to) for bar-style elements such as `h1`.
    pub fn gradient(&self, e: Element) -> Option<(Color, Color)> {
        self.gradients[e.index()]
    }

    /// A palette colour by name.
    pub fn color(&self, name: &str) -> Option<Rgb> {
        self.palette.get(name).copied()
    }

    /// The built-in `emde` theme (Catppuccin Mocha / Latte palettes).
    /// `background` is the terminal background if known; it drives `surface`.
    pub fn fallback(variant: Variant, background: Option<Rgb>) -> Theme {
        let pal = match variant {
            Variant::Dark => MOCHA,
            Variant::Light => LATTE,
        };
        let palette: BTreeMap<String, Rgb> = pal.iter().map(|&(k, v)| (k.to_string(), v)).collect();
        let c = |name: &str| Color::Rgb(palette[name]);
        let base = background.unwrap_or(palette["base"]);
        let surface = surface_for(base, variant);
        let mix = |name: &str, t: f32| Color::Rgb(mix_oklab(base, palette[name], t));
        let fg = |col: Color| StylePatch {
            fg: Some(col),
            ..StylePatch::default()
        };
        let attrs = |col: Option<Color>, set: Attrs| StylePatch {
            fg: col,
            set,
            ..StylePatch::default()
        };
        let on = |fgc: Option<Color>, bg: Color, set: Attrs| StylePatch {
            fg: fgc,
            bg: Some(bg),
            set,
            ..StylePatch::default()
        };
        let surface_c = Color::Rgb(surface);
        let base_c = Color::Rgb(base);
        use Element as E;
        let patches = [
            (E::Heading, attrs(Some(c("blue")), Attrs::BOLD)),
            (E::H1, on(Some(base_c), c("blue"), Attrs::BOLD)),
            (E::H2, attrs(Some(c("mauve")), Attrs::BOLD)),
            (E::H3, attrs(Some(c("teal")), Attrs::BOLD)),
            (E::H4, attrs(Some(Color::Default), Attrs::BOLD)),
            (
                E::H5,
                attrs(Some(c("subtext")), Attrs::BOLD | Attrs::ITALIC),
            ),
            (E::H6, attrs(Some(c("muted")), Attrs::ITALIC)),
            (E::HeadingRule, fg(c("mauve"))),
            (E::Emph, attrs(None, Attrs::ITALIC)),
            (E::Strong, attrs(None, Attrs::BOLD)),
            (E::Strike, attrs(Some(c("muted")), Attrs::STRIKE)),
            (E::Mark, on(None, mix("yellow", 0.3), Attrs::empty())),
            (E::Kbd, on(Some(c("text")), surface_c, Attrs::BOLD)),
            (
                E::Link,
                StylePatch {
                    fg: Some(c("blue")),
                    underline: Some(Underline::Single),
                    ..StylePatch::default()
                },
            ),
            (
                E::LinkUrl,
                StylePatch {
                    fg: Some(c("muted")),
                    underline: Some(Underline::None),
                    ..StylePatch::default()
                },
            ),
            (E::LinkFocus, attrs(None, Attrs::REVERSE)),
            (
                E::FootnoteRef,
                StylePatch {
                    underline: Some(Underline::None),
                    ..StylePatch::default()
                },
            ),
            (E::Code, fg(c("peach"))),
            (E::CodeInline, on(None, surface_c, Attrs::empty())),
            (
                E::CodeBlock,
                on(Some(Color::Default), surface_c, Attrs::empty()),
            ),
            (E::Quote, fg(c("subtext"))),
            (E::Alert, attrs(None, Attrs::BOLD)),
            (E::AlertNote, fg(c("blue"))),
            (E::AlertTip, fg(c("green"))),
            (E::AlertImportant, fg(c("mauve"))),
            (E::AlertWarning, fg(c("yellow"))),
            (E::AlertCaution, fg(c("red"))),
            (E::ListMarker, fg(c("blue"))),
            (E::TaskDone, fg(c("green"))),
            (E::TaskTodo, fg(c("muted"))),
            (E::TableHeader, on(None, surface_c, Attrs::BOLD)),
            (E::TableZebra, on(None, mix("surface", 0.5), Attrs::empty())),
            (E::Footnote, fg(c("subtext"))),
            (E::ImageAlt, attrs(None, Attrs::ITALIC)),
            (E::ImageCaption, attrs(Some(c("subtext")), Attrs::ITALIC)),
            (E::Math, fg(c("teal"))),
            (E::MathVar, attrs(None, Attrs::ITALIC)),
            (E::MathText, fg(Color::Default)),
            (
                E::MathError,
                StylePatch {
                    fg: Some(c("red")),
                    underline: Some(Underline::Curly),
                    underline_color: Some(c("red")),
                    ..StylePatch::default()
                },
            ),
            (E::Muted, fg(c("muted"))),
            (E::Status, on(Some(c("subtext")), surface_c, Attrs::empty())),
            (E::StatusMsg, fg(c("yellow"))),
            (
                E::SearchMatch,
                on(Some(base_c), c("yellow"), Attrs::empty()),
            ),
            (E::SearchCurrent, on(Some(base_c), c("peach"), Attrs::BOLD)),
            (E::Selection, attrs(None, Attrs::REVERSE)),
            (E::Prompt, fg(c("blue"))),
            (E::Hint, on(Some(base_c), c("blue"), Attrs::BOLD)),
            (E::TocCurrent, attrs(Some(c("blue")), Attrs::BOLD)),
        ];
        let gradients = [(E::H1, c("blue"), c("mauve"))];
        let code_theme = match variant {
            Variant::Dark => "OneHalfDark",
            Variant::Light => "OneHalfLight",
        };
        Theme::from_patches(
            "emde", variant, palette, base, surface, code_theme, &patches, &gradients,
        )
    }

    /// A frozen theme for snapshot tests: never change its colours, so
    /// snapshots only move when rendering logic changes.
    pub fn test() -> Theme {
        Theme::fallback(Variant::Dark, Some(Rgb(0x1e, 0x1e, 0x2e)))
    }
}

/// Panel colour derived from the page background: a small OKLab lightness
/// step, with a floor/ceiling so pure black/white backgrounds still get a
/// visible panel.
pub fn surface_for(base: Rgb, variant: Variant) -> Rgb {
    let l = crate::color::to_oklab(base).l;
    match variant {
        Variant::Dark => adjust_lightness(base, (l + 0.06).max(0.2) - l),
        Variant::Light => adjust_lightness(base, (l - 0.05).min(0.93) - l),
    }
}

/// Catppuccin Mocha (MIT) with emde's role names.
const MOCHA: &[(&str, Rgb)] = &[
    ("text", Rgb(0xcd, 0xd6, 0xf4)),
    ("subtext", Rgb(0xa6, 0xad, 0xc8)),
    ("muted", Rgb(0x6c, 0x70, 0x86)),
    ("surface", Rgb(0x31, 0x32, 0x44)),
    ("base", Rgb(0x1e, 0x1e, 0x2e)),
    ("blue", Rgb(0x89, 0xb4, 0xfa)),
    ("accent", Rgb(0x89, 0xb4, 0xfa)),
    ("mauve", Rgb(0xcb, 0xa6, 0xf7)),
    ("teal", Rgb(0x94, 0xe2, 0xd5)),
    ("green", Rgb(0xa6, 0xe3, 0xa1)),
    ("yellow", Rgb(0xf9, 0xe2, 0xaf)),
    ("peach", Rgb(0xfa, 0xb3, 0x87)),
    ("red", Rgb(0xf3, 0x8b, 0xa8)),
    ("pink", Rgb(0xf5, 0xc2, 0xe7)),
    ("sky", Rgb(0x89, 0xdc, 0xeb)),
];

/// Catppuccin Latte (MIT) with emde's role names.
const LATTE: &[(&str, Rgb)] = &[
    ("text", Rgb(0x4c, 0x4f, 0x69)),
    ("subtext", Rgb(0x5c, 0x5f, 0x77)),
    ("muted", Rgb(0x8c, 0x8f, 0xa1)),
    ("surface", Rgb(0xcc, 0xd0, 0xda)),
    ("base", Rgb(0xef, 0xf1, 0xf5)),
    ("blue", Rgb(0x1e, 0x66, 0xf5)),
    ("accent", Rgb(0x1e, 0x66, 0xf5)),
    ("mauve", Rgb(0x88, 0x39, 0xef)),
    ("teal", Rgb(0x17, 0x92, 0x99)),
    ("green", Rgb(0x40, 0xa0, 0x2b)),
    ("yellow", Rgb(0xdf, 0x8e, 0x1d)),
    ("peach", Rgb(0xfe, 0x64, 0x0b)),
    ("red", Rgb(0xd2, 0x0f, 0x39)),
    ("pink", Rgb(0xea, 0x76, 0xcb)),
    ("sky", Rgb(0x04, 0xa5, 0xe5)),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inheritance() {
        let t = Theme::fallback(Variant::Dark, None);
        // h4 sets only bold + default fg; heading colour must not leak in.
        assert_eq!(t.style(Element::H4).fg, Color::Default);
        assert!(t.style(Element::H4).attrs.contains(Attrs::BOLD));
        // alert_note inherits bold from alert and colour from itself.
        let note = t.style(Element::AlertNote);
        assert!(note.attrs.contains(Attrs::BOLD));
        assert_eq!(note.fg, Color::Rgb(Rgb(0x89, 0xb4, 0xfa)));
        // math_var is italic teal.
        let v = t.style(Element::MathVar);
        assert!(v.attrs.contains(Attrs::ITALIC));
        assert_eq!(v.fg, Color::Rgb(Rgb(0x94, 0xe2, 0xd5)));
        // link_url turns the link underline off.
        assert_eq!(t.style(Element::LinkUrl).underline, Underline::None);
        assert_eq!(t.style(Element::Link).underline, Underline::Single);
        // text is the terminal default.
        assert_eq!(t.style(Element::Text), Style::PLAIN);
    }

    #[test]
    fn surface_follows_background() {
        let dark = Theme::fallback(Variant::Dark, Some(Rgb(0, 0, 0)));
        assert!(dark.surface.0 >= 12, "{:?}", dark.surface);
        let light = Theme::fallback(Variant::Light, Some(Rgb(255, 255, 255)));
        assert!(light.surface.0 <= 240, "{:?}", light.surface);
        let mocha = Theme::fallback(Variant::Dark, None);
        assert!(mocha.surface > mocha.base);
        assert!(Theme::test().gradient(Element::H1).is_some());
    }

    fn builtin_theme(name: &str) -> chain::LoadedTheme {
        let mut diags = Vec::new();
        let t = chain::load(
            name,
            None,
            "",
            &crate::config::ConfigEnv::default(),
            &mut diags,
        );
        assert!(diags.is_empty(), "{name}: {diags:?}");
        t
    }

    /// Compare two themes element by element for a readable failure.
    fn assert_same(built: &Theme, expected: &Theme, what: &str) {
        assert_eq!(built.name, expected.name, "{what}: name");
        assert_eq!(built.variant, expected.variant, "{what}: variant");
        assert_eq!(built.palette, expected.palette, "{what}: palette");
        assert_eq!(built.base, expected.base, "{what}: base");
        assert_eq!(built.surface, expected.surface, "{what}: surface");
        assert_eq!(built.code_theme, expected.code_theme, "{what}: code theme");
        for &e in Element::ALL {
            assert_eq!(
                built.style(e),
                expected.style(e),
                "{what}: style of {}",
                e.name()
            );
            assert_eq!(
                built.gradient(e),
                expected.gradient(e),
                "{what}: gradient of {}",
                e.name()
            );
        }
        assert_eq!(built, expected, "{what}");
    }

    #[test]
    fn emde_toml_is_exactly_the_fallback_theme() {
        let emde = builtin_theme("emde");
        let backgrounds = [
            None,
            Some(Rgb(0, 0, 0)),
            Some(Rgb(255, 255, 255)),
            Some(Rgb(0x1e, 0x1e, 0x2e)),
            Some(Rgb(0xef, 0xf1, 0xf5)),
            Some(Rgb(0x28, 0x2c, 0x34)),
            Some(Rgb(0xfd, 0xf6, 0xe3)),
        ];
        for variant in Variant::ALL {
            for bg in backgrounds {
                let (built, problems) = build::build(&emde.name, &emde.patch, variant, bg);
                assert!(problems.is_empty(), "{problems:?}");
                assert_same(
                    &built,
                    &Theme::fallback(variant, bg),
                    &format!("{variant:?} on {bg:?}"),
                );
            }
        }
        let (test, _) = build::build(
            &emde.name,
            &emde.patch,
            Variant::Dark,
            Some(Rgb(0x1e, 0x1e, 0x2e)),
        );
        assert_same(&test, &Theme::test(), "test theme");
    }

    fn colours(t: &Theme) -> Vec<(&'static str, Color)> {
        Element::ALL
            .iter()
            .flat_map(|&e| {
                let s = t.style(e);
                [
                    (e.name(), s.fg),
                    (e.name(), s.bg),
                    (e.name(), s.underline_color),
                ]
            })
            .collect()
    }

    #[test]
    fn ansi_theme_uses_only_the_16_colours() {
        let ansi = builtin_theme("ansi");
        for variant in Variant::ALL {
            for bg in [None, Some(Rgb(0, 0, 0)), Some(Rgb(255, 255, 255))] {
                let (t, problems) = build::build(&ansi.name, &ansi.patch, variant, bg);
                assert!(problems.is_empty(), "{problems:?}");
                for (name, c) in colours(&t) {
                    assert!(
                        matches!(c, Color::Default | Color::Ansi(0..=15)),
                        "{name}: {c:?}"
                    );
                }
                assert!(t.palette.is_empty(), "no 24-bit palette entries");
                assert_eq!(t.code_theme, "ansi");
                assert!(Element::ALL.iter().all(|&e| t.gradient(e).is_none()));
            }
        }
    }

    #[test]
    fn mono_theme_has_no_colours() {
        let mono = builtin_theme("mono");
        for variant in Variant::ALL {
            let (t, problems) = build::build(&mono.name, &mono.patch, variant, Some(Rgb(9, 9, 9)));
            assert!(problems.is_empty(), "{problems:?}");
            for (name, c) in colours(&t) {
                assert_eq!(c, Color::Default, "{name}");
            }
            assert!(
                t.style(Element::H1)
                    .attrs
                    .contains(Attrs::BOLD | Attrs::REVERSE)
            );
            assert!(t.style(Element::Emph).attrs.contains(Attrs::ITALIC));
            assert_eq!(t.style(Element::Link).underline, Underline::Single);
            assert!(
                t.style(Element::Rule).attrs.contains(Attrs::DIM),
                "decorations are dim"
            );
            assert_eq!(t.style(Element::H6).attrs, Attrs::ITALIC);
        }
    }

    #[test]
    fn built_in_themes_share_palette_names() {
        let names = |t: &chain::LoadedTheme| -> Vec<Vec<String>> {
            t.patch
                .palette
                .iter()
                .map(|p| p.keys().cloned().collect())
                .collect()
        };
        let emde = names(&builtin_theme("emde"));
        for name in builtin::names() {
            assert_eq!(names(&builtin_theme(name)), emde, "{name}");
        }
    }

    /// The built-in themes that inherit `emde` and bring a palette of their own.
    fn palette_themes() -> impl Iterator<Item = &'static str> {
        builtin::names().filter(|n| ![builtin::DEFAULT, builtin::ANSI, builtin::MONO].contains(n))
    }

    #[test]
    fn palette_themes_resolve_for_both_variants() {
        let emde = builtin_theme("emde");
        assert_eq!(palette_themes().count(), 5);
        for name in palette_themes() {
            let loaded = builtin_theme(name);
            for variant in Variant::ALL {
                for bg in [None, Some(Rgb(0, 0, 0)), Some(Rgb(255, 255, 255))] {
                    let (t, problems) = build::build(&loaded.name, &loaded.patch, variant, bg);
                    assert!(problems.is_empty(), "{name} {variant:?}: {problems:?}");
                    assert_eq!(t.name, name);
                    assert_eq!(t.variant, variant);
                    assert_eq!(
                        t.palette.len(),
                        MOCHA.len(),
                        "{name}: every role is a colour"
                    );
                    assert!(t.gradient(Element::H1).is_some(), "{name}: the h1 bar");
                    // The palette's own page colour, unless the terminal's is known.
                    let own = t.color("base");
                    assert_eq!(Some(t.base), bg.or(own), "{name} {variant:?}");
                    if cfg!(feature = "highlight") {
                        assert!(
                            crate::highlight::list_code_themes().contains(&t.code_theme.as_str()),
                            "{name} {variant:?}: unknown code theme `{}`",
                            t.code_theme
                        );
                    }
                }
            }
            // Its dark palette is its own; light keeps emde's only where it
            // has none upstream (nord), and then only some colours change.
            let (dark, _) = build::build(name, &loaded.patch, Variant::Dark, None);
            let (emde_dark, _) = build::build("emde", &emde.patch, Variant::Dark, None);
            assert_ne!(dark.base, emde_dark.base, "{name}");
            let (light, _) = build::build(name, &loaded.patch, Variant::Light, None);
            let (emde_light, _) = build::build("emde", &emde.patch, Variant::Light, None);
            assert_eq!(
                light.base == emde_light.base,
                name == "nord",
                "{name}: light falls back to emde's palette only for nord"
            );
            assert_ne!(dark.code_theme, light.code_theme, "{name}");
        }
    }

    /// Decorations need WCAG contrast 3:1; everything else is read as text
    /// and needs 4.5:1.
    const DECORATIONS: &[Element] = &[
        Element::HeadingRule,
        Element::Strike,
        Element::LinkUrl,
        Element::LinkRef,
        Element::CodeLabel,
        Element::CodeGutter,
        Element::QuoteBar,
        Element::ListMarker,
        Element::TaskDone,
        Element::TaskTodo,
        Element::TableBorder,
        Element::Rule,
        Element::FrontMatterKey,
        Element::Html,
        Element::ImageAlt,
        Element::ImageFrame,
        Element::Muted,
    ];

    /// Every element whose colours are less than the WCAG minimum apart, on
    /// the variant's own page colour: `(element, contrast, minimum)`. The
    /// terminal's default foreground is taken to be the palette's `text`,
    /// a gradient bar is checked at both ends, code labels and gutters on
    /// the code panel, and alerts (title and text) on their tint.
    fn contrast_failures(t: &Theme) -> Vec<(&'static str, f32, f32)> {
        let text = t.color("text").unwrap_or(t.base);
        let rgb = |c: Color, default: Rgb| match c {
            Color::Rgb(c) => Some(c),
            Color::Default => Some(default),
            Color::Ansi(_) | Color::Indexed(_) => None,
        };
        let mut out = Vec::new();
        for &e in Element::ALL {
            let s = t.style(e);
            let min = if DECORATIONS.contains(&e) { 3.0 } else { 4.5 };
            let Some(fg) = rgb(s.fg, text) else { continue };
            let mut backgrounds: Vec<Rgb> = rgb(s.bg, t.base).into_iter().collect();
            if let Some((_, Color::Rgb(to))) = t.gradient(e) {
                backgrounds.push(to);
            }
            if matches!(e, Element::CodeLabel | Element::CodeGutter) {
                backgrounds.extend(rgb(t.style(Element::CodeBlock).bg, t.base));
            }
            for bg in backgrounds {
                let ratio = crate::color::contrast(fg, bg);
                if ratio < min {
                    out.push((e.name(), ratio, min));
                }
            }
        }
        // Alerts are drawn on a tint of their colour (layout's ALERT_TINT).
        for e in [
            Element::AlertNote,
            Element::AlertTip,
            Element::AlertImportant,
            Element::AlertWarning,
            Element::AlertCaution,
        ] {
            let Some(fg) = rgb(t.style(e).fg, text) else {
                continue;
            };
            let tint = mix_oklab(t.base, fg, crate::layout::blocks::ALERT_TINT);
            for (what, c) in [(e.name(), fg), ("text on an alert", text)] {
                let ratio = crate::color::contrast(c, tint);
                if ratio < 4.5 {
                    out.push((what, ratio, 4.5));
                }
            }
        }
        out
    }

    #[test]
    fn palette_themes_have_readable_contrast() {
        for name in palette_themes() {
            let loaded = builtin_theme(name);
            for variant in Variant::ALL {
                // The palette's page colour, and the terminal's when it is
                // pure black or white.
                let terminal = match variant {
                    Variant::Dark => Rgb(0, 0, 0),
                    Variant::Light => Rgb(255, 255, 255),
                };
                for bg in [None, Some(terminal)] {
                    let (t, _) = build::build(name, &loaded.patch, variant, bg);
                    let failures = contrast_failures(&t);
                    assert!(
                        failures.is_empty(),
                        "{name} {variant:?} on {bg:?}: {failures:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_contrast_check_finds_faint_colours() {
        // Catppuccin Latte's yellow and peach are too light for text on its
        // own page colour, which is why the light palettes darken theirs.
        let latte = Theme::fallback(Variant::Light, None);
        let failures = contrast_failures(&latte);
        assert!(failures.iter().any(|f| f.0 == "code"), "{failures:?}");
        assert!(failures.iter().any(|f| f.0 == "alert_warning"));
        assert!(failures.iter().any(|f| f.0 == "alert_tip"), "on its tint");
        let mocha = Theme::fallback(Variant::Dark, None);
        let failures = contrast_failures(&mocha);
        // Mocha's overlay0 is faint on the code panel, and as a heading.
        assert!(
            failures
                .iter()
                .all(|f| ["h6", "code_label", "code_gutter"].contains(&f.0)),
            "{failures:?}"
        );
    }

    #[test]
    fn variant_indices() {
        for (i, v) in Variant::ALL.iter().enumerate() {
            assert_eq!(v.index(), i);
        }
        assert_eq!(Variant::for_background(Rgb(0, 0, 0)), Variant::Dark);
        assert_eq!(Variant::for_background(Rgb(250, 250, 250)), Variant::Light);
    }
}
