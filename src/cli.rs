//! Command-line interface (`emde [OPTIONS] [FILE[#anchor]|DIR|-]...`).
//!
//! Flags that are settings go through the configuration layers as
//! [`Overrides`] ([`Cli::overrides`]), so a flag beats `--set`, `--set`
//! beats the config file and the file beats the defaults: there is one
//! source of truth for every setting. The other flags choose what to do:
//! show documents (the default), check or print the configuration, list
//! themes and languages, or report on the terminal (`--doctor`).
//!
//! Pager-only requests (`--toc`, `--anchor`, `FILE#anchor`) are carried in
//! [`crate::app::PagerRequest`].

use std::path::{Path, PathBuf};

use clap::{Parser, ValueEnum};

use crate::config::{BackgroundMode, ColorChoice, ConfigEnv, LoadOptions, Overrides};
use crate::options::{Align, BlockGlyphs, DisplayMath, ImageMode, InlineMath, When};

/// An ultra-fast terminal Markdown reader.
#[derive(Debug, Parser)]
#[command(name = "emde", version, about, max_term_width = 100)]
pub struct Cli {
    /// Markdown files to show: `-` reads standard input, `FILE#anchor` opens
    /// the pager at a heading, a directory shows its README.
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// Write the documents as styled text, without the pager.
    #[arg(short = 'p', long, help_heading = "Output")]
    pub plain: bool,

    /// When to use the built-in pager on a terminal (default always; auto:
    /// for documents taller than the screen).
    #[arg(long, value_enum, value_name = "WHEN", help_heading = "Output")]
    pub paging: Option<WhenArg>,

    /// When to use colours, or how many.
    #[arg(long, value_enum, value_name = "WHEN", help_heading = "Output")]
    pub color: Option<ColorArg>,

    /// When to make links clickable (OSC 8).
    #[arg(long, value_enum, value_name = "WHEN", help_heading = "Output")]
    pub hyperlinks: Option<WhenArg>,

