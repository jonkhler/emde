//! Every input of pulldown-cmark's own test suite (the CommonMark spec
//! examples plus its GFM, math, footnote, table, definition-list and
//! regression cases) parses to a valid document under every option
//! combination, deterministically.

use emde::ir::Document;
use emde::options::HtmlMode;
use emde::parse::{ParseOptions, parse, parse_source};
use emde::source::Source;

const CORPUS: &str = include_str!("fixtures/pulldown-cmark-suite.txt");
const SEPARATOR: &str = "⸻⸻⸻ example ";

/// The corpus examples as `(name, markdown)`.
fn examples() -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for line in CORPUS.split_inclusive('\n') {
        if let Some(name) = line.strip_prefix(SEPARATOR) {
            out.push((name.trim_end(), String::new()));
        } else if let Some((_, text)) = out.last_mut() {
            text.push_str(line);
        }
    }
    out
}

/// Option sets that exercise different parser paths.
fn option_sets() -> Vec<(&'static str, ParseOptions)> {
    let default = ParseOptions::default();
    let mut everything = default;
    everything.heading_attributes = true;
    everything.wikilinks = true;
    everything.markdown.smart_punctuation = true;
    let mut minimal = default;
    minimal.markdown.math = false;
    minimal.markdown.linkify = false;
    minimal.markdown.definition_lists = false;
    minimal.tex_delimiters = false;
    vec![
        ("default", default),
        ("everything", everything),
        ("minimal", minimal),
        (
            "html-raw",
            ParseOptions {
                html: HtmlMode::Raw,
                ..default
            },
        ),
        (
            "html-strip",
            ParseOptions {
                html: HtmlMode::Strip,
                ..default
            },
        ),
    ]
}

fn assert_valid(doc: &Document, what: &str) {
    if let Err(e) = doc.validate() {
        panic!("{what}: {e}\n{}", doc.dump());
    }
}

#[test]
fn corpus_is_complete() {
    let examples = examples();
    assert_eq!(examples.len(), 1148);
    assert!(examples.iter().all(|(_, md)| md.ends_with('\n')));
    assert!(
        examples
            .iter()
            .any(|(name, _)| name.starts_with("spec.rs:"))
    );
}

#[test]
fn every_example_parses_to_a_valid_document() {
    for (name, md) in examples() {
        for (label, opts) in option_sets() {
            let doc = parse(&md, &opts);
            assert_valid(&doc, &format!("{name} [{label}]"));
            let _ = doc.plain_text();
        }
    }
}

#[test]
fn parsing_is_deterministic() {
    let opts = ParseOptions::default();
    for (name, md) in examples() {
        assert_eq!(parse(&md, &opts), parse(&md, &opts), "{name}");
    }
}

#[test]
fn whole_corpus_as_one_document() {
    // One large document mixing every construct, through the source
    // sanitiser, exercises interactions between examples.
    let all: String = examples().into_iter().map(|(_, md)| md + "\n").collect();
    let source = Source::from_text(&all);
    let doc = parse_source(&source, &ParseOptions::default());
    assert_valid(&doc, "whole corpus");
    assert!(doc.headings.len() > 100);
    assert!(doc.links.len() > 100);
}
