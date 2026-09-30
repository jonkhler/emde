//! Front matter: YAML (`---`) and TOML (`+++`) blocks.
//!
//! emde does not depend on a YAML parser. It only recognises *flat* front
//! matter, where every entry is a single `key: value` (YAML) or
//! `key = value` (TOML) line with a scalar or one-line collection value.
//! That covers the usual `title`/`date`/`tags` headers, which are shown as a
//! key/value card; anything nested is shown as a code block instead.

use crate::ir::{FrontMatter, FrontMatterFormat};

/// Build the front matter block for the raw text between the delimiters.
pub(crate) fn parse(format: FrontMatterFormat, raw: &str) -> FrontMatter {
    let fields = match format {
        FrontMatterFormat::Yaml => flat_yaml(raw),
        FrontMatterFormat::Toml => flat_toml(raw),
    };
    FrontMatter {
        format,
        raw: raw.into(),
        fields: fields.map(|f| {
            f.into_iter()
                .map(|(k, v)| (k.into_boxed_str(), v.into_boxed_str()))
                .collect()
        }),
    }
}

type Fields = Vec<(String, String)>;

/// Flat YAML: `key: value` lines, blank lines and `#` comments.
fn flat_yaml(raw: &str) -> Option<Fields> {
    let mut fields = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Indentation, sequences and document markers mean nested data.
        if line.starts_with([' ', '\t', '-']) || trimmed == "..." {
            return None;
        }
        let (key, value) = split_yaml_pair(trimmed)?;
        let value = value.trim();
        if value.starts_with(['|', '>', '&', '*', '!']) {
            return None; // block scalars, anchors, aliases, tags
        }
        let key = unquote_yaml(key.trim())?;
        if key.is_empty() {
            return None;
        }
        fields.push((key, yaml_scalar(value)?));
    }
    Some(fields)
}

/// Split at the first `:` that is followed by a space or the line end and
/// is not inside a quoted key.
fn split_yaml_pair(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut quote = None;
    for (i, &b) in bytes.iter().enumerate() {
        match (quote, b) {
            (None, b'"' | b'\'') if i == 0 => quote = Some(b),
            (Some(q), _) if b == q => quote = None,
            (None, b':') if bytes.get(i + 1).is_none_or(|&n| n == b' ' || n == b'\t') => {
                return Some((line.get(..i)?, line.get(i + 1..)?));
            }
            _ => {}
        }
    }
    None
}

fn unquote_yaml(s: &str) -> Option<String> {
    if let Some(inner) = s.strip_prefix('"') {
        return Some(unescape_double(inner.strip_suffix('"')?));
    }
    if let Some(inner) = s.strip_prefix('\'') {
        return Some(inner.strip_suffix('\'')?.replace("''", "'"));
    }
    Some(s.to_string())
}

/// A YAML scalar value on one line: quotes removed, trailing comments cut.
fn yaml_scalar(value: &str) -> Option<String> {
    if value.starts_with(['"', '\'']) {
        let end = closing_quote(value)?;
        let rest = value.get(end + 1..)?.trim();
        if !rest.is_empty() && !rest.starts_with('#') {
            return None;
        }
        return unquote_yaml(value.get(..=end)?);
    }
    if value.starts_with(['[', '{']) {
        // One-line flow collections are shown as written.
        return balanced_flow(value).then(|| value.to_string());
    }
    let cut = value
        .find(" #")
        .map_or(value, |i| value.get(..i).unwrap_or(value));
    Some(cut.trim_end().to_string())
}

/// Byte index of the quote closing the one that starts `value`.
fn closing_quote(value: &str) -> Option<usize> {
    let bytes = value.as_bytes();
    let q = *bytes.first()?;
    let mut i = 1;
    while let Some(&b) = bytes.get(i) {
        if q == b'"' && b == b'\\' {
            i += 2;
            continue;
        }
        if b == q {
            if q == b'\'' && bytes.get(i + 1) == Some(&b'\'') {
                i += 2; // '' escapes a quote
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Whether the brackets of a one-line flow collection balance.
fn balanced_flow(value: &str) -> bool {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in value.chars() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '[' | '{') => depth += 1,
            (None, ']' | '}') => depth -= 1,
            _ => {}
        }
    }
    depth == 0 && quote.is_none()
}

/// Unescape the common escapes of a double-quoted YAML/TOML string.
fn unescape_double(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Flat TOML: `key = value` lines (no tables), blank lines and comments.
fn flat_toml(raw: &str) -> Option<Fields> {
    let mut fields = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            return None; // [table] or [[array-of-tables]]
        }
        let (key, value) = split_toml_pair(line)?;
        let key = key.trim();
        let key = match key.strip_prefix('"').and_then(|k| k.strip_suffix('"')) {
            Some(k) => unescape_double(k),
            None => match key.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')) {
                Some(k) => k.to_string(),
                None => key.to_string(),
            },
        };
        if key.is_empty() {
            return None;
        }
        fields.push((key, toml_value(value.trim())?));
    }
    Some(fields)
}