    /// When to number links and list their URLs after each section.
    #[arg(
        long = "link-refs",
        value_enum,
        value_name = "WHEN",
        help_heading = "Output"
    )]
    pub link_refs: Option<WhenArg>,

    /// Draw decorations with ASCII characters only.
    #[arg(long, help_heading = "Output")]
    pub ascii: bool,

    /// Total width in columns (default: the terminal's, else $COLUMNS,
    /// else 80; 0 means the same).
    #[arg(short = 'w', long, value_name = "N", help_heading = "Geometry")]
    pub width: Option<u16>,

    /// Widest text column (0: no limit).
    #[arg(
        short = 'm',
        long = "max-width",
        value_name = "N",
        help_heading = "Geometry"
    )]
    pub max_width: Option<u16>,

    /// Where the text column goes on a wide terminal.
    #[arg(long, value_enum, value_name = "WHERE", help_heading = "Geometry")]
    pub align: Option<AlignArg>,

    /// Theme: a built-in one (`--list-themes`), a name from ~/.config/emde/themes, or a path.
    #[arg(short = 't', long, value_name = "NAME|PATH", help_heading = "Theming")]
    pub theme: Option<String>,

    /// Which variant of the theme to use (auto: from the terminal's
    /// background colour).
    #[arg(long, value_enum, value_name = "WHICH", help_heading = "Theming")]
    pub background: Option<BackgroundArg>,

    /// Code theme: a built-in name (see --list-code-themes) or a .tmTheme
    /// path.
    #[arg(
        long = "code-theme",
        value_name = "NAME|PATH",
        help_heading = "Theming"
    )]
    pub code_theme: Option<String>,

    /// How to show images.
    #[arg(long, value_enum, value_name = "HOW", help_heading = "Images")]
    pub images: Option<ImagesArg>,

    /// Glyphs for images drawn as text.
    #[arg(long, value_enum, value_name = "GLYPHS", help_heading = "Images")]
    pub blocks: Option<BlocksArg>,

    /// Fetch http(s) images (with curl).
    #[arg(long = "remote-images", help_heading = "Images")]
    pub remote_images: bool,

    /// How inline math is shown.
    #[arg(long, value_enum, value_name = "HOW", help_heading = "Math")]
    pub math: Option<MathArg>,

    /// How display math is shown.
    #[arg(
        long = "math-display",
        value_enum,
        value_name = "HOW",
        help_heading = "Math"
    )]
    pub math_display: Option<MathDisplayArg>,

    /// Number the lines of code blocks.
    #[arg(long = "line-numbers", help_heading = "Code")]
    pub line_numbers: bool,

    /// Cut long code lines instead of wrapping them.
    #[arg(long = "no-wrap-code", help_heading = "Code")]
    pub no_wrap_code: bool,

    /// Open the pager with the outline shown.
    #[arg(long, help_heading = "Pager")]
    pub toc: bool,

    /// Open the pager at this heading (its slug, as in `FILE#slug`).
    #[arg(long, value_name = "SLUG", help_heading = "Pager")]
    pub anchor: Option<String>,

    /// Read this config file instead of the usual one.
    #[arg(short = 'c', long, value_name = "PATH", help_heading = "Configuration")]
    pub config: Option<PathBuf>,

    /// Read no config file (the built-in defaults, --set and flags only).
    #[arg(long = "no-config", help_heading = "Configuration")]
    pub no_config: bool,

    /// Set a config key, as a line of TOML (`render.max_width=80`,
    /// `theme.code=Nord`); may be repeated.
    #[arg(
        short = 's',
        long = "set",
        value_name = "KEY=VALUE",
        help_heading = "Configuration"
    )]
    pub set: Vec<String>,

    /// Print the default configuration, which documents every key.
    #[arg(long = "print-default-config", help_heading = "Configuration")]
    pub print_default_config: bool,

    /// Report problems in the configuration (exit status 1 if there are any).
    #[arg(long = "check-config", help_heading = "Configuration")]
    pub check_config: bool,

    /// Show what emde detected about the terminal, and why.
    #[arg(
        long,
        value_enum,
        value_name = "FORMAT",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "text",
        help_heading = "Diagnostics"
    )]
    pub doctor: Option<DoctorArg>,

    /// Ask the terminal again instead of using the cached answers.
    #[arg(long, help_heading = "Diagnostics")]
    pub reprobe: bool,

    /// List the available themes.
    #[arg(long = "list-themes", help_heading = "Diagnostics")]
    pub list_themes: bool,

    /// List the built-in code themes.
    #[arg(long = "list-code-themes", help_heading = "Diagnostics")]
    pub list_code_themes: bool,

    /// List the languages code blocks can be highlighted in.
    #[arg(long = "list-languages", help_heading = "Diagnostics")]
    pub list_languages: bool,

    /// Print the licences of the embedded syntaxes and code themes.
    #[arg(long, help_heading = "Diagnostics")]
    pub credits: bool,

    /// Explain every problem in full (configuration, content, images).
    #[arg(short = 'v', long, help_heading = "Diagnostics")]
    pub verbose: bool,

    /// Print the parsed document (`ir`) or the laid-out lines (`lines`).
    #[arg(long, value_enum, value_name = "WHAT", hide = true)]
    pub dump: Option<DumpArg>,
}

/// `--align`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum AlignArg {
    /// Centred when the terminal is wider than the text column.
    Center,
    /// At the left margin.
    Left,
}

/// `--color`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ColorArg {
    /// Detect from the terminal and the environment.
    Auto,
    /// Colour even when not writing to a terminal.
    Always,
    /// No escape sequences at all.
    Never,
    /// 24-bit colour.
    Truecolor,
    /// The xterm 256-colour palette.
    #[value(name = "256")]
    Ansi256,
    /// The 16 ANSI colours.
    #[value(name = "16")]
    Ansi16,
}

/// `auto | always | never`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum WhenArg {
    /// Decide from the terminal.
    Auto,
    /// Always.
    Always,
    /// Never.
    Never,
}

/// `--background`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum BackgroundArg {
    /// From the terminal's background colour (dark when unknown).
    Auto,
    /// The dark variant.
    Dark,
    /// The light variant.
    Light,
}

/// `--images`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ImagesArg {
    /// The best the terminal supports.
    Auto,
    /// kitty graphics (Unicode placeholders where possible).
    Kitty,
    /// iTerm2 inline images.
    Iterm,
    /// DEC sixel.
    Sixel,
    /// Coloured block glyphs (text).
    Blocks,
    /// Only the alt text.
    None,
}

