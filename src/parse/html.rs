//! A small, tolerant HTML tag lexer and entity decoder.
//!
//! Markdown documents use a little HTML (`<br>`, `<img>`, `<details>`,
//! `<p align="center">`, …). This lexer splits such fragments into text,
//! tags and comments. It never fails: anything that is not a complete tag is
//! text, an unterminated comment runs to the end of the input, and attribute
//! values are entity-decoded on request.

use std::borrow::Cow;

use crate::ir::Length;

/// A lexical token of an HTML fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Token<'a> {
    /// Text between tags (entities not yet decoded).
    Text(&'a str),
    /// An opening (or self-closing) tag.
    Start(Tag<'a>),
    /// A closing tag, with the name as written.
    End(&'a str),
    /// `<!-- … -->`.
    Comment,
    /// `<!DOCTYPE …>`, `<?…?>`, `<![CDATA[…]]>`.
    Other,
}

/// An opening tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tag<'a> {
    /// The tag name as written (compare with [`Tag::is`]).
    pub(crate) name: &'a str,
    /// The raw attribute text.
    attrs: &'a str,
    /// Written as `<name … />`.
    pub(crate) self_closing: bool,
}

impl<'a> Tag<'a> {
    /// Whether this tag has the given (lowercase) name.
    pub(crate) fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    /// The entity-decoded value of an attribute (`""` for a bare attribute).
    pub(crate) fn attr(&self, name: &str) -> Option<Cow<'a, str>> {
        Attrs { rest: self.attrs }
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| decode_entities(v))
    }
}

/// Iterator over `(name, raw value)` attribute pairs.
struct Attrs<'a> {
    rest: &'a str,
}

impl<'a> Iterator for Attrs<'a> {
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        let s = self
            .rest
            .trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '/');
        if s.is_empty() {
            self.rest = s;
            return None;
        }
        let name_len = s
            .find(|c: char| c.is_ascii_whitespace() || c == '=' || c == '/')
            .unwrap_or(s.len())
            .max(1);
        let name = s.get(..name_len).unwrap_or(s);
        let after = s.get(name_len..).unwrap_or("").trim_start();
        let Some(value_part) = after.strip_prefix('=') else {
            self.rest = after;
            return Some((name, ""));
        };
        let v = value_part.trim_start();
        let (value, rest) = match v.as_bytes().first() {
            Some(&q @ (b'"' | b'\'')) => {
                let body = v.get(1..).unwrap_or("");
                match body.find(q as char) {
                    Some(end) => (
                        body.get(..end).unwrap_or(""),
                        body.get(end + 1..).unwrap_or(""),
                    ),
                    None => (body, ""),
                }
            }
            _ => {
                let end = v.find(|c: char| c.is_ascii_whitespace()).unwrap_or(v.len());
                (v.get(..end).unwrap_or(v), v.get(end..).unwrap_or(""))
            }
        };
        self.rest = rest;
        Some((name, value))
    }
}

/// Splits an HTML fragment into [`Token`]s.
#[derive(Clone, Debug)]
pub(crate) struct Lexer<'a> {
    rest: &'a str,
}

impl<'a> Lexer<'a> {
    /// A lexer over an HTML fragment.
    pub(crate) fn new(input: &'a str) -> Lexer<'a> {
        Lexer { rest: input }
    }
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        if self.rest.is_empty() {
            return None;
        }
        if self.rest.starts_with('<') {
            if let Some((token, len)) = markup(self.rest) {
                self.rest = self.rest.get(len..).unwrap_or("");
                return Some(token);
            }
            // A '<' that starts no markup is text.
            let (lt, rest) = self.rest.split_at_checked(1).unwrap_or((self.rest, ""));
            self.rest = rest;
            return Some(Token::Text(lt));
        }
        let end = memchr::memchr(b'<', self.rest.as_bytes()).unwrap_or(self.rest.len());
        let (text, rest) = self.rest.split_at_checked(end).unwrap_or((self.rest, ""));
        self.rest = rest;
        Some(Token::Text(text))
    }
}

