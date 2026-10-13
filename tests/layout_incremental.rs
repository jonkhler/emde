//! A layout made a few blocks at a time ([`LayoutJob`]) is the layout made
//! in one go: after every step its lines (with their spans and text) are
//! the first lines of the complete layout, and once done it is equal to
//! it in every table. Checked on the fixtures, every example of
//! pulldown-cmark's test suite and a generated document with everything
//! that carries over from block to block (link references flushed at
//! section ends, heading numbers, footnotes, collapsed `<details>`, wide
//! blocks), at several widths, with and without the pager's settings.

mod common;

use common::{FIXTURES, FakeHighlighter, fixture, parse};
use emde::ir::Document;
use emde::layout::{Folds, Interactive, Layout, LayoutInputs, LayoutJob, NoImages, layout_with};
use emde::options::{RenderOptions, When};
use emde::term::Caps;
use emde::theme::Theme;

const CORPUS: &str = include_str!("fixtures/pulldown-cmark-suite.txt");
const SEPARATOR: &str = "⸻⸻⸻ example ";

/// The examples of the pulldown-cmark suite, concatenated in groups (each
/// on its own is mostly one or two blocks).
fn corpus_groups() -> Vec<String> {
    let mut examples: Vec<String> = Vec::new();
    for line in CORPUS.split_inclusive('\n') {
        if line.starts_with(SEPARATOR) {
            examples.push(String::new());
        } else if let Some(text) = examples.last_mut() {
            text.push_str(line);
        }
    }
    examples.chunks(25).map(|g| g.join("\n\n")).collect()
}

/// A document where what one block leaves for the next matters.
fn carry_over() -> String {
    let mut md = String::new();
    for i in 0..12 {
        md.push_str(&format!(
            "## Section {i}\n\nSee [the docs][d{i}] and [another](https://example.com/{i}) \
             with a note[^n{i}].\n\n- item\n  - nested [link](https://x.example/{i})\n\n\
             <details>\n<summary>More {i}</summary>\n\n### Hidden {i}\n\nBody {i}.\n\n</details>\n\n\
             | a | b |\n|---|---|\n| {} | wide |\n\n```rust\nfn f{i}() {{ let x = \"{}\"; }}\n```\n\n\
             [d{i}]: https://docs.example/{i}\n[^n{i}]: Footnote {i}.\n\n",
            "cell ".repeat(i * 3),
            "long ".repeat(i * 4),
        ));
    }
    md
}

/// Lay `doc` out in steps (stopping whenever `stop` says so) and check it
/// against the layout made in one go.
fn check(
    name: &str,
    doc: &Document,
    inputs: LayoutInputs<'_>,
    width: u16,
    stop: &dyn Fn(&Layout) -> bool,
) {
    let whole = layout_with(
        doc,
        width,
        inputs.theme,
        inputs.caps,
        inputs.opts,
        inputs.highlighter,
        inputs.sizer,
        inputs.interactive,
    );
    let (mut job, mut partial) = LayoutJob::start(inputs, width);
    assert!(partial.lines.is_empty() || doc.blocks.is_empty(), "{name}");
    let mut steps = 0;
    loop {
        let done = job.advance(&mut partial, inputs, &mut |l| stop(l));
        steps += 1;
        assert_eq!(done, job.is_done(), "{name}");
        if done {
            break;
        }
        let what = format!("{name} w{width} after step {steps}");
        same_start(&partial.lines, &whole.lines, &format!("{what}: lines"));
        same_start(&partial.spans, &whole.spans, &format!("{what}: spans"));
        assert!(whole.text.starts_with(&partial.text), "{what}: text");
        // The blocks laid out have their lines; only the last one's end
        // may still move (the blank line after it comes with the next).
        let (last, before) = partial.block_lines.split_last().expect("a block");
        same_start(before, &whole.block_lines, &format!("{what}: block lines"));
        assert_eq!(last.start, whole.block_lines[before.len()].start, "{what}");
        assert!(steps <= doc.blocks.len() + 1, "{name}: no progress");
    }
    assert_eq!(partial, whole, "{name} w{width}: the finished layout");
}

/// `part` is the start of `whole` (the first difference is reported).
fn same_start<T: PartialEq + std::fmt::Debug>(part: &[T], whole: &[T], what: &str) {
    assert!(
        part.len() <= whole.len(),
        "{what}: {} > {}",
        part.len(),
        whole.len()
    );
    if let Some(i) = part.iter().zip(whole).position(|(a, b)| a != b) {
        panic!("{what}: [{i}] is {:?}, not {:?}", part[i], whole[i]);
    }
}

fn documents() -> Vec<(String, Document)> {
    let mut docs: Vec<(String, Document)> = FIXTURES
        .iter()
        .map(|name| ((*name).to_owned(), parse(&fixture(name))))
        .collect();
    docs.extend(
        corpus_groups()
            .iter()
            .enumerate()
            .map(|(i, md)| (format!("corpus group {i}"), parse(md))),
    );
    docs.push(("carry-over".to_owned(), parse(&carry_over())));
    docs
}

#[test]
fn stepwise_layouts_are_the_whole_layout() {
    let theme = Theme::test();
    let toggled = [1, 3, 4];
    let pager = Interactive {
        hscroll: true,
        details: Some(Folds {
            open: false,
            toggled: &toggled,
        }),
    };
    let mut numbered = RenderOptions::default();
    numbered.heading.numbers = true;
    numbered.link_refs = When::Always;
    let optionss = [RenderOptions::default(), numbered];
    let capss = [Caps::plain(), Caps::full()];
    for (name, doc) in &documents() {
        for (opts, caps) in optionss.iter().zip(&capss) {
            for interactive in [Interactive::default(), pager] {
                let inputs = LayoutInputs {
                    doc,
                    theme: &theme,
                    caps,
                    opts,
                    highlighter: &FakeHighlighter,
                    sizer: &NoImages,
                    interactive,
                };
                for width in [24, 80] {
                    // A block at a time.
                    check(name, doc, inputs, width, &|_| true);
                    // A screenful at a time.
                    check(name, doc, inputs, width, &|l| l.lines.len() % 7 >= 3);
                }
            }
        }
    }
}

#[test]
fn a_finished_job_stays_finished() {
    let theme = Theme::test();
    let doc = parse(&carry_over());
    let inputs = LayoutInputs {
        doc: &doc,
        theme: &theme,
        caps: &Caps::full(),
        opts: &RenderOptions::default(),
        highlighter: &FakeHighlighter,
        sizer: &NoImages,
        interactive: Interactive::default(),
    };
    let (mut job, mut layout) = LayoutJob::start(inputs, 60);
    assert!(!job.is_done());
    // Enough after the first block: it stops there.
    assert!(!job.advance(&mut layout, inputs, &mut |_| true));
    assert_eq!(layout.block_lines.len(), 1);
    job.finish(&mut layout, inputs);
    assert!(job.is_done());
    let done = layout.clone();
    assert!(job.advance(&mut layout, inputs, &mut |_| true));
    assert_eq!(layout, done);
    // An empty document is done after one step.
    let empty = Document::default();
    let inputs = LayoutInputs {
        doc: &empty,
        ..inputs
    };
    let (mut job, mut layout) = LayoutJob::start(inputs, 60);
    assert!(job.advance(&mut layout, inputs, &mut |_| true));
    assert!(layout.lines.is_empty());
}
