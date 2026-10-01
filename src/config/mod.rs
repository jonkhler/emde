//! Configuration: embedded defaults, themes, the user file, `--set` and
//! flag layers.
//!
//! Settings come in layers, later ones winning:
//!
//! 1. the embedded [`DEFAULT_CONFIG`] (`assets/default.toml`, which also
//!    documents every key and is what `--print-default-config` prints);
//! 2. the theme chain (theme data only: palette, styles, code theme);
//! 3. the user file (`--config`, `$EMDE_CONFIG`,
//!    `$XDG_CONFIG_HOME/emde/config.toml`, `~/.config/emde/config.toml`);
//! 4. each `--set KEY=VALUE`, as a one-line TOML document;
//! 5. dedicated command-line flags ([`Overrides`]).
//!
//! [`load`] never fails: unknown keys, bad values and unreadable files
//! become [`Diagnostic`]s and the affected settings keep their lower-layer
//! values, so a bad config never stops a document from being shown.
//! [`check()`] reports the same diagnostics for `--check-config`.
//!
//! The result is a [`Config`]: [`RenderOptions`], [`PagerOptions`],
//! [`TerminalOptions`] and [`ThemeOptions`], plus the theme data that
//! [`build_theme`] turns into a [`Theme`] once the terminal's colour depth
//! and background are known.

mod check;
pub(crate) mod de;
pub(crate) mod layer;
pub(crate) mod paths;
mod resolve;
mod set;
pub(crate) mod suggest;
pub(crate) mod value;

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

pub use paths::ConfigEnv;
pub use value::{BackgroundMode, ColorChoice, OpenCommand, SearchCase};

use crate::options::{Align, BlockGlyphs, DisplayMath, ImageMode, InlineMath, RenderOptions, When};
use crate::style::Rgb;
use crate::term::ColorDepth;
use crate::theme::spec::ThemePatch;
use crate::theme::{Theme, Variant, builtin, chain};
use check::{Checks, check_aliases, check_code_themes, check_colours, check_keys};
use de::{Origin, Parsed};
use layer::{ConfigLayer, Merge as _};

/// The embedded default configuration (`assets/default.toml`).
pub const DEFAULT_CONFIG: &str = include_str!("../../assets/default.toml");

/// How serious a configuration problem is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Something was ignored (an unknown key, an out-of-range value).
    Warning,
    /// Something could not be used (a file, a table with a type error).
    Error,
}

/// A problem found while loading configuration or themes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Where: a path with an optional `:line`, `--set …`, or a built-in.
    pub location: String,
    /// What is wrong; may span several lines (TOML error snippets).
    pub message: String,
}

/// Displayed as `severity: location: message`. Text that came from input
/// (keys, values, paths) is shown safely: control characters other than
/// newlines and tabs become visible symbols instead of reaching the terminal.
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let severity = match self.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        let message = printable(&self.message);
        if self.location.is_empty() {
            write!(f, "{severity}: {message}")
        } else {
            write!(f, "{severity}: {}: {message}", printable(&self.location))
        }
    }
}

/// `s` with control characters (except `\n` and `\t`) replaced by visible
/// symbols: C0 as the Control Pictures block (`␛`), DEL as `␡`, C1 as `�`.
fn printable(s: &str) -> Cow<'_, str> {
    let unsafe_char = |c: char| c.is_control() && c != '\n' && c != '\t';
    if !s.chars().any(unsafe_char) {
        return Cow::Borrowed(s);
    }
    s.chars()
        .map(|c| match u32::from(c) {
            _ if !unsafe_char(c) => c,
            n @ 0..=0x1f => char::from_u32(0x2400 + n).unwrap_or('\u{fffd}'),
            0x7f => '\u{2421}',
            _ => '\u{fffd}',
        })
        .collect::<String>()
        .into()
}