/// Lex the markup at the start of `s` (which starts with `<`): the token and
/// its length, or `None` if it is not markup.
fn markup(s: &str) -> Option<(Token<'_>, usize)> {
    let bytes = s.as_bytes();
    match bytes.get(1)? {
        b'!' if s.starts_with("<!--") => {
            let body = s.get(4..).unwrap_or("");
            let len = if body.starts_with('>') {
                5 // <!-->
            } else if body.starts_with("->") {
                6 // <!--->
            } else {
                body.find("-->").map_or(s.len(), |i| 4 + i + 3)
            };
            Some((Token::Comment, len))
        }
        b'!' if s.starts_with("<![CDATA[") => {
            let len = s.find("]]>").map_or(s.len(), |i| i + 3);
            Some((Token::Other, len))
        }
        b'!' | b'?' => s.find('>').map(|i| (Token::Other, i + 1)),
        b'/' => {
            let name = tag_name(s.get(2..)?)?;
            let close = s.find('>')?;
            Some((Token::End(name), close + 1))
        }
        b if b.is_ascii_alphabetic() => {
            let name = tag_name(s.get(1..)?)?;
            let after = 1 + name.len();
            let close = tag_end(bytes, after)?;
            let inner = s.get(after..close).unwrap_or("");
            let self_closing = inner.trim_end().ends_with('/');
            let attrs = if self_closing {
                inner.trim_end().strip_suffix('/').unwrap_or(inner)
            } else {
                inner
            };
            // The name must be followed by whitespace, '/' or '>'.
            if !inner.is_empty()
                && !inner.starts_with(|c: char| c.is_ascii_whitespace() || c == '/')
            {
                return None;
            }
            Some((
                Token::Start(Tag {
                    name,
                    attrs,
                    self_closing,
                }),
                close + 1,
            ))
        }
        _ => None,
    }
}

/// The tag name at the start of `s`: an ASCII letter followed by letters,
/// digits, `-`, `_`, `:` or `.`.
fn tag_name(s: &str) -> Option<&str> {
    let first = s.bytes().next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let len = s
        .bytes()
        .position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.')))
        .unwrap_or(s.len());
    s.get(..len)
}

/// Index of the `>` ending a start tag, skipping quoted attribute values.
fn tag_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    let mut after_eq = false;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'>' => return Some(i),
            b'"' | b'\'' if after_eq => {
                let close = bytes.get(i + 1..)?.iter().position(|&c| c == b)?;
                i += close + 2;
                after_eq = false;
                continue;
            }
            b'=' => after_eq = true,
            b if b.is_ascii_whitespace() => {}
            _ => after_eq = false,
        }
        i += 1;
    }
    None
}

/// Decode HTML character references: about a hundred named entities and
/// all numeric ones. Unknown references are kept as written.
pub(crate) fn decode_entities(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or(""));
        let after = rest.get(amp + 1..).unwrap_or("");
        match entity(after) {
            Some((decoded, len)) => {
                out.push_str(&decoded);
                rest = after.get(len..).unwrap_or("");
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Decode the reference after an `&`: the text and the bytes consumed
/// (including the `;`).
fn entity(s: &str) -> Option<(Cow<'static, str>, usize)> {
    // The longest reference name is well under 32 bytes.
    let semi = s.bytes().take(34).position(|b| b == b';')?;
    let body = s.get(..semi)?;
    let decoded = if let Some(num) = body.strip_prefix('#') {
        let (digits, radix) = match num.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16),
            None => (num, 10),
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return None;
        }
        // Too many digits overflow to an invalid code point (→ U+FFFD).
        let cp = u32::from_str_radix(digits, radix).unwrap_or(u32::MAX);
        Cow::Owned(numeric_char(cp).to_string())
    } else {
        Cow::Borrowed(named_entity(body)?)
    };
    Some((decoded, semi + 1))
}

/// The character for a numeric reference, per the HTML standard: invalid
/// code points become U+FFFD, and 0x80–0x9F map through Windows-1252.
fn numeric_char(cp: u32) -> char {
    const CP1252: [u32; 32] = [
        0x20ac, 0x81, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8d, 0x017d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013,
        0x2014, 0x02dc, 0x2122, 0x0161, 0x203a, 0x0153, 0x9d, 0x017e, 0x0178,
    ];
    let cp = match cp {
        0 => 0xfffd,
        0x80..=0x9f => CP1252.get((cp - 0x80) as usize).copied().unwrap_or(0xfffd),
        _ => cp,
    };
    char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER)
}

