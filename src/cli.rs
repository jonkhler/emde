//! Command-line interface.

use std::path::PathBuf;

use clap::Parser;

/// An ultra-fast terminal Markdown reader.
#[derive(Debug, Parser)]
#[command(name = "emde", version, about)]
pub struct Cli {
    /// Markdown files to show (`-` reads standard input).
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,
}
