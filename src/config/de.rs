//! Reading TOML documents into layers, with diagnostics.
//!
//! [`parse`] never fails. A syntax error drops the whole document; a type
//! error drops only the top-level table (or key) it occurs in, recovered by
//! re-reading the document as a spanned table and deserialising it one
//! top-level entry at a time. Unknown keys are collected with
//! `serde_ignored` and reported with a did-you-mean suggestion; errors keep
//! toml's caret snippet.

use std::borrow::Cow;
use std::fmt;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use toml::Spanned;
use toml::de::{DeTable, DeValue};

use super::layer::{ConfigLayer, Merge, section_keys, settings_sections};
use super::suggest::did_you_mean;
use super::{Diagnostic, Severity};
use crate::theme::element::Element;
use crate::theme::spec::{StyleSpec, ThemeFile, VariantTable};

/// Where a document came from, for messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The embedded `default.toml`.
    Defaults,
    /// A built-in theme.
    BuiltinTheme(&'static str),
    /// A file on disk.
    File(PathBuf),
    /// One `--set` argument.
    Set(String),
    /// The command-line flags.
    Flags,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Defaults => f.write_str("built-in defaults"),
            Origin::BuiltinTheme(name) => write!(f, "built-in theme `{name}`"),
            Origin::File(path) => write!(f, "{}", path.display()),
            Origin::Set(arg) => write!(f, "--set {arg}"),
            Origin::Flags => f.write_str("command-line flags"),
        }
    }
}

/// Which kind of document is being read (decides key suggestions).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Schema {
    /// The config file, `default.toml` and `--set` documents.
    Config,
    /// A theme file.
    Theme,
}

/// A parsed document, with what is needed to point at lines in it.
#[derive(Clone, Debug)]
pub(crate) struct Parsed<T> {
    pub(crate) value: T,
    pub(crate) origin: Origin,
    pub(crate) src: Cow<'static, str>,
}

impl<T> Parsed<T> {
    /// Line numbers of keys, when the document is a file.
    fn key_lines(&self) -> Option<KeyLines<'_>> {
        match self.origin {
            Origin::Set(_) | Origin::Flags => None,
            _ => KeyLines::new(&self.src),
        }
    }

    /// `origin:line`, or just the origin.
    fn location_at(&self, line: Option<usize>) -> String {
        match line {
            Some(line) => format!("{}:{line}", self.origin),
            None => self.origin.to_string(),
        }
    }

    /// `origin:line` for a key path, or just the origin.
    pub(crate) fn location(&self, path: &[&str]) -> String {
        self.location_at(self.key_lines().and_then(|k| k.line(path)))
    }

    /// A diagnostic about the value at `path`.
    pub(crate) fn diagnostic(
        &self,
        severity: Severity,
        path: &[&str],
        message: String,
    ) -> Diagnostic {
        Diagnostic {
            severity,
            location: self.location(path),
            message,
        }
    }

    /// Report diagnostics about key paths, in source order.
    pub(crate) fn report(
        &self,
        severity: Severity,
        items: Vec<(Vec<&str>, String)>,
        diags: &mut Vec<Diagnostic>,
    ) {
        if items.is_empty() {
            return;
        }
        let lines = self.key_lines();
        let mut located: Vec<(Option<usize>, Diagnostic)> = items
            .into_iter()
            .map(|(path, message)| {
                let line = lines.as_ref().and_then(|k| k.line(&path));
                let location = self.location_at(line);
                (
                    line,
                    Diagnostic {
                        severity,
                        location,
                        message,
                    },
                )
            })
            .collect();
        located.sort_by_key(|(line, _)| *line);
        diags.extend(located.into_iter().map(|(_, d)| d));
    }
}

/// Parse a document into `T`, reporting problems into `diags`.
pub(crate) fn parse<T>(
    src: Cow<'static, str>,
    origin: Origin,
    schema: Schema,
    diags: &mut Vec<Diagnostic>,
) -> Parsed<T>
where
    T: DeserializeOwned + Default + Merge,
{
    let (value, unknown) = read::<T>(&src, &origin, diags);
    let parsed = Parsed { value, origin, src };
    let items = unknown
        .iter()
        .map(|path| {
            let refs: Vec<&str> = path.iter().map(String::as_str).collect();
            let message = unknown_key_message(&refs, schema);
            (refs, message)
        })
        .collect();
    parsed.report(Severity::Warning, items, diags);
    parsed
}

/// What is dropped when a document or one of its tables is unusable.
fn ignoring(origin: &Origin, table: Option<&str>) -> String {
    match (origin, table) {
        (Origin::Set(_) | Origin::Flags, _) => "ignoring this setting".to_owned(),
        (_, None) => "ignoring the whole file".to_owned(),
        (_, Some(what)) => format!("ignoring {what}"),
    }
}

