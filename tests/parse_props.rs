//! Property tests for the whole front end (source → parse): arbitrary bytes
//! and random Markdown built from syntax fragments always give a valid,
//! deterministic document with no control characters in its text.

use emde::options::HtmlMode;
use emde::parse::{ParseOptions, parse, parse_source};
use emde::source::{Origin, Source};
use proptest::prelude::*;

/// Fragments of Markdown, HTML and TeX syntax, joined at random.
#[rustfmt::skip]
const FRAGMENTS: &[&str] = &[
    "word ", "text", " ", "\n", "\n\n", "  \n", "\t", "# ", "## ", "- ", "* ", "1. ", "> ", "> > ",
    "- [ ] ", "- [x] ", "    ", "```\n", "```rust\n", "```math\n", "~~~", "---\n", "***", "| a | b |\n",
    "|---|:-:|\n", "| `c` | $x$ |\n", "*", "**", "_", "~~", "`", "``", "$", "$$", "\\(", "\\)", "\\[",
    "\\]", "\\", "[link](https://x.org/a?b=c)", "[ref][r]", "[r]: https://r.org\n", "<https://a.b>",
    "https://bare.example.com/p", "www.example.org", "me@example.org", "![alt](i.png \"t\")",
    "[![b](b.svg)](https://l)", "[^1]", "[^1]: note\n", "[^n]: *nested* [^1]\n", "<br>", "<b>",
    "</b>", "<kbd>K</kbd>", "<sub>2</sub>", "<details>\n", "<summary>S</summary>\n", "</details>\n",
    "<div align=\"center\">\n", "</div>\n", "<p align=\"right\">", "</p>", "<center>", "</center>",
    "<h2 align=\"center\">H</h2>\n", "<img src=\"x.png\" width=\"10\">", "<picture>",
    "<source media=\"m\" srcset=\"s\">", "</picture>", "<a name=\"n\"></a>", "<a href=\"#n\">",
    "</a>", "<!--", "-->", "<pre>", "</pre>", "<script>", "</script>", "&amp;", "&#27;", "&#x9b;",
    "&nbsp;", "&bogus;", "日本語", "한국어", "👨\u{200d}👩\u{200d}👧", "e\u{301}", "\u{ad}",
    "\u{2028}", "\u{1b}[2J", "\u{0}", "\r\n", "Term\n: def\n", "+++\n", "\\{x\\}", "^",
    "x^{2}_{1}",
];

fn markdown() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(FRAGMENTS), 0..60).prop_map(|v| v.concat())
}

fn option_sets() -> [ParseOptions; 3] {
    let default = ParseOptions::default();
    let mut all = default;
    all.heading_attributes = true;
    all.wikilinks = true;
    all.markdown.smart_punctuation = true;
    [
        default,
        all,
        ParseOptions {
            html: HtmlMode::Raw,
            ..default
        },
    ]
}

fn check(source: &Source, opts: &ParseOptions) -> Result<(), TestCaseError> {
    let doc = parse_source(source, opts);
    if let Err(e) = doc.validate() {
        return Err(TestCaseError::fail(format!("{e}\n{}", doc.dump())));
    }
    let text = doc.plain_text();
    prop_assert!(
        !text.contains(['\u{1b}', '\u{9b}', '\u{0}', '\r']),
        "{text:?}"
    );
    prop_assert_eq!(&doc, &parse_source(source, opts));
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: ProptestConfig::default().cases.max(1000),
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn random_markdown_gives_valid_documents(md in markdown()) {
        let source = Source::from_text(&md);
        for opts in option_sets() {
            check(&source, &opts)?;
        }
    }

    #[test]
    fn arbitrary_bytes_give_valid_documents(bytes in prop::collection::vec(any::<u8>(), 0..400)) {
        let source = Source::from_bytes(bytes, Origin::Memory);
        check(&source, &ParseOptions::default())?;
    }

    /// Sanitising happens once: parsing already-clean text directly gives
    /// the same document as going through `Source`.
    #[test]
    fn source_text_is_a_fixed_point(md in markdown()) {
        let source = Source::from_text(&md);
        let again = Source::from_text(&source.text);
        prop_assert_eq!(&again.text, &source.text);
        prop_assert!(again.diagnostics.is_empty());
        prop_assert_eq!(parse(&source.text, &ParseOptions::default()), parse(&again.text, &ParseOptions::default()));
    }
}
