//! Property tests of layout and rendering over generated Markdown at widths
//! 1–200:
//!
//! * no rendered line is wider than the terminal, for any capabilities;
//! * every word of the input is shown, in reading order;
//! * layout is deterministic (and independent of layouts at other widths);
//! * line positions (`SrcPos`) never decrease, block ranges tile the lines;
//! * rendered bytes never contain escapes the document supplied, and every
//!   OSC 8 link opened on a line is closed on it.

mod common;

use common::{lay_out, parse};
use emde::options::RenderOptions;
use emde::render::{RenderConfig, debug_text, plain_text, to_bytes};
use emde::term::{Caps, ColorDepth};
use emde::text::str_width;
use proptest::prelude::*;

/// Markdown, HTML, TeX and Unicode fragments, joined at random.
#[rustfmt::skip]
const FRAGMENTS: &[&str] = &[
    "word ", "text", " ", "\n", "\n\n", "  \n", "\t", "# ", "## ", "### ", "- ", "* ", "1. ", "> ",
    "> > ", "- [ ] ", "- [x] ", "    ", "```\n", "```rust\n", "```diff\n+a\n-b\n```\n", "```math\n",
    "~~~", "---\n", "***", "| a | b |\n", "|---|:-:|\n", "| `c` | $x$ |\n", "*", "**", "_", "~~",
    "`", "``", "$", "$$", "\\(", "\\)", "\\[", "\\]", "\\", "[link](https://x.org/a?b=c)",
    "[ref][r]", "[r]: https://r.org\n", "<https://a.b>", "https://bare.example.com/path/to/x",
    "www.example.org", "me@example.org", "![alt](i.png \"t\")", "[![b](b.svg)](https://l)",
    "[^1]", "[^1]: note\n", "[^n]: *nested* [^1]\n", "<br>", "<b>", "</b>", "<kbd>K</kbd>",
    "<sub>2</sub>", "<sup>n</sup>", "<details>\n", "<summary>S</summary>\n", "</details>\n",
    "<div align=\"center\">\n", "</div>\n", "<p align=\"right\">", "</p>", "<center>", "</center>",
    "<h2 align=\"center\">H</h2>\n", "<img src=\"x.png\" width=\"10\">", "<mark>m</mark>",
    "<a name=\"n\"></a>", "<a href=\"#n\">", "</a>", "<!--", "-->", "<pre>", "</pre>", "&amp;",
    "&#27;", "&#x9b;", "&nbsp;", "日本語", "한국어", "👨\u{200d}👩\u{200d}👧", "❤\u{fe0f}", "e\u{301}",
    "\u{ad}", "\u{2028}", "\u{1b}[2J", "\u{0}", "\r\n", "Term\n: def\n", "+++\n", "> [!NOTE]\n",
    "> [!WARNING]\n", "x^{2}_{1}", "supercalifragilisticexpialidocious", "a-b-c-d-e-f-g",
    "\u{3000}", "ＡＢＣ", "\u{1f1fa}\u{1f1f8}",
];

fn markdown() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(FRAGMENTS), 0..50).prop_map(|v| v.concat())
}

fn caps_sets() -> Vec<Caps> {
    vec![
        Caps::plain(),
        Caps::full(),
        Caps {
            color: ColorDepth::Ansi256,
            ..Caps::full()
        },
        Caps {
            color: ColorDepth::Ansi16,
            hyperlinks: false,
            ..Caps::full()
        },
        Caps {
            color: ColorDepth::Mono,
            ..Caps::full()
        },
    ]
}

/// Layout-level invariants.
fn check_layout(l: &emde::layout::Layout, doc: &emde::ir::Document) -> Result<(), TestCaseError> {
    for (i, line) in l.lines.iter().enumerate() {
        let text = l.line_text(i);
        prop_assert_eq!(
            str_width(&text, false),
            usize::from(line.cols),
            "line {}: {:?}",
            i,
            text
        );
        prop_assert!(usize::from(l.indent) + usize::from(line.cols) <= usize::from(l.width));
        if i > 0 {
            prop_assert!(
                l.lines[i - 1].pos <= line.pos,
                "position decreases at line {}",
                i
            );
        }
    }
    prop_assert!(
        !l.text
            .contains(['\u{ad}', '\u{1b}', '\u{9b}', '\n', '\t', '\r', '\0']),
        "unsafe text in the arena: {:?}",
        l.text
    );
    let mut next = 0;
    for r in &l.block_lines {
        prop_assert_eq!(r.start, next);
        next = r.end;
    }
    if !l.block_lines.is_empty() {
        prop_assert_eq!(next as usize, l.lines.len());
    }
    prop_assert_eq!(l.block_lines.len(), doc.blocks.len());
    Ok(())
}

