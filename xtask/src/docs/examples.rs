//! Every TOML example in the documentation must work: it is loaded through
//! emde's configuration loader (`emde::config::check`, the same checks as
//! `emde --check-config`) and must produce no warning and no error.
//!
//! Code theme names and languages are only looked up when emde is built
//! with syntax highlighting: in `cargo test --workspace` (whose emde has
//! its default features), not in `cargo xtask docs`, which builds emde
//! without them (see `xtask/Cargo.toml`).
//!
//! A ```` ```toml ```` block is a config file. A ```` ```toml theme ```` block
//! is a theme file: it is installed as `themes/example.toml` in a scratch
//! configuration directory and selected with `theme.name = "example"`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use emde::config::{self, ConfigEnv, LoadOptions};

/// A fenced code block of a Markdown document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    /// The info string (`toml`, `toml theme`, …).
    pub(crate) info: String,
    /// The line the fence is on (1-based).
    pub(crate) line: usize,
    /// The contents.
    pub(crate) text: String,
}

/// Check every TOML block of the document `name`.
pub(crate) fn check(name: &str, markdown: &str) -> Result<(), String> {
    let scratch = Scratch::new()?;
    for block in blocks(markdown) {
        let mut words = block.info.split_whitespace();
        if words.next() != Some("toml") {
            continue;
        }
        let result = match words.next() {
            None => scratch.check_config(&block.text),
            Some("theme") => scratch.check_theme(&block.text),
            Some(other) => Err(format!("unknown kind of TOML example `{other}`")),
        };
        result.map_err(|e| format!("{name}:{}: the TOML example {e}", block.line))?;
    }
    Ok(())
}

/// The fenced code blocks (```` ``` ```` fences, at any indentation).
pub(crate) fn blocks(markdown: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut open: Option<(Block, usize)> = None;
    for (n, line) in markdown.lines().enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        match &mut open {
            None => {
                if let Some(info) = trimmed.strip_prefix("```") {
                    let block = Block {
                        info: info.trim().to_owned(),
                        line: n + 1,
                        text: String::new(),
                    };
                    open = Some((block, indent));
                }
            }
            Some((block, fence_indent)) => {
                if trimmed.trim_end() == "```" {
                    if let Some((block, _)) = open.take() {
                        out.push(block);
                    }
                } else {
                    let strip = indent.min(*fence_indent);
                    block.text.push_str(line.get(strip..).unwrap_or(line));
                    block.text.push('\n');
                }
            }
        }
    }
    out
}

/// A scratch configuration directory, removed when dropped.
struct Scratch {
    dir: PathBuf,
}

/// Numbers scratch directories, so that several can exist at once.
static SCRATCH_COUNT: AtomicUsize = AtomicUsize::new(0);

impl Scratch {
    fn new() -> Result<Scratch, String> {
        let n = SCRATCH_COUNT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("emde-xtask-docs-{}-{n}", std::process::id()));
        let themes = dir.join("emde").join("themes");
        fs::create_dir_all(&themes).map_err(|e| format!("{}: {e}", themes.display()))?;
        Ok(Scratch { dir })
    }

    /// Where `--config` files and the themes directory are.
    fn env(&self) -> ConfigEnv {
        ConfigEnv {
            emde_config: None,
            xdg_config_home: Some(self.dir.clone()),
            home: None,
        }
    }

    fn write(&self, rel: &Path, text: &str) -> Result<PathBuf, String> {
        let path = self.dir.join(rel);
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }

    /// Load `text` as the config file.
    fn check_config(&self, text: &str) -> Result<(), String> {
        let path = self.write(Path::new("config.toml"), text)?;
        clean(&LoadOptions {
            config: Some(path),
            env: self.env(),
            ..LoadOptions::default()
        })
    }

    /// Load `text` as the theme file `example.toml`, and use it.
    fn check_theme(&self, text: &str) -> Result<(), String> {
        self.write(&Path::new("emde").join("themes").join("example.toml"), text)?;
        clean(&LoadOptions {
            set: vec!["theme.name=example".to_owned()],
            env: self.env(),
            ..LoadOptions::default()
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Fail with every diagnostic the configuration has.
fn clean(opts: &LoadOptions) -> Result<(), String> {
    let report = config::check(opts);
    if report.is_clean() {
        return Ok(());
    }
    let problems: Vec<String> = report.diagnostics.iter().map(ToString::to_string).collect();
    Err(format!("does not load cleanly:\n{}", problems.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_fenced_blocks() {
        let md = "text\n```toml\na = 1\n```\n\n- item\n\n  ```toml theme\n  b = 2\n  ```\n\
                  ```\nplain\n```\n";
        let b = blocks(md);
        assert_eq!(b.len(), 3);
        assert_eq!(
            (b[0].info.as_str(), b[0].line, b[0].text.as_str()),
            ("toml", 2, "a = 1\n")
        );
        assert_eq!(
            (b[1].info.as_str(), b[1].text.as_str()),
            ("toml theme", "b = 2\n")
        );
        assert_eq!(b[2].info, "");
    }

    #[test]
    fn good_and_bad_examples() {
        check("good", "```toml\n[render]\nmax_width = 80\n```\n").unwrap();
        let err = check("bad", "x\n```toml\n[render]\nmax_wdth = 80\n```\n").unwrap_err();
        assert!(err.starts_with("bad:2: "), "{err}");
        assert!(err.contains("max_wdth"), "{err}");
        let theme = "```toml theme\nname = \"mine\"\ninherits = \"emde\"\n\
                     [palette]\naccent = \"#ff8800\"\n```\n";
        check("theme", theme).unwrap();
        let bad_theme = "```toml theme\ninherits = \"nope\"\n```\n";
        assert!(check("bad theme", bad_theme).is_err());
        assert!(check("kind", "```toml other\n```\n").is_err());
        // Other languages are not checked.
        check("other", "```sh\nemde --nope\n```\n").unwrap();
    }
}