/// Deserialise, recovering from type errors; returns the unknown key paths.
fn read<T>(src: &str, origin: &Origin, diags: &mut Vec<Diagnostic>) -> (T, Vec<Vec<String>>)
where
    T: DeserializeOwned + Default + Merge,
{
    let de = match toml::Deserializer::parse(src) {
        Ok(de) => de,
        Err(e) => {
            diags.push(Diagnostic {
                severity: Severity::Error,
                location: origin.to_string(),
                message: format!(
                    "invalid TOML, {}\n{}",
                    ignoring(origin, None),
                    e.to_string().trim_end()
                ),
            });
            return (T::default(), Vec::new());
        }
    };
    let mut unknown = Vec::new();
    match serde_ignored::deserialize(de, |path| unknown.push(segments(&path))) {
        Ok(value) => (value, unknown),
        Err(first) => recover(src, origin, first, diags),
    }
}

/// Deserialise the document one top-level entry at a time, dropping the
/// entries that fail.
fn recover<T>(
    src: &str,
    origin: &Origin,
    mut first: toml::de::Error,
    diags: &mut Vec<Diagnostic>,
) -> (T, Vec<Vec<String>>)
where
    T: DeserializeOwned + Default + Merge,
{
    let mut acc = T::default();
    let mut unknown = Vec::new();
    let mut errors: Vec<(usize, Diagnostic)> = Vec::new();
    if let Ok(root) = DeTable::parse(src) {
        let span = root.span();
        for (key, value) in root.into_inner() {
            let name = key.get_ref().to_string();
            let what = match value.get_ref() {
                DeValue::Table(_) => format!("[{name}]"),
                _ => format!("`{name}`"),
            };
            let mut table = DeTable::new();
            table.insert(key, value);
            let de = toml::Deserializer::from(Spanned::new(span.clone(), table));
            let part: Result<T, _> =
                serde_ignored::deserialize(de, |path| unknown.push(segments(&path)));
            match part {
                Ok(part) => acc.merge(part),
                Err(mut e) => {
                    e.set_input(Some(src));
                    let at = e.span().map_or(usize::MAX, |s| s.start);
                    let message = format!(
                        "invalid value, {}\n{}",
                        ignoring(origin, Some(&what)),
                        e.to_string().trim_end()
                    );
                    errors.push((
                        at,
                        Diagnostic {
                            severity: Severity::Error,
                            location: origin.to_string(),
                            message,
                        },
                    ));
                }
            }
        }
    }
    if errors.is_empty() {
        first.set_input(Some(src));
        let message = format!(
            "invalid value, {}\n{}",
            ignoring(origin, None),
            first.to_string().trim_end()
        );
        diags.push(Diagnostic {
            severity: Severity::Error,
            location: origin.to_string(),
            message,
        });
    }
    errors.sort_by_key(|(at, _)| *at);
    diags.extend(errors.into_iter().map(|(_, d)| d));
    (acc, unknown)
}

/// The key segments of a `serde_ignored` path.
fn segments(path: &serde_ignored::Path<'_>) -> Vec<String> {
    fn walk(path: &serde_ignored::Path<'_>, out: &mut Vec<String>) {
        use serde_ignored::Path;
        match path {
            Path::Root => {}
            Path::Seq { parent, index } => {
                walk(parent, out);
                out.push(index.to_string());
            }
            Path::Map { parent, key } => {
                walk(parent, out);
                out.push(key.clone());
            }
            Path::Some { parent }
            | Path::NewtypeStruct { parent }
            | Path::NewtypeVariant { parent } => walk(parent, out),
        }
    }
    let mut out = Vec::new();
    walk(path, &mut out);
    out
}

/// Finds the lines of keys in a document (parsed once, spans kept).
struct KeyLines<'a> {
    src: &'a str,
    root: Spanned<DeTable<'a>>,
}