/// Every rendered line fits the terminal.
fn check_width(out: &str, width: u16) -> Result<(), TestCaseError> {
    for line in out.lines() {
        prop_assert!(
            str_width(line, false) <= usize::from(width),
            "{} columns > {}: {:?}",
            str_width(line, false),
            width,
            line
        );
    }
    Ok(())
}

/// Escapes in rendered bytes are emde's own: SGR and balanced OSC 8.
fn check_bytes(bytes: &[u8]) -> Result<(), TestCaseError> {
    let text = String::from_utf8_lossy(bytes);
    for line in text.split('\n') {
        let opens = line.matches("\u{1b}]8;id=").count();
        let closes = line.matches("\u{1b}]8;;\u{1b}\\").count();
        prop_assert_eq!(opens, closes, "unbalanced OSC 8: {:?}", line);
        // Every ESC starts an SGR (`ESC [ … m`) or an OSC 8.
        let mut rest = line;
        while let Some(i) = rest.find('\u{1b}') {
            let seq = &rest[i..];
            let ok = seq.starts_with("\u{1b}]8;")
                || seq.starts_with("\u{1b}\\")
                || (seq.starts_with("\u{1b}[")
                    && seq[2..].find('m').is_some_and(|m| {
                        seq[2..2 + m]
                            .bytes()
                            .all(|b| b.is_ascii_digit() || b == b';' || b == b':')
                    }));
            prop_assert!(ok, "unexpected escape in {:?}", line);
            rest = &seq[1..];
        }
        prop_assert!(!line.contains('\u{9b}'), "C1 CSI in output");
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: ProptestConfig::default().cases.max(400),
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn nothing_is_wider_than_the_terminal(md in markdown(), width in 1u16..=200) {
        let doc = parse(&md);
        let opts = RenderOptions::default();
        for caps in caps_sets() {
            let l = lay_out(&doc, width, &caps, &opts);
            check_layout(&l, &doc)?;
            check_width(&plain_text(&doc, &l), width)?;
            let cfg = RenderConfig {
                link_id_prefix: "t".into(),
                ..RenderConfig::from_caps(&caps)
            };
            check_bytes(&to_bytes(&doc, &l, &cfg))?;
        }
    }

    #[test]
    fn ascii_and_narrow_options_fit_too(md in markdown(), width in 1u16..=60) {
        let doc = parse(&md);
        let mut opts = RenderOptions {
            ascii: true,
            max_width: 0,
            ..RenderOptions::default()
        };
        opts.code.wrap = false;
        opts.code.line_numbers = true;
        for caps in [Caps::plain(), Caps::full()] {
            let l = lay_out(&doc, width, &caps, &opts);
            check_layout(&l, &doc)?;
            check_width(&plain_text(&doc, &l), width)?;
        }
    }

    #[test]
    fn layout_is_deterministic(md in markdown(), w1 in 1u16..=200, w2 in 1u16..=200) {
        let doc = parse(&md);
        let opts = RenderOptions::default();
        let caps = Caps::full();
        let a = lay_out(&doc, w1, &caps, &opts);
        let _ = lay_out(&doc, w2, &caps, &opts);
        let b = lay_out(&doc, w1, &caps, &opts);
        prop_assert_eq!(&a.lines, &b.lines);
        prop_assert_eq!(&a.text, &b.text);
        prop_assert_eq!(&a.spans, &b.spans);
        prop_assert_eq!(&a.link_hits, &b.link_hits);
        prop_assert_eq!(debug_text(&a, caps.color, true), debug_text(&b, caps.color, true));
    }
}

// ----- words -----------------------------------------------------------------

/// Words that no decoration or label contains.
const WORDS: &[&str] = &[
    "alpha",
    "bravo",
    "charlie",
    "delta",
    "echo",
    "foxtrot",
    "golf",
    "hotel",
    "india",
    "juliet",
    "kilo",
    "lima",
    "mike",
    "november",
    "oscar",
    "papa",
    "quebec",
    "romeo",
    "sierra",
    "tango",
    "uniform",
    "victor",
    "whiskey",
    "xray",
    "yankee",
    "zulu",
    "supercalifragilisticexpialidocious",
];

fn word() -> impl Strategy<Value = &'static str> {
    prop::sample::select(WORDS)
}

