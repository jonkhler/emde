//! emde — an ultra-fast, lightweight terminal Markdown reader.
//!
//! Pipeline: source → pulldown-cmark events → owned IR (`ir`) →
//! layout(width) → styled lines → stream writer (`render`) or built-in pager.

use std::io::Write as _;
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

/// Entry point used by the binary.
pub fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    if cli.files.is_empty() {
        let _ = writeln!(std::io::stderr(), "emde: no input (rendering lands in M1)");
        return ExitCode::from(2);
    }
    let _ = writeln!(std::io::stderr(), "emde: rendering lands in M1");
    ExitCode::SUCCESS
}