/// Built-in pager settings (`[pager]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PagerOptions {
    /// When the pager runs (on a terminal only, whatever this says):
    /// `always` (the default) whatever the length, `auto` for documents
    /// taller than the screen.
    pub enabled: When,
    /// Mouse wheel scrolling and link clicks.
    pub mouse: bool,
    /// Reload when the file changes.
    pub watch: bool,
    /// Lines per wheel step.
    pub scroll_lines: u16,
    pub search_case: SearchCase,
    /// How external links are opened.
    pub open: OpenCommand,
    /// `[pager.keys]`: action names and their keys, in order (see
    /// [`crate::pager::keymap::Keymap::new`]).
    pub keys: Vec<(String, Vec<String>)>,
}

impl Default for PagerOptions {
    fn default() -> Self {
        PagerOptions {
            enabled: When::Always,
            mouse: true,
            watch: true,
            scroll_lines: 3,
            search_case: SearchCase::Smart,
            open: OpenCommand::Auto,
            keys: Vec::new(),
        }
    }
}

/// Terminal detection settings (`[terminal]`, plus `render.color` and
/// `render.hyperlinks`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalOptions {
    /// Query the terminal once (background colour, graphics, cell size).
    pub probe: bool,
    /// Probe deadline; 0 = automatic (150 ms locally, 1000 ms over SSH).
    pub probe_timeout_ms: u32,
    /// `render.color`.
    pub color: ColorChoice,
    /// `render.hyperlinks`: OSC 8 links.
    pub hyperlinks: When,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        TerminalOptions {
            probe: true,
            probe_timeout_ms: 0,
            color: ColorChoice::Auto,
            hyperlinks: When::Auto,
        }
    }
}

impl TerminalOptions {
    /// The probe deadline to use.
    pub fn probe_timeout(&self, over_ssh: bool) -> std::time::Duration {
        let ms = match self.probe_timeout_ms {
            0 if over_ssh => 1000,
            0 => 150,
            ms => ms,
        };
        std::time::Duration::from_millis(u64::from(ms))
    }
}

/// Theme selection (`[theme]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeOptions {
    /// Theme name or path.
    pub name: String,
    pub background: BackgroundMode,
    /// Code theme override; `None` uses the theme's choice (`code = "auto"`).
    pub code: Option<String>,
    /// Whether the user chose the theme (then emde never switches to `ansi`
    /// or `mono` on its own).
    pub explicit: bool,
}

impl Default for ThemeOptions {
    fn default() -> Self {
        ThemeOptions {
            name: builtin::DEFAULT.to_owned(),
            background: BackgroundMode::Auto,
            code: None,
            explicit: false,
        }
    }
}

impl ThemeOptions {
    /// The theme variant for a terminal background (dark when unknown).
    pub fn variant(&self, background: Option<Rgb>) -> Variant {
        match self.background {
            BackgroundMode::Dark => Variant::Dark,
            BackgroundMode::Light => Variant::Light,
            BackgroundMode::Auto => background.map_or(Variant::Dark, Variant::for_background),
        }
    }
}

/// Settings given by dedicated command-line flags (the highest layer).
/// `None` leaves the setting to the lower layers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `--paging` (and `--plain`, which means `never`).
    pub paging: Option<When>,
    /// `--width` (0 = the terminal width).
    pub width: Option<u16>,
    /// `--max-width`.
    pub max_width: Option<u16>,
    /// `--align`.
    pub align: Option<Align>,
    /// `--color`.
    pub color: Option<ColorChoice>,
    /// `--hyperlinks`.
    pub hyperlinks: Option<When>,
    /// `--link-refs`.
    pub link_refs: Option<When>,
    /// `--ascii`.
    pub ascii: Option<bool>,
    /// `--theme`.
    pub theme: Option<String>,
    /// `--background`.
    pub background: Option<BackgroundMode>,
    /// `--code-theme`.
    pub code_theme: Option<String>,
    /// `--images`.
    pub images: Option<ImageMode>,
    /// `--blocks`.
    pub blocks: Option<BlockGlyphs>,
    /// `--remote-images`.
    pub remote_images: Option<bool>,
    /// `--math`.
    pub math: Option<InlineMath>,
    /// `--math-display`.
    pub math_display: Option<DisplayMath>,
    /// `--line-numbers`.
    pub line_numbers: Option<bool>,
    /// `--no-wrap-code` (as `Some(false)`).
    pub wrap_code: Option<bool>,
}

