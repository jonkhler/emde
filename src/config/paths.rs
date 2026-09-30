//! Locating the config file and the themes directory.
//!
//! The config file is, in order: `--config PATH`, `$EMDE_CONFIG`,
//! `$XDG_CONFIG_HOME/emde/config.toml` (when `XDG_CONFIG_HOME` is absolute)
//! and `~/.config/emde/config.toml`, on Linux and macOS alike. Themes live
//! in the `themes` directory next to it.

use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Config and theme files larger than this are refused.
pub(crate) const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The environment used to locate configuration (a snapshot, so tests can
/// supply their own).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigEnv {
    /// `$EMDE_CONFIG`: an explicit config file.
    pub emde_config: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`.
    pub xdg_config_home: Option<PathBuf>,
    /// The home directory (`$HOME`).
    pub home: Option<PathBuf>,
}

fn non_empty(v: Option<OsString>) -> Option<PathBuf> {
    v.filter(|v| !v.is_empty()).map(PathBuf::from)
}

impl ConfigEnv {
    /// Read the variables from the current process.
    pub fn from_process() -> ConfigEnv {
        ConfigEnv {
            emde_config: non_empty(std::env::var_os("EMDE_CONFIG")),
            xdg_config_home: non_empty(std::env::var_os("XDG_CONFIG_HOME")),
            home: non_empty(std::env::var_os("HOME")).or_else(std::env::home_dir),
        }
    }

    /// emde's configuration directories, most specific first.
    pub fn config_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(xdg) = self.xdg_config_home.as_ref().filter(|p| p.is_absolute()) {
            dirs.push(xdg.join("emde"));
        }
        if let Some(home) = &self.home {
            let dir = home.join(".config").join("emde");
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    /// Directories searched for `NAME.toml` themes, most specific first.
    pub fn theme_dirs(&self) -> Vec<PathBuf> {
        self.config_dirs()
            .into_iter()
            .map(|d| d.join("themes"))
            .collect()
    }

    /// Expand a leading `~/` to the home directory.
    pub(crate) fn expand_tilde(&self, path: &str) -> PathBuf {
        match (path.strip_prefix("~/"), &self.home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(path),
        }
    }
}

/// A config file to read, and whether it must exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigFile {
    pub(crate) path: PathBuf,
    /// Named explicitly (`--config`, `$EMDE_CONFIG`): a missing file is an error.
    pub(crate) required: bool,
}

/// The config file to use, if any.
pub(crate) fn find_config(explicit: Option<&Path>, env: &ConfigEnv) -> Option<ConfigFile> {
    if let Some(path) = explicit.or(env.emde_config.as_deref()) {
        return Some(ConfigFile {
            path: path.to_path_buf(),
            required: true,
        });
    }
    env.config_dirs()
        .into_iter()
        .map(|d| d.join("config.toml"))
        .find(|p| p.is_file())
        .map(|path| ConfigFile {
            path,
            required: false,
        })
}

/// Read a file of at most [`MAX_FILE_BYTES`] (so `/dev/zero` or a huge
/// file named by mistake cannot stall emde).
pub(crate) fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("cannot read the file: {e}"))?;
    let mut buf = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read the file: {e}"))?;
    if buf.len() as u64 > MAX_FILE_BYTES {
        return Err(format!(
            "the file is larger than {} KiB",
            MAX_FILE_BYTES / 1024
        ));
    }
    Ok(buf)
}

/// Read a UTF-8 text file of at most [`MAX_FILE_BYTES`].
pub(crate) fn read_text(path: &Path) -> Result<String, String> {
    String::from_utf8(read_bytes(path)?).map_err(|_| "the file is not valid UTF-8".to_owned())
}

/// Whether a theme or code-theme reference is a path rather than a name.
pub(crate) fn looks_like_path(s: &str) -> bool {
    s.contains(['/', '\\'])
        || s.starts_with('~')
        || s.starts_with('.')
        || s.to_ascii_lowercase().ends_with(".toml")
        || s.to_ascii_lowercase().ends_with(".tmtheme")
}

/// Resolve a path written in a file against that file's directory.
pub(crate) fn resolve_relative(env: &ConfigEnv, path: &str, base_dir: Option<&Path>) -> PathBuf {
    let p = env.expand_tilde(path);
    match base_dir {
        Some(dir) if p.is_relative() => dir.join(p),
        _ => p,
    }
}

#[cfg(test)]
pub(crate) mod testdir {
    //! Throw-away directories for tests (no extra dependency needed).

    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A directory removed on drop.
    pub(crate) struct TestDir(PathBuf);

    impl TestDir {
        pub(crate) fn new(tag: &str) -> TestDir {
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("emde-test-{}-{tag}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TestDir(dir)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }

        /// Write a file (creating parent directories) and return its path.
        pub(crate) fn write(&self, rel: &str, contents: &str) -> PathBuf {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, contents).unwrap();
            p
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testdir::TestDir;
    use super::*;

    fn env(dir: &Path, xdg: Option<&str>) -> ConfigEnv {
        ConfigEnv {
            emde_config: None,
            xdg_config_home: xdg.map(|x| {
                if x.starts_with('/') {
                    dir.join(x.trim_start_matches('/'))
                } else {
                    PathBuf::from(x)
                }
            }),
            home: Some(dir.join("home")),
        }
    }

    #[test]
    fn process_environment() {
        let e = ConfigEnv::from_process();
        // Whatever the environment, the result is usable and consistent.
        assert!(
            e.emde_config
                .as_ref()
                .is_none_or(|p| !p.as_os_str().is_empty())
        );
        assert_eq!(e.theme_dirs().len(), e.config_dirs().len());
    }

    #[test]
    fn config_dirs_order() {
        let d = TestDir::new("dirs");
        let e = env(d.path(), Some("/xdg"));
        assert_eq!(
            e.config_dirs(),
            vec![
                d.path().join("xdg/emde"),
                d.path().join("home/.config/emde")
            ]
        );
        assert_eq!(e.theme_dirs()[0], d.path().join("xdg/emde/themes"));
        // A relative XDG_CONFIG_HOME is ignored, as the spec says.
        let e = env(d.path(), Some("relative"));
        assert_eq!(e.config_dirs(), vec![d.path().join("home/.config/emde")]);
        assert!(ConfigEnv::default().config_dirs().is_empty());
    }

    #[test]
    fn discovery_order() {
        let d = TestDir::new("find");
        let e = env(d.path(), Some("/xdg"));
        assert_eq!(find_config(None, &e), None);

        let home_cfg = d.write("home/.config/emde/config.toml", "");
        assert_eq!(find_config(None, &e).map(|f| f.path), Some(home_cfg));

        let xdg_cfg = d.write("xdg/emde/config.toml", "");
        let found = find_config(None, &e).unwrap();
        assert_eq!(found.path, xdg_cfg);
        assert!(!found.required);

        let env_cfg = d.path().join("env.toml");
        let e2 = ConfigEnv {
            emde_config: Some(env_cfg.clone()),
            ..e.clone()
        };
        let found = find_config(None, &e2).unwrap();
        assert_eq!(found.path, env_cfg, "EMDE_CONFIG wins even when missing");
        assert!(found.required);

        let explicit = d.path().join("explicit.toml");
        assert_eq!(
            find_config(Some(&explicit), &e2).map(|f| f.path),
            Some(explicit)
        );
    }

    #[test]
    fn reading() {
        let d = TestDir::new("read");
        let ok = d.write("ok.toml", "a = 1");
        assert_eq!(read_text(&ok).unwrap(), "a = 1");
        assert!(read_text(&d.path().join("missing.toml")).is_err());
        let bad = d.path().join("bad.toml");
        std::fs::write(&bad, [0xff, 0xfe]).unwrap();
        assert_eq!(read_text(&bad).unwrap_err(), "the file is not valid UTF-8");
        let big = d.write("big.toml", &"#".repeat(MAX_FILE_BYTES as usize + 1));
        assert!(read_text(&big).unwrap_err().contains("larger than"));
        assert!(read_text(d.path()).is_err(), "a directory");
        assert_eq!(read_bytes(&bad).unwrap(), [0xff, 0xfe]);
    }

    #[cfg(unix)]
    #[test]
    fn endless_files_are_cut_off() {
        let zero = Path::new("/dev/zero");
        if zero.exists() {
            assert!(read_bytes(zero).unwrap_err().contains("larger than"));
        }
    }

    #[test]
    fn paths() {
        assert!(looks_like_path("themes/x.toml"));
        assert!(looks_like_path("x.toml"));
        assert!(looks_like_path("~/x"));
        assert!(looks_like_path("Dark.tmTheme"));
        assert!(!looks_like_path("nord"));
        assert!(!looks_like_path("Solarized (dark)"));
        let e = ConfigEnv {
            home: Some("/home/u".into()),
            ..ConfigEnv::default()
        };
        assert_eq!(e.expand_tilde("~/t.toml"), PathBuf::from("/home/u/t.toml"));
        assert_eq!(e.expand_tilde("t.toml"), PathBuf::from("t.toml"));
        assert_eq!(
            resolve_relative(&e, "t.toml", Some(Path::new("/cfg"))),
            PathBuf::from("/cfg/t.toml")
        );
        assert_eq!(
            resolve_relative(&e, "/abs/t.toml", Some(Path::new("/cfg"))),
            PathBuf::from("/abs/t.toml")
        );
        assert_eq!(
            resolve_relative(&e, "t.toml", None),
            PathBuf::from("t.toml")
        );
    }
}
