//! Tests of loading, layering, validation and theme building.

use std::collections::BTreeSet;

use super::de::dotted_keys;
use super::layer::section_keys;
use super::paths::testdir::TestDir;
use super::*;
use crate::options::{Align, H1Style, Height, TableBorder};
use crate::style::{Attrs, Color, Underline};
use crate::theme::Element;

/// A home directory with an optional `~/.config/emde/config.toml`.
struct Setup {
    dir: TestDir,
}

impl Setup {
    fn new(tag: &str) -> Setup {
        Setup {
            dir: TestDir::new(tag),
        }
    }

    fn env(&self) -> ConfigEnv {
        ConfigEnv {
            emde_config: None,
            xdg_config_home: None,
            home: Some(self.dir.path().to_path_buf()),
        }
    }

    /// Write `~/.config/emde/<rel>`.
    fn write(&self, rel: &str, src: &str) -> PathBuf {
        self.dir.write(&format!(".config/emde/{rel}"), src)
    }

    fn opts(&self) -> LoadOptions {
        LoadOptions {
            env: self.env(),
            ..LoadOptions::default()
        }
    }

    fn load(&self, set: &[&str]) -> Loaded {
        let mut opts = self.opts();
        opts.set = set.iter().map(|s| (*s).to_owned()).collect();
        load(&opts)
    }

