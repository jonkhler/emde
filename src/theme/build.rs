//! Building a resolved [`Theme`] from theme data for one variant.

use std::collections::BTreeMap;

use super::color::{BASE, PaletteProblem, Resolver, UnknownColor, resolve_palette};
use super::spec::{StyleSpec, ThemePatch};
use super::{Element, Theme, Variant, surface_for};
use crate::style::{Attrs, Color, Rgb, StylePatch};

/// The code theme used when a theme names none.
pub(crate) fn default_code_theme(variant: Variant) -> &'static str {
    match variant {
        Variant::Dark => "OneHalfDark",
        Variant::Light => "OneHalfLight",
    }
}

/// The page background assumed when neither the terminal nor the palette
/// provides one (Catppuccin's base colours).
fn default_base(variant: Variant) -> Rgb {
    match variant {
        Variant::Dark => Rgb(0x1e, 0x1e, 0x2e),
        Variant::Light => Rgb(0xef, 0xf1, 0xf5),
    }
}

/// A problem found while building a theme. Building never fails: the
/// affected colour is left unset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BuildProblem {
    Palette(PaletteProblem),
    UnknownColor {
        element: Element,
        field: &'static str,
        name: String,
    },
}

/// Build the theme for `variant` from merged theme data.
///
/// `background` is the terminal's background colour if known; it becomes
/// the page colour `base` (else the palette's `base` is used) and `surface`
/// is derived from it with [`surface_for`].
pub(crate) fn build(
    name: &str,
    patch: &ThemePatch,
    variant: Variant,
    background: Option<Rgb>,
) -> (Theme, Vec<BuildProblem>) {
    let entries = patch.palette.get(variant.index());
    let (palette, palette_problems) = entries.map(resolve_palette).unwrap_or_default();
    let mut problems: Vec<BuildProblem> = palette_problems
        .into_iter()
        .map(BuildProblem::Palette)
        .collect();
    let base = background
        .or_else(|| match palette.get(BASE) {
            Some(Color::Rgb(c)) => Some(*c),
            _ => None,
        })
        .unwrap_or_else(|| default_base(variant));
    let surface = surface_for(base, variant);
    let resolver = Resolver {
        palette: &palette,
        base,
        surface,
    };

    let mut patches = Vec::new();
    let mut ends = Vec::new();
    for (&element, spec) in patch.styles.get(variant.index()).into_iter().flatten() {
        let (p, end) = style_patch(element, spec, &resolver, &mut problems);
        patches.push((element, p));
        if let Some(end) = end {
            ends.push((element, end));
        }
    }
    let gradients = gradients(&patches, &ends);

    let rgb_palette: BTreeMap<String, Rgb> = palette
        .iter()
        .filter_map(|(k, c)| match c {
            Color::Rgb(rgb) => Some((k.clone(), *rgb)),
            _ => None,
        })
        .collect();
    let code = patch
        .code
        .get(variant.index())
        .cloned()
        .flatten()
        .unwrap_or_else(|| default_code_theme(variant).to_owned());
    let theme = Theme::from_patches(
        name,
        variant,
        rgb_palette,
        base,
        surface,
        code,
        &patches,
        &gradients,
    );
    (theme, problems)
}

/// Resolve one element's spec into a style patch and an optional gradient end.
fn style_patch(
    element: Element,
    spec: &StyleSpec,
    resolver: &Resolver<'_>,
    problems: &mut Vec<BuildProblem>,
) -> (StylePatch, Option<Color>) {
    let mut color = |field: &'static str, value: &Option<_>| -> Option<Color> {
        match resolver.resolve(value.as_ref()?) {
            Ok(c) => Some(c),
            Err(UnknownColor(name)) => {
                problems.push(BuildProblem::UnknownColor {
                    element,
                    field,
                    name,
                });
                None
            }
        }
    };
    let (mut set, mut clear) = (Attrs::empty(), Attrs::empty());
    for (flag, value) in [
        (Attrs::BOLD, spec.bold),
        (Attrs::ITALIC, spec.italic),
        (Attrs::DIM, spec.dim),
        (Attrs::STRIKE, spec.strikethrough),
        (Attrs::REVERSE, spec.reverse),
        (Attrs::OVERLINE, spec.overline),
    ] {
        match value {
            Some(true) => set |= flag,
            Some(false) => clear |= flag,
            None => {}
        }
    }
    let patch = StylePatch {
        fg: color("fg", &spec.fg),
        bg: color("bg", &spec.bg),
        underline_color: color("underline_color", &spec.underline_color),
        set,
        clear,
        underline: spec.underline,
    };
    (patch, color("bg_to", &spec.bg_to))
}

