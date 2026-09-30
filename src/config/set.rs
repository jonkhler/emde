//! `--set KEY=VALUE`: one setting as a one-line TOML document.
//!
//! The argument becomes `KEY = VALUE`. When that is not valid TOML, or the
//! value has the wrong type for the key, the value is quoted and tried
//! again, so bare words work: `--set theme.code=Nord`,
//! `--set pager.open=open -a Safari`, `--set theme.code=1337`. Key segments
//! that are not bare TOML keys are quoted too (`code.aliases.c++=cpp`).

use std::borrow::Cow;

use super::de::{Origin, Parsed, parse_config};
use super::layer::ConfigLayer;
use super::{Diagnostic, Severity};

/// Parse one `--set` argument. Problems are reported into `diags`; a
/// setting that cannot be used yields `None`.
pub(crate) fn parse_set(arg: &str, diags: &mut Vec<Diagnostic>) -> Option<Parsed<ConfigLayer>> {
    let origin = Origin::Set(arg.to_owned());
    let fail = |diags: &mut Vec<Diagnostic>, message: &str| {
        diags.push(Diagnostic {
            severity: Severity::Error,
            location: origin.to_string(),
            message: message.to_owned(),
        });
    };
    let Some((key, value)) = arg.split_once('=') else {
        fail(
            diags,
            "expected KEY=VALUE, e.g. `--set render.max_width=80`",
        );
        return None;
    };
    let key = match toml_key(key.trim()) {
        Ok(key) => key,
        Err(msg) => {
            fail(diags, &msg);
            return None;
        }
    };
    let value = value.trim();

    let bare_doc = format!("{key} = {value}");
    let (first, first_diags) = attempt(&bare_doc, &origin);
    if !has_errors(&first_diags) {
        diags.extend(first_diags);
        return Some(first);
    }
    let (quoted, quoted_diags) = attempt(&format!("{key} = {}", quote(value)), &origin);
    if !has_errors(&quoted_diags) {
        diags.extend(quoted_diags);
        return Some(quoted);
    }
    // Report the problem with the value as meant: when the bare value is not
    // TOML at all, the quoted attempt's type error says what is wrong.
    let syntax_error = toml::de::DeTable::parse(&bare_doc).is_err();
    diags.extend(if syntax_error {
        quoted_diags
    } else {
        first_diags
    });
    None
}

fn attempt(doc: &str, origin: &Origin) -> (Parsed<ConfigLayer>, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let parsed = parse_config(Cow::Owned(doc.to_owned()), origin.clone(), &mut diags);
    (parsed, diags)
}

fn has_errors(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| d.severity == Severity::Error)
}

/// A dotted key with every segment that is not a bare key quoted.
fn toml_key(key: &str) -> Result<String, String> {
    if key.is_empty() {
        return Err("missing key before `=`".into());
    }
    if key.chars().any(char::is_control) {
        return Err("control characters are not allowed in keys".into());
    }
    if key.contains(['"', '\'']) {
        // Already quoted by the user; TOML will judge it.
        return Ok(key.to_owned());
    }
    let mut out = Vec::new();
    for seg in key.split('.') {
        let seg = seg.trim();
        if seg.is_empty() {
            return Err(format!("empty segment in key `{key}`"));
        }
        let bare = seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        out.push(if bare { seg.to_owned() } else { quote(seg) });
    }
    Ok(out.join("."))
}

