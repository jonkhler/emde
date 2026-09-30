//! From merged layers to resolved options.
//!
//! [`validate`] runs on each layer before merging and removes values that
//! parse but make no sense (a tab width of 0, glyphs with control
//! characters), so the next lower layer's value applies. After merging, the
//! `*_options` functions map the layer onto the option structs; any value
//! still missing falls back to the struct's `Default`, which matches
//! `default.toml`.

use super::layer::ConfigLayer;
use super::value::{AmbiguousWidth, TmuxPassthrough, has_control};
use super::{PagerOptions, TerminalOptions, ThemeOptions};
use crate::options::{
    CodeOptions, Glyphs, H1Style, HeadingOptions, ImageOptions, MarkdownOptions, MathMode,
    RenderOptions, TableOptions,
};
use crate::theme::color::ColorSpec;
use crate::theme::spec::{StyleSpec, ThemePatch};
use crate::theme::{Element, Variant};

/// Longest accepted probe timeout.
pub(crate) const MAX_PROBE_TIMEOUT_MS: u32 = 10_000;

/// A value removed by [`validate`]: its key path and why.
pub(crate) type Invalid = (Vec<&'static str>, String);

/// Remove unusable values from a layer, reporting each.
pub(crate) fn validate(layer: &mut ConfigLayer) -> Vec<Invalid> {
    let mut out: Vec<Invalid> = Vec::new();
    let mut check = |ok: bool, path: &[&'static str], why: &str| {
        if !ok {
            out.push((
                path.to_vec(),
                format!("`{}` {why}; ignoring it", path.join(".")),
            ));
        }
        ok
    };
    let text_ok = |s: &str| !s.trim().is_empty() && !has_control(s);
    let glyph_ok = |s: &str| !s.is_empty() && !has_control(s);

    let t = &mut layer.theme;
    clear_if(&mut t.name, |n| {
        !check(
            text_ok(n),
            &["theme", "name"],
            "must be a theme name or path",
        )
    });
    clear_if(&mut t.code, |c| {
        !check(
            text_ok(c),
            &["theme", "code"],
            "must be a code theme name or path",
        )
    });

    let c = &mut layer.code;
    clear_if(&mut c.tab_width, |w| {
        !check(
            (1..=16).contains(w),
            &["code", "tab_width"],
            "must be from 1 to 16",
        )
    });
    let before = c.aliases.0.len();
    c.aliases.0.retain(|k, v| text_ok(k) && text_ok(v));
    check(
        c.aliases.0.len() == before,
        &["code", "aliases"],
        "has empty or invalid entries",
    );

    clear_if(&mut layer.math.max_height, |h| {
        !check(*h > 0, &["math", "max_height"], "must be at least 1")
    });
    clear_if(&mut layer.images.max_pixels, |p| {
        !check(*p > 0, &["images", "max_pixels"], "must be at least 1")
    });
    clear_if(&mut layer.pager.scroll_lines, |n| {
        !check(*n > 0, &["pager", "scroll_lines"], "must be at least 1")
    });
    clear_if(&mut layer.terminal.probe_timeout_ms, |ms| {
        !check(
            *ms <= MAX_PROBE_TIMEOUT_MS,
            &["terminal", "probe_timeout_ms"],
            &format!("must be at most {MAX_PROBE_TIMEOUT_MS}"),
        )
    });

    let g = &mut layer.glyphs;
    let many_ok = |v: &[String]| !v.is_empty() && v.len() <= 16 && v.iter().all(|s| glyph_ok(s));
    clear_if(&mut g.bullets, |b| {
        !check(
            many_ok(b),
            &["glyphs", "bullets"],
            "must be 1 to 16 non-empty glyphs",
        )
    });
    clear_if(&mut g.task, |t| {
        !check(
            many_ok(t),
            &["glyphs", "task"],
            "must be two non-empty glyphs",
        )
    });
    clear_if(&mut g.quote, |s| {
        !check(
            glyph_ok(s),
            &["glyphs", "quote"],
            "must be a non-empty glyph",
        )
    });
    clear_if(&mut g.rule, |s| {
        !check(
            glyph_ok(s),
            &["glyphs", "rule"],
            "must be a non-empty glyph",
        )
    });
    clear_if(&mut g.wrap_marker, |s| {
        !check(
            glyph_ok(s),
            &["glyphs", "wrap_marker"],
            "must be a non-empty glyph",
        )
    });
    clear_if(&mut layer.heading.markers, |m| {
        !check(
            !m.iter().any(|s| has_control(s)),
            &["heading", "markers"],
            "must not contain control characters",
        )
    });
    out
}

/// Set `slot` to `None` when its value is rejected.
fn clear_if<T>(slot: &mut Option<T>, reject: impl FnOnce(&T) -> bool) {
    if slot.as_ref().is_some_and(reject) {
        *slot = None;
    }
}

/// `[render]`, `[heading]`, `[code]`, `[tables]`, `[math]`, `[images]`,
/// `[markdown]` and `[glyphs]` as [`RenderOptions`].
pub(crate) fn render_options(l: &ConfigLayer) -> RenderOptions {
    let d = RenderOptions::default();
    let r = &l.render;
    let ambiguous_wide = r
        .ambiguous_width
        .map_or(d.ambiguous_wide, |a| a == AmbiguousWidth::Wide);
    RenderOptions {
        width: r.width.map_or(d.width, |w| (w > 0).then_some(w)),
        max_width: r.max_width.unwrap_or(d.max_width),
        margin: r.margin.unwrap_or(d.margin),
        align: r.align.unwrap_or(d.align),
        link_refs: r.link_refs.unwrap_or(d.link_refs),
        ascii: r.ascii.unwrap_or(d.ascii),
        ambiguous_wide,
        gradients: r.gradients.unwrap_or(d.gradients),
        front_matter: r.front_matter.unwrap_or(d.front_matter),
        html: r.html.unwrap_or(d.html),
        heading: heading_options(l, d.heading),
        glyphs: glyphs(l, d.glyphs),
        code: code_options(l, d.code),
        tables: TableOptions {
            zebra: l.tables.zebra.unwrap_or(d.tables.zebra),
        },
        math: math_mode(l, d.math, ambiguous_wide),
        images: image_options(l, d.images),
        markdown: markdown_options(l, d.markdown),
    }
}

fn heading_options(l: &ConfigLayer, d: HeadingOptions) -> HeadingOptions {
    let h = &l.heading;
    HeadingOptions {
        h1: h.h1.unwrap_or(d.h1),
        h2: h.h2.unwrap_or(d.h2),
        markers: h.markers.clone().unwrap_or(d.markers),
        numbers: h.numbers.unwrap_or(d.numbers),
    }
}

/// How `h1` is drawn when no layer above the defaults sets `heading.h1`.
///
/// A bar needs a background. When the user's own `[style.h1]` replaces the
/// theme's h1 without one (in both variants, and inheriting none from
/// `heading` or `text`), the bar would be empty, so the heading is drawn as
/// styled text with its underline: `[style.h1] fg = "accent"`,
/// `underline = "curly"` gives accent text with a curly underline.
pub(crate) fn default_h1_style(
    configured: H1Style,
    theme: Option<&ThemePatch>,
    user: &ThemePatch,
) -> H1Style {
    let draws_a_bar = |spec: &StyleSpec| {
        spec.bg.as_ref().is_some_and(|c| *c != ColorSpec::Default)
            || spec.bg_to.is_some()
            || spec.reverse == Some(true)
    };
    let text_only = Variant::ALL.iter().all(|v| {
        let i = v.index();
        let spec = |e: Element| {
            let own = user.styles.get(i).and_then(|s| s.get(&e));
            own.or_else(|| theme?.styles.get(i)?.get(&e))
        };
        let Some(h1) = user.styles.get(i).and_then(|s| s.get(&Element::H1)) else {
            return false;
        };
        !draws_a_bar(h1)
            && [Element::Heading, Element::Text]
                .into_iter()
                .all(|e| spec(e).is_none_or(|s| !draws_a_bar(s)))
    });
    match configured {
        H1Style::Bar if text_only => H1Style::Underline,
        other => other,
    }
}

fn glyphs(l: &ConfigLayer, d: Glyphs) -> Glyphs {
    let g = &l.glyphs;
    Glyphs {
        bullets: g.bullets.clone().unwrap_or(d.bullets),
        task: g.task.clone().unwrap_or(d.task),
        quote: g.quote.clone().unwrap_or(d.quote),
        rule: g.rule.clone().unwrap_or(d.rule),
        table: g.table.unwrap_or(d.table),
        wrap_marker: g.wrap_marker.clone().unwrap_or(d.wrap_marker),
        icons: g.icons.unwrap_or(d.icons),
    }
}

fn code_options(l: &ConfigLayer, d: CodeOptions) -> CodeOptions {
    let c = &l.code;
    CodeOptions {
        wrap: c.wrap.unwrap_or(d.wrap),
        line_numbers: c.line_numbers.unwrap_or(d.line_numbers),
        tab_width: c.tab_width.unwrap_or(d.tab_width),
        label: c.label.unwrap_or(d.label),
        max_highlight_bytes: c.max_highlight_bytes.unwrap_or(d.max_highlight_bytes),
        style: c.style.unwrap_or(d.style),
        aliases: c
            .aliases
            .0
            .iter()
            .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
            .collect(),
    }
}

fn math_mode(l: &ConfigLayer, d: MathMode, ambiguous_wide: bool) -> MathMode {
    let m = &l.math;
    let o = d.opts;
    MathMode {
        inline: l.render.math.unwrap_or(d.inline),
        display: m.display.unwrap_or(d.display),
        tex_delimiters: m.tex_delimiters.unwrap_or(d.tex_delimiters),
        opts: emde_math::MathOptions {
            letters: m.letters.unwrap_or(o.letters),
            scripts: m.scripts.unwrap_or(o.scripts),
            fractions: m.fractions.unwrap_or(o.fractions),
            bold: m.bold.unwrap_or(o.bold),
            ambiguous_wide,
            max_height: m.max_height.unwrap_or(o.max_height),
        },
    }
}

fn image_options(l: &ConfigLayer, d: ImageOptions) -> ImageOptions {
    let i = &l.images;
    ImageOptions {
        mode: l.render.images.unwrap_or(d.mode),
        blocks: i.blocks.unwrap_or(d.blocks),
        max_height: i.max_height.unwrap_or(d.max_height),
        remote: i.remote.unwrap_or(d.remote),
        tmux_passthrough: i
            .tmux_passthrough
            .map_or(d.tmux_passthrough, |t| t == TmuxPassthrough::IfEnabled),
        max_pixels: i.max_pixels.unwrap_or(d.max_pixels),
    }
}

fn markdown_options(l: &ConfigLayer, d: MarkdownOptions) -> MarkdownOptions {
    let m = &l.markdown;
    MarkdownOptions {
        math: m.math.unwrap_or(d.math),
        linkify: m.linkify.unwrap_or(d.linkify),
        definition_lists: m.definition_lists.unwrap_or(d.definition_lists),
        smart_punctuation: m.smart_punctuation.unwrap_or(d.smart_punctuation),
    }
}

/// `[pager]` as [`PagerOptions`].
pub(crate) fn pager_options(l: &ConfigLayer) -> PagerOptions {
    let d = PagerOptions::default();
    let p = &l.pager;
    PagerOptions {
        enabled: p.enabled.unwrap_or(d.enabled),
        mouse: p.mouse.unwrap_or(d.mouse),
        watch: p.watch.unwrap_or(d.watch),
        scroll_lines: p.scroll_lines.unwrap_or(d.scroll_lines),
        search_case: p.search_case.unwrap_or(d.search_case),
        open: p.open.clone().unwrap_or(d.open),
    }
}

/// `[terminal]` plus `render.color` and `render.hyperlinks` as [`TerminalOptions`].
pub(crate) fn terminal_options(l: &ConfigLayer) -> TerminalOptions {
    let d = TerminalOptions::default();
    TerminalOptions {
        probe: l.terminal.probe.unwrap_or(d.probe),
        probe_timeout_ms: l.terminal.probe_timeout_ms.unwrap_or(d.probe_timeout_ms),
        color: l.render.color.unwrap_or(d.color),
        hyperlinks: l.render.hyperlinks.unwrap_or(d.hyperlinks),
    }
}

/// `[theme]` as [`ThemeOptions`]. `explicit` says whether a layer above the
/// defaults chose the theme.
pub(crate) fn theme_options(l: &ConfigLayer, explicit: bool) -> ThemeOptions {
    let d = ThemeOptions::default();
    let t = &l.theme;
    ThemeOptions {
        name: t.name.clone().unwrap_or(d.name),
        background: t.background.unwrap_or(d.background),
        code: t
            .code
            .clone()
            .filter(|c| !c.trim().eq_ignore_ascii_case("auto")),
        explicit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::layer::{CodeLayer, GlyphsLayer, ThemeLayer};

    #[test]
    fn validation_clears_bad_values() {
        let mut layer = ConfigLayer {
            theme: ThemeLayer {
                name: Some("  ".into()),
                code: Some("Nord".into()),
                ..ThemeLayer::default()
            },
            code: CodeLayer {
                tab_width: Some(0),
                ..CodeLayer::default()
            },
            glyphs: GlyphsLayer {
                bullets: Some(vec![]),
                quote: Some("\u{1b}[31m".into()),
                rule: Some("-".into()),
                ..GlyphsLayer::default()
            },
            ..ConfigLayer::default()
        };
        layer.pager.scroll_lines = Some(0);
        layer.terminal.probe_timeout_ms = Some(60_000);
        layer.code.aliases.0.insert("ok".into(), "rust".into());
        layer.code.aliases.0.insert("bad".into(), "".into());
        let invalid = validate(&mut layer);
        let paths: Vec<String> = invalid.iter().map(|(p, _)| p.join(".")).collect();
        assert_eq!(
            paths,
            [
                "theme.name",
                "code.tab_width",
                "code.aliases",
                "pager.scroll_lines",
                "terminal.probe_timeout_ms",
                "glyphs.bullets",
                "glyphs.quote",
            ]
        );
        assert_eq!(
            invalid[1].1,
            "`code.tab_width` must be from 1 to 16; ignoring it"
        );
        assert_eq!(layer.theme.name, None);
        assert_eq!(layer.theme.code.as_deref(), Some("Nord"));
        assert_eq!(layer.code.tab_width, None);
        assert_eq!(layer.glyphs.rule.as_deref(), Some("-"));
        assert_eq!(layer.code.aliases.0.len(), 1);
    }

    #[test]
    fn empty_layer_resolves_to_defaults() {
        let l = ConfigLayer::default();
        assert_eq!(render_options(&l), RenderOptions::default());
        assert_eq!(pager_options(&l), PagerOptions::default());
        assert_eq!(terminal_options(&l), TerminalOptions::default());
        assert_eq!(theme_options(&l, false), ThemeOptions::default());
    }

    #[test]
    fn special_mappings() {
        let mut l = ConfigLayer::default();
        l.render.width = Some(0);
        l.render.ambiguous_width = Some(AmbiguousWidth::Wide);
        l.images.tmux_passthrough = Some(TmuxPassthrough::Never);
        l.code
            .aliases
            .0
            .insert(" Docker ".into(), " dockerfile ".into());
        l.theme.code = Some("Auto".into());
        let r = render_options(&l);
        assert_eq!(r.width, None, "0 means the terminal width");
        assert!(r.ambiguous_wide);
        assert!(
            r.math.opts.ambiguous_wide,
            "math follows render.ambiguous_width"
        );
        assert!(!r.images.tmux_passthrough);
        assert_eq!(
            r.code.aliases,
            vec![("docker".to_owned(), "dockerfile".to_owned())]
        );
        assert_eq!(theme_options(&l, true).code, None, "`auto` is no override");
        l.render.width = Some(120);
        assert_eq!(render_options(&l).width, Some(120));
    }
}