/// `--blocks`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum BlocksArg {
    /// Half blocks (every font has them).
    Half,
    /// Quadrants.
    Quadrant,
    /// Sextants.
    Sextant,
    /// Octants.
    Octant,
    /// Octants or sextants where the terminal draws them itself.
    Auto,
}

/// `--math`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MathArg {
    /// One line of Unicode.
    Unicode,
    /// Plain ASCII.
    Ascii,
    /// The TeX source.
    Raw,
}

/// `--math-display`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MathDisplayArg {
    /// Stacked fractions, limits and tall delimiters.
    #[value(name = "2d")]
    TwoD,
    /// One wrapped line.
    Linear,
    /// The TeX source.
    Raw,
}

/// `--doctor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DoctorArg {
    /// A few lines for people.
    Text,
    /// Every decision, the raw answers and the environment, as JSON.
    Json,
}

/// `--dump`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DumpArg {
    /// The parsed document model.
    Ir,
    /// The laid-out lines.
    Lines,
}

impl From<WhenArg> for When {
    fn from(w: WhenArg) -> When {
        match w {
            WhenArg::Auto => When::Auto,
            WhenArg::Always => When::Always,
            WhenArg::Never => When::Never,
        }
    }
}

impl From<ColorArg> for ColorChoice {
    fn from(c: ColorArg) -> ColorChoice {
        match c {
            ColorArg::Auto => ColorChoice::Auto,
            ColorArg::Always => ColorChoice::Always,
            ColorArg::Never => ColorChoice::Never,
            ColorArg::Truecolor => ColorChoice::TrueColor,
            ColorArg::Ansi256 => ColorChoice::Ansi256,
            ColorArg::Ansi16 => ColorChoice::Ansi16,
        }
    }
}

impl From<AlignArg> for Align {
    fn from(a: AlignArg) -> Align {
        match a {
            AlignArg::Center => Align::Center,
            AlignArg::Left => Align::Left,
        }
    }
}

impl From<BackgroundArg> for BackgroundMode {
    fn from(b: BackgroundArg) -> BackgroundMode {
        match b {
            BackgroundArg::Auto => BackgroundMode::Auto,
            BackgroundArg::Dark => BackgroundMode::Dark,
            BackgroundArg::Light => BackgroundMode::Light,
        }
    }
}

impl From<ImagesArg> for ImageMode {
    fn from(i: ImagesArg) -> ImageMode {
        match i {
            ImagesArg::Auto => ImageMode::Auto,
            ImagesArg::Kitty => ImageMode::Kitty,
            ImagesArg::Iterm => ImageMode::Iterm,
            ImagesArg::Sixel => ImageMode::Sixel,
            ImagesArg::Blocks => ImageMode::Blocks,
            ImagesArg::None => ImageMode::None,
        }
    }
}

impl From<BlocksArg> for BlockGlyphs {
    fn from(b: BlocksArg) -> BlockGlyphs {
        match b {
            BlocksArg::Half => BlockGlyphs::Half,
            BlocksArg::Quadrant => BlockGlyphs::Quadrant,
            BlocksArg::Sextant => BlockGlyphs::Sextant,
            BlocksArg::Octant => BlockGlyphs::Octant,
            BlocksArg::Auto => BlockGlyphs::Auto,
        }
    }
}

impl From<MathArg> for InlineMath {
    fn from(m: MathArg) -> InlineMath {
        match m {
            MathArg::Unicode => InlineMath::Unicode,
            MathArg::Ascii => InlineMath::Ascii,
            MathArg::Raw => InlineMath::Raw,
        }
    }
}

impl From<MathDisplayArg> for DisplayMath {
    fn from(d: MathDisplayArg) -> DisplayMath {
        match d {
            MathDisplayArg::TwoD => DisplayMath::TwoD,
            MathDisplayArg::Linear => DisplayMath::Linear,
            MathDisplayArg::Raw => DisplayMath::Raw,
        }
    }
}

/// `Some(true)` for a switch that was given, `None` (no opinion) otherwise.
fn switch(on: bool) -> Option<bool> {
    on.then_some(true)
}

/// A document to show: a path (or `-`) and the anchor after `#`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileArg {
    /// The file, directory or `-`.
    pub path: PathBuf,
    /// The heading to open at (`FILE#anchor`).
    pub anchor: Option<String>,
}

