//! Orchestration: CLI → source → parse → capabilities → layout → sink.
//!
//! Stream mode: every input is read, parsed, laid out for the terminal (or
//! `--width`, `$COLUMNS`, 80) and written to standard output, one after
//! another. A reader that goes away (`| head`) ends the run quietly.
//!
//! Exit status: 0 on success (a closed pipe included), 1 when an input
//! could not be read (the other inputs are still shown), 2 for usage
//! errors (bad flags, no input at all).

use std::io::{self, IsTerminal as _, Write as _};
use std::path::Path;
use std::process::ExitCode;

use crate::cli::{Cli, DumpArg};
use crate::highlight::PlainHighlighter;
use crate::layout::{NoImages, layout};
use crate::options::{Height, RenderOptions};
use crate::parse::{ParseOptions, parse_source};
use crate::render::{RenderConfig, StreamSink, is_broken_pipe};
use crate::source::{Input, Source, SourceError};
use crate::term::env::Env;
use crate::term::{Caps, color};
use crate::theme::{Theme, Variant};

/// Width when nothing says otherwise.
const DEFAULT_WIDTH: u16 = 80;

/// Tallest figure in stream mode.
const STREAM_IMAGE_ROWS: u16 = 30;

/// Run emde with parsed arguments.
pub fn run(cli: &Cli) -> ExitCode {
    // Stream mode leaves the terminal as it found it: nothing to restore.
    crate::panic::install_hook(|| {});
    let env = Env::from_process();
    let is_tty = io::stdout().is_terminal();
    let opts = stream_options(cli.render_options(RenderOptions::default()));
    let mut caps = color::decide(&env, is_tty, cli.color_choice(), cli.hyperlinks());
    let width = terminal_width(opts.width, is_tty, &env, &mut caps);
    let theme = Theme::fallback(Variant::Dark, None);
    let parse_opts = ParseOptions::from(&opts);

    let args: Vec<Option<&Path>> = if cli.files.is_empty() {
        vec![None]
    } else {
        cli.files.iter().map(|p| Some(p.as_path())).collect()
    };
    let mut sink = StreamSink::new(io::stdout().lock(), RenderConfig::from_caps(&caps));
    let mut status = ExitCode::SUCCESS;
    for arg in args {
        let input = match Input::from_arg(arg) {
            Ok(input) => input,
            Err(e) => {
                error(&e);
                return ExitCode::from(2);
            }
        };
        let source = match Source::load(&input) {
            Ok(source) => source,
            Err(e @ SourceError::NoInput) => {
                error(&e);
                return ExitCode::from(2);
            }
            Err(e) => {
                error(&e);
                status = ExitCode::from(1);
                continue;
            }
        };
        let doc = parse_source(&source, &parse_opts);
        let written = match cli.dump {
            Some(DumpArg::Ir) => sink.write_text(&doc.dump()),
            Some(DumpArg::Lines) => {
                let l = layout(
                    &doc,
                    width,
                    &theme,
                    &caps,
                    &opts,
                    &PlainHighlighter,
                    &NoImages,
                );
                sink.write_text(&l.dump())
            }
            None => {
                let l = layout(
                    &doc,
                    width,
                    &theme,
                    &caps,
                    &opts,
                    &PlainHighlighter,
                    &NoImages,
                );
                sink.write_document(&doc, &l)
            }
        };
        if let Err(e) = written {
            return write_failed(&e);
        }
    }
    match sink.finish() {
        Ok(()) => status,
        Err(e) => write_failed(&e),
    }
}

/// Stream-mode adjustments: figures are at most 30 rows tall.
fn stream_options(mut opts: RenderOptions) -> RenderOptions {
    opts.images.max_height = match opts.images.max_height {
        Height::Rows(n) => Height::Rows(n.min(STREAM_IMAGE_ROWS)),
        Height::Percent(_) => Height::Rows(STREAM_IMAGE_ROWS),
    };
    opts
}

/// The total width: `--width`, else the terminal's (recorded in
/// `caps.size`), else `$COLUMNS`, else 80.
fn terminal_width(forced: Option<u16>, is_tty: bool, env: &Env, caps: &mut Caps) -> u16 {
    if is_tty && let Ok((cols, rows)) = crossterm::terminal::size() {
        caps.size = Some((cols, rows));
        if forced.is_none() && cols > 0 {
            return cols;
        }
    }
    forced
        .filter(|&w| w > 0)
        .or_else(|| columns(env))
        .unwrap_or(DEFAULT_WIDTH)
}

/// `$COLUMNS`, if it is a positive number.
fn columns(env: &Env) -> Option<u16> {
    env.non_empty("COLUMNS")?
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|&w| w > 0)
}

/// Print an error on standard error.
fn error(e: &dyn std::fmt::Display) {
    let _ = writeln!(io::stderr(), "emde: {e}");
}

/// The exit status after a failed write: a closed pipe is a normal end.
fn write_failed(e: &io::Error) -> ExitCode {
    if is_broken_pipe(e) {
        ExitCode::SUCCESS
    } else {
        error(e);
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_precedence() {
        let env = Env::from_pairs(&[("COLUMNS", "132")]);
        let mut caps = Caps::plain();
        assert_eq!(terminal_width(Some(60), false, &env, &mut caps), 60);
        assert_eq!(terminal_width(None, false, &env, &mut caps), 132);
        assert_eq!(terminal_width(None, false, &Env::default(), &mut caps), 80);
        let bad = Env::from_pairs(&[("COLUMNS", "wide")]);
        assert_eq!(terminal_width(None, false, &bad, &mut caps), 80);
        let zero = Env::from_pairs(&[("COLUMNS", "0")]);
        assert_eq!(terminal_width(Some(0), false, &zero, &mut caps), 80);
        assert_eq!(caps.size, None, "no terminal size without a terminal");
    }

    #[test]
    fn stream_mode_caps_figures() {
        let o = stream_options(RenderOptions::default());
        assert_eq!(o.images.max_height, Height::Rows(30));
        let mut tall = RenderOptions::default();
        tall.images.max_height = Height::Rows(50);
        assert_eq!(stream_options(tall).images.max_height, Height::Rows(30));
        let mut short = RenderOptions::default();
        short.images.max_height = Height::Rows(5);
        assert_eq!(stream_options(short).images.max_height, Height::Rows(5));
    }

    #[test]
    fn broken_pipe_is_success() {
        let e = io::Error::from(io::ErrorKind::BrokenPipe);
        assert_eq!(write_failed(&e), ExitCode::SUCCESS);
    }
}