impl Overrides {
    /// The flags as a configuration layer.
    fn to_layer(&self) -> ConfigLayer {
        let mut l = ConfigLayer::default();
        l.pager.enabled = self.paging;
        l.render.width = self.width;
        l.render.max_width = self.max_width;
        l.render.align = self.align;
        l.render.color = self.color;
        l.render.hyperlinks = self.hyperlinks;
        l.render.link_refs = self.link_refs;
        l.render.ascii = self.ascii;
        l.render.images = self.images;
        l.render.math = self.math;
        l.theme.name.clone_from(&self.theme);
        l.theme.background = self.background;
        l.theme.code.clone_from(&self.code_theme);
        l.images.blocks = self.blocks;
        l.images.remote = self.remote_images;
        l.math.display = self.math_display;
        l.code.line_numbers = self.line_numbers;
        l.code.wrap = self.wrap_code;
        l
    }
}

/// What to load.
///
/// The `Default` has an empty [`ConfigEnv`], which finds no config file or
/// installed theme (what tests want); the program fills `env` with
/// [`ConfigEnv::from_process`], as [`LoadOptions::from_process`] does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadOptions {
    /// `--config PATH`.
    pub config: Option<PathBuf>,
    /// `--no-config`: skip the user file.
    pub no_config: bool,
    /// `--set KEY=VALUE` arguments, in order.
    pub set: Vec<String>,
    /// Dedicated flags.
    pub overrides: Overrides,
    /// Where to look for files.
    pub env: ConfigEnv,
}

impl LoadOptions {
    /// Options that look for files where the running process would
    /// (`$EMDE_CONFIG`, `$XDG_CONFIG_HOME`, `$HOME`), with no flags yet.
    pub fn from_process() -> LoadOptions {
        LoadOptions {
            env: ConfigEnv::from_process(),
            ..LoadOptions::default()
        }
    }
}

/// The resolved configuration.
///
/// Deliberately has no `Default`: it only comes from [`load`], which starts
/// from the embedded defaults.
#[derive(Clone, Debug)]
pub struct Config {
    pub render: RenderOptions,
    pub pager: PagerOptions,
    pub terminal: TerminalOptions,
    pub theme: ThemeOptions,
    /// Name of the configured theme (after lookup).
    theme_name: String,
    /// The configured theme's merged `inherits` chain; `None` for the
    /// built-in default theme when nothing changes it, which is exactly
    /// [`Theme::fallback`] (so `emde.toml` need not be parsed).
    theme_patch: Option<ThemePatch>,
    /// The user's own `[palette]` and `[style.*]` tables, merged.
    user_patch: ThemePatch,
}

impl Config {
    /// The display name of the configured theme.
    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }
}

/// The result of [`load`].
#[derive(Clone, Debug)]
pub struct Loaded {
    pub config: Config,
    /// The user config file that was read, if any.
    pub path: Option<PathBuf>,
    /// Everything that was ignored or could not be used, in order.
    pub diagnostics: Vec<Diagnostic>,
}

