//! Command-line interface.
//!
//! The flags of stream mode (`emde [FILE|-]...`). Each flag maps onto
//! [`RenderOptions`] or the terminal decision ([`ColorChoice`], `--hyperlinks`)
//! and only overrides what it names; [`Cli::render_options`] applies them to
//! a base set of options.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use crate::options::{Align, DisplayMath, InlineMath, RenderOptions, When};
use crate::term::color::ColorChoice;

/// An ultra-fast terminal Markdown reader.
#[derive(Debug, Parser)]
#[command(name = "emde", version, about)]
pub struct Cli {
    /// Markdown files to show (`-` reads standard input; a directory shows
    /// its README).
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// Write the document as a stream of styled text (no pager).
    #[arg(short = 'p', long)]
    pub plain: bool,

    /// Total width in columns (default: the terminal's, else $COLUMNS, else 80).
    #[arg(short = 'w', long, value_name = "N")]
    pub width: Option<u16>,

    /// Widest text column (0 = no limit).
    #[arg(short = 'm', long = "max-width", value_name = "N")]
    pub max_width: Option<u16>,

    /// Where the text column goes on a wide terminal.
    #[arg(long, value_enum, value_name = "WHERE")]
    pub align: Option<AlignArg>,

    /// When to use colours, or how many.
    #[arg(long, value_enum, value_name = "WHEN")]
    pub color: Option<ColorArg>,

    /// When to make links clickable (OSC 8).
    #[arg(long, value_enum, value_name = "WHEN")]
    pub hyperlinks: Option<WhenArg>,

    /// When to number links and list their URLs after each section.
    #[arg(long = "link-refs", value_enum, value_name = "WHEN")]
    pub link_refs: Option<WhenArg>,

    /// Draw decorations with ASCII characters only.
    #[arg(long)]
    pub ascii: bool,

    /// How inline math is shown.
    #[arg(long, value_enum, value_name = "HOW")]
    pub math: Option<MathArg>,

    /// How display math is shown.
    #[arg(long = "math-display", value_enum, value_name = "HOW")]
    pub math_display: Option<MathDisplayArg>,

    /// Number the lines of code blocks.
    #[arg(long = "line-numbers")]
    pub line_numbers: bool,

    /// Cut long code lines instead of wrapping them.
    #[arg(long = "no-wrap-code")]
    pub no_wrap_code: bool,

    /// Print the parsed document (`ir`) or the laid-out lines (`lines`).
    #[arg(long, value_enum, value_name = "WHAT", hide = true)]
    pub dump: Option<DumpArg>,
}

/// `--align`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum AlignArg {
    Center,
    Left,
}

/// `--color`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ColorArg {
    Auto,
    Always,
    Never,
    Truecolor,
    #[value(name = "256")]
    Ansi256,
    #[value(name = "16")]
    Ansi16,
}

/// `auto | always | never`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum WhenArg {
    Auto,
    Always,
    Never,
}

/// `--math`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MathArg {
    Unicode,
    Ascii,
    Raw,
}

/// `--math-display`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MathDisplayArg {
    #[value(name = "2d")]
    TwoD,
    Linear,
    Raw,
}

/// `--dump`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DumpArg {
    Ir,
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

impl Cli {
    /// `base` with the flags applied.
    pub fn render_options(&self, base: RenderOptions) -> RenderOptions {
        let mut o = base;
        if let Some(w) = self.width {
            o.width = (w > 0).then_some(w);
        }
        if let Some(m) = self.max_width {
            o.max_width = m;
        }
        if let Some(a) = self.align {
            o.align = match a {
                AlignArg::Center => Align::Center,
                AlignArg::Left => Align::Left,
            };
        }
        if let Some(r) = self.link_refs {
            o.link_refs = r.into();
        }
        if self.ascii {
            o.ascii = true;
        }
        if let Some(m) = self.math {
            o.math.inline = match m {
                MathArg::Unicode => InlineMath::Unicode,
                MathArg::Ascii => InlineMath::Ascii,
                MathArg::Raw => InlineMath::Raw,
            };
        }
        if let Some(d) = self.math_display {
            o.math.display = match d {
                MathDisplayArg::TwoD => DisplayMath::TwoD,
                MathDisplayArg::Linear => DisplayMath::Linear,
                MathDisplayArg::Raw => DisplayMath::Raw,
            };
        }
        if self.line_numbers {
            o.code.line_numbers = true;
        }
        if self.no_wrap_code {
            o.code.wrap = false;
        }
        o
    }

    /// The `--color` choice.
    pub fn color_choice(&self) -> ColorChoice {
        self.color.map_or(ColorChoice::Auto, ColorChoice::from)
    }

    /// The `--hyperlinks` choice.
    pub fn hyperlinks(&self) -> When {
        self.hyperlinks.map_or(When::Auto, When::from)
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
    fn every_flag_maps_onto_options() {
        let cli = parse(&[
            "-p",
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
        assert!(cli.plain);
        assert_eq!(cli.files, [PathBuf::from("a.md"), PathBuf::from("-")]);
        let o = cli.render_options(RenderOptions::default());
        assert_eq!(o.width, Some(60));
        assert_eq!(o.max_width, 0);
        assert_eq!(o.align, Align::Left);
        assert_eq!(o.link_refs, When::Always);
        assert!(o.ascii);
        assert_eq!(o.math.inline, InlineMath::Raw);
        assert_eq!(o.math.display, DisplayMath::Linear);
        assert!(o.code.line_numbers);
        assert!(!o.code.wrap);
        assert_eq!(cli.color_choice(), ColorChoice::Ansi256);
        assert_eq!(cli.hyperlinks(), When::Never);
    }

    #[test]
    fn defaults_change_nothing() {
        let cli = parse(&[]).unwrap();
        assert_eq!(
            cli.render_options(RenderOptions::default()),
            RenderOptions::default()
        );
        assert_eq!(cli.color_choice(), ColorChoice::Auto);
        assert_eq!(cli.hyperlinks(), When::Auto);
        assert_eq!(
            parse(&["-w", "0"])
                .unwrap()
                .render_options(RenderOptions::default())
                .width,
            None
        );
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
            assert_eq!(parse(&["--color", arg]).unwrap().color_choice(), want);
        }
        assert!(parse(&["--color", "8"]).is_err());
    }

    #[test]
    fn math_values_and_dump() {
        assert_eq!(
            parse(&["--math-display", "2d"]).unwrap().math_display,
            Some(MathDisplayArg::TwoD)
        );
        assert_eq!(
            parse(&["--dump", "lines"]).unwrap().dump,
            Some(DumpArg::Lines)
        );
        assert_eq!(parse(&["--dump", "ir"]).unwrap().dump, Some(DumpArg::Ir));
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