/// Named character references (case-sensitive, as in HTML).
fn named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "ensp" => "\u{2002}",
        "emsp" => "\u{2003}",
        "thinsp" => "\u{2009}",
        "zwnj" => "\u{200c}",
        "zwj" => "\u{200d}",
        "shy" => "\u{ad}",
        "ndash" => "–",
        "mdash" => "—",
        "lsquo" => "‘",
        "rsquo" => "’",
        "sbquo" => "‚",
        "ldquo" => "“",
        "rdquo" => "”",
        "bdquo" => "„",
        "laquo" => "«",
        "raquo" => "»",
        "lsaquo" => "‹",
        "rsaquo" => "›",
        "hellip" => "…",
        "bull" => "•",
        "middot" => "·",
        "prime" => "′",
        "Prime" => "″",
        "dagger" => "†",
        "Dagger" => "‡",
        "permil" => "‰",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "deg" => "°",
        "plusmn" => "±",
        "times" => "×",
        "divide" => "÷",
        "micro" => "µ",
        "para" => "¶",
        "sect" => "§",
        "cent" => "¢",
        "pound" => "£",
        "euro" => "€",
        "yen" => "¥",
        "curren" => "¤",
        "iexcl" => "¡",
        "iquest" => "¿",
        "not" => "¬",
        "ordf" => "ª",
        "ordm" => "º",
        "sup1" => "¹",
        "sup2" => "²",
        "sup3" => "³",
        "frac14" => "¼",
        "frac12" => "½",
        "frac34" => "¾",
        "larr" => "←",
        "uarr" => "↑",
        "rarr" => "→",
        "darr" => "↓",
        "harr" => "↔",
        "lArr" => "⇐",
        "rArr" => "⇒",
        "hArr" => "⇔",
        "crarr" => "↵",
        "le" => "≤",
        "ge" => "≥",
        "ne" => "≠",
        "asymp" => "≈",
        "equiv" => "≡",
        "infin" => "∞",
        "minus" => "−",
        "sum" => "∑",
        "prod" => "∏",
        "radic" => "√",
        "part" => "∂",
        "nabla" => "∇",
        "isin" => "∈",
        "notin" => "∉",
        "forall" => "∀",
        "exist" => "∃",
        "empty" => "∅",
        "cap" => "∩",
        "cup" => "∪",
        "and" => "∧",
        "or" => "∨",
        "sim" => "∼",
        "prop" => "∝",
        "there4" => "∴",
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ε",
        "theta" => "θ",
        "lambda" => "λ",
        "mu" => "μ",
        "pi" => "π",
        "sigma" => "σ",
        "tau" => "τ",
        "phi" => "φ",
        "omega" => "ω",
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Phi" => "Φ",
        "Omega" => "Ω",
        "hearts" => "♥",
        "spades" => "♠",
        "clubs" => "♣",
        "diams" => "♦",
        "check" => "✓",
        "cross" => "✗",
        "star" => "☆",
        "starf" => "★",
        "auml" => "ä",
        "ouml" => "ö",
        "uuml" => "ü",
        "Auml" => "Ä",
        "Ouml" => "Ö",
        "Uuml" => "Ü",
        "szlig" => "ß",
        "eacute" => "é",
        "egrave" => "è",
        "aacute" => "á",
        "agrave" => "à",
        "ccedil" => "ç",
        "ntilde" => "ñ",
        _ => return None,
    })
}

