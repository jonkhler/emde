//! Finding themes and following `inherits`.
//!
//! A theme reference is a built-in name (`emde`, `nord`, …), the name
//! of `NAME.toml` in a themes directory, or a path (relative paths are
//! relative to the file that names them). A theme's `inherits` chain may be
//! at most [`MAX_DEPTH`] themes long and must not loop; the files are merged
//! from the root down, each one's element styles replacing its parent's.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use super::builtin;
use super::spec::{ThemeFile, ThemePatch};
use crate::config::de::{Origin, Parsed, Schema, parse};
use crate::config::paths::{ConfigEnv, looks_like_path, read_text, resolve_relative};
use crate::config::suggest::did_you_mean;
use crate::config::{Diagnostic, Severity};

/// Maximum number of themes in an `inherits` chain.
pub const MAX_DEPTH: usize = 8;

/// Where a theme comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThemeRef {
    /// A built-in theme: name and TOML source.
    Builtin(&'static str, &'static str),
    /// A theme file.
    File(PathBuf),
}

impl ThemeRef {
    /// Identity for loop detection.
    fn identity(&self) -> String {
        match self {
            ThemeRef::Builtin(name, _) => format!("builtin:{name}"),
            ThemeRef::File(p) => std::fs::canonicalize(p)
                .unwrap_or_else(|_| p.clone())
                .display()
                .to_string(),
        }
    }

    /// The name used when the theme file has no `name`.
    fn default_name(&self) -> String {
        match self {
            ThemeRef::Builtin(name, _) => (*name).to_owned(),
            ThemeRef::File(p) => p.file_stem().map_or_else(
                || p.display().to_string(),
                |s| s.to_string_lossy().into_owned(),
            ),
        }
    }
}

/// A theme with its whole `inherits` chain merged.
#[derive(Clone, Debug)]
pub(crate) struct LoadedTheme {
    /// Display name (the first theme's `name`, else the name it was found by).
    pub(crate) name: String,
    /// The merged theme data.
    pub(crate) patch: ThemePatch,
    /// Every file of the chain, the requested theme first.
    pub(crate) docs: Vec<Parsed<ThemeFile>>,
    /// The requested theme could not be used; this is the built-in default.
    pub(crate) fell_back: bool,
}

/// Names of the themes available in the themes directories.
fn installed(env: &ConfigEnv) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for dir in env.theme_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut found: Vec<(String, PathBuf)> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml") && p.is_file())
            .filter_map(|p| Some((p.file_stem()?.to_str()?.to_owned(), p)))
            .collect();
        found.sort();
        for (name, path) in found {
            if !out.iter().any(|(n, _)| *n == name) {
                out.push((name, path));
            }
        }
    }
    out
}

/// A theme available to `--list-themes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeInfo {
    pub name: String,
    /// The file, or `None` for a built-in theme.
    pub path: Option<PathBuf>,
}

/// Built-in themes, then the files in the themes directories (a file with
/// a built-in's name is shadowed by the built-in).
pub fn list_themes(env: &ConfigEnv) -> Vec<ThemeInfo> {
    let mut out: Vec<ThemeInfo> = builtin::names()
        .map(|n| ThemeInfo {
            name: n.to_owned(),
            path: None,
        })
        .collect();
    for (name, path) in installed(env) {
        if !out.iter().any(|t| t.name == name) {
            out.push(ThemeInfo {
                name,
                path: Some(path),
            });
        }
    }
    out
}

/// Resolve a theme reference.
pub(crate) fn find(
    spec: &str,
    base_dir: Option<&Path>,
    env: &ConfigEnv,
) -> Result<ThemeRef, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty theme name".to_owned());
    }
    if let Some((name, src)) = builtin::get(spec) {
        return Ok(ThemeRef::Builtin(name, src));
    }
    if looks_like_path(spec) {
        return Ok(ThemeRef::File(resolve_relative(env, spec, base_dir)));
    }
    let installed = installed(env);
    if let Some((_, path)) = installed.iter().find(|(n, _)| n == spec) {
        return Ok(ThemeRef::File(path.clone()));
    }
    let mut names: Vec<&str> = builtin::names().collect();
    names.extend(installed.iter().map(|(n, _)| n.as_str()));
    let mut msg = format!("unknown theme `{spec}`");
    if let Some(s) = did_you_mean(spec, names.iter().copied()) {
        msg.push_str(&format!(" (did you mean `{s}`?)"));
    } else {
        msg.push_str(&format!(" (available: {})", names.join(", ")));
    }
    Err(msg)
}

