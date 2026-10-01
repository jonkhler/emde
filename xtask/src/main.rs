//! Development tasks for emde: `cargo xtask <task>`.
//!
//! * `ci [--full]` – formatting, lints, tests, the documentation (up to date,
//!   and rustdoc without warnings), dependency guards, cargo-deny, MSRV check
//!   and the binary size budget (`--full` adds a feature powerset check).
//! * `deps` – fail if a banned crate (e.g. the C Oniguruma binding) is in the tree.
//! * `size` – build the release binary and enforce the size budget.
//! * `gen [--check]` – regenerate checked-in data tables (math symbols,
//!   kitty diacritics, block glyphs) from pinned Unicode data.
//! * `docs [--check]` – regenerate `CONFIG.md` and the README's key table,
//!   and write the man page and shell completions into the target directory
//!   (see [`docs`]).
//! * `fuzz [--secs N] [--jobs N] [--asan] [TARGET...]` – run the fuzz
//!   targets of `fuzz/` (see [`fuzz`]; needs nightly and cargo-fuzz).

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod docs;
mod fuzz;
mod gen_gfx;
mod gen_math;

/// Soft target for the stripped release binary (default features).
const SIZE_TARGET: u64 = 6 * 1024 * 1024 + 512 * 1024;
/// Hard cap enforced by CI.
const SIZE_CAP: u64 = 7 * 1024 * 1024;
/// Minimum supported Rust version (icy_sixel -> quantette sets this floor).
const MSRV: &str = "1.90";
/// Crates that must never appear in the dependency graph, for any feature set.
const BANNED: &[&str] = &[
    "openssl-sys",
    "ratatui",
    "tokio",
    "serde_yaml",
    "serde_yml",
    "yaml-rust",
    "ansi_colours",
];
/// Feature sets checked by the dependency guard.
const FEATURE_SETS: &[&[&str]] = &[&[], &["--all-features"], &["--no-default-features"]];
/// The pure-Rust build: every default feature, but the `fancy` regex engine.
const PURE_FEATURES: &[&str] = &[
    "--no-default-features",
    "--features",
    "highlight,fancy,tmtheme,images,sixel,simd",
];
/// Crates (C code) that must not appear in the pure-Rust build.
const NOT_IN_PURE: &[&str] = &["onig", "onig_sys"];

type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let task = args.first().map(String::as_str).unwrap_or("help");
    let result = match task {
        "ci" => ci(args.iter().any(|a| a == "--full")),
        "deps" => deps(),
        "size" => size(),
        "gen" => {
            let check = args.iter().any(|a| a == "--check");
            gen_math::run(check).and_then(|()| gen_gfx::run(check))
        }
        "docs" => docs::run(args.iter().any(|a| a == "--check")),
        "fuzz" => fuzz::run(&args[1..]),
        _ => {
            println!(
                "usage: cargo xtask <ci [--full] | deps | size | gen [--check] | docs [--check] \
                 | fuzz [--secs N] [--jobs N] [--asan] [TARGET...]>"
            );
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cargo() -> Command {
    Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
}

/// The repository root (the parent of the xtask crate).
fn repo_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot locate the repository root".to_string())
}

fn run(step: &str, cmd: &mut Command) -> Result {
    eprintln!("xtask: == {step}");
    let status = cmd
        .status()
        .map_err(|e| format!("{step}: cannot run: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{step} failed ({status})"))
    }
}

fn output(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {cmd:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{cmd:?} failed ({}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

fn ci(full: bool) -> Result {
    run("fmt", cargo().args(["fmt", "--all", "--check"]))?;
    run(
        "clippy (default features)",
        cargo().args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ]),
    )?;
    run(
        "clippy (no default features)",
        cargo().args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--no-default-features",
            "--",
            "-D",
            "warnings",
        ]),
    )?;
    run("test", cargo().args(["test", "--workspace", "--quiet"]))?;
    eprintln!("xtask: == docs up to date");
    docs::run(true)?;
    run(
        "rustdoc",
        cargo()
            .args(["doc", "--workspace", "--no-deps", "--quiet"])
            .env("RUSTDOCFLAGS", "-D warnings"),
    )?;
    deps()?;
    run(
        "cargo deny",
        cargo().args(["deny", "--log-level", "error", "check"]),
    )?;
    msrv()?;
    if full {
        run(
            "feature powerset",
            cargo().args([
                "hack",
                "check",
                "--package",
                "emde",
                "--feature-powerset",
                "--depth",
                "2",
                // `highlight` needs one of the two regex engines.
                "--at-least-one-of",
                "onig,fancy",
                "--no-dev-deps",
            ]),
        )?;
    }
    size()
}

