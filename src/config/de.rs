//! Reading TOML documents into layers, with diagnostics.
//!
//! [`parse`] never fails. A syntax error drops the whole document; a type
//! error drops only the section it occurs in: a top-level key, or the
//! innermost table (`[render]`, `[style.h1]`) holding the bad value. That is
//! recovered by re-reading the document as a spanned table and deserialising
//! it one piece at a time. Unknown keys are collected with `serde_ignored`
//! and reported with a did-you-mean suggestion; errors keep toml's caret
//! snippet.

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use toml::Spanned;
use toml::de::{DeString, DeTable, DeValue};

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

impl Origin {
    /// Whether locations in this document are worth a line number (files,
    /// not one-line `--set` documents or the flags).
    fn has_lines(&self) -> bool {
        !matches!(self, Origin::Set(_) | Origin::Flags)
    }

    /// `origin:line` for a byte offset into the document, or just the origin.
    fn at_offset(&self, src: &str, offset: Option<usize>) -> String {
        match offset {
            Some(at) if self.has_lines() => format!("{self}:{}", line_number(src, at)),
            _ => self.to_string(),
        }
    }
}

/// The 1-based line of a byte offset (offsets past the end count as the end).
fn line_number(src: &str, offset: usize) -> usize {
    let before = src
        .as_bytes()
        .get(..offset.min(src.len()))
        .unwrap_or_default();
    before.iter().filter(|&&b| b == b'\n').count() + 1
}

/// The newlines of a document, for line numbers of many offsets in
/// O(log n) each ([`line_number`] scans the document every time).
struct LineIndex(Vec<usize>);

impl LineIndex {
    fn new(src: &str) -> LineIndex {
        LineIndex(memchr::memchr_iter(b'\n', src.as_bytes()).collect())
    }

    /// The 1-based line of a byte offset, as [`line_number`] counts it.
    fn line(&self, offset: usize) -> usize {
        self.0.partition_point(|&newline| newline < offset) + 1
    }
}

/// Most diagnostics of one kind shown per document; the rest are counted.
/// A pathological file must not bury the terminal in messages, nor spend
/// seconds on did-you-mean suggestions nobody reads.
pub(crate) const MAX_REPORTED: usize = 20;

/// A problem at a key path whose message is built only if it is shown.
pub(crate) type Deferred<'a> = (Vec<&'a str>, Box<dyn FnOnce() -> String + 'a>);

