//! Every example of pulldown-cmark's test suite (the CommonMark spec plus
//! its GFM, math, footnote, table and regression cases) lays out at several
//! widths and capabilities without a line wider than the terminal, with
//! non-decreasing positions, and shows every word of its text in order.
//!
//! Words are ASCII letter runs from the document model, except math (the
//! math renderer rewrites TeX), super/subscripts (mapped to Unicode
//! characters) and tables (wrapped cells interleave on screen).

mod common;

use common::lay_out;
use emde::ir::{Block, Document, InlineFlags, Inlines, RunKind};
use emde::options::RenderOptions;
use emde::parse::{ParseOptions, parse};
use emde::render::plain_text;
use emde::term::Caps;
use emde::text::str_width;

const CORPUS: &str = include_str!("fixtures/pulldown-cmark-suite.txt");
const SEPARATOR: &str = "⸻⸻⸻ example ";

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

/// The shown text of a document, a separator between pieces.
struct Words<'d> {
    doc: &'d Document,
    out: String,
}

impl Words<'_> {
    fn inlines(&mut self, t: &Inlines) {
        for (range, run) in t.runs_with_ranges() {
            let skip = run.kind == RunKind::Math
                || run.flags.intersects(InlineFlags::SUP | InlineFlags::SUB);
            if skip {
                self.out.push(' ');
            } else {
                self.out.push_str(t.slice(range));
            }
        }
        self.out.push(' ');
    }

    fn blocks(&mut self, blocks: &[Block]) {
        for b in blocks {
            match b {
                Block::Para(t) | Block::Heading { text: t, .. } => self.inlines(t),
                Block::Code(c) => {
                    self.out.push_str(&c.code);
                    self.out.push(' ');
                }
                Block::Quote { body, .. } | Block::Align { body, .. } => self.blocks(body),
                Block::Details { summary, body } => {
                    self.inlines(summary);
                    self.blocks(body);
                }
                Block::List(list) => {
                    for item in &list.items {
                        self.blocks(&item.body);
                    }
                }
                Block::DefList(items) => {
                    for item in items {
                        self.inlines(&item.term);
                        for def in &item.defs {
                            self.blocks(def);
                        }
                    }
                }
                Block::Figure(f) => {
                    self.out.push_str(self.doc.caption(f));
                    self.out.push(' ');
                }
                Block::FootnoteSection => {
                    let doc = self.doc;
                    for f in &doc.footnotes {
                        self.blocks(&f.body);
                    }
                }
                Block::FrontMatter(fm) => {
                    self.out.push_str(&fm.raw);
                    self.out.push(' ');
                }
                Block::Html(h) => {
                    self.out.push_str(h);
                    self.out.push(' ');
                }
                Block::Math(_) | Block::Table(_) | Block::Rule => self.out.push(' '),
            }
        }
    }
}

fn expected_words(doc: &Document) -> Vec<String> {
    let mut w = Words {
        doc,
        out: String::new(),
    };
    w.blocks(&doc.blocks);
    w.out
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn letters(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphabetic).collect()
}

#[test]
fn corpus_lays_out_at_every_width() {
    let opts = RenderOptions::default();
    for (name, md) in examples() {
        let doc = parse(&md, &ParseOptions::default());
        for width in [1u16, 7, 20, 80] {
            for caps in [Caps::plain(), Caps::full()] {
                let l = lay_out(&doc, width, &caps, &opts);
                for (i, line) in l.lines.iter().enumerate() {
                    assert!(
                        usize::from(l.indent) + usize::from(line.cols) <= usize::from(width),
                        "{name} at {width}: line {i}"
                    );
                    if i > 0 {
                        assert!(
                            l.lines[i - 1].pos <= line.pos,
                            "{name} at {width}: line {i}"
                        );
                    }
                }
                let out = plain_text(&doc, &l);
                for line in out.lines() {
                    assert!(
                        str_width(line, false) <= usize::from(width),
                        "{name} at {width}"
                    );
                }
            }
        }
    }
}

#[test]
fn corpus_shows_every_word_in_order() {
    let opts = RenderOptions::default();
    for (name, md) in examples() {
        let doc = parse(&md, &ParseOptions::default());
        let words = expected_words(&doc);
        for width in [20u16, 80] {
            for caps in [Caps::plain(), Caps::full()] {
                let l = lay_out(&doc, width, &caps, &opts);
                let out = plain_text(&doc, &l);
                let hay = letters(&out);
                let mut pos = 0;
                for w in &words {
                    match hay.get(pos..).and_then(|rest| rest.find(w.as_str())) {
                        Some(i) => pos += i + w.len(),
                        None => panic!(
                            "{name} at {width}: {w:?} missing\n--- markdown\n{md}--- output\n{out}"
                        ),
                    }
                }
            }
        }
    }
}
