//! The settings reference: every key of `assets/default.toml`, with its
//! default value and the comment written above it.
//!
//! `default.toml` is read line by line, so the values appear as they are
//! written there (`40_000_000`, not `40000000`). The result is checked
//! against a real TOML parse of the file: the same keys, and each value
//! written here parses to the value the file has. Comment blocks that are
//! not attached to a key are section notes; commented-out tables
//! (`# [palette]`, `# [style.<element>]`) are documented by hand in the
//! template and left out here.
//!
//! Settings that a command-line flag also sets say so; [`FLAGS`] lists the
//! pairs, and each pair is checked: the flag and the equivalent `--set` load
//! the same configuration.

use std::collections::BTreeMap;

use clap::Parser as _;
use emde::cli::Cli;
use emde::config::{self, ConfigEnv, LoadOptions};

use super::markdown;

/// A command-line flag that also sets a configuration key.
struct Flag {
    /// The key.
    key: &'static str,
    /// How the reference shows the flag (Markdown).
    shown: &'static str,
    /// Arguments that set the key, to check the pair.
    args: &'static [&'static str],
    /// The `--set` value that does what `args` do.
    value: &'static str,
}

const fn flag(
    key: &'static str,
    shown: &'static str,
    args: &'static [&'static str],
    value: &'static str,
) -> Flag {
    Flag {
        key,
        shown,
        args,
        value,
    }
}

/// Every flag that sets a configuration key.
#[rustfmt::skip]
const FLAGS: &[Flag] = &[
    flag("theme.name", "`--theme`, `-t`", &["--theme", "ansi"], "ansi"),
    flag("theme.background", "`--background`", &["--background", "light"], "light"),
    flag("theme.code", "`--code-theme`", &["--code-theme", "Nord"], "Nord"),
    flag("render.width", "`--width`, `-w`", &["--width", "60"], "60"),
    flag("render.max_width", "`--max-width`, `-m`", &["--max-width", "60"], "60"),
    flag("render.align", "`--align`", &["--align", "left"], "left"),
    flag("render.images", "`--images`", &["--images", "blocks"], "blocks"),
    flag("render.math", "`--math`", &["--math", "raw"], "raw"),
    flag("render.color", "`--color`", &["--color", "256"], "256"),
    flag("render.hyperlinks", "`--hyperlinks`", &["--hyperlinks", "never"], "never"),
    flag("render.link_refs", "`--link-refs`", &["--link-refs", "always"], "always"),
    flag("render.ascii", "`--ascii`", &["--ascii"], "true"),
    flag("code.line_numbers", "`--line-numbers`", &["--line-numbers"], "true"),
    flag("code.wrap", "`--no-wrap-code` (false)", &["--no-wrap-code"], "false"),
    flag("math.display", "`--math-display`", &["--math-display", "linear"], "linear"),
    flag("images.blocks", "`--blocks`", &["--blocks", "sextant"], "sextant"),
    flag("images.remote", "`--remote-images` (true)", &["--remote-images"], "true"),
    flag("pager.enabled", "`--paging`; `--plain` (`-p`) means never", &["--paging", "auto"], "auto"),
];

/// One `[section]` of the defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Section {
    /// The table name, e.g. `render` or `code.aliases`.
    pub(crate) name: String,
    /// Comment blocks not attached to a key.
    pub(crate) notes: Vec<Note>,
    /// The keys, in file order.
    pub(crate) keys: Vec<Setting>,
}

/// A comment block of a section that documents no key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Note {
    /// Prose, joined into one paragraph.
    Text(String),
    /// Commented-out `key = value` lines: an example.
    Example(Vec<String>),
}

/// One key with its default and documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Setting {
    pub(crate) key: String,
    /// The value as written in `default.toml`.
    pub(crate) value: String,
    /// The comment above the key, joined into one paragraph.
    pub(crate) doc: String,
}

/// Parse `default.toml` into sections, and check the result against a
/// TOML parse of the same text.
pub(crate) fn parse(src: &str) -> Result<Vec<Section>, String> {
    let sections = scan(src)?;
    verify(&sections, src)?;
    Ok(sections)
}

