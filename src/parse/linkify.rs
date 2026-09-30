//! Bare URLs and email addresses in text (GFM autolink literals).
//!
//! pulldown-cmark only links `<…>` autolinks. This finds `https://…`-style
//! URLs, `www.` domains and email addresses in plain text with the
//! `linkify` crate, which already excludes trailing punctuation and
//! unbalanced closing brackets. Like GitHub, a domain without a scheme is
//! only linked when it starts with `www.` (so `README.md` stays text).

use std::ops::Range;

use linkify::{LinkFinder, LinkKind};

/// A link found in text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    /// Byte range of the link text.
    pub(crate) range: Range<usize>,
    /// The destination (`http://` added to `www.` links, `mailto:` to emails).
    pub(crate) url: String,
    /// An email address rather than a URL.
    pub(crate) email: bool,
}

/// Append the links found in `text` to `out`.
pub(crate) fn find(text: &str, out: &mut Vec<Found>) {
    let bytes = text.as_bytes();
    // Cheap checks first: this runs for every stretch of plain text.
    let has_www = has_www(bytes);
    if !has_www && memchr::memchr2(b':', b'@', bytes).is_none() {
        return;
    }
    let mut finder = LinkFinder::new();
    finder.url_must_have_scheme(!has_www);
    for link in finder.links(text) {
        let s = link.as_str();
        let (url, email) = match link.kind() {
            LinkKind::Email => (format!("mailto:{s}"), true),
            LinkKind::Url if s.contains("://") => (s.to_string(), false),
            LinkKind::Url if s.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("www.")) => {
                (format!("http://{s}"), false)
            }
            _ => continue,
        };
        out.push(Found {
            range: link.start()..link.end(),
            url,
            email,
        });
    }
}

/// Whether `bytes` contains `www.` (in any case).
fn has_www(bytes: &[u8]) -> bool {
    memchr::memchr_iter(b'.', bytes).any(|i| {
        i.checked_sub(3)
            .and_then(|s| bytes.get(s..i))
            .is_some_and(|w| w.eq_ignore_ascii_case(b"www"))
    })
}

/// Whether a link text looks like a URL (`scheme://…` or `www.…`).
pub(crate) fn looks_like_url(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.windows(3).any(|w| w == b"://")
        || bytes
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case(b"www."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(text: &str) -> Vec<(&str, String)> {
        let mut out = Vec::new();
        find(text, &mut out);
        out.into_iter()
            .map(|f| (&text[f.range.clone()], f.url))
            .collect()
    }

    #[test]
    fn urls_with_schemes() {
        assert_eq!(
            links("see https://example.com/a?b=c."),
            [(
                "https://example.com/a?b=c",
                "https://example.com/a?b=c".to_string()
            )]
        );
        assert_eq!(
            links("(http://x.org/wiki/Foo_(bar)) done"),
            [(
                "http://x.org/wiki/Foo_(bar)",
                "http://x.org/wiki/Foo_(bar)".to_string()
            )]
        );
        assert_eq!(
            links("ftp://files.example.org, ok")[0].0,
            "ftp://files.example.org"
        );
    }

    #[test]
    fn www_domains_get_http() {
        assert_eq!(
            links("visit www.example.com today"),
            [("www.example.com", "http://www.example.com".to_string())]
        );
    }

    #[test]
    fn bare_domains_and_files_are_not_links() {
        assert!(links("edit README.md or example.com").is_empty());
        assert!(links("version 1.2.3: done").is_empty());
        assert!(links("ratio 3:4").is_empty());
    }

    #[test]
    fn emails() {
        assert_eq!(
            links("mail me@example.org."),
            [("me@example.org", "mailto:me@example.org".to_string())]
        );
        assert!(links("user@localhost").is_empty());
    }

    #[test]
    fn url_detection_helpers() {
        assert!(has_www(b"see WwW.x.org"));
        assert!(!has_www(b"ww.x.org .www"));
        assert!(looks_like_url("https://x"));
        assert!(looks_like_url("WWW.example.com"));
        assert!(!looks_like_url("docs"));
    }

    #[test]
    fn mixed() {
        let got = links("a www.x.org b https://y.org c z@w.org, README.md");
        let texts: Vec<&str> = got.iter().map(|(t, _)| *t).collect();
        assert_eq!(texts, ["www.x.org", "https://y.org", "z@w.org"]);
    }
}