impl FileArg {
    /// Split `FILE#anchor`. A path that exists as written is never split
    /// (file names may contain `#`); otherwise the text after the last `#`
    /// is the anchor when the part before it exists (or is `-`).
    pub fn parse(arg: &Path) -> FileArg {
        let whole = FileArg {
            path: arg.to_path_buf(),
            anchor: None,
        };
        if arg.exists() {
            return whole;
        }
        let Some(text) = arg.to_str() else {
            return whole;
        };
        match text.rsplit_once('#') {
            Some((file, anchor))
                if !file.is_empty()
                    && !anchor.is_empty()
                    && (file == "-" || Path::new(file).exists()) =>
            {
                FileArg {
                    path: PathBuf::from(file),
                    anchor: Some(anchor.to_owned()),
                }
            }
            _ => whole,
        }
    }
}

impl Cli {
    /// The flags that are settings, as the highest configuration layer.
    /// `--plain` means `--paging=never`.
    pub fn overrides(&self) -> Overrides {
        let paging = if self.plain {
            Some(When::Never)
        } else {
            self.paging.map(When::from)
        };
        Overrides {
            paging,
            width: self.width,
            max_width: self.max_width,
            align: self.align.map(Align::from),
            color: self.color.map(ColorChoice::from),
            hyperlinks: self.hyperlinks.map(When::from),
            link_refs: self.link_refs.map(When::from),
            ascii: switch(self.ascii),
            theme: self.theme.clone(),
            background: self.background.map(BackgroundMode::from),
            code_theme: self.code_theme.clone(),
            images: self.images.map(ImageMode::from),
            blocks: self.blocks.map(BlockGlyphs::from),
            remote_images: switch(self.remote_images),
            math: self.math.map(InlineMath::from),
            math_display: self.math_display.map(DisplayMath::from),
            line_numbers: switch(self.line_numbers),
            wrap_code: self.no_wrap_code.then_some(false),
        }
    }

    /// What configuration to load, looking for files where `env` says.
    pub fn load_options(&self, env: ConfigEnv) -> LoadOptions {
        LoadOptions {
            config: self.config.clone(),
            no_config: self.no_config,
            set: self.set.clone(),
            overrides: self.overrides(),
            env,
        }
    }

    /// The documents to show, with their anchors.
    pub fn file_args(&self) -> Vec<FileArg> {
        self.files.iter().map(|f| FileArg::parse(f)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("emde").chain(args.iter().copied()))
    }

    #[test]
    fn definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_setting_flag_becomes_an_override() {
        let cli = parse(&[
            "--paging",
            "always",
            "-w",
            "60",
            "-m",
            "0",
            "--align",
            "left",
            "--color",
            "256",
            "--hyperlinks",
            "never",
            "--link-refs",
            "always",
            "--ascii",
            "-t",
            "ansi",
            "--background",
            "light",
            "--code-theme",
            "Nord",
            "--images",
            "kitty",
            "--blocks",
            "sextant",
            "--remote-images",
            "--math",
            "raw",
            "--math-display",
            "linear",
            "--line-numbers",
            "--no-wrap-code",
            "a.md",
            "-",
        ])
        .unwrap();
        assert_eq!(cli.files, [PathBuf::from("a.md"), PathBuf::from("-")]);
        assert_eq!(
            cli.overrides(),
            Overrides {
                paging: Some(When::Always),
                width: Some(60),
                max_width: Some(0),
                align: Some(Align::Left),
                color: Some(ColorChoice::Ansi256),
                hyperlinks: Some(When::Never),
                link_refs: Some(When::Always),
                ascii: Some(true),
                theme: Some("ansi".into()),
                background: Some(BackgroundMode::Light),
                code_theme: Some("Nord".into()),
                images: Some(ImageMode::Kitty),
                blocks: Some(BlockGlyphs::Sextant),
                remote_images: Some(true),
                math: Some(InlineMath::Raw),
                math_display: Some(DisplayMath::Linear),
                line_numbers: Some(true),
                wrap_code: Some(false),
            }
        );
    }