/// The line scanner.
fn scan(src: &str) -> Result<Vec<Section>, String> {
    let mut sections: Vec<Section> = Vec::new();
    let mut comment: Vec<String> = Vec::new();
    for (n, raw) in src.lines().enumerate() {
        let line = raw.trim();
        let at = |msg: &str| format!("line {}: {msg}", n + 1);
        if line.is_empty() {
            flush_note(&mut sections, &mut comment);
        } else if let Some(text) = line.strip_prefix('#') {
            comment.push(text.strip_prefix(' ').unwrap_or(text).to_owned());
        } else if let Some(name) = line.strip_prefix('[') {
            // A comment right above a header introduces the new section.
            let intro = std::mem::take(&mut comment);
            let name = name
                .strip_suffix(']')
                .filter(|n| !n.starts_with('['))
                .ok_or_else(|| at("expected a `[table]` header"))?;
            sections.push(Section {
                name: name.trim().to_owned(),
                notes: Vec::new(),
                keys: Vec::new(),
            });
            comment = intro;
            flush_note(&mut sections, &mut comment);
        } else {
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| at("expected `key = value`"))?;
            let section = sections
                .last_mut()
                .ok_or_else(|| at("a key before the first table"))?;
            let key = key.trim();
            if comment.is_empty() {
                return Err(at(&format!("`{key}` has no comment to document it")));
            }
            let value = value.trim();
            if has_comment(value) {
                return Err(at("end-of-line comments are not supported"));
            }
            section.keys.push(Setting {
                key: key.to_owned(),
                value: value.to_owned(),
                doc: std::mem::take(&mut comment).join(" "),
            });
        }
    }
    flush_note(&mut sections, &mut comment);
    Ok(sections)
}

/// A comment block followed by a blank line or a header: a note of the
/// current section. The preamble before the first table and commented-out
/// tables are documented elsewhere.
fn flush_note(sections: &mut [Section], comment: &mut Vec<String>) {
    let lines = std::mem::take(comment);
    let Some(section) = sections.last_mut() else {
        return;
    };
    if lines.first().is_none_or(|l| l.starts_with('[')) {
        return;
    }
    let (examples, prose): (Vec<String>, Vec<String>) =
        lines.into_iter().partition(|l| is_key_value(l));
    if !prose.is_empty() {
        section.notes.push(Note::Text(prose.join(" ")));
    }
    if !examples.is_empty() {
        section.notes.push(Note::Example(examples));
    }
}

/// Whether a comment line is a commented-out `key = value` line.
fn is_key_value(line: &str) -> bool {
    line.contains('=') && toml::from_str::<toml::Table>(line).is_ok()
}

/// Whether a value has a `#` comment after it (outside strings).
fn has_comment(value: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for c in value.chars() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => return true,
            None => {}
        }
    }
    false
}

/// The scan must agree with a TOML parse: the same keys, and every value as
/// written parses to the file's value.
fn verify(sections: &[Section], src: &str) -> Result<(), String> {
    let doc: toml::Table = toml::from_str(src).map_err(|e| e.to_string())?;
    let mut parsed = BTreeMap::new();
    flatten("", &doc, &mut parsed);
    let mut scanned = BTreeMap::new();
    for section in sections {
        for s in &section.keys {
            let path = format!("{}.{}", section.name, s.key);
            let one: toml::Table = toml::from_str(&format!("v = {}", s.value))
                .map_err(|e| format!("{path}: cannot parse `{}`: {e}", s.value))?;
            if scanned.insert(path.clone(), one["v"].clone()).is_some() {
                return Err(format!("{path} is listed twice"));
            }
        }
    }
    let keys = |m: &BTreeMap<String, toml::Value>| m.keys().cloned().collect::<Vec<_>>();
    if keys(&parsed) != keys(&scanned) {
        return Err(format!(
            "the line scan found {:?}, TOML has {:?}",
            keys(&scanned),
            keys(&parsed)
        ));
    }
    for (path, value) in &scanned {
        if parsed.get(path) != Some(value) {
            return Err(format!(
                "{path}: the value as written is not the value parsed"
            ));
        }
    }
    Ok(())
}

/// Every leaf of a table with its dotted path.
fn flatten(prefix: &str, table: &toml::Table, out: &mut BTreeMap<String, toml::Value>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(t) => flatten(&path, t, out),
            v => {
                out.insert(path, v.clone());
            }
        }
    }
}

/// The reference: a heading and a table of keys per section. Fails when a
/// [`FLAGS`] pair does not hold or names a key that does not exist.
pub(crate) fn markdown(sections: &[Section]) -> Result<String, String> {
    check_flags(sections)?;
    let mut out = String::new();
    for section in sections {
        out.push_str(&format!("### `[{}]`\n\n", section.name));
        for note in &section.notes {
            match note {
                Note::Text(t) => {
                    out.push_str(&markdown::paragraph(t));
                    out.push_str("\n\n");
                }
                Note::Example(lines) => {
                    out.push_str(&format!("```toml\n[{}]\n", section.name));
                    for l in lines {
                        out.push_str(l);
                        out.push('\n');
                    }
                    out.push_str("```\n\n");
                }
            }
        }
        if section.keys.is_empty() {
            continue;
        }
        let rows: Vec<Vec<String>> = section
            .keys
            .iter()
            .map(|s| {
                let path = format!("{}.{}", section.name, s.key);
                let mut doc = markdown::text(&s.doc);
                if let Some(f) = FLAGS.iter().find(|f| f.key == path) {
                    doc.push_str(&format!(" Flag: {}.", f.shown));
                }
                vec![markdown::code(&s.key), markdown::code(&s.value), doc]
            })
            .collect();
        out.push_str(&markdown::table(&["Key", "Default", "Description"], &rows));
        out.push('\n');
    }
    Ok(out)
}