    /// Diagnostics as text, with the temporary directory shown as `~`.
    fn report(&self, loaded: &Loaded) -> String {
        let dir = self.dir.path().display().to_string();
        loaded
            .diagnostics
            .iter()
            .map(|d| d.to_string().replace(&dir, "~"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Load a config file's text.
fn load_config(tag: &str, src: &str) -> (Setup, Loaded) {
    let setup = Setup::new(tag);
    setup.write("config.toml", src);
    let loaded = setup.load(&[]);
    (setup, loaded)
}

fn no_diagnostics(loaded: &Loaded) {
    assert!(loaded.diagnostics.is_empty(), "{:#?}", loaded.diagnostics);
}

#[test]
fn default_config_is_complete_and_clean() {
    let mut diags = Vec::new();
    let parsed = de::parse_config(Cow::Borrowed(DEFAULT_CONFIG), Origin::Defaults, &mut diags);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        parsed.value.unset_paths(),
        Vec::<String>::new(),
        "every setting has a default"
    );
    let mut layer = parsed.value;
    assert!(resolve::validate(&mut layer).is_empty());
}

#[test]
fn default_config_keys_match_the_schema() {
    let in_file: BTreeSet<String> = dotted_keys(DEFAULT_CONFIG).into_iter().collect();
    let mut expected = BTreeSet::new();
    for section in layer::settings_sections() {
        expected.insert(section.to_owned());
        for key in section_keys(section).unwrap_or_default() {
            expected.insert(format!("{section}.{key}"));
        }
    }
    assert_eq!(
        in_file, expected,
        "default.toml has exactly the schema's keys"
    );
}

#[test]
fn default_config_documents_every_element_and_style_field() {
    let comments: String = DEFAULT_CONFIG
        .lines()
        .filter(|l| l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let words: BTreeSet<&str> = comments
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .collect();
    for e in Element::ALL {
        assert!(
            words.contains(e.name()),
            "element {} is not documented",
            e.name()
        );
    }
    for key in crate::theme::spec::StyleSpec::KEYS {
        assert!(words.contains(key), "style field {key} is not documented");
    }
}

#[test]
fn every_default_key_is_commented() {
    let lines: Vec<&str> = DEFAULT_CONFIG.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let is_key = !line.starts_with('#') && !line.starts_with('[') && line.contains(" = ");
        if is_key {
            let prev = i
                .checked_sub(1)
                .and_then(|p| lines.get(p))
                .copied()
                .unwrap_or("");
            assert!(prev.starts_with('#'), "`{line}` has no comment above it");
        }
    }
}

#[test]
fn defaults_resolve_to_the_option_defaults() {
    let setup = Setup::new("defaults");
    let loaded = setup.load(&[]);
    no_diagnostics(&loaded);
    assert_eq!(loaded.path, None);
    let c = &loaded.config;
    assert_eq!(c.render, RenderOptions::default());
    assert_eq!(c.pager, PagerOptions::default());
    assert_eq!(c.terminal, TerminalOptions::default());
    assert_eq!(c.theme, ThemeOptions::default());
    assert_eq!(c.theme_name(), "emde");
}

#[test]
fn the_users_example_config() {
    let (_setup, loaded) = load_config(
        "example",
        r##"
[theme]
background = "auto"
code = "OneHalfDark"

[palette]
accent = "#89b4fa"
muted = "#6c7086"

[style.h1]
fg = "accent"
bold = true
underline = "curly"

[render]
max_width = 100
images = "auto"
math = "unicode"
"##,
    );
    no_diagnostics(&loaded);
    let c = &loaded.config;
    assert_eq!(c.theme.code.as_deref(), Some("OneHalfDark"));
    assert!(!c.theme.explicit, "the theme itself was not chosen");
    for bg in [None, Some(Rgb(0, 0, 0)), Some(Rgb(0xff, 0xff, 0xff))] {
        let theme = build_theme(c, bg, ColorDepth::TrueColor);
        let h1 = theme.style(Element::H1);
        assert_eq!(h1.fg, Color::Rgb(Rgb(0x89, 0xb4, 0xfa)), "{bg:?}");
        assert!(h1.attrs.contains(Attrs::BOLD));
        assert_eq!(h1.underline, Underline::Curly);
        assert_eq!(h1.bg, Color::Default, "[style.h1] replaces the theme's bar");
        assert_eq!(theme.gradient(Element::H1), None);
        assert_eq!(theme.code_theme, "OneHalfDark");
        // The rest of the theme is untouched.
        assert_eq!(theme.style(Element::H2).attrs, Attrs::BOLD);
        assert_eq!(theme.color("muted"), Some(Rgb(0x6c, 0x70, 0x86)));
    }
}

#[test]
fn layers_apply_in_order() {
    let setup = Setup::new("order");
    setup.write("config.toml", "[render]\nmax_width = 90\nmargin = 5\n");
    let mut opts = setup.opts();
    assert_eq!(load(&opts).config.render.max_width, 90);
    opts.set = vec!["render.max_width=80".into()];
    let c = load(&opts).config;
    assert_eq!(c.render.max_width, 80, "--set beats the file");
    assert_eq!(c.render.margin, 5, "the file beats the defaults");
    opts.set.push("render.max_width=75".into());
    assert_eq!(load(&opts).config.render.max_width, 75, "later --set wins");
    opts.overrides.max_width = Some(70);
    assert_eq!(load(&opts).config.render.max_width, 70, "flags beat --set");
    opts.no_config = true;
    let c = load(&opts).config;
    assert_eq!(c.render.margin, 2, "--no-config skips the file");
    assert_eq!(c.render.max_width, 70);
}

#[test]
fn overrides_map_onto_settings() {
    let setup = Setup::new("flags");
    let mut opts = setup.opts();
    opts.overrides = Overrides {
        paging: Some(When::Never),
        width: Some(60),
        align: Some(Align::Left),
        color: Some(ColorChoice::Ansi256),
        hyperlinks: Some(When::Never),
        link_refs: Some(When::Always),
        ascii: Some(true),
        background: Some(BackgroundMode::Light),
        code_theme: Some("Nord".into()),
        images: Some(ImageMode::Blocks),
        blocks: Some(BlockGlyphs::Octant),
        remote_images: Some(true),
        math: Some(InlineMath::Raw),
        math_display: Some(DisplayMath::Linear),
        line_numbers: Some(true),
        wrap_code: Some(false),
        ..Overrides::default()
    };
    let loaded = load(&opts);
    no_diagnostics(&loaded);
    let c = loaded.config;
    assert_eq!(c.pager.enabled, When::Never);
    assert_eq!(c.render.width, Some(60));
    assert_eq!(c.render.align, Align::Left);
    assert_eq!(c.render.link_refs, When::Always);
    assert_eq!(c.terminal.color, ColorChoice::Ansi256);
    assert_eq!(c.terminal.hyperlinks, When::Never);
    assert!(c.render.ascii);
    assert_eq!(c.theme.background, BackgroundMode::Light);
    assert_eq!(c.theme.code.as_deref(), Some("Nord"));
    assert_eq!(c.render.images.mode, ImageMode::Blocks);
    assert_eq!(c.render.images.blocks, BlockGlyphs::Octant);
    assert!(c.render.images.remote);
    assert_eq!(c.render.math.inline, InlineMath::Raw);
    assert_eq!(c.render.math.display, DisplayMath::Linear);
    assert!(c.render.code.line_numbers);
    assert!(!c.render.code.wrap);
    assert!(!c.theme.explicit);

    opts.overrides = Overrides {
        theme: Some("mono".into()),
        ..Overrides::default()
    };
    let c = load(&opts).config;
    assert!(c.theme.explicit);
    assert_eq!(c.theme_name(), "mono");
}

#[test]
fn every_section_maps_onto_the_options() {
    let (_setup, loaded) = load_config(
        "sections",
        r###"
[render]
width = 72
margin = 0
link_refs = "never"
ambiguous_width = 2
gradients = "never"
front_matter = "hide"
html = "strip"
[heading]
h1 = "underline"
markers = ["# ", "## ", "", "", "", ""]
numbers = true
[code]
tab_width = 8
label = false
max_highlight_bytes = 1000
style = "frame"
[code.aliases]
Foo = "rust"
[tables]
zebra = false
[math]
letters = "unicode-italic"
fractions = "slash"
scripts = "full"
bold = "unicode"
max_height = 5
tex_delimiters = false
[images]
max_height = 20
tmux_passthrough = "never"
max_pixels = 1000
[markdown]
math = false
linkify = false
definition_lists = false
smart_punctuation = true
[glyphs]
bullets = ["-"]
task = ["[ ]", "[x]"]
quote = "|"
rule = "-"
table = "ascii"
wrap_marker = ">"
icons = "ascii"
[pager]
mouse = false
watch = false
scroll_lines = 5
search_case = "insensitive"
open = "firefox"
[terminal]
probe = false
probe_timeout_ms = 500
"###,
    );
    no_diagnostics(&loaded);
    let c = loaded.config;
    let r = &c.render;
    assert_eq!(r.width, Some(72));
    assert_eq!(r.margin, 0);
    assert_eq!(r.link_refs, When::Never);
    assert!(r.ambiguous_wide && r.math.opts.ambiguous_wide);
    assert_eq!(r.gradients, When::Never);
    assert_eq!(r.front_matter, crate::options::FrontMatterMode::Hide);
    assert_eq!(r.html, crate::options::HtmlMode::Strip);
    assert_eq!(r.heading.h1, H1Style::Underline);
    assert_eq!(r.heading.markers[1], "## ");
    assert!(r.heading.numbers);
    assert_eq!(r.code.tab_width, 8);
    assert!(!r.code.label);
    assert_eq!(r.code.max_highlight_bytes, 1000);
    assert_eq!(r.code.style, crate::options::CodeStyle::Frame);
    assert_eq!(r.code.aliases, vec![("foo".to_owned(), "rust".to_owned())]);
    assert!(!r.tables.zebra);
    assert_eq!(r.math.opts.letters, emde_math::Letters::UnicodeItalic);
    assert_eq!(r.math.opts.fractions, emde_math::Fractions::Slash);
    assert_eq!(r.math.opts.scripts, emde_math::ScriptSet::Full);
    assert_eq!(r.math.opts.bold, emde_math::Bold::Unicode);
    assert_eq!(r.math.opts.max_height, 5);
    assert!(!r.math.tex_delimiters);
    assert_eq!(r.images.max_height, Height::Rows(20));
    assert!(!r.images.tmux_passthrough);
    assert_eq!(r.images.max_pixels, 1000);
    assert!(!r.markdown.math && !r.markdown.linkify && !r.markdown.definition_lists);
    assert!(r.markdown.smart_punctuation);
    assert_eq!(r.glyphs.bullets, vec!["-".to_owned()]);
    assert_eq!(r.glyphs.task, ["[ ]".to_owned(), "[x]".to_owned()]);
    assert_eq!(r.glyphs.table, TableBorder::Ascii);
    assert_eq!(r.glyphs.icons, crate::options::IconSet::Ascii);
    assert_eq!(r.glyphs.wrap_marker, ">");
    assert!(!c.pager.mouse && !c.pager.watch);
    assert_eq!(c.pager.scroll_lines, 5);
    assert_eq!(c.pager.search_case, SearchCase::Insensitive);
    assert_eq!(c.pager.open, OpenCommand::Command("firefox".into()));
    assert!(!c.terminal.probe);
    assert_eq!(
        c.terminal.probe_timeout(true),
        std::time::Duration::from_millis(500)
    );
}

#[test]
fn probe_timeouts() {
    let t = TerminalOptions::default();
    assert_eq!(t.probe_timeout(false).as_millis(), 150);
    assert_eq!(t.probe_timeout(true).as_millis(), 1000);
}

#[test]
fn unknown_keys_warn_with_suggestions() {
    let (setup, loaded) = load_config(
        "unknown",
        r##"colour = "never"

[rendr]
max_width = 80

[render]
max_widht = 80
line_numbers = true

[style.h7]
bold = true

[style.h1]
bolt = true

[dark.palette]
accent = "#000000"
"##,
    );
    insta::assert_snapshot!("unknown_keys", setup.report(&loaded));
    assert!(!loaded.has_errors());
}

#[test]
fn type_errors_drop_one_section() {
    let (setup, loaded) = load_config(
        "types",
        r#"[render]
max_width = "wide"
margin = 4

[pager]
mouse = false

[math]
display = "3d"
"#,
    );
    insta::assert_snapshot!("type_errors", setup.report(&loaded));
    assert!(loaded.has_errors());
    let c = loaded.config;
    assert_eq!(c.render.margin, 2, "[render] is dropped as a whole");
    assert!(!c.pager.mouse, "[pager] survives");
    assert_eq!(c.render.math.display, DisplayMath::TwoD);
}

#[test]
fn syntax_errors_drop_the_file() {
    let (setup, loaded) = load_config("syntax", "[render]\nmax_width = 80\nmargin = = 2\n");
    insta::assert_snapshot!("syntax_error", setup.report(&loaded));
    assert_eq!(loaded.config.render.max_width, 100);
}

#[test]
fn invalid_values_are_ignored_with_a_warning() {
    let (setup, loaded) = load_config(
        "invalid",
        "[code]\ntab_width = 0\n[glyphs]\nbullets = []\nquote = \"x\"\n",
    );
    insta::assert_snapshot!("invalid_values", setup.report(&loaded));
    assert_eq!(loaded.config.render.code.tab_width, 4);
    assert_eq!(loaded.config.render.glyphs.quote, "x");
}

#[test]
fn set_arguments_report_problems() {
    let setup = Setup::new("set");
    let loaded = setup.load(&[
        "render.max_width=abc",
        "render.margin",
        "pager.mouse=maybe",
        "theme.code=Nord",
        "render.colour=never",
    ]);
    insta::assert_snapshot!("set_problems", setup.report(&loaded));
    assert_eq!(loaded.config.theme.code.as_deref(), Some("Nord"));
    assert!(loaded.config.pager.mouse, "an unusable --set is ignored");
}

#[test]
fn unknown_colours_warn() {
    let (setup, loaded) = load_config(
        "colours",
        r##"[palette]
link = "acent"
loop_a = "loop_b"
loop_b = "loop_a"
self = "self"

[palette.light]
only_light = "#ffffff"

[style.h1]
fg = "acent"

[style.h2]
fg = "only_light"

[dark.style.h3]
bg = "surfce/20%"
"##,
    );
    insta::assert_snapshot!("unknown_colours", setup.report(&loaded));
}

#[test]
fn missing_files_are_errors_only_when_named() {
    let setup = Setup::new("missing");
    let mut opts = setup.opts();
    opts.config = Some(setup.dir.path().join("nope.toml"));
    let loaded = load(&opts);
    assert_eq!(loaded.diagnostics.len(), 1);
    assert!(
        loaded.diagnostics[0]
            .message
            .starts_with("config file ignored: cannot read")
    );
    assert_eq!(loaded.path, None);
    // No file in the default locations is fine.
    no_diagnostics(&setup.load(&[]));
    // $EMDE_CONFIG must exist too.
    let mut opts = setup.opts();
    opts.env.emde_config = Some(setup.dir.path().join("env.toml"));
    assert_eq!(load(&opts).diagnostics.len(), 1);
    setup.dir.write("env.toml", "[render]\nmargin = 7\n");
    let loaded = load(&opts);
    no_diagnostics(&loaded);
    assert_eq!(loaded.config.render.margin, 7);
    assert_eq!(loaded.path, Some(setup.dir.path().join("env.toml")));
}

#[test]
fn user_styles_replace_theme_styles_and_palettes_merge() {
    let (_setup, loaded) = load_config(
        "merge",
        r##"[palette]
accent = "#ff0000"

[palette.light]
accent = "#00ff00"

[style.h2]
italic = true

[style.link]
underline = "dashed"
"##,
    );
    no_diagnostics(&loaded);
    let c = &loaded.config;
    let dark = build_theme(c, None, ColorDepth::TrueColor);
    // h2 loses the theme's mauve: it keeps heading's colour and bold.
    let h2 = dark.style(Element::H2);
    assert_eq!(
        h2.fg,
        Color::Rgb(Rgb(0xff, 0, 0)),
        "heading's accent, from the user palette"
    );
    assert_eq!(h2.attrs, Attrs::BOLD | Attrs::ITALIC);
    // The palette merges per key: the theme's other colours stay.
    assert_eq!(dark.color("mauve"), Some(Rgb(0xcb, 0xa6, 0xf7)));
    assert_eq!(
        dark.style(Element::H1).bg,
        Color::Rgb(Rgb(0xff, 0, 0)),
        "h1 bar follows accent"
    );
    let light = build_theme(c, Some(Rgb(255, 255, 255)), ColorDepth::TrueColor);
    assert_eq!(light.variant, Variant::Light);
    assert_eq!(light.color("accent"), Some(Rgb(0, 0xff, 0)));
    assert_eq!(light.style(Element::Link).underline, Underline::Dashed);
    assert_eq!(
        light.style(Element::Link).fg,
        Color::Default,
        "the theme's link colour is replaced too"
    );
}

#[test]
fn user_layers_merge_styles_field_by_field() {
    let setup = Setup::new("fields");
    setup.write("config.toml", "[style.h1]\nfg = \"accent\"\nbold = true\n");
    let loaded = setup.load(&["style.h1.underline=curly"]);
    no_diagnostics(&loaded);
    let h1 = build_theme(&loaded.config, None, ColorDepth::TrueColor).style(Element::H1);
    assert_eq!(h1.fg, Color::Rgb(Rgb(0x89, 0xb4, 0xfa)));
    assert!(h1.attrs.contains(Attrs::BOLD));
    assert_eq!(h1.underline, Underline::Curly);
    assert_eq!(h1.bg, Color::Default);
}

#[test]
fn the_theme_follows_the_colour_depth_unless_chosen() {
    let setup = Setup::new("depth");
    let c = setup.load(&[]).config;
    let name = |d| build_theme(&c, None, d).name;
    assert_eq!(name(ColorDepth::TrueColor), "emde");
    assert_eq!(name(ColorDepth::Ansi256), "emde");
    assert_eq!(name(ColorDepth::Ansi16), "ansi");
    assert_eq!(name(ColorDepth::Mono), "mono");
    assert_eq!(name(ColorDepth::None), "mono");

    let chosen = setup.load(&["theme.name=emde"]).config;
    assert!(chosen.theme.explicit);
    assert_eq!(build_theme(&chosen, None, ColorDepth::Ansi16).name, "emde");
    assert_eq!(build_theme(&chosen, None, ColorDepth::Mono).name, "emde");

    let ansi = setup.load(&["theme.name=ansi"]).config;
    assert_eq!(build_theme(&ansi, None, ColorDepth::TrueColor).name, "ansi");
}

#[test]
fn user_styles_apply_to_automatic_themes() {
    let setup = Setup::new("auto-user");
    setup.write("config.toml", "[style.h2]\nunderline = \"double\"\n");
    let c = setup.load(&[]).config;
    let t = build_theme(&c, None, ColorDepth::Ansi16);
    assert_eq!(t.name, "ansi");
    assert_eq!(t.style(Element::H2).underline, Underline::Double);
    assert_eq!(t.code_theme, "ansi");
    let t = build_theme(&c, None, ColorDepth::Mono);
    assert_eq!(t.style(Element::H2).underline, Underline::Double);
}

#[test]
fn background_choice() {
    let setup = Setup::new("bg");
    let auto = setup.load(&[]).config;
    assert_eq!(
        build_theme(&auto, None, ColorDepth::TrueColor).variant,
        Variant::Dark
    );
    let white = Some(Rgb(255, 255, 255));
    let t = build_theme(&auto, white, ColorDepth::TrueColor);
    assert_eq!(t.variant, Variant::Light);
    assert_eq!(t.base, Rgb(255, 255, 255));
    assert_eq!(t.code_theme, "OneHalfLight");
    let dark = setup.load(&["theme.background=dark"]).config;
    assert_eq!(
        build_theme(&dark, white, ColorDepth::TrueColor).variant,
        Variant::Dark
    );
    assert_eq!(
        build_theme(&auto, Some(Rgb(0x1e, 0x1e, 0x2e)), ColorDepth::TrueColor),
        Theme::test(),
        "the defaults on a Mocha background are the test theme"
    );
}

#[test]
fn themes_from_the_themes_directory_and_relative_paths() {
    let setup = Setup::new("themes");
    setup.write(
        "themes/night.toml",
        "name = \"Night\"\ninherits = \"emde\"\ncode = \"Nord\"\n[palette]\naccent = \"#123456\"\n",
    );
    setup.write("config.toml", "[theme]\nname = \"night\"\n");
    let loaded = setup.load(&[]);
    no_diagnostics(&loaded);
    let c = &loaded.config;
    assert!(c.theme.explicit);
    assert_eq!(c.theme_name(), "Night");
    let t = build_theme(c, None, ColorDepth::Ansi16);
    assert_eq!(t.name, "Night", "a chosen theme is kept at 16 colours");
    assert_eq!(t.code_theme, "Nord");
    assert_eq!(t.style(Element::Link).fg, Color::Rgb(Rgb(0x12, 0x34, 0x56)));

    // A relative path in the config file is relative to the file.
    setup.write("mine/local.toml", "inherits = \"../themes/night.toml\"\n");
    setup.write("config.toml", "[theme]\nname = \"mine/local.toml\"\n");
    let loaded = setup.load(&[]);
    no_diagnostics(&loaded);
    assert_eq!(loaded.config.theme_name(), "local");
    let t = build_theme(&loaded.config, None, ColorDepth::TrueColor);
    assert_eq!(t.code_theme, "Nord");
}

#[test]
fn bad_theme_names_fall_back_to_emde() {
    let (setup, loaded) = load_config("badtheme", "[theme]\nname = \"nrod\"\n");
    insta::assert_snapshot!("bad_theme", setup.report(&loaded));
    assert_eq!(loaded.config.theme_name(), "emde");
    assert!(
        !loaded.config.theme.explicit,
        "a theme that does not load is no choice"
    );
    let t = build_theme(&loaded.config, None, ColorDepth::Ansi16);
    assert_eq!(t.name, "ansi");
}

#[cfg(feature = "highlight")]
#[test]
fn unknown_code_themes_warn() {
    let (setup, loaded) = load_config("code", "[theme]\ncode = \"OneHalfDrak\"\n");
    insta::assert_snapshot!("unknown_code_theme", setup.report(&loaded));
    let (_setup, loaded) = load_config("code2", "[theme]\ncode = \"one-half-dark\"\n");
    no_diagnostics(&loaded);
    let (_setup, loaded) = load_config("code3", "[theme]\ncode = \"auto\"\n");
    no_diagnostics(&loaded);
    assert_eq!(loaded.config.theme.code, None);
}

#[test]
fn check_reports() {
    let setup = Setup::new("check");
    let report = check(&setup.opts());
    assert!(report.is_clean());
    assert_eq!(report.exit_code(), 0);
    assert_eq!(report.to_string(), "no config file (built-in defaults): OK");

    let path = setup.write("config.toml", "[render]\nmargin = \"x\"\n");
    let report = check(&setup.opts());
    assert_eq!(report.exit_code(), 1);
    assert_eq!(report.path.as_deref(), Some(path.as_path()));
    let text = report.to_string();
    assert!(text.ends_with(": 1 error, 0 warnings"), "{text}");

    setup.write("config.toml", "[pager]\nmouse = false\nmosue = true\n");
    let report = check(&setup.opts());
    assert_eq!(report.exit_code(), 1, "warnings fail the check too");
    assert!(report.to_string().ends_with(": 0 errors, 1 warning"));
}

#[test]
fn diagnostics_display() {
    let d = Diagnostic {
        severity: Severity::Warning,
        location: "a.toml:3".into(),
        message: "unknown key `x`".into(),
    };
    assert_eq!(d.to_string(), "warning: a.toml:3: unknown key `x`");
    let d = Diagnostic {
        location: String::new(),
        severity: Severity::Error,
        ..d
    };
    assert_eq!(d.to_string(), "error: unknown key `x`");
}

#[test]
fn the_default_theme_fast_path_matches_the_theme_file() {
    let setup = Setup::new("fast");
    let fast = setup.load(&[]).config;
    assert!(fast.theme_patch.is_none(), "emde.toml is not read");
    // Mocha's own accent: the file is read, but the dark theme is unchanged.
    let slow = setup.load(&["palette.dark.accent=#89b4fa"]).config;
    assert!(slow.theme_patch.is_some());
    for bg in [None, Some(Rgb(0, 0, 0)), Some(Rgb(0x28, 0x2c, 0x34))] {
        // (At 16 colours and without colour the user's palette applies to
        // `ansi` and `mono`, so only the depths using `emde` compare.)
        for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
            assert_eq!(
                build_theme(&fast, bg, depth),
                build_theme(&slow, bg, depth),
                "{bg:?} {depth:?}"
            );
        }
    }
}

#[test]
fn diagnostics_never_print_control_characters() {
    let d = Diagnostic {
        severity: Severity::Warning,
        location: "a\u{1b}]0;title\u{7}.toml".into(),
        message: "unknown key `x\u{1b}[31m`\nnext\tline \u{9b}".into(),
    };
    assert_eq!(
        d.to_string(),
        "warning: a␛]0;title␇.toml: unknown key `x␛[31m`\nnext\tline \u{fffd}"
    );
    // Keys from a file reach messages unchanged, and are shown safely.
    let (setup, loaded) = load_config("ctl", "[render]\n\"evil\\u001b[2J\" = 1\n");
    let text = setup.report(&loaded);
    assert!(text.contains("evil␛[2J"), "{text}");
    assert!(!text.contains('\u{1b}'));
}