impl<'a> KeyLines<'a> {
    fn new(src: &'a str) -> Option<KeyLines<'a>> {
        Some(KeyLines {
            src,
            root: DeTable::parse(src).ok()?,
        })
    }

    /// The 1-based line of the key at `path` (or of its deepest existing prefix).
    fn line(&self, path: &[&str]) -> Option<usize> {
        let mut table = self.root.get_ref();
        let mut offset = None;
        for seg in path {
            let Some((key, value)) = table.get_key_value(*seg) else {
                break;
            };
            offset = Some(key.span().start);
            match value.get_ref() {
                DeValue::Table(t) => table = t,
                _ => break,
            }
        }
        let before = self.src.get(..offset?)?;
        Some(before.bytes().filter(|&b| b == b'\n').count() + 1)
    }
}

/// The 1-based line of the key at `path` in `src`.
#[cfg(test)]
fn line_of(src: &str, path: &[&str]) -> Option<usize> {
    KeyLines::new(src)?.line(path)
}

/// Keys allowed below `parent` (`None` where keys are free-form or unknown).
fn children(schema: Schema, parent: &[&str]) -> Option<Vec<&'static str>> {
    let elements = || Element::ALL.iter().map(|e| e.name()).collect();
    match (schema, parent) {
        (Schema::Config, []) => Some(ConfigLayer::KEYS.to_vec()),
        (Schema::Theme, []) => Some(ThemeFile::KEYS.to_vec()),
        (Schema::Theme, ["code"]) => Some(vec!["dark", "light"]),
        (_, ["dark" | "light"]) => Some(VariantTable::KEYS.to_vec()),
        (_, ["style"] | ["dark" | "light", "style"]) => Some(elements()),
        (_, ["style", _] | ["dark" | "light", "style", _]) => Some(StyleSpec::KEYS.to_vec()),
        (Schema::Config, [section]) => section_keys(section).map(<[_]>::to_vec),
        _ => None,
    }
}

/// The warning text for an unknown key.
fn unknown_key_message(path: &[&str], schema: Schema) -> String {
    let dotted = path.join(".");
    match (suggest_key(path, schema), path) {
        (Some(s), _) => format!("unknown key `{dotted}` (did you mean `{s}`?)"),
        (None, [key]) if schema == Schema::Theme && is_settings_section(key) => {
            format!("unknown key `{dotted}`: settings belong in the config file, not in a theme")
        }
        (None, _) => format!("unknown key `{dotted}`"),
    }
}

fn is_settings_section(key: &str) -> bool {
    settings_sections().any(|s| s == key)
}

/// A replacement for a misspelt or misplaced key, as a full dotted path.
fn suggest_key(path: &[&str], schema: Schema) -> Option<String> {
    let (&leaf, parent) = path.split_last()?;
    let with_parent = |s: &str| {
        let mut full: Vec<&str> = parent.to_vec();
        full.push(s);
        full.join(".")
    };
    // `[dark.palette]` is spelt `[palette.dark]`.
    if let ([variant @ ("dark" | "light")], "palette") = (parent, leaf) {
        return Some(format!("palette.{variant}"));
    }
    if let Some(siblings) = children(schema, parent)
        && let Some(s) = did_you_mean(leaf, siblings.iter().copied())
    {
        return Some(with_parent(s));
    }
    // A setting in the wrong table: `[render] line_numbers` → `code.line_numbers`,
    // or a misspelt one at the top level: `colour` → `render.color`.
    if schema == Schema::Config && path.len() <= 2 {
        let parent_name = parent.first().copied();
        let elsewhere: Vec<String> = settings_sections()
            .filter(|s| Some(*s) != parent_name)
            .flat_map(|s| {
                section_keys(s)
                    .unwrap_or_default()
                    .iter()
                    .map(move |k| format!("{s}.{k}"))
            })
            .collect();
        let leaf_of = |full: &str| full.split_once('.').map_or("", |(_, k)| k).to_owned();
        if let Some(exact) = elsewhere.iter().find(|full| leaf_of(full) == leaf) {
            return Some(exact.clone());
        }
        let leaves: Vec<String> = elsewhere.iter().map(|f| leaf_of(f)).collect();
        if let Some(near) = did_you_mean(leaf, leaves.iter().map(String::as_str))
            && let Some(i) = leaves.iter().position(|l| l == near)
        {
            return elsewhere.get(i).cloned();
        }
    }
    None
}

/// All dotted keys of a TOML document (tables and values), for the
/// completeness tests of `default.toml`.
#[cfg(test)]
pub(crate) fn dotted_keys(src: &str) -> Vec<String> {
    fn walk(prefix: &str, table: &DeTable<'_>, out: &mut Vec<String>) {
        for (k, v) in table {
            let key = super::layer::join_key(prefix, k.get_ref());
            if let DeValue::Table(t) = v.get_ref() {
                walk(&key, t, out);
            }
            out.push(key);
        }
    }
    let mut out = Vec::new();
    if let Ok(root) = DeTable::parse(src) {
        walk("", root.get_ref(), &mut out);
    }
    out
}