impl Loaded {
    /// Whether any problem is an error (not just a warning).
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

/// Load the configuration. Never fails; see [`Loaded::diagnostics`].
pub fn load(opts: &LoadOptions) -> Loaded {
    load_with(opts, Checks::Quick)
}

/// [`load`], checking code themes and languages as far as `checks` says.
fn load_with(opts: &LoadOptions, checks: Checks) -> Loaded {
    let mut diags = Vec::new();

    let mut defaults =
        de::parse_config(Cow::Borrowed(DEFAULT_CONFIG), Origin::Defaults, &mut diags);
    let (path, mut user_docs) = user_layers(opts, &mut diags);
    for doc in &mut user_docs {
        doc.pager_keys(&mut diags);
    }
    for doc in std::iter::once(&mut defaults).chain(user_docs.iter_mut()) {
        for (key, message) in resolve::validate(&mut doc.value) {
            diags.push(doc.diagnostic(Severity::Warning, &key, message));
        }
    }

    let (theme, explicit) = select_theme(&defaults, &user_docs, &opts.env, &mut diags);
    let theme_docs = theme.as_ref().map_or(&[][..], |t| &t.docs[..]);
    if let Some(t) = &theme {
        check_colours(theme_docs, &user_docs, &t.patch, &mut diags);
    }
    check_code_themes(theme_docs, &user_docs, checks, &mut diags);
    check_keys(&user_docs, &mut diags);
    if checks == Checks::Thorough {
        check_aliases(&user_docs, &mut diags);
    }

    // Merge the settings, and separately the user's theme tables.
    let h1_chosen = user_docs.iter().any(|d| d.value.heading.h1.is_some());
    let mut merged = defaults.value;
    merged.take_theme_tables();
    let mut user_patch = ThemePatch::default();
    for doc in user_docs {
        let mut layer = doc.value;
        let (palette, style, dark, light) = layer.take_theme_tables();
        user_patch.patch(ThemePatch::from_tables(
            &palette, &style, &dark, &light, None,
        ));
        merged.merge(layer);
    }

    let mut render = resolve::render_options(&merged);
    if !h1_chosen {
        let theme_patch = theme.as_ref().map(|t| &t.patch);
        render.heading.h1 = resolve::default_h1_style(render.heading.h1, theme_patch, &user_patch);
    }
    let config = Config {
        render,
        pager: resolve::pager_options(&merged),
        terminal: resolve::terminal_options(&merged),
        theme: resolve::theme_options(&merged, explicit),
        theme_name: theme
            .as_ref()
            .map_or_else(|| builtin::DEFAULT.to_owned(), |t| t.name.clone()),
        theme_patch: theme.map(|t| t.patch),
        user_patch,
    };
    Loaded {
        config,
        path,
        diagnostics: diags,
    }
}

/// Load the theme named by the highest layer that names one, and say
/// whether the user chose it. The built-in default theme is
/// [`Theme::fallback`] and is only read (`None` otherwise) when the user's
/// tables change it. A theme that could not be loaded is no choice.
fn select_theme(
    defaults: &Parsed<ConfigLayer>,
    user_docs: &[Parsed<ConfigLayer>],
    env: &ConfigEnv,
    diags: &mut Vec<Diagnostic>,
) -> (Option<chain::LoadedTheme>, bool) {
    let chosen = user_docs.iter().rev().find_map(|d| {
        let name = d.value.theme.name.clone()?;
        Some((name, d.location(&["theme", "name"])))
    });
    let requested = chosen.is_some();
    let (spec, requested_at) = chosen.unwrap_or_else(|| {
        let name = defaults.value.theme.name.clone();
        let name = name.unwrap_or_else(|| builtin::DEFAULT.to_owned());
        (name, defaults.origin.to_string())
    });
    let changes_theme = user_docs.iter().any(|d| d.value.has_theme_tables());
    let theme = (spec != builtin::DEFAULT || changes_theme)
        .then(|| chain::load(&spec, None, &requested_at, env, diags));
    let explicit = requested && !theme.as_ref().is_some_and(|t| t.fell_back);
    (theme, explicit)
}

/// The user file, each `--set`, and the flags, in priority order.
fn user_layers(
    opts: &LoadOptions,
    diags: &mut Vec<Diagnostic>,
) -> (Option<PathBuf>, Vec<Parsed<ConfigLayer>>) {
    let env = &opts.env;
    let mut docs = Vec::new();
    let mut path = None;
    let found = if opts.no_config {
        None
    } else {
        paths::find_config(opts.config.as_deref(), env)
    };
    if let Some(file) = found {
        match paths::read_text(&file.path) {
            Ok(src) => {
                let origin = Origin::File(file.path.clone());
                let mut doc = de::parse_config(Cow::Owned(src), origin, diags);
                resolve_paths(&mut doc.value, file.path.parent(), env);
                docs.push(doc);
                path = Some(file.path);
            }
            Err(message) => diags.push(Diagnostic {
                severity: Severity::Error,
                location: file.path.display().to_string(),
                message: format!("config file ignored: {message}"),
            }),
        }
    }
    for arg in &opts.set {
        if let Some(mut doc) = set::parse_set(arg, diags) {
            resolve_paths(&mut doc.value, None, env);
            docs.push(doc);
        }
    }
    let mut flags = opts.overrides.to_layer();
    resolve_paths(&mut flags, None, env);
    docs.push(Parsed {
        value: flags,
        origin: Origin::Flags,
        src: Cow::Borrowed(""),
    });
    (path, docs)
}

/// Make theme and code-theme paths absolute relative to the file that
/// names them, and expand `~/`.
fn resolve_paths(layer: &mut ConfigLayer, base_dir: Option<&Path>, env: &ConfigEnv) {
    for slot in [&mut layer.theme.name, &mut layer.theme.code] {
        if let Some(s) = slot.as_mut().filter(|s| paths::looks_like_path(s)) {
            *s = paths::resolve_relative(env, s, base_dir)
                .display()
                .to_string();
        }
    }
}

/// The outcome of `--check-config`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckReport {
    /// The config file that was checked, if any.
    pub path: Option<PathBuf>,
    pub diagnostics: Vec<Diagnostic>,
}

