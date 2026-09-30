//! Replacing control characters with visible Unicode *control pictures*.
//!
//! A document must never be able to drive the terminal: an `ESC` or a C1
//! control in Markdown text, code, link text or an entity like `&#27;` would
//! otherwise reach the output verbatim. Every control character except `\n`
//! and `\t` is therefore replaced:
//!
//! | Input | Shown as |
//! |---|---|
//! | C0 `U+0000`–`U+001F` (not `\t`, `\n`) | `U+2400`–`U+241F` (`␀` … `␟`, `ESC` → `␛`) |
//! | `DEL` `U+007F` | `␡` |
//! | C1 `U+0080`–`U+009F` | `␛` plus the 7-bit equivalent (`U+009B` CSI → `␛[`) |

use std::borrow::Cow;

/// Whether `c` is replaced by [`sanitize`].
#[inline]
pub fn is_unsafe_control(c: char) -> bool {
    matches!(c, '\0'..='\x08' | '\x0b'..='\x1f' | '\x7f'..='\u{9f}')
}

/// Append the visible replacement of a control character to `out`.
///
/// Returns `false` (and appends nothing) when `c` is not an unsafe control.
pub fn push_picture(out: &mut String, c: char) -> bool {
    let cp = u32::from(c);
    let picture = match cp {
        0x00..=0x08 | 0x0b..=0x1f => char::from_u32(0x2400 + cp),
        0x7f => Some('\u{2421}'),
        0x80..=0x9f => {
            out.push('\u{241b}');
            char::from_u32(cp - 0x40)
        }
        _ => None,
    };
    match picture {
        Some(p) => {
            out.push(p);
            true
        }
        None => false,
    }
}

/// Whether `s` contains a character that [`sanitize`] would replace.
///
/// A byte scan: C0 controls and `DEL` are single bytes, C1 controls are
/// encoded as `0xC2 0x80..=0x9F`.
#[inline]
pub fn needs_sanitize(s: &str) -> bool {
    let b = s.as_bytes();
    b.iter().enumerate().any(|(i, &c)| {
        (c < 0x20 && c != b'\n' && c != b'\t')
            || c == 0x7f
            || (c == 0xc2 && b.get(i + 1).is_some_and(|n| (0x80..=0x9f).contains(n)))
    })
}

/// Replace unsafe control characters (see the module docs) with visible
/// control pictures. Borrows when nothing needs replacing.
pub fn sanitize(s: &str) -> Cow<'_, str> {
    if !needs_sanitize(s) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if !push_picture(&mut out, c) {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_newline_and_tab() {
        assert!(matches!(sanitize("a\tb\nc"), Cow::Borrowed(_)));
        assert!(!needs_sanitize("plain © text — ok\u{a0}"));
    }

    #[test]
    fn replaces_c0_del_and_esc() {
        assert_eq!(sanitize("\x1b[31mred"), "␛[31mred");
        assert_eq!(sanitize("a\0b\x07c\x7f"), "a␀b␇c␡");
        assert_eq!(sanitize("\r"), "␍");
        assert_eq!(sanitize("\x0b\x0c"), "␋␌");
    }

    #[test]
    fn replaces_c1_with_escaped_form() {
        assert_eq!(sanitize("x\u{9b}2Jy"), "x␛[2Jy");
        assert_eq!(sanitize("\u{85}"), "␛E");
        assert_eq!(sanitize("\u{9d}8;;"), "␛]8;;");
        assert_eq!(sanitize("\u{80}"), "␛@");
    }

    #[test]
    fn leaves_other_latin1_alone() {
        // U+00A0..U+00FF share the 0xC2/0xC3 lead bytes but are not controls.
        assert_eq!(sanitize("\u{a0}\u{a9}\u{ad}"), "\u{a0}\u{a9}\u{ad}");
        assert!(!needs_sanitize("\u{a0}"));
    }

    #[test]
    fn predicate_matches_replacement() {
        for cp in 0..0x250u32 {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let mut s = String::new();
            assert_eq!(push_picture(&mut s, c), is_unsafe_control(c), "U+{cp:04X}");
            let text = c.to_string();
            assert_eq!(needs_sanitize(&text), is_unsafe_control(c), "U+{cp:04X}");
        }
    }
}
