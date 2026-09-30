//! Classifying link destinations into [`Target`]s.
//!
//! * `#frag` → [`Target::Anchor`] (percent-decoded; GitHub's
//!   `user-content-` prefix removed).
//! * A URL scheme (`https:`, `ftp:`, …; at least two letters, so `C:` is not
//!   one) or `//host` → [`Target::External`]; `mailto:` → [`Target::Email`];
//!   `file:` URLs are treated as local paths.
//! * Anything else is a local path: Markdown files (`.md`, `.markdown`, …)
//!   and directories (trailing `/`) are [`Target::LocalDoc`], other files
//!   [`Target::LocalFile`]. Query strings are dropped.

use std::borrow::Cow;
use std::path::PathBuf;

use crate::ir::Target;

/// Classify a link destination.
pub(crate) fn classify(url: &str) -> Target {
    let url = url.trim();
    if let Some(frag) = url.strip_prefix('#') {
        return Target::Anchor(anchor_name(frag).into());
    }
    if let Some(scheme) = scheme(url) {
        if scheme.eq_ignore_ascii_case("mailto") {
            return Target::Email;
        }
        if scheme.eq_ignore_ascii_case("file") {
            let rest = url.get(scheme.len() + 1..).unwrap_or("");
            let rest = rest.strip_prefix("//").unwrap_or(rest);
            let rest = rest.strip_prefix("localhost").unwrap_or(rest);
            return local(rest);
        }
        return Target::External;
    }
    if url.starts_with("//") {
        return Target::External;
    }
    local(url)
}

/// Classify a wiki link (`[[Page Name#Section]]`): a page name without an
/// extension refers to `Page Name.md`.
pub(crate) fn classify_wiki(dest: &str) -> Target {
    let (page, frag) = match dest.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (dest, None),
    };
    let page = page.trim();
    if page.is_empty() {
        return Target::Anchor(anchor_name(frag.unwrap_or("")).into());
    }
    let mut path = PathBuf::from(page);
    if path.extension().is_none() {
        path.set_extension("md");
    }
    Target::LocalDoc {
        path,
        anchor: frag.map(|f| anchor_name(f).into()),
    }
}

/// A local path with an optional `#fragment` and `?query`.
fn local(url: &str) -> Target {
    let (path, frag) = match url.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (url, None),
    };
    let path = path.split_once('?').map_or(path, |(p, _)| p);
    if path.is_empty() {
        return Target::Anchor(anchor_name(frag.unwrap_or("")).into());
    }
    let decoded = percent_decode(path);
    if is_markdown_path(&decoded) || decoded.ends_with('/') {
        Target::LocalDoc {
            path: PathBuf::from(decoded.as_ref()),
            anchor: frag.map(|f| anchor_name(f).into()),
        }
    } else {
        Target::LocalFile(PathBuf::from(decoded.as_ref()))
    }
}

/// A fragment as an anchor name: percent-decoded, `user-content-` removed.
fn anchor_name(frag: &str) -> String {
    let decoded = percent_decode(frag);
    decoded
        .strip_prefix("user-content-")
        .unwrap_or(&decoded)
        .to_string()
}

/// The scheme of a URL (`[A-Za-z][A-Za-z0-9+.-]+` before a `:`).
fn scheme(url: &str) -> Option<&str> {
    let colon = url.find(':')?;
    let scheme = url.get(..colon)?;
    let mut bytes = scheme.bytes();
    let first_ok = bytes.next().is_some_and(|b| b.is_ascii_alphabetic());
    let rest_ok = bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    (first_ok && rest_ok && scheme.len() >= 2).then_some(scheme)
}

/// Whether a path names a Markdown file.
pub(crate) fn is_markdown_path(path: &str) -> bool {
    const EXTS: &[&str] = &["md", "markdown", "mdown", "mkd", "mkdn", "mdwn", "mdtxt"];
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && EXTS.iter().any(|e| ext.eq_ignore_ascii_case(e))
    })
}

/// Decode `%XX` escapes (invalid UTF-8 becomes `U+FFFD`; malformed escapes
/// are kept as written).
pub(crate) fn percent_decode(s: &str) -> Cow<'_, str> {
    if !s.contains('%') {
        return Cow::Borrowed(s);
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = |j: usize| bytes.get(j).and_then(|&h| (h as char).to_digit(16));
        match (b, hex(i + 1), hex(i + 2)) {
            (b'%', Some(hi), Some(lo)) => {
                out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
                i += 3;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    Cow::Owned(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(path: &str, anchor: Option<&str>) -> Target {
        Target::LocalDoc {
            path: path.into(),
            anchor: anchor.map(Into::into),
        }
    }

    #[test]
    fn anchors() {
        assert_eq!(classify("#install"), Target::Anchor("install".into()));
        assert_eq!(
            classify("#user-content-install"),
            Target::Anchor("install".into())
        );
        assert_eq!(classify("#caf%C3%A9"), Target::Anchor("café".into()));
        assert_eq!(classify(""), Target::Anchor("".into()));
        assert_eq!(classify("?tab=readme"), Target::Anchor("".into()));
    }

    #[test]
    fn external_and_email() {
        assert_eq!(classify("https://example.com/a"), Target::External);
        assert_eq!(classify("HTTP://EXAMPLE.COM"), Target::External);
        assert_eq!(classify("ftp://x"), Target::External);
        assert_eq!(classify("//cdn.example.com/x.png"), Target::External);
        assert_eq!(classify("git+ssh://host/repo"), Target::External);
        assert_eq!(classify("mailto:me@example.org"), Target::Email);
    }

    #[test]
    fn local_documents_and_files() {
        assert_eq!(classify("docs/guide.md"), doc("docs/guide.md", None));
        assert_eq!(
            classify("./README.markdown#usage"),
            doc("./README.markdown", Some("usage"))
        );
        assert_eq!(classify("docs/"), doc("docs/", None));
        assert_eq!(classify("My%20Notes.md"), doc("My Notes.md", None));
        assert_eq!(
            classify("guide.md?plain=1#L10"),
            doc("guide.md", Some("L10"))
        );
        assert_eq!(classify("image.png"), Target::LocalFile("image.png".into()));
        assert_eq!(
            classify("/abs/file.txt"),
            Target::LocalFile("/abs/file.txt".into())
        );
        assert_eq!(classify(".md"), Target::LocalFile(".md".into()));
        assert_eq!(classify("file:///tmp/notes.md"), doc("/tmp/notes.md", None));
        // A drive letter is not a scheme.
        assert_eq!(classify("C:/x.md"), doc("C:/x.md", None));
    }

    #[test]
    fn wiki_links() {
        assert_eq!(classify_wiki("Page Name"), doc("Page Name.md", None));
        assert_eq!(
            classify_wiki("notes.txt#top"),
            doc("notes.txt", Some("top"))
        );
        assert_eq!(classify_wiki("#local"), Target::Anchor("local".into()));
    }

    #[test]
    fn percent_decoding_is_total() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%FF"), "\u{fffd}");
        assert!(matches!(percent_decode("plain"), Cow::Borrowed(_)));
    }
}