/// Parse an HTML length: `200`, `200px`, `12.5` (truncated) or `50%`.
pub(crate) fn parse_length(s: &str) -> Option<Length> {
    let s = s.trim();
    let (num, percent) = match s.strip_suffix('%') {
        Some(n) => (n.trim_end(), true),
        None => (s.strip_suffix("px").unwrap_or(s).trim_end(), false),
    };
    let int = num.split_once('.').map_or(num, |(i, _)| i);
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = int.parse::<u32>().ok()?;
    Some(if percent {
        Length::Percent(n)
    } else {
        Length::Px(n)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(s: &str) -> Vec<Token<'_>> {
        Lexer::new(s).collect()
    }

    fn start<'a>(t: &Token<'a>) -> Tag<'a> {
        match t {
            Token::Start(tag) => tag.clone(),
            other => panic!("not a start tag: {other:?}"),
        }
    }

    #[test]
    fn text_and_tags() {
        let t = tokens("a <b>bold</b> c");
        assert_eq!(t.len(), 5);
        assert_eq!(t[0], Token::Text("a "));
        assert!(start(&t[1]).is("b"));
        assert_eq!(t[2], Token::Text("bold"));
        assert_eq!(t[3], Token::End("b"));
        assert_eq!(t[4], Token::Text(" c"));
    }

    #[test]
    fn attributes() {
        let t = tokens(r#"<IMG src="a.png" alt='x > y' width=200 hidden data-x = "1"/>"#);
        assert_eq!(t.len(), 1);
        let tag = start(&t[0]);
        assert!(tag.is("img"));
        assert!(tag.self_closing);
        assert_eq!(tag.attr("SRC").as_deref(), Some("a.png"));
        assert_eq!(tag.attr("alt").as_deref(), Some("x > y"));
        assert_eq!(tag.attr("width").as_deref(), Some("200"));
        assert_eq!(tag.attr("hidden").as_deref(), Some(""));
        assert_eq!(tag.attr("data-x").as_deref(), Some("1"));
        assert_eq!(tag.attr("missing"), None);
    }

    #[test]
    fn attribute_entities_are_decoded() {
        let t = tokens(r#"<a href="?a=1&amp;b=2" title="&quot;hi&quot;">"#);
        let tag = start(&t[0]);
        assert_eq!(tag.attr("href").as_deref(), Some("?a=1&b=2"));
        assert_eq!(tag.attr("title").as_deref(), Some("\"hi\""));
    }

    #[test]
    fn comments_and_other_markup() {
        assert_eq!(tokens("<!-- x -->y"), [Token::Comment, Token::Text("y")]);
        assert_eq!(tokens("<!-- open"), [Token::Comment]);
        assert_eq!(tokens("<!-->a"), [Token::Comment, Token::Text("a")]);
        assert_eq!(tokens("<!--->a"), [Token::Comment, Token::Text("a")]);
        assert_eq!(tokens("<!DOCTYPE html>"), [Token::Other]);
        assert_eq!(tokens("<?php x ?>"), [Token::Other]);
        assert_eq!(tokens("<![CDATA[x<y]]>z"), [Token::Other, Token::Text("z")]);
    }

    #[test]
    fn stray_angle_brackets_are_text() {
        assert_eq!(
            tokens("a < b"),
            [Token::Text("a "), Token::Text("<"), Token::Text(" b")]
        );
        assert_eq!(tokens("<3"), [Token::Text("<"), Token::Text("3")]);
        assert_eq!(tokens("<b"), [Token::Text("<"), Token::Text("b")]);
        assert_eq!(tokens("</ b>"), [Token::Text("<"), Token::Text("/ b>")]);
        assert_eq!(tokens(r#"<a title="unterminated>"#).len(), 2);
        // `<a.b>` is a (custom) tag, `<abc@example>` is not.
        assert!(matches!(tokens("<x.y>")[0], Token::Start(_)));
        assert_eq!(tokens("<me@x.org>")[0], Token::Text("<"));
    }

    #[test]
    fn end_tags_tolerate_junk() {
        assert_eq!(tokens("</DIV >"), [Token::End("DIV")]);
        assert_eq!(tokens("</p foo>"), [Token::End("p")]);
    }

    #[test]
    fn entity_decoding() {
        assert_eq!(
            decode_entities("AT&amp;T &lt;tag&gt; &copy; &hellip;"),
            "AT&T <tag> © …"
        );
        assert_eq!(decode_entities("&#65;&#x42;&#X43;"), "ABC");
        assert_eq!(
            decode_entities("&#0; &#xD800; &#x110000; &#150;"),
            "\u{fffd} \u{fffd} \u{fffd} –"
        );
        assert_eq!(decode_entities("&nosuch; & &amp &;"), "&nosuch; & &amp &;");
        assert_eq!(
            decode_entities("&#27;"),
            "\u{1b}",
            "sanitising is the caller's job"
        );
        assert_eq!(decode_entities("&#99999999999;"), "\u{fffd}");
        assert!(matches!(decode_entities("plain"), Cow::Borrowed(_)));
    }

    #[test]
    fn lengths() {
        assert_eq!(parse_length("200"), Some(Length::Px(200)));
        assert_eq!(parse_length(" 120px "), Some(Length::Px(120)));
        assert_eq!(parse_length("12.5"), Some(Length::Px(12)));
        assert_eq!(parse_length("50%"), Some(Length::Percent(50)));
        assert_eq!(parse_length("auto"), None);
        assert_eq!(parse_length(""), None);
        assert_eq!(parse_length("-3"), None);
    }
}