/// Read one theme file.
fn read(
    theme: &ThemeRef,
    env: &ConfigEnv,
    diags: &mut Vec<Diagnostic>,
) -> Result<Parsed<ThemeFile>, String> {
    let (src, origin, dir) = match theme {
        ThemeRef::Builtin(name, src) => (Cow::Borrowed(*src), Origin::BuiltinTheme(name), None),
        ThemeRef::File(path) => {
            let text = read_text(path).map_err(|e| format!("theme {}: {e}", path.display()))?;
            (
                Cow::Owned(text),
                Origin::File(path.clone()),
                path.parent().map(Path::to_path_buf),
            )
        }
    };
    let mut parsed = parse::<ThemeFile>(src, origin, Schema::Theme, diags);
    // Code theme paths are relative to the theme file.
    if let Some(dir) = dir {
        use super::spec::CodeThemeSpec;
        let fix = |s: &mut String| {
            if looks_like_path(s) {
                *s = resolve_relative(env, s, Some(&dir)).display().to_string();
            }
        };
        match &mut parsed.value.code {
            Some(CodeThemeSpec::Both(s)) => fix(s),
            Some(CodeThemeSpec::PerVariant { dark, light }) => {
                dark.iter_mut().chain(light.iter_mut()).for_each(fix);
            }
            None => {}
        }
    }
    Ok(parsed)
}

/// Load a theme and its `inherits` chain.
///
/// `requested_at` locates the setting that named the theme (for messages).
/// A theme that cannot be found or read falls back to the built-in default;
/// a broken `inherits` link is reported and the chain is cut there.
pub(crate) fn load(
    spec: &str,
    base_dir: Option<&Path>,
    requested_at: &str,
    env: &ConfigEnv,
    diags: &mut Vec<Diagnostic>,
) -> LoadedTheme {
    if let Some(theme) = load_chain(spec, base_dir, requested_at, env, diags) {
        return theme;
    }
    // The built-in default always loads; an empty theme is the last resort.
    let default = load_chain(builtin::DEFAULT, None, requested_at, env, diags);
    LoadedTheme {
        fell_back: true,
        ..default.unwrap_or_else(|| LoadedTheme {
            name: builtin::DEFAULT.to_owned(),
            patch: ThemePatch::default(),
            docs: Vec::new(),
            fell_back: true,
        })
    }
}

/// Follow a theme's `inherits` chain; `None` when the theme itself cannot
/// be used.
fn load_chain(
    spec: &str,
    base_dir: Option<&Path>,
    requested_at: &str,
    env: &ConfigEnv,
    diags: &mut Vec<Diagnostic>,
) -> Option<LoadedTheme> {
    let mut docs: Vec<Parsed<ThemeFile>> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut first_ref = None;
    let mut next = Some((spec.to_owned(), base_dir.map(Path::to_path_buf)));
    while let Some((spec, dir)) = next.take() {
        // Where the reference was written: the config, or the inheriting theme.
        let at = |docs: &[Parsed<ThemeFile>]| {
            docs.last()
                .map_or_else(|| requested_at.to_owned(), |d| d.location(&["inherits"]))
        };
        // What happens when this link cannot be used.
        let consequence = if docs.is_empty() {
            format!("using the built-in `{}` theme", builtin::DEFAULT)
        } else {
            "ignoring `inherits`".to_owned()
        };
        let theme = match find(&spec, dir.as_deref(), env) {
            Ok(t) => t,
            Err(msg) => {
                diags.push(error(at(&docs), format!("{msg}; {consequence}")));
                break;
            }
        };
        let identity = theme.identity();
        if seen.iter().any(|(id, _)| *id == identity) {
            let chain: Vec<&str> = seen
                .iter()
                .map(|(_, n)| n.as_str())
                .chain([spec.as_str()])
                .collect();
            diags.push(error(
                at(&docs),
                format!(
                    "theme inheritance loops: {}; ignoring `inherits`",
                    chain.join(" → ")
                ),
            ));
            break;
        }
        if docs.len() >= MAX_DEPTH {
            diags.push(error(
                at(&docs),
                format!("theme inheritance is deeper than {MAX_DEPTH} themes; ignoring `inherits = \"{spec}\"`"),
            ));
            break;
        }
        let doc = match read(&theme, env, diags) {
            Ok(doc) => doc,
            Err(msg) => {
                diags.push(error(at(&docs), format!("{msg}; {consequence}")));
                break;
            }
        };
        let parent_dir = match &theme {
            ThemeRef::File(p) => p.parent().map(Path::to_path_buf),
            ThemeRef::Builtin(..) => None,
        };
        next = doc.value.inherits.clone().map(|s| (s, parent_dir));
        seen.push((identity, spec));
        first_ref.get_or_insert(theme);
        docs.push(doc);
    }

    let first = first_ref?;
    let mut patch = ThemePatch::default();
    for doc in docs.iter().rev() {
        patch.overlay(ThemePatch::from_theme_file(&doc.value));
    }
    let name = docs
        .first()
        .and_then(|d| d.value.name.clone())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| first.default_name());
    Some(LoadedTheme {
        name,
        patch,
        docs,
        fell_back: false,
    })
}