    #[test]
    fn no_flags_override_nothing() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.overrides(), Overrides::default());
        assert!(!cli.verbose && !cli.toc && cli.doctor.is_none());
    }

    #[test]
    fn plain_means_no_pager() {
        let cli = parse(&["-p", "--paging", "always"]).unwrap();
        assert_eq!(cli.overrides().paging, Some(When::Never));
        assert_eq!(
            parse(&["--plain"]).unwrap().overrides().paging,
            Some(When::Never)
        );
    }

    #[test]
    fn config_flags() {
        let cli = parse(&[
            "-c",
            "my.toml",
            "--no-config",
            "-s",
            "render.margin=4",
            "--set",
            "theme.code=Nord",
            "--color=never",
        ])
        .unwrap();
        let load = cli.load_options(ConfigEnv::default());
        assert_eq!(load.config, Some(PathBuf::from("my.toml")));
        assert!(load.no_config);
        assert_eq!(load.set, ["render.margin=4", "theme.code=Nord"]);
        assert_eq!(load.overrides.color, Some(ColorChoice::Never));
        assert_eq!(load.env, ConfigEnv::default());
    }

    #[test]
    fn colour_values() {
        for (arg, want) in [
            ("auto", ColorChoice::Auto),
            ("always", ColorChoice::Always),
            ("never", ColorChoice::Never),
            ("truecolor", ColorChoice::TrueColor),
            ("256", ColorChoice::Ansi256),
            ("16", ColorChoice::Ansi16),
        ] {
            let cli = parse(&["--color", arg]).unwrap();
            assert_eq!(cli.overrides().color, Some(want));
        }
        assert!(parse(&["--color", "8"]).is_err());
    }

    #[test]
    fn doctor_takes_an_optional_format() {
        assert_eq!(parse(&["--doctor"]).unwrap().doctor, Some(DoctorArg::Text));
        assert_eq!(
            parse(&["--doctor=json"]).unwrap().doctor,
            Some(DoctorArg::Json)
        );
        // Without `=`, the next word is a file, not the format.
        let cli = parse(&["--doctor", "json"]).unwrap();
        assert_eq!(cli.doctor, Some(DoctorArg::Text));
        assert_eq!(cli.files, [PathBuf::from("json")]);
        assert!(parse(&["--doctor=xml"]).is_err());
    }

    #[test]
    fn diagnostics_and_pager_flags() {
        let cli = parse(&[
            "--reprobe",
            "--list-themes",
            "--list-code-themes",
            "--list-languages",
            "--credits",
            "--print-default-config",
            "--check-config",
            "--toc",
            "--anchor",
            "install",
            "-v",
        ])
        .unwrap();
        assert!(cli.reprobe && cli.list_themes && cli.list_code_themes);
        assert!(cli.list_languages && cli.credits && cli.print_default_config);
        assert!(cli.check_config && cli.toc && cli.verbose);
        assert_eq!(cli.anchor.as_deref(), Some("install"));
    }

    #[test]
    fn dump_values() {
        assert_eq!(
            parse(&["--dump", "lines"]).unwrap().dump,
            Some(DumpArg::Lines)
        );
        assert_eq!(parse(&["--dump", "ir"]).unwrap().dump, Some(DumpArg::Ir));
    }

    #[test]
    fn file_anchors() {
        let dir = std::env::temp_dir().join(format!("emde-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let doc = dir.join("doc.md");
        std::fs::write(&doc, "# x").unwrap();
        let hashed = dir.join("odd#name.md");
        std::fs::write(&hashed, "# y").unwrap();
        let arg = |s: &str| FileArg::parse(Path::new(s));
        let with_anchor = format!("{}#install", doc.display());
        assert_eq!(
            arg(&with_anchor),
            FileArg {
                path: doc.clone(),
                anchor: Some("install".into())
            }
        );
        // An existing file name with `#` is taken as it is.
        let odd = hashed.display().to_string();
        assert_eq!(arg(&odd).anchor, None);
        // Nothing to split, or nothing that exists before the `#`.
        assert_eq!(arg("missing.md#x").path, PathBuf::from("missing.md#x"));
        assert_eq!(arg(&format!("{}#", doc.display())).anchor, None);
        assert_eq!(arg("-#intro").anchor.as_deref(), Some("intro"));
        assert_eq!(arg("-").path, PathBuf::from("-"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_errors_exit_with_two() {
        let err = parse(&["--no-such-flag"]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        let err = parse(&["-w", "wide"]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        let help = parse(&["--help"]).unwrap_err();
        assert_eq!(help.exit_code(), 0);
    }
}
