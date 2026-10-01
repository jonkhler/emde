//! `cargo xtask dist [--target TRIPLE]`: a release archive.
//!
//! Builds the release binary (for the host, or `--target`), then packs
//! `emde-<version>-<target>.tar.gz` under `$CARGO_TARGET_DIR/dist/`:
//!
//! ```text
//! emde-0.1.0-x86_64-unknown-linux-gnu/
//!   emde                    the binary
//!   LICENSE README.md CONFIG.md
//!   man/emde.1
//!   completions/emde.bash completions/_emde completions/emde.fish
//! ```
//!
//! plus `<archive>.sha256` (`<hex>  <file name>`, as `sha256sum -c` reads
//! it). The release workflow uploads both; `install.sh` downloads and
//! checks them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::docs::man;
use crate::{Result, cargo, output, repo_root, run, target_dir};

/// emde's version (the workspace shares one).
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Files copied from the repository root into the archive.
const DOCS: &[&str] = &["LICENSE", "README.md", "CONFIG.md"];

pub(crate) fn run_dist(args: &[String]) -> Result {
    let target = match args.iter().position(|a| a == "--target") {
        Some(i) => Some(
            args.get(i + 1)
                .ok_or("--target needs a target triple")?
                .clone(),
        ),
        None => None,
    };
    let triple = match &target {
        Some(t) => t.clone(),
        None => host_triple()?,
    };

    let mut build = cargo();
    build.args(["build", "--release", "--locked", "--package", "emde"]);
    if let Some(t) = &target {
        build.args(["--target", t]);
    }
    run(&format!("release build ({triple})"), &mut build)?;

    let out = target_dir()?;
    let bin_dir = match &target {
        Some(t) => out.join(t).join("release"),
        None => out.join("release"),
    };
    let name = format!("emde-{VERSION}-{triple}");
    let dist = out.join("dist");
    let stage = dist.join(&name);
    if stage.exists() {
        fs::remove_dir_all(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
    }
    fs::create_dir_all(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;

    copy(&bin_dir.join("emde"), &stage.join("emde"))?;
    let root = repo_root()?;
    for doc in DOCS {
        copy(&root.join(doc), &stage.join(doc))?;
    }
    man::write_man_page(&stage.join("man"))?;
    man::write_completions(&stage.join("completions"))?;

    let archive = dist.join(format!("{name}.tar.gz"));
    run(
        "archive",
        Command::new("tar")
            .arg("-C")
            .arg(&dist)
            .arg("-czf")
            .arg(&archive)
            .arg(&name),
    )?;
    let sum = sha256(&archive)?;
    let file_name = format!("{name}.tar.gz");
    let sum_path = dist.join(format!("{file_name}.sha256"));
    fs::write(&sum_path, format!("{sum}  {file_name}\n"))
        .map_err(|e| format!("{}: {e}", sum_path.display()))?;
    eprintln!("xtask: == dist: {}", archive.display());
    eprintln!("xtask: == sha256: {sum}");
    Ok(())
}

fn copy(from: &Path, to: &Path) -> Result {
    fs::copy(from, to)
        .map(drop)
        .map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))
}

/// The host's target triple (`rustc -vV`).
fn host_triple() -> Result<String> {
    let info = output(Command::new("rustc").arg("-vV"))?;
    info.lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_string)
        .ok_or_else(|| "rustc -vV printed no host triple".to_string())
}

/// SHA-256 of a file as lowercase hex, with `sha256sum` (Linux) or
/// `shasum -a 256` (macOS).
fn sha256(path: &PathBuf) -> Result<String> {
    let line = output(Command::new("sha256sum").arg(path))
        .or_else(|_| output(Command::new("shasum").args(["-a", "256"]).arg(path)))?;
    line.split_whitespace()
        .next()
        .filter(|h| h.len() == 64)
        .map(str::to_string)
        .ok_or_else(|| format!("unexpected checksum output: {line}"))
}