/// The diagnostic that stands for `hidden` problems not shown.
pub(crate) fn more_not_shown(severity: Severity, location: String, hidden: usize) -> Diagnostic {
    let kind = match (severity, hidden) {
        (Severity::Warning, 1) => "warning",
        (Severity::Warning, _) => "warnings",
        (Severity::Error, 1) => "error",
        (Severity::Error, _) => "errors",
    };
    Diagnostic {
        severity,
        location,
        message: format!("{hidden} more {kind} like these not shown"),
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
        if self.origin.has_lines() {
            KeyLines::new(&self.src)
        } else {
            None
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

    /// Report problems at key paths in source order: the first
    /// [`MAX_REPORTED`], then how many more there are.
    pub(crate) fn report(
        &self,
        severity: Severity,
        items: Vec<Deferred<'_>>,
        diags: &mut Vec<Diagnostic>,
    ) {
        if items.is_empty() {
            return;
        }
        let lines = self.key_lines();
        let mut located: Vec<_> = items
            .into_iter()
            .map(|(path, message)| (lines.as_ref().and_then(|k| k.line(&path)), message))
            .collect();
        located.sort_by_key(|(line, _)| *line);
        let hidden = located.len().saturating_sub(MAX_REPORTED);
        for (line, message) in located.into_iter().take(MAX_REPORTED) {
            diags.push(Diagnostic {
                severity,
                location: self.location_at(line),
                message: message(),
            });
        }
        if hidden > 0 {
            diags.push(more_not_shown(severity, self.origin.to_string(), hidden));
        }
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
    let items: Vec<Deferred<'_>> = unknown
        .iter()
        .map(|path| {
            let refs: Vec<&str> = path.iter().map(String::as_str).collect();
            let for_message = refs.clone();
            let message = move || unknown_key_message(&for_message, schema);
            (refs, Box::new(message) as Box<dyn FnOnce() -> String>)
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
                location: origin.at_offset(src, e.span().map(|s| s.start)),
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

/// Deserialise a document with a type error piece by piece, keeping every
/// piece that is usable on its own.
///
/// The pieces are the top-level entries. A table that fails is split
/// further, into its own values (one piece: the TOML section) and each of
/// its sub-tables, so an error drops only the innermost table holding it: a
/// bad colour in `[style.h1]` loses `[style.h1]` and keeps `[style.h2]`, a
/// bad `[render]` value loses `[render]`.
fn recover<T>(
    src: &str,
    origin: &Origin,
    mut first: toml::de::Error,
    diags: &mut Vec<Diagnostic>,
) -> (T, Vec<Vec<String>>)
where
    T: DeserializeOwned + Default + Merge,
{
    let mut r = Recovery {
        root_span: 0..src.len(),
        acc: T::default(),
        unknown: Vec::new(),
        errors: Vec::new(),
    };
    if let Ok(root) = DeTable::parse(src) {
        r.root_span = root.span();
        for (key, value) in root.into_inner() {
            r.root_entry(key, value);
        }
    }
    if r.errors.is_empty() {
        // Every piece works on its own, so the pieces cannot be trusted
        // together: report the document's own error and use none of it.
        first.set_input(Some(src));
        diags.push(Diagnostic {
            severity: Severity::Error,
            location: origin.at_offset(src, first.span().map(|s| s.start)),
            message: format!(
                "invalid value, {}\n{}",
                ignoring(origin, None),
                first.to_string().trim_end()
            ),
        });
        return (T::default(), Vec::new());
    }
    // Rendering an error scans the document, so only the shown ones are.
    r.errors
        .sort_by_key(|(e, _)| e.span().map_or(usize::MAX, |s| s.start));
    let hidden = r.errors.len().saturating_sub(MAX_REPORTED);
    for (mut e, what) in r.errors.into_iter().take(MAX_REPORTED) {
        e.set_input(Some(src));
        diags.push(Diagnostic {
            severity: Severity::Error,
            location: origin.at_offset(src, e.span().map(|s| s.start)),
            message: format!(
                "invalid value, {}\n{}",
                ignoring(origin, Some(&what)),
                e.to_string().trim_end()
            ),
        });
    }
    if hidden > 0 {
        diags.push(more_not_shown(Severity::Error, origin.to_string(), hidden));
    }
    // Pieces that were retried after a failure report their unknown keys
    // again.
    r.unknown.sort();
    r.unknown.dedup();
    (r.acc, r.unknown)
}

/// A key of a spanned TOML table.
type Key<'i> = Spanned<DeString<'i>>;

/// A table on the way down to a piece: its key and the span of its value.
type PathSeg<'i> = (Key<'i>, Range<usize>);

/// How deep [`Recovery`] splits failing tables (`dark.style.h1` is 3).
const MAX_SPLIT_DEPTH: usize = 4;

/// The state of [`recover`].
struct Recovery<T> {
    /// Span of the whole document (for the tables built around pieces).
    root_span: Range<usize>,
    /// The pieces merged so far.
    acc: T,
    unknown: Vec<Vec<String>>,
    /// Each dropped piece's error, and what the piece is (`[style.h1]`).
    errors: Vec<(toml::de::Error, String)>,
}

impl<T: DeserializeOwned + Merge> Recovery<T> {
    /// One top-level entry: kept whole if it works, else split if it is a
    /// table of tables, else dropped.
    fn root_entry<'i>(&mut self, key: Key<'i>, value: Spanned<DeValue<'i>>) {
        let name = key.get_ref().to_string();
        let path = [(key.clone(), value.span())];
        let mut single = DeTable::new();
        single.insert(key, value.clone());
        let Err(e) = self.attempt(&[], single) else {
            return;
        };
        match value.into_inner() {
            DeValue::Table(table) if self.can_split(&path, &table) => self.split(&path, table),
            DeValue::Table(_) => self.errors.push((e, format!("[{name}]"))),
            _ => self.errors.push((e, format!("`{name}`"))),
        }
    }

    /// Recover the table at `path` from its pieces: its own values together,
    /// and each sub-table on its own (split further when it fails too).
    fn split<'i>(&mut self, path: &[PathSeg<'i>], table: DeTable<'i>) {
        let (subs, own): (Vec<_>, Vec<_>) = table
            .into_iter()
            .partition(|(_, v)| matches!(v.get_ref(), DeValue::Table(_)));
        if !own.is_empty() {
            let own: DeTable<'i> = own.into_iter().collect();
            if let Err(e) = self.attempt(path, own) {
                self.errors.push((e, section(path)));
            }
        }
        for (key, value) in subs {
            let span = value.span();
            let DeValue::Table(sub) = value.into_inner() else {
                continue;
            };
            let mut sub_path = path.to_vec();
            sub_path.push((key, span));
            let Err(e) = self.attempt(&sub_path, sub.clone()) else {
                continue;
            };
            if self.can_split(&sub_path, &sub) {
                self.split(&sub_path, sub);
            } else {
                self.errors.push((e, section(&sub_path)));
            }
        }
    }

    /// Whether a failing table at `path` is worth splitting: it holds tables,
    /// is not nested too deep, and a table is allowed there at all (else
    /// every piece would fail for the same reason).
    fn can_split<'i>(&self, path: &[PathSeg<'i>], table: &DeTable<'i>) -> bool {
        path.len() < MAX_SPLIT_DEPTH
            && has_sub_tables(table)
            && self
                .deserialize(path, DeTable::new(), &mut Vec::new())
                .is_ok()
    }

    /// Deserialise a document holding only `leaf` at `path`, and keep it if
    /// that works.
    fn attempt<'i>(
        &mut self,
        path: &[PathSeg<'i>],
        leaf: DeTable<'i>,
    ) -> Result<(), toml::de::Error> {
        let mut unknown = Vec::new();
        let part = self.deserialize(path, leaf, &mut unknown);
        // Unknown keys count even in a piece that fails: it may be
        // dropped as a whole.
        self.unknown.append(&mut unknown);
        self.acc.merge(part?);
        Ok(())
    }

    /// Deserialise a document holding only `leaf` at `path`.
    fn deserialize<'i>(
        &self,
        path: &[PathSeg<'i>],
        leaf: DeTable<'i>,
        unknown: &mut Vec<Vec<String>>,
    ) -> Result<T, toml::de::Error> {
        let mut table = leaf;
        for (key, span) in path.iter().rev() {
            let mut outer = DeTable::new();
            outer.insert(
                key.clone(),
                Spanned::new(span.clone(), DeValue::Table(table)),
            );
            table = outer;
        }
        let de = toml::Deserializer::from(Spanned::new(self.root_span.clone(), table));
        serde_ignored::deserialize(de, |p| unknown.push(segments(&p)))
    }
}

