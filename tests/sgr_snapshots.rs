//! Byte-exact output: small documents rendered to escape sequences, with
//! `ESC` shown as `\e` and `BEL` as `\a` (a control picture such as `␛`
//! is text from the document). These pin the SGR diffing, colour
//! downsampling, styled underlines and OSC 8 links byte for byte.

mod common;

use std::collections::BTreeMap;

use common::{FakeHighlighter, lay_out, parse};
use emde::layout::{NoImages, layout};
use emde::options::{H1Style, RenderOptions};
use emde::render::{RenderConfig, to_bytes};
use emde::style::{Attrs, Color, Rgb, StylePatch, Underline};
use emde::term::{Caps, ColorDepth};
use emde::theme::{Element, Theme, Variant};

/// Render `md` at `width` for `caps`, escapes made visible.
fn bytes(md: &str, width: u16, caps: &Caps) -> String {
    bytes_with(md, width, caps, &RenderOptions::default())
}

fn bytes_with(md: &str, width: u16, caps: &Caps, opts: &RenderOptions) -> String {
    let doc = parse(md);
    let l = lay_out(&doc, width, caps, opts);
    let cfg = RenderConfig {
        link_id_prefix: "e1".into(),
        ..RenderConfig::from_caps(caps)
    };
    visible(&to_bytes(&doc, &l, &cfg))
}

fn visible(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .replace('\u{1b}', "\\e")
        .replace('\u{7}', "\\a")
}

fn depth(color: ColorDepth) -> Caps {
    Caps {
        color,
        ..Caps::full()
    }
}

const INLINE: &str = "Some *em*, **strong**, ~~gone~~, `code`, a [link](<https://example.com/ä b>) and a note[^1].\n\n[^1]: Note.";

#[test]
fn inline_truecolor() {
    insta::assert_snapshot!(bytes(INLINE, 60, &Caps::full()));
}

#[test]
fn inline_256() {
    insta::assert_snapshot!(bytes(INLINE, 60, &depth(ColorDepth::Ansi256)));
}

#[test]
fn inline_16() {
    insta::assert_snapshot!(bytes(INLINE, 60, &depth(ColorDepth::Ansi16)));
}

#[test]
fn inline_mono() {
    insta::assert_snapshot!(bytes(INLINE, 60, &depth(ColorDepth::Mono)));
}

#[test]
fn inline_without_escapes() {
    let out = bytes(INLINE, 60, &Caps::plain());
    assert!(!out.contains("\\e"), "{out}");
    insta::assert_snapshot!(out);
}

#[test]
fn wrapped_link_reuses_its_id() {
    let md = "Read [the long documentation page](https://example.com/docs) today.";
    insta::assert_snapshot!(bytes(md, 24, &Caps::full()));
}

#[test]
fn h1_gradient_bar() {
    insta::assert_snapshot!(bytes("# Title", 24, &Caps::full()));
}

#[test]
fn h1_at_lower_depths() {
    let out = format!(
        "{}{}{}",
        bytes("# Title", 24, &depth(ColorDepth::Ansi256)),
        bytes("# Title", 24, &depth(ColorDepth::Ansi16)),
        bytes("# Title", 24, &depth(ColorDepth::Mono)),
    );
    insta::assert_snapshot!(out);
}

/// The user's example: `[style.h1] fg = "accent"`, `bold = true`,
/// `underline = "curly"` gives `ESC[1;4:3;38;2;137;180;250m`.
#[test]
fn users_curly_h1() {
    let accent = Rgb(0x89, 0xb4, 0xfa);
    let base = Rgb(0x1e, 0x1e, 0x2e);
    let patch = StylePatch {
        fg: Some(Color::Rgb(accent)),
        set: Attrs::BOLD,
        underline: Some(Underline::Curly),
        ..StylePatch::default()
    };
    let theme = Theme::from_patches(
        "user",
        Variant::Dark,
        BTreeMap::from([("accent".to_string(), accent)]),
        base,
        Rgb(0x31, 0x32, 0x44),
        "none",
        &[(Element::H1, patch)],
        &[],
    );
    let doc = parse("# Title");
    let mut opts = RenderOptions::default();
    opts.heading.h1 = H1Style::Underline;
    for (styled, want) in [
        (true, "\\e[1;4:3;38;2;137;180;250mTitle\\e[0m"),
        (false, "\\e[1;4;38;2;137;180;250mTitle\\e[0m"),
    ] {
        let caps = Caps {
            styled_underline: styled,
            ..Caps::full()
        };
        let l = layout(&doc, 30, &theme, &caps, &opts, &FakeHighlighter, &NoImages);
        let cfg = RenderConfig::from_caps(&caps);
        let out = visible(&to_bytes(&doc, &l, &cfg));
        assert!(out.contains(want), "{out}");
    }
}

#[test]
fn h2_rule_fades() {
    insta::assert_snapshot!(bytes("## Section", 30, &Caps::full()));
}

#[test]
fn code_panel_with_highlighting() {
    let md = "```rust\nfn main() {\n\tlet x = \"hi\"; // greet\n}\n```";
    insta::assert_snapshot!(bytes(md, 40, &Caps::full()));
}

#[test]
fn alert_tint() {
    insta::assert_snapshot!(bytes("> [!WARNING]\n> Mind the *gap*.", 30, &Caps::full()));
}

#[test]
fn table_header_and_zebra() {
    let md = "| a | b |\n|---|--:|\n| 1 | 2 |\n| 3 | 4 |";
    insta::assert_snapshot!(bytes(md, 30, &Caps::full()));
}

#[test]
fn dangerous_urls_are_not_linked() {
    let md = "[x](https://a.org/\u{1b}]52;c;AAAA) [y](javascript:alert(1)) [z](mailto:a@b.org)";
    let out = bytes(md, 60, &Caps::full());
    // The ESC became a picture, so the first link is not linked at all;
    // scripts are never linked; mail is.
    assert!(!out.contains("52;c"), "{out}");
    assert!(!out.contains("javascript"), "{out}");
    assert!(out.contains("\\e]8;id=e1-2;mailto:a@b.org"), "{out}");
    insta::assert_snapshot!(out);
}

/// No style or link survives the end of a line: the last SGR sequence of
/// every styled line is a reset and the last OSC 8 is a close.
#[test]
fn styles_never_bleed_past_a_line() {
    for name in common::FIXTURES {
        let md = common::fixture(name);
        let out = bytes(&md, 80, &Caps::full());
        for line in out.lines() {
            if let Some(i) = line.rfind("\\e[") {
                let sgr = &line[i..];
                let end = sgr.find('m').map_or(sgr.len(), |m| m + 1);
                assert_eq!(&sgr[..end], "\\e[0m", "{name}: {line}");
            }
            if let Some(i) = line.rfind("\\e]8;") {
                assert!(line[i..].starts_with("\\e]8;;\\e\\"), "{name}: {line}");
            }
        }
    }
}