impl CheckReport {
    /// No warnings and no errors.
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// The process exit code for `--check-config`: 1 on any problem.
    pub fn exit_code(&self) -> u8 {
        u8::from(!self.is_clean())
    }
}

impl fmt::Display for CheckReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for d in &self.diagnostics {
            writeln!(f, "{d}")?;
        }
        let what = self.path.as_ref().map_or_else(
            || "no config file (built-in defaults)".to_owned(),
            |p| p.display().to_string(),
        );
        let errors = self
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let warnings = self.diagnostics.len() - errors;
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        if self.is_clean() {
            write!(f, "{what}: OK")
        } else {
            write!(
                f,
                "{what}: {errors} error{}, {warnings} warning{}",
                plural(errors),
                plural(warnings)
            )
        }
    }
}

/// Load the configuration for `--check-config` (exit code 1 on any problem).
/// Unlike [`load`], this also reads `.tmTheme` files and looks up
/// `[code.aliases]` targets, so a broken theme file or a misspelt language
/// is reported here rather than only showing as plain code.
pub fn check(opts: &LoadOptions) -> CheckReport {
    let loaded = load_with(opts, Checks::Thorough);
    CheckReport {
        path: loaded.path,
        diagnostics: loaded.diagnostics,
    }
}

/// Build the theme for the terminal.
///
/// `background` is the terminal's background colour if detected; it picks
/// the dark or light variant (for `background = "auto"`) and the page and
/// panel colours. Unless the user chose a theme, emde uses `mono` when the
/// terminal shows no colour ([`ColorDepth::Mono`], [`ColorDepth::None`]) and
/// `ansi` on 16-colour terminals. The user's `[palette]` and `[style.*]`
/// tables apply to whichever theme is used.
pub fn build_theme(config: &Config, background: Option<Rgb>, depth: ColorDepth) -> Theme {
    let variant = config.theme.variant(background);
    let auto = match depth {
        _ if config.theme.explicit => None,
        ColorDepth::None | ColorDepth::Mono => Some(builtin::MONO),
        ColorDepth::Ansi16 => Some(builtin::ANSI),
        ColorDepth::Ansi256 | ColorDepth::TrueColor => None,
    };
    let (name, mut patch) = match (auto, &config.theme_patch) {
        (Some(name), _) if name != config.theme_name => {
            let mut ignored = Vec::new();
            let t = chain::load(name, None, "", &ConfigEnv::default(), &mut ignored);
            (t.name, t.patch)
        }
        (_, Some(patch)) => (config.theme_name.clone(), patch.clone()),
        (_, None) => return with_code_theme(Theme::fallback(variant, background), config),
    };
    patch.overlay(config.user_patch.clone());
    let (theme, _) = crate::theme::build::build(&name, &patch, variant, background);
    with_code_theme(theme, config)
}

/// Apply the `[theme] code` override.
fn with_code_theme(mut theme: Theme, config: &Config) -> Theme {
    if let Some(code) = &config.theme.code {
        theme.code_theme.clone_from(code);
    }
    theme
}

#[cfg(test)]
mod tests;