/// Banned-crate guard. `cargo tree -i <crate>` exits 101 when the crate is
/// absent, which is indistinguishable from other failures, so list the whole
/// resolved tree per feature set and search it instead.
fn deps() -> Result {
    for feats in FEATURE_SETS {
        let label = if feats.is_empty() {
            "default"
        } else {
            feats[0]
        };
        guard(label, feats, BANNED)?;
    }
    guard("pure Rust", PURE_FEATURES, NOT_IN_PURE)
}

/// Fail if any crate of `banned` is in the dependency tree for `feats`.
fn guard(label: &str, feats: &[&str], banned: &[&str]) -> Result {
    eprintln!("xtask: == dependency guard ({label})");
    let tree = output(
        cargo()
            .args([
                "tree",
                "--workspace",
                "--target",
                "all",
                "-e",
                "normal,build",
                "--prefix",
                "none",
                "-f",
                "{p}",
            ])
            .args(feats),
    )?;
    let mut hits: Vec<&str> = tree
        .lines()
        .filter(|line| {
            let name = line.split_whitespace().next().unwrap_or("");
            banned.contains(&name)
        })
        .collect();
    hits.sort_unstable();
    hits.dedup();
    if hits.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "banned crate(s) in the {label} dependency tree: {hits:?} \
             (inspect with `cargo tree -e features -i <crate> {}`)",
            feats.join(" ")
        ))
    }
}

fn msrv() -> Result {
    // `$CARGO` is the toolchain's cargo binary, not the rustup proxy, so
    // `cargo +1.90` does not work here; go through `rustup run` instead.
    let installed = Command::new("rustup")
        .args(["run", MSRV, "rustc", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !installed {
        return Err(format!(
            "MSRV toolchain {MSRV} missing: `rustup toolchain install {MSRV} --profile minimal`"
        ));
    }
    run(
        &format!("MSRV {MSRV} check"),
        Command::new("rustup")
            .args(["run", MSRV, "cargo", "check"])
            .args(["--workspace", "--all-features", "--locked"])
            .env_remove("RUSTUP_TOOLCHAIN"),
    )
}

fn target_dir() -> Result<PathBuf> {
    if let Ok(dir) = env::var("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let meta = output(cargo().args(["metadata", "--format-version", "1", "--no-deps"]))?;
    let key = "\"target_directory\":\"";
    let start = meta
        .find(key)
        .ok_or("no target_directory in cargo metadata")?
        + key.len();
    let end = meta[start..].find('"').ok_or("malformed cargo metadata")? + start;
    Ok(PathBuf::from(&meta[start..end]))
}

fn size() -> Result {
    run(
        "release build",
        cargo().args(["build", "--release", "--package", "emde", "--quiet"]),
    )?;
    let bin = target_dir()?.join("release").join("emde");
    let bytes = std::fs::metadata(&bin)
        .map_err(|e| format!("{}: {e}", bin.display()))?
        .len();
    let mib = bytes as f64 / (1024.0 * 1024.0);
    eprintln!(
        "xtask: == size: {} is {bytes} bytes ({mib:.2} MiB)",
        bin.display()
    );
    if bytes > SIZE_CAP {
        return Err(format!(
            "binary exceeds the {} MiB cap",
            SIZE_CAP / (1024 * 1024)
        ));
    }
    if bytes > SIZE_TARGET {
        eprintln!("xtask: warning: binary is above the 6.5 MiB target");
    }
    Ok(())
}
