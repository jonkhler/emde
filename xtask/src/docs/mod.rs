//! `cargo xtask docs [--check]`: the generated documentation.
//!
//! * `CONFIG.md`, the configuration reference: `docs/CONFIG.template.md`
//!   with the generated parts filled in, namely every setting of
//!   `assets/default.toml` with its default and its comment ([`settings`]),
//!   the theme elements with their parents, the built-in themes and the
//!   default palette ([`theme`]), and the pager's key bindings
//!   (`emde::pager::keymap`).
//! * Two generated blocks of `README.md` (between `BEGIN GENERATED name`
//!   and `END GENERATED name` comments): the key bindings (`keys`), and the
//!   `--doctor` report of the iTerm2 → SSH → tmux session (`doctor`), as the
//!   snapshot test of `src/term/doctor.rs` keeps it.
//! * The man page `$CARGO_TARGET_DIR/man/emde.1` and bash, zsh and fish
//!   completions in `$CARGO_TARGET_DIR/completions/`, from `emde::cli::Cli`
//!   ([`man`]).
//!
//! Every TOML example in `README.md` and `CONFIG.md` must load through
//! emde's own configuration loader without a single warning ([`examples`]),
//! so the documentation cannot describe settings that do not exist.
//!
//! With `--check` nothing is written: the task fails when `CONFIG.md` or a
//! generated block of the README is not exactly what the generator produces
//! (`cargo xtask ci` runs this), and the man page and completions are only
//! rendered in memory.

pub(crate) mod examples;
mod man;
mod markdown;
mod settings;
mod theme;

use std::fs;
use std::path::Path;

use emde::pager::keymap;

/// The configuration reference's template, relative to the repository root.
const CONFIG_TEMPLATE: &str = "docs/CONFIG.template.md";
/// Where the configuration reference goes.
const CONFIG_MD: &str = "CONFIG.md";
/// The README, which holds generated blocks.
const README_MD: &str = "README.md";
/// The generated block of the README that lists the key bindings.
const README_KEYS: &str = "keys";
/// The generated block of the README with a `--doctor` report.
const README_DOCTOR: &str = "doctor";
/// The `--doctor` report shown in the README: the snapshot that the tests of
/// `src/term/doctor.rs` keep of the iTerm2 → SSH → tmux 3.4 session.
const DOCTOR_SNAPSHOT: &str =
    "src/term/snapshots/emde__term__doctor__tests__full__users_session_report.snap";

/// Regenerate the documentation, or with `check` verify it is up to date.
pub(crate) fn run(check: bool) -> Result<(), String> {
    let root = crate::repo_root()?;
    let config_md = config_reference(&root)?;
    let readme_path = root.join(README_MD);
    let doctor = doctor_sample(&root)?;
    let readme = read(&readme_path)?;
    let readme = markdown::replace_generated(&readme, README_KEYS, &keymap::markdown(false))
        .and_then(|r| markdown::replace_generated(&r, README_DOCTOR, &doctor))
        .map_err(|e| format!("{README_MD}: {e}"))?;
    examples::check(README_MD, &readme)?;
    examples::check(CONFIG_MD, &config_md)?;

    let mut stale = Vec::new();
    for (rel, text) in [(CONFIG_MD, &config_md), (README_MD, &readme)] {
        let path = root.join(rel);
        if fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
            continue;
        }
        if check {
            stale.push(rel);
        } else {
            fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            eprintln!("xtask: wrote {rel}");
        }
    }

    if check {
        // Not checked in: only make sure they can still be generated.
        man::man_page()?;
        man::completions()?;
    } else {
        let target = crate::target_dir()?;
        let page = man::write_man_page(&target.join("man"))?;
        eprintln!("xtask: wrote {}", page.display());
        for path in man::write_completions(&target.join("completions"))? {
            eprintln!("xtask: wrote {}", path.display());
        }
    }

    if stale.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "generated documentation is out of date: {} (run `cargo xtask docs`)",
            stale.join(", ")
        ))
    }
}

/// `CONFIG.md`: the template with every `{{name}}` line replaced by the
/// generated part of that name.
fn config_reference(root: &Path) -> Result<String, String> {
    let template = read(&root.join(CONFIG_TEMPLATE))?;
    theme::check_style_fields(&template).map_err(|e| format!("{CONFIG_TEMPLATE}: {e}"))?;
    let default_toml = read(&root.join("assets/default.toml"))?;
    let sections = settings::parse(&default_toml).map_err(|e| format!("default.toml: {e}"))?;
    let themes = theme::Themes::load(root)?;
    let parts = [
        ("settings", settings::markdown(&sections)?),
        ("themes", themes.builtin_markdown()?),
        ("palette", themes.palette_markdown()?),
        ("elements", themes.elements_markdown()?),
        ("keys", keymap::markdown(true)),
    ];
    let body = markdown::fill_template(&template, &parts)
        .map_err(|e| format!("{CONFIG_TEMPLATE}: {e}"))?;
    Ok(format!(
        "<!-- Generated by `cargo xtask docs` from {CONFIG_TEMPLATE}, assets/default.toml, \
         assets/themes/, src/theme/element.rs and src/pager/keymap.rs. Do not edit: \
         change those and run `cargo xtask docs`. -->\n\n{body}"
    ))
}

/// The README's `--doctor` sample: the report of [`DOCTOR_SNAPSHOT`] as a
/// text block.
fn doctor_sample(root: &Path) -> Result<String, String> {
    let snapshot = read(&root.join(DOCTOR_SNAPSHOT))?;
    let report = snapshot_contents(&snapshot)
        .ok_or_else(|| format!("{DOCTOR_SNAPSHOT}: not an insta snapshot"))?;
    Ok(format!("```text\n{}\n```\n", report.trim_end_matches('\n')))
}

/// What an insta snapshot file holds after its `---` header.
fn snapshot_contents(snapshot: &str) -> Option<&str> {
    let rest = snapshot.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    rest.get(end + "\n---\n".len()..)
}

/// Read a UTF-8 file, with the path in the error.
fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test` catches stale documentation too, not only `cargo xtask
    /// ci`: after a change to the key table, the defaults, the theme
    /// elements or the `--doctor` report, run `cargo xtask docs`.
    #[test]
    fn generated_documentation_is_up_to_date() {
        if let Err(e) = run(true) {
            panic!("{e}");
        }
    }

    #[test]
    fn snapshots() {
        let snap = "---\nsource: src/a.rs\nexpression: x\n---\n line one\nline two\n";
        assert_eq!(snapshot_contents(snap), Some(" line one\nline two\n"));
        assert_eq!(snapshot_contents("no header"), None);
        let root = crate::repo_root().unwrap();
        let sample = doctor_sample(&root).unwrap();
        assert!(sample.starts_with("```text\n terminal  "), "{sample}");
        assert!(sample.contains("\n tip       `set -g allow-passthrough on`"));
        assert!(sample.ends_with("\n```\n"));
    }
}
