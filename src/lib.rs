//! emde — an ultra-fast, lightweight terminal Markdown reader.
//!
//! Pipeline: source → pulldown-cmark events → owned IR (`ir`) →
//! layout(width) → styled lines → stream writer (`render`) or built-in pager.

use std::process::ExitCode;

use clap::Parser as _;

pub mod app;
pub mod cli;
pub mod color;
pub mod config;
pub mod gfx;
pub mod highlight;
pub mod ir;
pub mod layout;
pub mod options;
pub mod panic;
pub mod parse;
pub mod render;
pub mod source;
pub mod style;
pub mod term;
pub mod text;
pub mod theme;

/// Entry point used by the binary: parse the arguments (usage errors exit
/// with status 2, `--help` and `--version` with 0) and run.
pub fn main() -> ExitCode {
    let cli = match cli::Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let code = e.exit_code();
            let _ = e.print();
            return ExitCode::from(u8::try_from(code).unwrap_or(2));
        }
    };
    app::run(&cli)
}