/// Gradients run from an element's (inherited) background to its `bg_to`.
/// Only 24-bit endpoints make a gradient; otherwise the bar stays solid.
fn gradients(
    patches: &[(Element, StylePatch)],
    ends: &[(Element, Color)],
) -> Vec<(Element, Color, Color)> {
    let own_bg = |e: Element| {
        patches
            .iter()
            .find(|(pe, _)| *pe == e)
            .and_then(|(_, p)| p.bg)
    };
    let inherited_bg = |mut e: Element| loop {
        if let Some(bg) = own_bg(e) {
            return Some(bg);
        }
        e = e.parent()?;
    };
    ends.iter()
        .filter_map(|&(e, to)| match (inherited_bg(e)?, to) {
            (from @ Color::Rgb(_), Color::Rgb(_)) => Some((e, from, to)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Underline;
    use crate::theme::spec::ThemeFile;

    fn patch(src: &str) -> ThemePatch {
        ThemePatch::from_theme_file(&toml::from_str::<ThemeFile>(src).unwrap())
    }

    #[test]
    fn resolves_styles_and_inheritance() {
        let p = patch(
            "[palette]\naccent = \"#89b4fa\"\nbase = \"#101010\"\n\
             [style.heading]\nfg = \"accent\"\nbold = true\n\
             [style.h2]\nitalic = true\nbold = false\nunderline = \"dotted\"\n",
        );
        let (t, problems) = build("t", &p, Variant::Dark, None);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            t.base,
            Rgb(0x10, 0x10, 0x10),
            "palette base without a background"
        );
        assert_eq!(t.surface, surface_for(t.base, Variant::Dark));
        let h2 = t.style(Element::H2);
        assert_eq!(h2.fg, Color::Rgb(Rgb(0x89, 0xb4, 0xfa)));
        assert_eq!(h2.attrs, Attrs::ITALIC, "bold cleared, italic set");
        assert_eq!(h2.underline, Underline::Dotted);
        assert_eq!(t.style(Element::H3).attrs, Attrs::BOLD);
        assert_eq!(t.code_theme, "OneHalfDark");
        assert_eq!(t.color("accent"), Some(Rgb(0x89, 0xb4, 0xfa)));
    }

    #[test]
    fn background_drives_base_and_surface() {
        let p = patch("[style.code_block]\nbg = \"surface\"\n[style.h1]\nfg = \"base\"");
        let bg = Rgb(0, 0, 0);
        let (t, _) = build("t", &p, Variant::Dark, Some(bg));
        assert_eq!(t.base, bg);
        assert_eq!(
            t.style(Element::CodeBlock).bg,
            Color::Rgb(surface_for(bg, Variant::Dark))
        );
        assert_eq!(t.style(Element::H1).fg, Color::Rgb(bg));
        let (light, _) = build("t", &p, Variant::Light, None);
        assert_eq!(light.base, Rgb(0xef, 0xf1, 0xf5));
        assert_eq!(light.code_theme, "OneHalfLight");
    }

    #[test]
    fn gradients_need_rgb_ends() {
        let p = patch(
            "[palette]\na = \"#102030\"\nb = \"#405060\"\n\
             [style.heading]\nbg = \"a\"\n[style.h1]\nbg_to = \"b\"\n\
             [style.h2]\nbg = \"red\"\nbg_to = \"b\"\n[style.h3]\nbg_to = \"b\"\nbg = \"default\"",
        );
        let (t, _) = build("t", &p, Variant::Dark, None);
        assert_eq!(
            t.gradient(Element::H1),
            Some((
                Color::Rgb(Rgb(0x10, 0x20, 0x30)),
                Color::Rgb(Rgb(0x40, 0x50, 0x60))
            )),
            "inherited background as the start"
        );
        assert_eq!(t.gradient(Element::H2), None, "ANSI start");
        assert_eq!(t.gradient(Element::H3), None, "default start");
        assert_eq!(t.gradient(Element::H4), None, "not inherited");
    }

    #[test]
    fn unknown_colours_are_reported_and_left_unset() {
        let p = patch("[palette]\nx = \"nope\"\n[style.h1]\nfg = \"acent\"\nbold = true");
        let (t, problems) = build("t", &p, Variant::Dark, None);
        assert_eq!(t.style(Element::H1).fg, Color::Default);
        assert!(t.style(Element::H1).attrs.contains(Attrs::BOLD));
        assert!(problems.contains(&BuildProblem::UnknownColor {
            element: Element::H1,
            field: "fg",
            name: "acent".into()
        }));
        assert!(
            problems
                .iter()
                .any(|p| matches!(p, BuildProblem::Palette(_)))
        );
    }

    #[test]
    fn non_rgb_palette_entries_stay_out_of_the_theme_palette() {
        let p = patch("[palette]\na = \"red\"\nb = \"#fff\"\nc = 200\nd = \"default\"");
        let (t, _) = build("t", &p, Variant::Dark, None);
        assert_eq!(t.palette.len(), 1);
        assert_eq!(t.color("b"), Some(Rgb(255, 255, 255)));
    }
}
