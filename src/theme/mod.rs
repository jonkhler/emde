//! Themes: one resolved [`Style`] per [`Element`], plus the palette and the
//! code-highlighting theme name.
//!
//! The resolved [`Theme`] is the contract between configuration (which builds
//! it from theme files, `[palette]` and `[style.*]` overrides) and layout
//! (which only calls [`Theme::style`] / [`Theme::gradient`]).

pub mod element;

use std::collections::BTreeMap;

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
#[derive(Clone, Debug)]
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
}