/// A piece of inline text around a word. Some glue an atom (a code span,
/// a key cap, a link's reference number) to a word, so a word too long for
/// its line has an atom to keep whole.
fn inline() -> impl Strategy<Value = String> {
    (word(), word(), 0u8..11).prop_map(|(w, v, how)| match how {
        0 => format!("*{w}*"),
        1 => format!("**{w}**"),
        2 => format!("`{w} {w}`"),
        3 => format!("[{w}](https://x.org/{w})"),
        4 => format!("~~{w}~~"),
        5 => format!("{w}`{v}`"),
        6 => format!("{w}<kbd>{v}</kbd>{w}"),
        7 => format!("[{w}{v}](https://x.org/)"),
        _ => w.to_string(),
    })
}

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(inline(), 1..10).prop_map(|v| v.join(" "))
}

fn block() -> impl Strategy<Value = String> {
    prop_oneof![
        text(),
        (1usize..=6, text()).prop_map(|(n, t)| format!("{} {t}", "#".repeat(n))),
        prop::collection::vec(text(), 1..4)
            .prop_map(|items| items.iter().map(|t| format!("- {t}\n")).collect()),
        prop::collection::vec(text(), 1..4).prop_map(|items| {
            items
                .iter()
                .enumerate()
                .map(|(i, t)| format!("{}. {t}\n", i + 1))
                .collect()
        }),
        (text(), text()).prop_map(|(a, b)| format!("- {a}\n  - {b}\n    - {a}")),
        text().prop_map(|t| format!("> {t}")),
        (text(), text()).prop_map(|(a, b)| format!("> {a}\n>\n> > {b}")),
        text().prop_map(|t| format!("> [!TIP]\n> {t}")),
        prop::collection::vec(word(), 1..12).prop_map(|w| format!("```\n{}\n```", w.join(" "))),
        text().prop_map(|t| format!("- [ ] {t}\n- [x] {t}")),
        (word(), text()).prop_map(|(w, t)| format!("{w}\n: {t}")),
        Just("---".to_string()),
    ]
}

fn document() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(block(), 1..8),
        prop::option::of(text()),
    )
        .prop_map(|(blocks, note)| {
            let mut md = blocks.join("\n\n");
            if let Some(note) = note {
                md.push_str(" [^f]\n\n[^f]: ");
                md.push_str(&note);
            }
            md
        })
}

/// The ASCII letters of a text, everything else dropped.
fn letters(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphabetic).collect()
}

/// Every known word of the document's text appears in the output, in order.
fn check_words(doc: &emde::ir::Document, out: &str) -> Result<(), TestCaseError> {
    let hay = letters(out);
    let mut pos = 0;
    for w in doc.plain_text().split(|c: char| !c.is_ascii_alphabetic()) {
        if !WORDS.contains(&w) {
            continue;
        }
        match hay.get(pos..).and_then(|rest| rest.find(w)) {
            Some(i) => pos += i + w.len(),
            None => {
                return Err(TestCaseError::fail(format!(
                    "{w:?} missing after position {pos}\n{out}"
                )));
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: ProptestConfig::default().cases.max(400),
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn every_word_is_shown_in_order(md in document(), width in 1u16..=200) {
        let doc = parse(&md);
        let opts = RenderOptions::default();
        for caps in [Caps::plain(), Caps::full()] {
            let l = lay_out(&doc, width, &caps, &opts);
            check_layout(&l, &doc)?;
            let out = plain_text(&doc, &l);
            check_width(&out, width)?;
            check_words(&doc, &out)?;
        }
    }
}

#[test]
fn letters_helper() {
    assert_eq!(letters("▎ al-pha ↳bra1vo"), "alphabravo");
}