/// Parse a config document.
pub(crate) fn parse_config(
    src: Cow<'static, str>,
    origin: Origin,
    diags: &mut Vec<Diagnostic>,
) -> Parsed<ConfigLayer> {
    parse(src, origin, Schema::Config, diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(src: &str) -> (ConfigLayer, Vec<Diagnostic>) {
        let mut diags = Vec::new();
        let parsed = parse_config(
            Cow::Owned(src.to_owned()),
            Origin::File("test.toml".into()),
            &mut diags,
        );
        (parsed.value, diags)
    }

    #[test]
    fn clean_document() {
        let (layer, diags) = config("[render]\nmax_width = 90\n[pager]\nmouse = false\n");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(layer.render.max_width, Some(90));
        assert_eq!(layer.pager.mouse, Some(false));
    }

    #[test]
    fn syntax_error_drops_the_document() {
        let (layer, diags) = config("[render]\nmax_width = = 3\n");
        assert_eq!(layer, ConfigLayer::default());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].message.contains("line 2"), "{}", diags[0].message);
    }

    #[test]
    fn type_error_drops_one_table() {
        let (layer, diags) = config(
            "[render]\nmax_width = \"wide\"\nmargin = 4\n[pager]\nmouse = false\n\
             [code]\ntab_width = 3\n",
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0]
                .message
                .starts_with("invalid value, ignoring [render]")
        );
        assert!(diags[0].message.contains("^^^^^^"), "caret snippet");
        assert_eq!(layer.render.margin, None, "the whole table is dropped");
        assert_eq!(layer.pager.mouse, Some(false), "other tables survive");
        assert_eq!(layer.code.tab_width, Some(3));
    }

    #[test]
    fn several_type_errors_are_all_reported() {
        let (layer, diags) =
            config("[render]\nmargin = -1\n[pager]\nmouse = 3\n[tables]\nzebra = false\n");
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert_eq!(layer.tables.zebra, Some(false));
    }

    #[test]
    fn root_value_type_error() {
        let (layer, diags) = config("render = 5\n[pager]\nwatch = false\n");
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0]
                .message
                .starts_with("invalid value, ignoring `render`")
        );
        assert!(
            diags[0]
                .message
                .ends_with("invalid type: integer `5`, expected a table"),
            "{}",
            diags[0].message
        );
        assert_eq!(layer.pager.watch, Some(false));
    }

    #[test]
    fn unknown_keys_are_located_and_suggested() {
        let (layer, diags) = config("[render]\nmargin = 1\nmax_widht = 3\n");
        assert_eq!(layer.render.margin, Some(1));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert_eq!(diags[0].location, "test.toml:3");
        assert_eq!(
            diags[0].message,
            "unknown key `render.max_widht` (did you mean `render.max_width`?)"
        );
    }

    #[test]
    fn suggestions() {
        let s = |p: &[&str]| suggest_key(p, Schema::Config);
        assert_eq!(s(&["rendr"]).as_deref(), Some("render"));
        assert_eq!(s(&["style", "h7"]).as_deref(), Some("style.h1"));
        assert_eq!(
            s(&["style", "h1", "bolt"]).as_deref(),
            Some("style.h1.bold")
        );
        assert_eq!(
            s(&["dark", "style", "h1", "itlaic"]).as_deref(),
            Some("dark.style.h1.italic")
        );
        assert_eq!(
            s(&["render", "line_numbers"]).as_deref(),
            Some("code.line_numbers")
        );
        assert_eq!(s(&["dark", "palette"]).as_deref(), Some("palette.dark"));
        assert_eq!(s(&["colour"]).as_deref(), Some("render.color"));
        assert_eq!(s(&["wrap"]).as_deref(), Some("code.wrap"));
        assert_eq!(s(&["zzzzzz"]), None);
        let t = |p: &[&str]| suggest_key(p, Schema::Theme);
        assert_eq!(t(&["inherit"]).as_deref(), Some("inherits"));
        assert_eq!(t(&["code", "drak"]).as_deref(), Some("code.dark"));
        assert_eq!(
            unknown_key_message(&["render"], Schema::Theme),
            "unknown key `render`: settings belong in the config file, not in a theme"
        );
    }

    #[test]
    fn line_lookup() {
        let src = "# c\n[render]\nmargin = 1\n\n[style.h1]\nfg = \"red\"\n";
        assert_eq!(line_of(src, &["render", "margin"]), Some(3));
        assert_eq!(line_of(src, &["style", "h1", "fg"]), Some(6));
        assert_eq!(
            line_of(src, &["render", "nope"]),
            Some(2),
            "deepest known prefix"
        );
        assert_eq!(line_of(src, &["nope"]), None);
        assert_eq!(line_of("not toml [", &["a"]), None);
    }

    #[test]
    fn origins() {
        assert_eq!(Origin::Defaults.to_string(), "built-in defaults");
        assert_eq!(
            Origin::BuiltinTheme("ansi").to_string(),
            "built-in theme `ansi`"
        );
        assert_eq!(Origin::Set("a=b".into()).to_string(), "--set a=b");
        assert_eq!(Origin::Flags.to_string(), "command-line flags");
    }
}