fn error(location: String, message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        location,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::paths::testdir::TestDir;
    use crate::style::Rgb;
    use crate::theme::Variant;
    use crate::theme::color::ColorSpec;
    use crate::theme::element::Element;

    fn env_for(dir: &TestDir) -> ConfigEnv {
        ConfigEnv {
            emde_config: None,
            xdg_config_home: None,
            home: Some(dir.path().to_path_buf()),
        }
    }

    fn load_ok(spec: &str, env: &ConfigEnv) -> LoadedTheme {
        let mut diags = Vec::new();
        let t = load(spec, None, "test", env, &mut diags);
        assert!(diags.is_empty(), "{diags:?}");
        t
    }

    #[test]
    fn builtins_load_cleanly() {
        let env = ConfigEnv::default();
        for name in builtin::names() {
            let t = load_ok(name, &env);
            assert_eq!(t.name, name);
            assert!(!t.fell_back);
            // `emde`, `ansi` and `mono` stand alone; the others inherit `emde`.
            let standalone = [builtin::DEFAULT, builtin::ANSI, builtin::MONO].contains(&name);
            assert_eq!(t.docs.len(), if standalone { 1 } else { 2 }, "{name}");
            for doc in &t.docs[1..] {
                assert_eq!(doc.value.name.as_deref(), Some(builtin::DEFAULT), "{name}");
            }
        }
    }

    #[test]
    fn themes_directory_and_paths() {
        let d = TestDir::new("chain-dir");
        let env = env_for(&d);
        d.write(
            ".config/emde/themes/nordish.toml",
            "name = \"Nordish\"\ninherits = \"emde\"\n",
        );
        let t = load_ok("nordish", &env);
        assert_eq!(t.name, "Nordish");
        assert_eq!(t.docs.len(), 2);

        let path = d.write("elsewhere/plain.toml", "[style.h1]\nbold = true\n");
        let t = load_ok(&path.display().to_string(), &env);
        assert_eq!(t.name, "plain", "file stem when the theme has no name");

        let listed = list_themes(&env);
        let builtins = builtin::names().count();
        assert_eq!(listed.len(), builtins + 1);
        assert_eq!(listed[builtins].name, "nordish");
        assert!(listed[0].path.is_none());
    }

    #[test]
    fn inherits_merges_root_first() {
        let d = TestDir::new("chain-merge");
        let env = env_for(&d);
        d.write(
            ".config/emde/themes/parent.toml",
            "inherits = \"emde\"\n[palette]\naccent = \"#ff0000\"\n[style.h2]\nitalic = true\n",
        );
        d.write(
            ".config/emde/themes/child.toml",
            "inherits = \"parent\"\ncode = \"Nord\"\n[palette.dark]\nmuted = \"#00ff00\"\n[style.h2]\nbold = true\n",
        );
        let t = load_ok("child", &env);
        assert_eq!(t.docs.len(), 3);
        let dark = Variant::Dark.index();
        assert_eq!(
            t.patch.palette[dark]["accent"],
            ColorSpec::Rgb(Rgb(255, 0, 0))
        );
        assert_eq!(
            t.patch.palette[dark]["muted"],
            ColorSpec::Rgb(Rgb(0, 255, 0))
        );
        assert_eq!(
            t.patch.palette[Variant::Light.index()]["muted"],
            ColorSpec::Rgb(Rgb(0x8c, 0x8f, 0xa1)),
            "light keeps emde's"
        );
        let h2 = &t.patch.styles[dark][&Element::H2];
        assert_eq!(h2.bold, Some(true));
        assert_eq!(h2.italic, None, "the child's h2 replaces the parent's");
        assert_eq!(t.patch.code[dark].as_deref(), Some("Nord"));
        assert!(
            t.patch.styles[dark].contains_key(&Element::H1),
            "emde's h1 is inherited"
        );
    }

    #[test]
    fn inheritance_loops_are_cut() {
        let d = TestDir::new("chain-loop");
        let env = env_for(&d);
        d.write(
            ".config/emde/themes/a.toml",
            "inherits = \"b\"\n[style.h1]\nbold = true\n",
        );
        d.write(
            ".config/emde/themes/b.toml",
            "inherits = \"a\"\n[style.h2]\nbold = true\n",
        );
        let mut diags = Vec::new();
        let t = load("a", None, "test", &env, &mut diags);
        assert_eq!(t.docs.len(), 2, "a and b load once each");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("loops: a → b → a"),
            "{}",
            diags[0].message
        );
        assert!(
            diags[0].location.ends_with("b.toml:1"),
            "{}",
            diags[0].location
        );
        // Self-inheritance is a loop too.
        d.write(".config/emde/themes/me.toml", "inherits = \"me\"\n");
        let mut diags = Vec::new();
        load("me", None, "test", &env, &mut diags);
        assert!(diags[0].message.contains("loops: me → me"));
    }

    #[test]
    fn inheritance_depth_is_limited() {
        let d = TestDir::new("chain-depth");
        let env = env_for(&d);
        for i in 0..12 {
            d.write(
                &format!(".config/emde/themes/t{i}.toml"),
                &format!("inherits = \"t{}\"\n", i + 1),
            );
        }
        d.write(".config/emde/themes/t12.toml", "");
        let mut diags = Vec::new();
        let t = load("t0", None, "test", &env, &mut diags);
        assert_eq!(t.docs.len(), MAX_DEPTH);
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("deeper than 8"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn unknown_and_unreadable_themes_fall_back() {
        let d = TestDir::new("chain-missing");
        let env = env_for(&d);
        let mut diags = Vec::new();
        let t = load("emdee", None, "config.toml:2", &env, &mut diags);
        assert_eq!(t.name, "emde");
        assert!(t.fell_back);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].location, "config.toml:2");
        assert_eq!(
            diags[0].message,
            "unknown theme `emdee` (did you mean `emde`?); using the built-in `emde` theme"
        );

        let mut diags = Vec::new();
        let t = load("zzzzzz", None, "x", &env, &mut diags);
        assert_eq!(t.name, "emde");
        assert!(
            diags[0]
                .message
                .contains("available: emde, ansi, mono, dracula, gruvbox, nord"),
            "{}",
            diags[0].message
        );

        let mut diags = Vec::new();
        let t = load("./missing.toml", Some(d.path()), "x", &env, &mut diags);
        assert_eq!(t.name, "emde");
        assert!(
            diags[0].message.contains("cannot read"),
            "{}",
            diags[0].message
        );

        // A parent that is missing cuts the chain but keeps the child.
        d.write(
            ".config/emde/themes/orphan.toml",
            "inherits = \"gone\"\n[style.h1]\nbold = true\n",
        );
        let mut diags = Vec::new();
        let t = load("orphan", None, "x", &env, &mut diags);
        assert_eq!(t.name, "orphan");
        assert_eq!(t.docs.len(), 1);
        assert!(diags[0].location.ends_with("orphan.toml:1"));
    }

    #[test]
    fn relative_paths_follow_the_file() {
        let d = TestDir::new("chain-rel");
        let env = env_for(&d);
        d.write(
            "themes/base.toml",
            "code = \"x.tmTheme\"\n[style.h1]\nbold = true\n",
        );
        let child = d.write("themes/child.toml", "inherits = \"./base.toml\"\n");
        let t = load_ok(&child.display().to_string(), &env);
        assert_eq!(t.docs.len(), 2);
        let code = t.patch.code[0].as_deref().unwrap();
        assert!(Path::new(code).is_absolute(), "{code}");
        assert!(code.ends_with("themes/x.tmTheme"), "{code}");
    }
}