/// Split at the first `=` outside a quoted key.
fn split_toml_pair(line: &str) -> Option<(&str, &str)> {
    let mut quote = None;
    for (i, b) in line.bytes().enumerate() {
        match (quote, b) {
            (None, b'"' | b'\'') => quote = Some(b),
            (Some(q), _) if b == q => quote = None,
            (None, b'=') => return Some((line.get(..i)?, line.get(i + 1..)?)),
            _ => {}
        }
    }
    None
}

fn toml_value(value: &str) -> Option<String> {
    if value.starts_with("\"\"\"") || value.starts_with("'''") {
        return None; // multi-line strings
    }
    if value.starts_with(['"', '\'']) {
        let end = closing_quote(value)?;
        let rest = value.get(end + 1..)?.trim();
        if !rest.is_empty() && !rest.starts_with('#') {
            return None;
        }
        let inner = value.get(1..end)?;
        return Some(if value.starts_with('"') {
            unescape_double(inner)
        } else {
            inner.to_string()
        });
    }
    if value.starts_with(['[', '{']) {
        let value = strip_toml_comment(value);
        return balanced_flow(value).then(|| value.to_string());
    }
    if value.is_empty() {
        return None;
    }
    Some(strip_toml_comment(value).to_string())
}

/// Cut a trailing `# comment` outside quotes.
fn strip_toml_comment(value: &str) -> &str {
    let mut quote = None;
    for (i, b) in value.bytes().enumerate() {
        match (quote, b) {
            (None, b'"' | b'\'') => quote = Some(b),
            (Some(q), _) if b == q => quote = None,
            (None, b'#') => return value.get(..i).unwrap_or(value).trim_end(),
            _ => {}
        }
    }
    value.trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(raw: &str) -> Option<Vec<(String, String)>> {
        parse(FrontMatterFormat::Yaml, raw).fields.map(|f| {
            f.into_iter()
                .map(|(k, v)| (k.into_string(), v.into_string()))
                .collect()
        })
    }

    fn toml(raw: &str) -> Option<Vec<(String, String)>> {
        parse(FrontMatterFormat::Toml, raw).fields.map(|f| {
            f.into_iter()
                .map(|(k, v)| (k.into_string(), v.into_string()))
                .collect()
        })
    }

    fn pairs(p: &[(&str, &str)]) -> Option<Vec<(String, String)>> {
        Some(p.iter().map(|&(k, v)| (k.into(), v.into())).collect())
    }

    #[test]
    fn flat_yaml_fields() {
        let raw = "title: \"Hello: world\"\n# comment\n\nauthor: Jane O'Neil\ntags: [a, b]\ndate: 2026-09-30 # c\nquote: 'it''s'\nempty:\n";
        assert_eq!(
            yaml(raw),
            pairs(&[
                ("title", "Hello: world"),
                ("author", "Jane O'Neil"),
                ("tags", "[a, b]"),
                ("date", "2026-09-30"),
                ("quote", "it's"),
                ("empty", ""),
            ])
        );
        assert_eq!(yaml("\"quoted key\": v\n"), pairs(&[("quoted key", "v")]));
        assert_eq!(
            yaml("url: http://x.org/a\n"),
            pairs(&[("url", "http://x.org/a")])
        );
    }

    #[test]
    fn nested_yaml_is_not_flat() {
        assert_eq!(yaml("authors:\n  - a\n  - b\n"), None);
        assert_eq!(yaml("- a\n"), None);
        assert_eq!(yaml("text: |\n  block\n"), None);
        assert_eq!(yaml("a: &anchor 1\n"), None);
        assert_eq!(yaml("just text\n"), None);
        assert_eq!(yaml("tags: [a, b\n"), None);
        assert_eq!(yaml("a: \"unterminated\n"), None);
    }

    #[test]
    fn flat_toml_fields() {
        let raw = "title = \"T \\\"q\\\"\"\ndraft = false # wip\ntags = [\"a\", \"b\"]\n'lit key' = 'x # y'\nn = 3\n";
        assert_eq!(
            toml(raw),
            pairs(&[
                ("title", "T \"q\""),
                ("draft", "false"),
                ("tags", "[\"a\", \"b\"]"),
                ("lit key", "x # y"),
                ("n", "3"),
            ])
        );
    }

    #[test]
    fn nested_toml_is_not_flat() {
        assert_eq!(toml("[extra]\na = 1\n"), None);
        assert_eq!(toml("a = \"\"\"\nmulti\n\"\"\"\n"), None);
        assert_eq!(toml("no equals\n"), None);
        assert_eq!(toml("a =\n"), None);
    }

    #[test]
    fn raw_is_kept() {
        let fm = parse(FrontMatterFormat::Yaml, "a:\n  b: c\n");
        assert_eq!(&*fm.raw, "a:\n  b: c\n");
        assert!(fm.fields.is_none());
        assert_eq!(yaml(""), pairs(&[]));
    }
}