/// A TOML basic string.
fn quote(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::When;
    use crate::theme::color::ColorSpec;
    use crate::theme::element::Element;

    fn set(arg: &str) -> (Option<ConfigLayer>, Vec<Diagnostic>) {
        let mut diags = Vec::new();
        let layer = parse_set(arg, &mut diags).map(|p| p.value);
        (layer, diags)
    }

    fn ok(arg: &str) -> ConfigLayer {
        let (layer, diags) = set(arg);
        assert!(diags.is_empty(), "{arg}: {diags:?}");
        layer.unwrap()
    }

    #[test]
    fn typed_values() {
        assert_eq!(ok("render.max_width=80").render.max_width, Some(80));
        assert_eq!(ok(" render.margin = 3 ").render.margin, Some(3));
        assert_eq!(ok("pager.mouse=false").pager.mouse, Some(false));
        assert_eq!(
            ok("render.hyperlinks=never").render.hyperlinks,
            Some(When::Never)
        );
        assert_eq!(
            ok("glyphs.bullets=[\"-\", \"*\"]").glyphs.bullets,
            Some(vec!["-".to_owned(), "*".to_owned()])
        );
        assert_eq!(ok("images.max_pixels=1_000").images.max_pixels, Some(1000));
    }

    #[test]
    fn bare_words_are_quoted() {
        assert_eq!(ok("theme.code=Nord").theme.code.as_deref(), Some("Nord"));
        assert_eq!(
            ok("theme.code=Monokai Extended").theme.code.as_deref(),
            Some("Monokai Extended")
        );
        assert_eq!(
            ok("theme.code=1337").theme.code.as_deref(),
            Some("1337"),
            "retyped"
        );
        assert_eq!(
            ok("theme.name=\"emde\"").theme.name.as_deref(),
            Some("emde")
        );
        assert_eq!(
            ok("glyphs.quote=|").glyphs.quote.as_deref(),
            Some("|"),
            "not valid TOML on its own"
        );
        assert_eq!(ok("glyphs.rule=\\").glyphs.rule.as_deref(), Some("\\"));
        assert!(matches!(
            ok("pager.open=open -a Safari").pager.open,
            Some(super::super::OpenCommand::Command(ref c)) if c == "open -a Safari"
        ));
    }

    #[test]
    fn keys_are_quoted_when_needed() {
        let layer = ok("code.aliases.c++=cpp");
        assert_eq!(
            layer.code.aliases.0.get("c++").map(String::as_str),
            Some("cpp")
        );
        let layer = ok("code.aliases.\"f#\"=fsharp");
        assert_eq!(
            layer.code.aliases.0.get("f#").map(String::as_str),
            Some("fsharp")
        );
    }

    #[test]
    fn styles_and_palette() {
        let layer = ok("style.h1.fg=red");
        assert_eq!(
            layer.style.0[&Element::H1].fg,
            Some(ColorSpec::Name("red".into()))
        );
        let layer = ok("palette.accent=#ff8800");
        assert!(
            layer.palette.both.contains_key("accent"),
            "`#` starts a comment in TOML"
        );
        let layer = ok("style.h1={ fg = \"accent\", bold = true }");
        assert_eq!(layer.style.0[&Element::H1].bold, Some(true));
    }

    #[test]
    fn errors() {
        for bad in [
            "render.max_width",
            "=3",
            ".a=3",
            "a..b=3",
            "render.\u{1b}=1",
        ] {
            let (layer, diags) = set(bad);
            assert!(layer.is_none(), "{bad}");
            assert_eq!(diags.len(), 1, "{bad}: {diags:?}");
            assert_eq!(diags[0].severity, Severity::Error);
        }
        // A bare word that fits nowhere reports the type error of the word.
        let (layer, diags) = set("render.max_width=abc");
        assert!(layer.is_none());
        assert!(
            diags[0]
                .message
                .contains("invalid type: string \"abc\", expected u16"),
            "{}",
            diags[0].message
        );
        // A valid TOML value of the wrong type reports that.
        let (_, diags) = set("render.max_width=-5");
        assert!(
            diags[0].message.contains("integer `-5`"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn unknown_keys_warn() {
        let (layer, diags) = set("render.max_widht=80");
        assert!(layer.is_some());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert_eq!(diags[0].location, "--set render.max_widht=80");
        assert!(diags[0].message.contains("did you mean `render.max_width`"));
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(quote("x\ty"), "\"x\\u0009y\"");
        assert_eq!(toml_key("a.b-c.d_e").unwrap(), "a.b-c.d_e");
        assert_eq!(toml_key("a.c++").unwrap(), "a.\"c++\"");
    }
}