/// Each flag of [`FLAGS`] must load the same configuration as the `--set`
/// it is documented as, and differ from the defaults.
fn check_flags(sections: &[Section]) -> Result<(), String> {
    let defaults = loaded(LoadOptions::default())?;
    for flag in FLAGS {
        let (key, args, value) = (flag.key, flag.args, flag.value);
        let (table, name) = key.rsplit_once('.').unwrap_or(("", key));
        let known = sections
            .iter()
            .any(|s| s.name == table && s.keys.iter().any(|k| k.key == name));
        if !known {
            return Err(format!("flag table: `{key}` is not a setting"));
        }
        let cli = Cli::try_parse_from(std::iter::once("emde").chain(args.iter().copied()))
            .map_err(|e| format!("flag table: {args:?}: {e}"))?;
        let by_flag = loaded(cli.load_options(ConfigEnv::default()))?;
        let by_set = loaded(LoadOptions {
            set: vec![format!("{key}={value}")],
            ..LoadOptions::default()
        })?;
        if by_flag != by_set {
            return Err(format!(
                "flag table: {args:?} does not do what `--set {key}={value}` does"
            ));
        }
        if by_flag == defaults {
            return Err(format!("flag table: {args:?} changes nothing"));
        }
    }
    Ok(())
}

/// The configuration `opts` load, for comparison (`Config` has no
/// `PartialEq`, but its `Debug` shows every field).
fn loaded(opts: LoadOptions) -> Result<String, String> {
    let loaded = config::load(&opts);
    if let Some(d) = loaded.diagnostics.first() {
        return Err(format!("flag table: {d}"));
    }
    Ok(format!("{:?}", loaded.config))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# preamble\n\n[a]\n# The width,\n# in columns.\nwidth = 0\n\
        # Glyphs.\nlist = [\"•\", \"|\"]\n\n# [palette]\n# hidden = \"x\"\n\n\
        # About b.\n[a.b]\n# Extra entries:\n# x = \"y\"\n";

    #[test]
    fn scan_sections_keys_and_notes() {
        let s = parse(SAMPLE).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "a");
        assert_eq!(
            s[0].keys[0],
            Setting {
                key: "width".into(),
                value: "0".into(),
                doc: "The width, in columns.".into()
            }
        );
        assert_eq!(s[0].keys[1].value, "[\"•\", \"|\"]");
        assert!(s[0].notes.is_empty(), "commented-out tables are skipped");
        assert_eq!(s[1].name, "a.b");
        assert_eq!(
            s[1].notes,
            [
                Note::Text("About b.".into()),
                Note::Text("Extra entries:".into()),
                Note::Example(vec!["x = \"y\"".into()])
            ]
        );
    }

    #[test]
    fn scan_errors() {
        assert!(parse("[a]\nx = 1\n").unwrap_err().contains("no comment"));
        assert!(parse("# c\nx = 1\n").unwrap_err().contains("first table"));
        assert!(parse("[a]\n# c\nx = 1 # one\n").is_err());
        assert!(parse("[[a]]\n").is_err());
        // A multi-line value is not a value the line scan can show.
        assert!(parse("[a]\n# c\nx = [\n1]\n").is_err());
    }

    #[test]
    fn comments_after_values() {
        assert!(has_comment("1 # x"));
        assert!(!has_comment("\"#rrggbb\""));
        assert!(!has_comment(r##""a\"#""##));
        assert!(!has_comment("'#'"));
    }

    #[test]
    fn the_real_defaults() {
        let src = include_str!("../../../assets/default.toml");
        let sections = parse(src).unwrap();
        let names: Vec<&str> = sections.iter().map(|s| s.name.as_str()).collect();
        for name in [
            "theme",
            "render",
            "code",
            "code.aliases",
            "pager",
            "terminal",
        ] {
            assert!(names.contains(&name), "{name} in {names:?}");
        }
        let md = markdown(&sections).unwrap();
        assert!(md.contains("| `max_width` | `100` |"), "{md}");
        assert!(md.contains("Flag: `--width`, `-w`."));
        assert!(md.contains("```toml\n[code.aliases]\n"));
    }
}
