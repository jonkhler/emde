//! emde — an ultra-fast, lightweight terminal Markdown reader.
//!
//! Pipeline: source → pulldown-cmark events → owned IR → layout(width) →
//! styled lines → stream writer or built-in pager.

use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser as _;

pub mod cli;

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