/// Whether a table holds tables (and so can be split into sections).
fn has_sub_tables(table: &DeTable<'_>) -> bool {
    table
        .values()
        .any(|v| matches!(v.get_ref(), DeValue::Table(_)))
}

/// `[a.b]` for a table path, quoting keys that are not bare TOML keys.
fn section(path: &[PathSeg<'_>]) -> String {
    let keys: Vec<String> = path.iter().map(|(k, _)| toml_key(k.get_ref())).collect();
    format!("[{}]", keys.join("."))
}

/// A key as written in TOML: bare when possible, else a basic string.
pub(crate) fn toml_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        key.to_owned()
    } else {
        basic_string(key)
    }
}

/// `s` as a TOML basic string: quoted, with `"`, `\` and control
/// characters escaped.
pub(crate) fn basic_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
    root: Spanned<DeTable<'a>>,
    lines: LineIndex,
}

impl<'a> KeyLines<'a> {
    fn new(src: &'a str) -> Option<KeyLines<'a>> {
        Some(KeyLines {
            root: DeTable::parse(src).ok()?,
            lines: LineIndex::new(src),
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
        Some(self.lines.line(offset?))
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

    /// The `ignoring …` part of each error message.
    fn dropped(diags: &[Diagnostic]) -> Vec<String> {
        diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .filter_map(|d| {
                let first = d.message.lines().next()?;
                Some(first.split_once("ignoring ")?.1.to_owned())
            })
            .collect()
    }

    #[test]
    fn a_bad_style_drops_only_its_own_table() {
        let (layer, diags) = config(
            "[style.h1]\nfg = \"#12\"\nbold = true\n[style.h2]\nitalic = true\n\
             [dark.style.h3]\nfg = \"nope nope\"\n[dark.style.h4]\nbold = true\n\
             [light.style.h5]\nitalic = true\n[render]\nmargin = 3\n",
        );
        assert_eq!(
            dropped(&diags),
            ["[style.h1]", "[dark.style.h3]"],
            "{diags:?}"
        );
        assert!(!layer.style.0.contains_key(&Element::H1));
        assert_eq!(layer.style.0[&Element::H2].italic, Some(true));
        assert!(!layer.dark.style.0.contains_key(&Element::H3));
        assert_eq!(layer.dark.style.0[&Element::H4].bold, Some(true));
        assert_eq!(layer.light.style.0[&Element::H5].italic, Some(true));
        assert_eq!(layer.render.margin, Some(3));
        // The location points at the bad value.
        assert_eq!(diags[0].location, "test.toml:2");
        assert!(diags[0].message.contains("^^^^^"), "{}", diags[0].message);
    }

    #[test]
    fn sections_and_their_sub_tables_are_separate() {
        // A bad value in [code] keeps [code.aliases], and the other way round.
        let (layer, diags) =
            config("[code]\ntab_width = \"x\"\n[code.aliases]\nsage = \"python\"\n");
        assert_eq!(dropped(&diags), ["[code]"]);
        assert_eq!(layer.code.tab_width, None);
        assert_eq!(layer.code.aliases.0.len(), 1);
        let (layer, diags) = config("[code]\ntab_width = 3\n[code.aliases]\nsage = 5\n");
        assert_eq!(dropped(&diags), ["[code.aliases]"]);
        assert_eq!(layer.code.tab_width, Some(3));
        assert!(layer.code.aliases.0.is_empty());
        // Flat palette entries and [palette.dark] are separate sections.
        let (layer, diags) = config(
            "[palette]\naccent = \"#12\"\nmuted = \"#fff\"\n[palette.dark]\ntext = \"#000\"\n",
        );
        assert_eq!(dropped(&diags), ["[palette]"]);
        assert!(layer.palette.both.is_empty());
        assert_eq!(layer.palette.dark.len(), 1);
        // Inline tables count as sections too.
        let (layer, diags) = config("[style]\nh1 = { fg = 300 }\nh2 = { bold = true }\n");
        assert_eq!(dropped(&diags), ["[style.h1]"]);
        assert_eq!(layer.style.0.len(), 1);
    }

    #[test]
    fn a_table_where_a_value_belongs_is_dropped_whole() {
        let (layer, diags) = config(
            "[render]\nmargin = 1\n[render.max_width]\na = 1\n[render.max_width.b.c.d]\ne = 2\n",
        );
        assert_eq!(dropped(&diags), ["[render.max_width]"], "{diags:?}");
        assert!(
            diags[0].message.contains("expected u16"),
            "{}",
            diags[0].message
        );
        assert_eq!(layer.render.margin, Some(1));
        // Keys that are not bare are quoted in the message.
        let (_, diags) = config("[palette.\"my colours\"]\na = \"#fff\"\n");
        assert_eq!(dropped(&diags), ["[palette.\"my colours\"]"]);
    }

    #[test]
    fn retried_tables_report_unknown_keys_once() {
        let (layer, diags) = config("[style.h1]\nbolt = true\n[style.h2]\nfg = \"#12\"\n");
        let unknown: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .collect();
        assert_eq!(unknown.len(), 1, "{diags:?}");
        assert!(unknown[0].message.contains("`style.h1.bolt`"));
        assert!(
            layer.style.0.contains_key(&Element::H1),
            "an empty but valid table"
        );
        assert!(!layer.style.0.contains_key(&Element::H2));
    }

    #[test]
    fn theme_file_keys_are_dropped_one_by_one() {
        let mut diags = Vec::new();
        let parsed = parse::<ThemeFile>(
            Cow::Borrowed(
                "name = 3\ninherits = \"emde\"\n[style.h1]\nbold = \"yes\"\n[style.h2]\nbold = true\n",
            ),
            Origin::File("t.toml".into()),
            Schema::Theme,
            &mut diags,
        );
        assert_eq!(dropped(&diags), ["`name`", "[style.h1]"]);
        assert_eq!(diags[0].location, "t.toml:1");
        assert_eq!(diags[1].location, "t.toml:4");
        let theme = parsed.value;
        assert_eq!(theme.name, None);
        assert_eq!(theme.inherits.as_deref(), Some("emde"));
        assert_eq!(theme.style.0.len(), 1);
    }

    #[test]
    fn set_documents_have_no_line_numbers() {
        let mut diags = Vec::new();
        parse_config(
            Cow::Borrowed("render.margin = \"x\""),
            Origin::Set("render.margin=x".into()),
            &mut diags,
        );
        assert_eq!(diags[0].location, "--set render.margin=x");
        assert!(
            diags[0]
                .message
                .starts_with("invalid value, ignoring this setting")
        );
    }

    #[test]
    fn many_unknown_keys_are_summarised() {
        let mut src = String::from("[render]\nmargin = 1\n");
        for i in (0..50).rev() {
            src.push_str(&format!("zz{i} = 1\n"));
        }
        let (layer, diags) = config(&src);
        assert_eq!(layer.render.margin, Some(1));
        assert_eq!(diags.len(), MAX_REPORTED + 1);
        assert_eq!(
            diags[0].location, "test.toml:3",
            "the first ones in the file"
        );
        assert!(diags[0].message.contains("`render.zz49`"));
        let last = &diags[MAX_REPORTED];
        assert_eq!(last.severity, Severity::Warning);
        assert_eq!(last.location, "test.toml");
        assert_eq!(last.message, "30 more warnings like these not shown");
    }

    #[test]
    fn many_bad_sections_are_summarised() {
        let mut src = String::from("[render]\nmargin = 1\n");
        for i in 0..(MAX_REPORTED + 1) {
            src.push_str(&format!("[palette.t{i}]\na = 1\n"));
        }
        let (layer, diags) = config(&src);
        assert_eq!(layer.render.margin, Some(1));
        assert_eq!(diags.len(), MAX_REPORTED + 1);
        assert!(diags[0].message.contains("ignoring [palette.t0]"));
        assert_eq!(
            diags[0].location, "test.toml:3",
            "the table is the bad value"
        );
        assert_eq!(
            diags[MAX_REPORTED].message,
            "1 more error like these not shown"
        );
    }

    #[test]
    fn line_index_matches_line_number() {
        for src in ["", "a", "a\n", "a\nb", "\n\n\nx\n", "é\nü\n"] {
            let index = LineIndex::new(src);
            for offset in 0..src.len() + 3 {
                assert_eq!(
                    index.line(offset),
                    line_number(src, offset),
                    "{src:?} {offset}"
                );
            }
        }
    }

    #[test]
    fn line_numbers_of_offsets() {
        assert_eq!(line_number("a\nb\nc", 0), 1);
        assert_eq!(line_number("a\nb\nc", 2), 2);
        assert_eq!(line_number("a\nb\nc", 4), 3);
        assert_eq!(line_number("a\nb\nc", 999), 3, "past the end");
        assert_eq!(line_number("é\nx", 1), 1, "inside a character");
    }

    #[test]
    fn keys_and_strings_are_quoted_for_toml() {
        assert_eq!(toml_key("tab_width"), "tab_width");
        assert_eq!(toml_key("c++"), "\"c++\"");
        assert_eq!(toml_key(""), "\"\"");
        assert_eq!(basic_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(basic_string("x\ty"), "\"x\\u0009y\"");
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
