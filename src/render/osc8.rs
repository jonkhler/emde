//! OSC 8 hyperlinks.
//!
//! A link is written as `ESC ] 8 ; id=ID ; URL ESC \` … `ESC ] 8 ; ; ESC \`,
//! opened and closed around each fragment on each line, with the same `id`
//! for every fragment so terminals highlight a wrapped link as one.
//!
//! Only links that leave the document get a URL: web and mail links, never
//! with a scheme that runs code (`javascript:`, `vbscript:`, `data:`).
//! Anchors, footnotes and local files are for the pager to follow. URLs are
//! made safe: any control character (or the control picture it was turned
//! into when the document was read) rejects the link, bytes outside
//! `0x21..=0x7E` are percent-encoded, and a URL longer than 2 KiB is not
//! linked at all (it still shows as text).

use crate::ir::{Link, Target};

/// Longest URL (after encoding) that is linked.
pub const MAX_URL: usize = 2048;

/// Whether `c` is a control character or a Unicode control picture.
fn is_control_like(c: char) -> bool {
    c.is_control() || ('\u{2400}'..='\u{2426}').contains(&c)
}

/// The URL to put in an OSC 8 sequence, or `None` when the text must not
/// be linked.
pub fn encode_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.chars().any(is_control_like) {
        return None;
    }
    let mut out = String::with_capacity(url.len());
    for &b in url.as_bytes() {
        if (0x21..=0x7e).contains(&b) {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(hex(b >> 4));
            out.push(hex(b & 0xf));
        }
        if out.len() > MAX_URL {
            return None;
        }
    }
    Some(out)
}

fn hex(n: u8) -> char {
    char::from(if n < 10 { b'0' + n } else { b'A' + n - 10 })
}

/// Schemes that run code when opened: never linked.
const REFUSED_SCHEMES: &[&str] = &["javascript", "vbscript", "data"];

/// Whether a URL uses a scheme that runs code when opened.
fn refused_scheme(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| {
        REFUSED_SCHEMES
            .iter()
            .any(|r| scheme.trim().eq_ignore_ascii_case(r))
    })
}

/// The OSC 8 URL of a link: web and mail links only, and never a scheme
/// that runs code (`javascript:`, `vbscript:`, `data:`).
pub fn link_url(link: &Link) -> Option<String> {
    match link.target {
        Target::External | Target::Email if !refused_scheme(&link.url) => encode_url(&link.url),
        _ => None,
    }
}

/// Open a link: `ESC ] 8 ; id=<prefix>-<n> ; <url> ESC \`.
pub fn open(out: &mut Vec<u8>, id_prefix: &str, n: u32, url: &str) {
    out.extend_from_slice(b"\x1b]8;id=");
    out.extend_from_slice(id_prefix.as_bytes());
    out.push(b'-');
    out.extend_from_slice(n.to_string().as_bytes());
    out.push(b';');
    out.extend_from_slice(url.as_bytes());
    out.extend_from_slice(b"\x1b\\");
}

/// Close the open link.
pub fn close(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b]8;;\x1b\\");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::LinkKind;

    fn link(url: &str, target: Target) -> Link {
        Link {
            url: url.into(),
            title: "".into(),
            target,
            kind: LinkKind::Inline,
        }
    }

    #[test]
    fn printable_ascii_is_kept() {
        assert_eq!(
            encode_url("https://example.com/a?b=c&d#e").as_deref(),
            Some("https://example.com/a?b=c&d#e")
        );
    }

    #[test]
    fn others_are_percent_encoded() {
        assert_eq!(
            encode_url("https://x.org/a b/ü").as_deref(),
            Some("https://x.org/a%20b/%C3%BC")
        );
        assert_eq!(
            encode_url("https://x.org/日").as_deref(),
            Some("https://x.org/%E6%97%A5")
        );
    }

    #[test]
    fn controls_and_pictures_are_rejected() {
        assert_eq!(encode_url("https://x.org/\u{1b}]52;c;x"), None);
        assert_eq!(encode_url("https://x.org/\u{9b}"), None);
        assert_eq!(encode_url("https://x.org/␛]52"), None);
        assert_eq!(encode_url(""), None);
        assert_eq!(encode_url("  "), None);
    }

    #[test]
    fn long_urls_are_not_linked() {
        let long = format!("https://x.org/{}", "a".repeat(MAX_URL));
        assert_eq!(encode_url(&long), None);
        let fits = format!("https://x.org/{}", "a".repeat(MAX_URL - 14));
        assert_eq!(encode_url(&fits).map(|u| u.len()), Some(MAX_URL));
        // Encoding counts: 700 × `ü` is 4200 encoded bytes.
        assert_eq!(encode_url(&"ü".repeat(700)), None);
    }

    #[test]
    fn scripts_are_never_linked() {
        for url in [
            "javascript:alert(1)",
            "JavaScript:x",
            " vbscript:x",
            "data:text/html,x",
        ] {
            assert!(link_url(&link(url, Target::External)).is_none(), "{url}");
        }
        assert!(link_url(&link("https://a.b/javascript:x", Target::External)).is_some());
    }

    #[test]
    fn only_external_targets_are_linked() {
        assert!(link_url(&link("https://a.b", Target::External)).is_some());
        assert!(link_url(&link("mailto:a@b.c", Target::Email)).is_some());
        assert!(link_url(&link("#x", Target::Anchor("x".into()))).is_none());
        let local = Target::LocalFile("a.txt".into());
        assert!(link_url(&link("a.txt", local)).is_none());
    }

    #[test]
    fn sequences() {
        let mut out = Vec::new();
        open(&mut out, "e42", 7, "https://a.b");
        close(&mut out);
        assert_eq!(out, b"\x1b]8;id=e42-7;https://a.b\x1b\\\x1b]8;;\x1b\\");
    }
}
