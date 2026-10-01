//! Parser tests: document structure, the HTML subset, math, links, and
//! robustness of the builder against arbitrary event orders.

use proptest::prelude::*;
use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, CowStr, Event, HeadingLevel, LinkType,
    MetadataBlockKind, Tag, TagEnd,
};

use super::builder::{Builder, MAX_DEPTH};
use super::{ParseOptions, parse};
use crate::ir::*;
use crate::options::HtmlMode;

// ----- helpers -----------------------------------------------------------------

/// Parse with default options and check every invariant.
fn doc(md: &str) -> Document {
    doc_with(md, &ParseOptions::default())
}

fn doc_with(md: &str, opts: &ParseOptions) -> Document {
    let d = parse(md, opts);
    check(&d);
    d
}

/// Feed raw events to a builder.
fn build(events: Vec<Event<'static>>) -> Document {
    let mut b = Builder::new(ParseOptions::default());
    for ev in events {
        b.event(ev);
    }
    let d = b.finish();
    check(&d);
    d
}

/// Structural checks over a whole document: [`Document::validate`] plus
/// depth and sanitising checks.
fn check(doc: &Document) {
    if let Err(e) = doc.validate() {
        panic!("invalid document: {e}\n{}", doc.dump());
    }
    fn depth(bs: &[Block]) -> usize {
        bs.iter()
            .map(|b| match b {
                Block::Quote { body, .. }
                | Block::Align { body, .. }
                | Block::Details { body, .. } => 1 + depth(body),
                Block::List(l) => 1 + l.items.iter().map(|i| depth(&i.body)).max().unwrap_or(0),
                Block::DefList(items) => {
                    1 + items
                        .iter()
                        .flat_map(|i| i.defs.iter().map(|d| depth(d)))
                        .max()
                        .unwrap_or(0)
                }
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }
    assert!(depth(&doc.blocks) <= MAX_DEPTH, "nesting too deep");
    let all = format!("{}{}", doc.plain_text(), doc.dump());
    assert!(!all.contains(['\x1b', '\u{9b}', '\r']), "unsanitised text");
    for img in &doc.images {
        assert!(!img.alt.contains('\n'));
    }
}

/// The inline text of every paragraph, in order (recursing into containers).
fn paras(doc: &Document) -> Vec<String> {
    fn walk(bs: &[Block], out: &mut Vec<String>) {
        for b in bs {
            match b {
                Block::Para(t) => out.push(t.text.clone()),
                Block::Quote { body, .. }
                | Block::Align { body, .. }
                | Block::Details { body, .. } => {
                    walk(body, out);
                }
                Block::List(l) => l.items.iter().for_each(|i| walk(&i.body, out)),
                Block::DefList(items) => items
                    .iter()
                    .for_each(|i| i.defs.iter().for_each(|d| walk(d, out))),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&doc.blocks, &mut out);
    out
}

fn only_para(doc: &Document) -> &Inlines {
    match doc.blocks.as_slice() {
        [Block::Para(t)] => t,
        other => panic!("expected one paragraph, got {other:#?}"),
    }
}

fn math_runs(t: &Inlines) -> Vec<&str> {
    t.runs_with_ranges()
        .filter(|(_, r)| r.kind == RunKind::Math)
        .map(|(range, _)| t.slice(range))
        .collect()
}

fn text(s: &'static str) -> Event<'static> {
    Event::Text(CowStr::Borrowed(s))
}

// ----- paragraphs and inline content -------------------------------------------

#[test]
fn emphasis_code_and_breaks() {
    let d = doc("Hello *em* **strong** ~~del~~ `code`  \nnext\nline");
    let t = only_para(&d);
    assert_eq!(t.text, "Hello em strong del code\nnext line");
    let runs: Vec<(String, RunKind, InlineFlags)> = t
        .runs_with_ranges()
        .map(|(r, run)| (t.slice(r).to_string(), run.kind, run.flags))
        .collect();
    assert!(runs.contains(&("em".into(), RunKind::Text, InlineFlags::EMPH)));
    assert!(runs.contains(&("strong".into(), RunKind::Text, InlineFlags::STRONG)));
    assert!(runs.contains(&("del".into(), RunKind::Text, InlineFlags::STRIKE)));
    assert!(runs.contains(&("code".into(), RunKind::Code, InlineFlags::empty())));
    assert_eq!(t.hard_breaks, [24]);
}

#[test]
fn soft_break_between_cjk_is_removed() {
    assert_eq!(
        only_para(&doc("日本語の\nテキスト")).text,
        "日本語のテキスト"
    );
    assert_eq!(only_para(&doc("中文\nEnglish")).text, "中文 English");
    assert_eq!(only_para(&doc("한국어\n문장")).text, "한국어 문장");
}

#[test]
fn whitespace_is_collapsed_but_code_is_verbatim() {
    let t = doc("a  b\tc `x  y`");
    assert_eq!(only_para(&t).text, "a b c x  y");
}

#[test]
fn control_characters_from_entities_are_sanitised() {
    let d = doc("esc &#27;[31m and &#x9b; in `code`\n\n```\n&#27; raw \u{1b} esc\n```");
    let all = d.plain_text();
    assert!(
        !all.contains('\u{1b}') && !all.contains('\u{9b}'),
        "{all:?}"
    );
    assert!(all.contains("␛[31m"));
}

#[test]
fn smart_punctuation_is_optional() {
    let src = "\"quote\" -- it's...";
    assert_eq!(only_para(&doc(src)).text, src);
    let mut opts = ParseOptions::default();
    opts.markdown.smart_punctuation = true;
    assert_eq!(only_para(&doc_with(src, &opts)).text, "“quote” – it’s…");
}

// ----- links ----------------------------------------------------------------------

#[test]
fn link_kinds_and_targets() {
    let d = doc(
        "[a](https://x.org \"T\") [b][r] <https://auto.org/p> <me@x.org> [c](#Sec) \
         [d](docs/guide.md#usage) [e](img.png)\n\n[r]: https://ref.org",
    );
    let got: Vec<(LinkKind, &str, Target)> = d
        .links
        .iter()
        .map(|l| (l.kind, &*l.url, l.target.clone()))
        .collect();
    assert_eq!(
        got,
        [
            (LinkKind::Inline, "https://x.org", Target::External),
            (LinkKind::Reference, "https://ref.org", Target::External),
            (LinkKind::Autolink, "https://auto.org/p", Target::External),
            (LinkKind::Email, "mailto:me@x.org", Target::Email),
            (LinkKind::Inline, "#Sec", Target::Anchor("Sec".into())),
            (
                LinkKind::Inline,
                "docs/guide.md#usage",
                Target::LocalDoc {
                    path: "docs/guide.md".into(),
                    anchor: Some("usage".into())
                }
            ),
            (
                LinkKind::Inline,
                "img.png",
                Target::LocalFile("img.png".into())
            ),
        ]
    );
    assert_eq!(&*d.links[0].title, "T");
    // Autolink text gets URL break points.
    assert!(!only_para(&d).extra_breaks.is_empty());
}

#[test]
fn wikilinks_when_enabled() {
    let opts = ParseOptions {
        wikilinks: true,
        ..ParseOptions::default()
    };
    let d = doc_with("see [[Page Name|the page]]", &opts);
    assert_eq!(only_para(&d).text, "see the page");
    assert_eq!(d.links[0].kind, LinkKind::Wiki);
    assert_eq!(
        d.links[0].target,
        Target::LocalDoc {
            path: "Page Name.md".into(),
            anchor: None
        }
    );
    // Off by default: literal brackets.
    assert_eq!(only_para(&doc("[[Page]]")).text, "[[Page]]");
}

#[test]
fn bare_urls_are_linkified_outside_code_and_links() {
    let d = doc("Go to https://example.com/a/b, or www.rust-lang.org. \
         Mail me@example.org. `https://code.example` and [https://in.link](https://x).");
    let urls: Vec<&str> = d.links.iter().map(|l| &*l.url).collect();
    assert_eq!(
        urls,
        [
            "https://x",
            "https://example.com/a/b",
            "http://www.rust-lang.org",
            "mailto:me@example.org"
        ]
    );
    let t = only_para(&d);
    let linked: Vec<&str> = t
        .runs_with_ranges()
        .filter(|(_, r)| r.link.is_some())
        .map(|(range, _)| t.slice(range))
        .collect();
    assert_eq!(
        linked,
        [
            "https://example.com/a/b",
            "www.rust-lang.org",
            "me@example.org",
            "https://in.link"
        ]
    );
    // Break points inside the bare URL.
    let start = t.text.find("https://example").unwrap() as u32;
    assert!(t.extra_breaks.contains(&(start + 8)));
    assert!(t.extra_breaks.contains(&(start + 20)));
}

#[test]
fn linkify_can_be_disabled() {
    let mut opts = ParseOptions::default();
    opts.markdown.linkify = false;
    assert!(doc_with("see https://example.com", &opts).links.is_empty());
}

#[test]
fn linkify_spans_emphasis_boundaries() {
    let d = doc("https://example.com/*path*");
    let t = only_para(&d);
    assert!(t.runs.iter().all(|r| r.link == Some(LinkId(0))), "{t:#?}");
}

// ----- images and figures ---------------------------------------------------------------

#[test]
fn figures_and_chips() {
    let d = doc("![alt *text*](a.png \"Title\")\n\n\
         [![linked](b.png)](https://x)\n\n\
         text ![chip](c.png) text\n\n\
         ![one](d.png) ![two](e.png)\n\n\
         ![](path/to/f%20g.svg?x=1)");
    let kinds: Vec<&str> = d
        .blocks
        .iter()
        .map(|b| match b {
            Block::Figure(_) => "figure",
            Block::Para(_) => "para",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["figure", "figure", "para", "para", "figure"]);
    assert_eq!(&*d.images[0].alt, "alt text");
    assert_eq!(d.caption(&Figure { image: ImageId(0) }), "Title");
    assert_eq!(d.images[1].link, Some(LinkId(0)));
    assert_eq!(paras(&d)[0], "text chip text");
    // An image without alt text: no caption, and its chip shows the file name.
    let Block::Figure(f) = &d.blocks[4] else {
        unreachable!()
    };
    assert_eq!(d.caption(f), "");
    let d2 = doc("x ![](path/to/f%20g.svg?x=1)");
    assert_eq!(only_para(&d2).text, "x f g.svg");
}

#[test]
fn html_images_with_size_hints_and_pictures() {
    let d = doc(
        "<img src=\"logo.png\" alt=\"Logo\" width=\"200\" height=\"50%\">\n\n\
         <picture>\n\
         <source media=\"(prefers-color-scheme: dark)\" srcset=\"dark.png 1x, dark2.png 2x\">\n\
         <source media=\"(prefers-color-scheme: light)\" srcset=\"light.png\">\n\
         <img src=\"fallback.png\" alt=\"Pic\">\n\
         </picture>\n",
    );
    assert!(matches!(
        d.blocks.as_slice(),
        [Block::Figure(_), Block::Figure(_)]
    ));
    let logo = &d.images[0];
    assert_eq!((&*logo.src, &*logo.alt), ("logo.png", "Logo"));
    assert_eq!(logo.width, Some(Length::Px(200)));
    assert_eq!(logo.height, Some(Length::Percent(50)));
    let pic = &d.images[1];
    assert_eq!(&*pic.src, "fallback.png");
    let media: Vec<Option<&str>> = pic.sources.iter().map(|s| s.media.as_deref()).collect();
    assert_eq!(
        media,
        [
            Some("(prefers-color-scheme: dark)"),
            Some("(prefers-color-scheme: light)")
        ]
    );
    assert_eq!(pic.sources[0].first_url(), "dark.png");
}

// ----- headings ------------------------------------------------------------------------------

#[test]
fn heading_index_slugs_and_parents() {
    let d = doc("# Intro\n\n## Setup\n\n### Details\n\n## Setup\n\n# Intro\n\n#### Deep");
    let got: Vec<(u8, &str, Option<u32>)> = d
        .headings
        .iter()
        .map(|h| (h.level, &*h.slug, h.parent.map(|p| p.0)))
        .collect();
    assert_eq!(
        got,
        [
            (1, "intro", None),
            (2, "setup", Some(0)),
            (3, "details", Some(1)),
            (2, "setup-1", Some(0)),
            (1, "intro-1", None),
            (4, "deep", Some(4)),
        ]
    );
    assert_eq!(
        d.resolve_anchor("setup-1"),
        Some(Anchor::Heading(HeadingId(3)))
    );
    assert_eq!(d.title.as_deref(), Some("Intro"));
}

#[test]
fn heading_slugs_follow_github() {
    let d = doc(
        "# 🎉 Features & *More*\n\n## `code()` stuff\n\n## Über Straße\n\n## ![logo](l.png) Name",
    );
    let slugs: Vec<&str> = d.headings.iter().map(|h| &*h.slug).collect();
    assert_eq!(
        slugs,
        ["-features--more", "code-stuff", "über-straße", "-name"]
    );
    assert_eq!(&*d.headings[3].title, "Name");
}

#[test]
fn heading_attributes_when_enabled() {
    let opts = ParseOptions {
        heading_attributes: true,
        ..ParseOptions::default()
    };
    let d = doc_with("# Title {#custom}\n\n# Custom", &opts);
    let slugs: Vec<&str> = d.headings.iter().map(|h| &*h.slug).collect();
    // The explicit id is reserved, so the generated slug avoids it.
    assert_eq!(slugs, ["custom", "custom-1"]);
    // Off by default: the attribute text stays.
    assert_eq!(
        &*doc("# Title {#custom}").headings[0].title,
        "Title {#custom}"
    );
}

#[test]
fn html_anchors() {
    let d = doc(
        "<a name=\"top\"></a>\n\n# Title <a id=\"t\"></a>\n\nPara <a id=\"p\"></a> here.\n\n\
         <a id=\"before-code\"></a>\n\n```\ncode\n```\n",
    );
    assert_eq!(d.resolve_anchor("top"), Some(Anchor::Heading(HeadingId(0))));
    assert_eq!(d.resolve_anchor("t"), Some(Anchor::Heading(HeadingId(0))));
    assert_eq!(d.resolve_anchor("p"), Some(Anchor::Block(BlockId(1))));
    assert_eq!(
        d.resolve_anchor("before-code"),
        Some(Anchor::Block(BlockId(2)))
    );
    assert_eq!(d.blocks.len(), 3, "anchor-only paragraphs vanish");
}

#[test]
fn html_headings_and_title() {
    let d = doc("<h1 align=\"center\">My <em>Project</em></h1>\n\n<h3 id=\"x\">Three</h3>\n");
    let [
        Block::Align { align, body },
        Block::Heading { level: 3, .. },
    ] = d.blocks.as_slice()
    else {
        panic!("{:#?}", d.blocks);
    };
    assert_eq!(*align, HAlign::Center);
    assert!(matches!(body.as_slice(), [Block::Heading { level: 1, .. }]));
    assert_eq!(d.title.as_deref(), Some("My Project"));
    assert_eq!(d.resolve_anchor("x"), Some(Anchor::Heading(HeadingId(1))));
    assert_eq!(
        d.resolve_anchor("my-project"),
        Some(Anchor::Heading(HeadingId(0)))
    );
}

#[test]
fn front_matter_title_wins() {
    let d = doc("---\ntitle: From FM\ntags: [a]\n---\n\n# Heading\n");
    assert_eq!(d.title.as_deref(), Some("From FM"));
    let fm = d.front_matter().unwrap();
    assert_eq!(fm.format, FrontMatterFormat::Yaml);
    assert_eq!(fm.fields.as_ref().unwrap().len(), 2);
    let toml = doc("+++\ntitle = \"T\"\n[extra]\nx = 1\n+++\n");
    let fm = toml.front_matter().unwrap();
    assert_eq!(fm.format, FrontMatterFormat::Toml);
    assert!(fm.fields.is_none(), "nested TOML is not flat");
    assert!(toml.title.is_none());
}

// ----- lists, quotes, tables, definition lists -----------------------------------------------

#[test]
fn tight_and_loose_lists() {
    let d = doc("- a\n- b\n\n1. x\n\n2. y\n\n7) seven\n");
    let lists: Vec<(Option<u64>, bool)> = d
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::List(l) => Some((l.start, l.tight)),
            _ => None,
        })
        .collect();
    assert_eq!(lists, [(None, true), (Some(1), false), (Some(7), true)]);
}

#[test]
fn task_items_with_paragraphs() {
    // mdcat #302: a task list item containing paragraphs.
    let d = doc("- [ ] task\n\n  more text\n\n  - [x] nested done\n- [x] done\n- plain\n");
    let Block::List(list) = &d.blocks[0] else {
        panic!()
    };
    let tasks: Vec<Option<bool>> = list.items.iter().map(|i| i.task).collect();
    assert_eq!(tasks, [Some(false), Some(true), None]);
    assert!(!list.tight);
    let first = &list.items[0].body;
    assert!(matches!(
        first.as_slice(),
        [Block::Para(_), Block::Para(_), Block::List(_)]
    ));
    let Block::List(nested) = &first[2] else {
        panic!()
    };
    assert_eq!(nested.items[0].task, Some(true));
}

#[test]
fn alerts_and_quotes() {
    let d = doc("> [!WARNING]\n> Careful\n\n> plain\n> > nested\n");
    assert!(matches!(
        d.blocks.as_slice(),
        [
            Block::Quote {
                alert: Some(Alert::Warning),
                ..
            },
            Block::Quote { alert: None, .. }
        ]
    ));
}

#[test]
fn tables_are_rectangular_with_inline_content() {
    let d = doc("| a | b | c |\n|:--|:-:|--:|\n| 1 | **2** |\n| `x` | [l](u) | $y$ | extra |\n");
    let Block::Table(t) = &d.blocks[0] else {
        panic!("{:#?}", d.blocks)
    };
    assert_eq!(
        t.align,
        [
            Some(HAlign::Left),
            Some(HAlign::Center),
            Some(HAlign::Right)
        ]
    );
    assert_eq!(t.rows.len(), 2);
    assert!(t.rows.iter().all(|r| r.len() == 3));
    assert_eq!(t.rows[0][2].text, "");
    assert_eq!(t.rows[1][2].runs[0].kind, RunKind::Math);
}

#[test]
fn footnotes_in_tables() {
    // mdcat #43: a footnote reference inside a table cell.
    let d = doc("| h[^1] |\n|---|\n| cell[^2] |\n\n[^1]: head note\n[^2]: cell note\n");
    let Block::Table(t) = &d.blocks[0] else {
        panic!()
    };
    assert_eq!(t.head[0].text, "h1");
    assert_eq!(t.rows[0][0].text, "cell2");
    assert_eq!(t.rows[0][0].runs[1].kind, RunKind::FootRef(FootnoteId(1)));
    assert_eq!(d.footnotes.len(), 2);
    assert!(matches!(d.blocks.last(), Some(Block::FootnoteSection)));
}

#[test]
fn footnotes_numbered_by_first_reference_unused_dropped() {
    let d = doc("Second[^b] first? No: [^a] then [^b] again.\n\n\
         [^a]: Note A with [^c].\n[^b]: Note B.\n[^c]: Note C.\n[^unused]: Dropped.\n");
    let labels: Vec<&str> = d.footnotes.iter().map(|f| &*f.label).collect();
    assert_eq!(labels, ["b", "a", "c"]);
    assert_eq!(d.footnotes[0].refs.len(), 2);
    assert_eq!(paras(&d)[0], "Second1 first? No: 2 then 1 again.");
    assert_eq!(d.plain_text().matches("Dropped").count(), 0);
    // References are atoms and links to their footnote.
    let Block::Para(p) = &d.blocks[0] else {
        panic!()
    };
    let fr = p
        .runs
        .iter()
        .find(|r| matches!(r.kind, RunKind::FootRef(_)))
        .unwrap();
    let link = d.link(fr.link.unwrap()).unwrap();
    assert_eq!(link.target, Target::Footnote(FootnoteId(0)));
    assert_eq!(p.atoms[0], 6..7);
}

#[test]
fn definition_lists() {
    let d = doc("Term\n: Def one\n: Def two\n\nOther\n: X\n");
    let Block::DefList(items) = &d.blocks[0] else {
        panic!("{:#?}", d.blocks)
    };
    let terms: Vec<&str> = items.iter().map(|i| &*i.term.text).collect();
    assert_eq!(terms, ["Term", "Other"]);
    assert_eq!(items[0].defs.len(), 2);
    let mut opts = ParseOptions::default();
    opts.markdown.definition_lists = false;
    assert!(matches!(
        doc_with("Term\n: Def\n", &opts).blocks[0],
        Block::Para(_)
    ));
}

#[test]
fn code_blocks_and_info_strings() {
    let d = doc(
        "```rust title=\"main.rs\"\nfn main() {}\n\tx\n```\n\n```{.python}\nx\n```\n\n\
         ```rust,ignore\ny\n```\n\n    indented\n",
    );
    let got: Vec<(Option<&str>, Option<&str>, &str)> = d
        .blocks
        .iter()
        .map(|b| match b {
            Block::Code(c) => (c.lang.as_deref(), c.title.as_deref(), c.code.as_str()),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        got,
        [
            (Some("rust"), Some("main.rs"), "fn main() {}\n\tx"),
            (Some("python"), None, "x"),
            (Some("rust"), None, "y"),
            (None, None, "indented"),
        ]
    );
}

// ----- math ---------------------------------------------------------------------------------

#[test]
fn dollar_math_and_the_digit_rule() {
    let d = doc("costs $5 and $10, or $5-$10; but $x^2$ is math.");
    let t = only_para(&d);
    assert_eq!(math_runs(t), ["x^2"]);
    assert_eq!(t.text, "costs $5 and $10, or $5-$10; but x^2 is math.");
}

#[test]
fn github_backtick_math_and_math_fences() {
    let d = doc("inline $`\\sqrt{2}`$ here\n\n```math\n\\frac12\n```\n");
    let [Block::Para(p), Block::Math(m)] = d.blocks.as_slice() else {
        panic!("{:#?}", d.blocks)
    };
    assert_eq!(math_runs(p), ["\\sqrt{2}"]);
    assert_eq!(&*m.tex, "\\frac12");
}

#[test]
fn tex_delimiters() {
    let d = doc("LLM style \\(a_1 + b_1\\) and\n\\[\nE = mc^2\n\\]\nafter");
    let [Block::Para(p1), Block::Math(m), Block::Para(p2)] = d.blocks.as_slice() else {
        panic!("{:#?}", d.blocks)
    };
    assert_eq!(math_runs(p1), ["a_1 + b_1"]);
    assert_eq!(p1.text, "LLM style a_1 + b_1 and");
    assert_eq!(&*m.tex, "E = mc^2");
    assert_eq!(p2.text, "after");

    let opts = ParseOptions {
        tex_delimiters: false,
        ..ParseOptions::default()
    };
    assert_eq!(only_para(&doc_with("x \\(a\\) y", &opts)).text, "x (a) y");
    // Math off disables the delimiters too.
    let mut opts = ParseOptions::default();
    opts.markdown.math = false;
    assert_eq!(
        only_para(&doc_with("x \\(a\\) $b$ y", &opts)).text,
        "x (a) $b$ y"
    );
}

#[test]
fn multi_line_display_math_survives_block_syntax() {
    // A line of `=` would make a setext heading, `- x` a list item.
    let d = doc(
        "Write it as\n\\[\n\\mathrm{KL}(q'\\|p_*)\n=\n\\mathbb E_q[x]\n\\]\nafter\n\n$$\na\n- b\n$$\n",
    );
    let [
        Block::Para(before),
        Block::Math(m1),
        Block::Para(after),
        Block::Math(m2),
    ] = d.blocks.as_slice()
    else {
        panic!("{:#?}", d.blocks)
    };
    assert_eq!(before.text, "Write it as");
    // Markdown escapes (`\|`, `p_*`) reach the TeX untouched.
    assert_eq!(&*m1.tex, "\\mathrm{KL}(q'\\|p_*)\n=\n\\mathbb E_q[x]");
    assert_eq!(after.text, "after");
    assert_eq!(&*m2.tex, "a\n- b");
    assert!(d.headings.is_empty());
}

#[test]
fn code_spans_stop_tex_delimiters() {
    let d = doc("\\( a `b` c \\)");
    assert_eq!(only_para(&d).text, "( a b c )");
    assert!(math_runs(only_para(&d)).is_empty());
}

#[test]
fn display_math_splits_paragraphs() {
    let d = doc("before $$x = 1$$ after\n\n$$\ny\n$$\n\n*em $$z$$ still em*");
    let shapes: Vec<&str> = d
        .blocks
        .iter()
        .map(|b| match b {
            Block::Para(_) => "para",
            Block::Math(_) => "math",
            _ => "other",
        })
        .collect();
    assert_eq!(
        shapes,
        ["para", "math", "para", "math", "para", "math", "para"]
    );
    // Emphasis continues after the lifted math.
    let Some(Block::Para(last)) = d.blocks.last() else {
        panic!()
    };
    assert_eq!(last.text, "still em");
    assert_eq!(last.runs[0].flags, InlineFlags::EMPH);
}

#[test]
fn display_math_stays_inline_in_cells_headings_and_links() {
    let d = doc("# H $$x$$\n\n| $$y$$ |\n|---|\n| $$z$$ |\n\n[a $$w$$ b](u)\n");
    assert!(matches!(&d.blocks[0], Block::Heading { text, .. } if math_runs(text) == ["x"]));
    let Block::Table(t) = &d.blocks[1] else {
        panic!()
    };
    assert_eq!(math_runs(&t.head[0]), ["y"]);
    assert_eq!(math_runs(&t.rows[0][0]), ["z"]);
    assert!(matches!(&d.blocks[2], Block::Para(p) if math_runs(p) == ["w"]));
}

#[test]
fn math_is_an_atom() {
    let d = doc("a $x + y$ b");
    let atoms = &only_para(&d).atoms;
    assert_eq!((atoms.len(), atoms.first()), (1, Some(&(2..7))));
}

// ----- HTML -----------------------------------------------------------------------------------

#[test]
fn details_across_html_blocks() {
    let d = doc(
        "<details>\n<summary>Click <b>me</b></summary>\n\nHidden *markdown*.\n\n- item\n\n\
         </details>\n\nAfter\n",
    );
    let [Block::Details { summary, body }, Block::Para(after)] = d.blocks.as_slice() else {
        panic!("{:#?}", d.blocks)
    };
    assert_eq!(summary.text, "Click me");
    assert_eq!(summary.runs[1].flags, InlineFlags::STRONG);
    assert!(matches!(body.as_slice(), [Block::Para(_), Block::List(_)]));
    assert_eq!(after.text, "After");
}

#[test]
fn unclosed_details_ends_with_its_container() {
    let d = doc("> <details>\n> <summary>S</summary>\n>\n> inside\n\noutside\n");
    let [Block::Quote { body, .. }, Block::Para(p)] = d.blocks.as_slice() else {
        panic!("{:#?}", d.blocks)
    };
    assert!(matches!(body.as_slice(), [Block::Details { .. }]));
    assert_eq!(p.text, "outside");
}

#[test]
fn align_divs_and_center() {
    let d = doc(
        "<div align=\"center\">\n\n# Title\n\n<div>\ninner\n</div>\n\ntext\n\n</div>\n\n\
         <center>c</center>\n\n<p align=\"right\">r</p>\n",
    );
    let aligns: Vec<(HAlign, usize)> = d
        .blocks
        .iter()
        .map(|b| match b {
            Block::Align { align, body } => (*align, body.len()),
            other => panic!("{other:#?}"),
        })
        .collect();
    assert_eq!(
        aligns,
        [(HAlign::Center, 3), (HAlign::Center, 1), (HAlign::Right, 1)]
    );
}

#[test]
fn unmatched_close_tags_are_ignored() {
    let d = doc("</div>\n\n</details>\n\ntext\n\n</p>\n");
    assert_eq!(paras(&d), ["text"]);
    assert_eq!(d.blocks.len(), 1);
}

#[test]
fn nested_p_is_auto_closed() {
    let d = doc("<p align=\"center\">one\n<p>two\n");
    let [Block::Align { body, .. }, Block::Para(two)] = d.blocks.as_slice() else {
        panic!("{:#?}", d.blocks)
    };
    assert!(matches!(body.as_slice(), [Block::Para(one)] if one.text == "one"));
    assert_eq!(two.text, "two");
}

#[test]
fn inline_html_formatting() {
    let d = doc(
        "<b>b</b> <i>i</i> <u>u</u> <ins>ins</ins> <s>s</s> <del>d</del> <mark>m</mark> \
         H<sub>2</sub>O x<sup>2</sup> <kbd>K</kbd> <code>c</code> <tt>t</tt> <span>plain</span> \
         <a href=\"https://x\">link</a> <!-- gone -->end",
    );
    let t = only_para(&d);
    assert_eq!(t.text, "b i u ins s d m H2O x2 K c t plain link end");
    let at = |w: &str| {
        let start = t.text.find(w).unwrap() as u32;
        t.runs_with_ranges()
            .find(|(r, _)| r.start <= start && start < r.end)
            .map(|(_, run)| (run.kind, run.flags, run.link.is_some()))
            .unwrap()
    };
    assert_eq!(at("b "), (RunKind::Text, InlineFlags::STRONG, false));
    assert_eq!(at("u "), (RunKind::Text, InlineFlags::UNDERLINE, false));
    assert_eq!(at("ins"), (RunKind::Text, InlineFlags::UNDERLINE, false));
    assert_eq!(at("d "), (RunKind::Text, InlineFlags::STRIKE, false));
    assert_eq!(at("m "), (RunKind::Text, InlineFlags::MARK, false));
    assert_eq!(at("2O"), (RunKind::Text, InlineFlags::SUB, false));
    assert_eq!(at("K"), (RunKind::Text, InlineFlags::KBD, false));
    assert_eq!(at("c "), (RunKind::Code, InlineFlags::empty(), false));
    assert_eq!(at("t "), (RunKind::Code, InlineFlags::empty(), false));
    assert_eq!(at("plain"), (RunKind::Text, InlineFlags::empty(), false));
    assert_eq!(at("link"), (RunKind::Text, InlineFlags::empty(), true));
    assert_eq!(d.links[0].kind, LinkKind::Html);
}

#[test]
fn unclosed_inline_html_ends_with_the_paragraph() {
    let d = doc("a <b>bold\n\nnext para");
    assert_eq!(paras(&d), ["a bold", "next para"]);
    let Block::Para(next) = &d.blocks[1] else {
        panic!()
    };
    assert_eq!(next.runs[0].flags, InlineFlags::empty());
}

#[test]
fn html_block_text_and_entities() {
    let d = doc("<div>\n  AT&amp;T &copy; &nbsp;x &#128512; &bogus;\n  <br>\n  line two\n</div>\n");
    assert_eq!(paras(&d), ["AT&T © \u{a0}x 😀 &bogus;\nline two"]);
}

#[test]
fn br_everywhere() {
    // mdcat #301: <br> in any position.
    let src = "<br>\n\na<br>b<br/><br />c<BR>\n\n# H<br>x\n\n| a<br>b |\n|---|\n| c<br> |\n\n\
               - item<br>\n> quote<br>tail\n\n<br><br>\n\n**x<br>y**\n";
    let d = doc(src);
    assert_eq!(paras(&d), ["a\nb\n\nc", "item", "quote\ntail", "x\ny"]);
    let Block::Heading { text, .. } = &d.blocks[1] else {
        panic!("{:#?}", d.blocks)
    };
    assert_eq!(text.text, "H\nx");
    let Block::Table(t) = &d.blocks[2] else {
        panic!()
    };
    assert_eq!(
        (t.head[0].text.as_str(), t.rows[0][0].text.as_str()),
        ("a\nb", "c")
    );
}

#[test]
fn script_style_and_comments_are_dropped() {
    let d = doc(
        "<script>\nalert('x')\n</script>\n\n<style>p{}</style>\n\ntext <script>bad()</script> more\n\n\
         <!--\nmulti\nline\n-->\n\nend\n",
    );
    assert_eq!(paras(&d), ["text more", "end"]);
}

#[test]
fn pre_blocks_become_code() {
    let d = doc("<pre>\n  keep   spacing\n  &lt;tag&gt;\n</pre>\n");
    let [Block::Code(c)] = d.blocks.as_slice() else {
        panic!("{:#?}", d.blocks)
    };
    assert_eq!(c.code, "  keep   spacing\n  <tag>");
}

#[test]
fn html_modes() {
    let src = "<div align=\"center\">\n<b>x</b>\n</div>\n\na <i>b</i> c\n";
    let raw = doc_with(
        src,
        &ParseOptions {
            html: HtmlMode::Raw,
            ..ParseOptions::default()
        },
    );
    assert!(matches!(&raw.blocks[0], Block::Html(h) if h.starts_with("<div")));
    let Block::Para(p) = &raw.blocks[1] else {
        panic!()
    };
    assert_eq!(p.text, "a <i>b</i> c");
    assert_eq!(p.runs[1].kind, RunKind::Html);

    let strip = doc_with(
        src,
        &ParseOptions {
            html: HtmlMode::Strip,
            ..ParseOptions::default()
        },
    );
    assert_eq!(paras(&strip), ["x", "a b c"]);
}

// ----- robustness -----------------------------------------------------------------------------

#[test]
fn weird_event_orders_do_not_panic() {
    use Event::*;
    let image = |dest: &'static str| {
        Start(Tag::Image {
            link_type: LinkType::Inline,
            dest_url: dest.into(),
            title: "".into(),
            id: "".into(),
        })
    };
    let cases: Vec<Vec<Event<'static>>> = vec![
        // Ends without starts.
        vec![
            End(TagEnd::Paragraph),
            End(TagEnd::List(true)),
            End(TagEnd::Item),
            End(TagEnd::BlockQuote(None)),
            End(TagEnd::TableCell),
            End(TagEnd::Link),
            End(TagEnd::Emphasis),
            End(TagEnd::Image),
            End(TagEnd::CodeBlock),
            End(TagEnd::HtmlBlock),
            End(TagEnd::Heading(HeadingLevel::H1)),
            text("x"),
        ],
        // Items, rows and cells outside their containers.
        vec![
            Start(Tag::Item),
            text("loose item"),
            End(TagEnd::Item),
            Start(Tag::TableCell),
            text("cell"),
            End(TagEnd::TableCell),
            Start(Tag::TableRow),
            End(TagEnd::TableRow),
            Start(Tag::DefinitionListDefinition),
            text("def"),
            End(TagEnd::DefinitionListDefinition),
        ],
        // Blocks inside leaves, leaves inside leaves.
        vec![
            Start(Tag::Paragraph),
            text("para"),
            Start(Tag::Heading {
                level: HeadingLevel::H2,
                id: None,
                classes: vec![],
                attrs: vec![],
            }),
            text("heading"),
            Start(Tag::BlockQuote(Some(BlockQuoteKind::Tip))),
            text("quoted"),
            Rule,
            End(TagEnd::Paragraph),
        ],
        // Paragraphs directly in lists and tables.
        vec![
            Start(Tag::List(Some(3))),
            Start(Tag::Paragraph),
            text("no item"),
            End(TagEnd::Paragraph),
            End(TagEnd::List(true)),
            Start(Tag::Table(vec![Alignment::Left])),
            Start(Tag::Paragraph),
            text("no cell"),
            End(TagEnd::Paragraph),
            Start(Tag::TableCell),
            text("c"),
            End(TagEnd::TableCell),
            End(TagEnd::Table),
        ],
        // Captures that never close.
        vec![
            Start(Tag::CodeBlock(CodeBlockKind::Fenced("rust".into()))),
            text("fn x() {}"),
            Start(Tag::Paragraph),
            text("after code"),
        ],
        vec![image("x.png"), text("alt"), image("y.png")],
        vec![Start(Tag::HtmlBlock), Html("<details><summary>s".into())],
        vec![
            Start(Tag::MetadataBlock(MetadataBlockKind::YamlStyle)),
            text("a: b\n"),
        ],
        // Inline events with no block around them.
        vec![
            TaskListMarker(true),
            FootnoteReference("n".into()),
            DisplayMath("x".into()),
            InlineMath("y".into()),
            SoftBreak,
            HardBreak,
            InlineHtml("<br>".into()),
            Html("</div>".into()),
            Code("c".into()),
        ],
        // Unbalanced inline formatting and links.
        vec![
            Start(Tag::Paragraph),
            Start(Tag::Emphasis),
            Start(Tag::Link {
                link_type: LinkType::Inline,
                dest_url: "u".into(),
                title: "".into(),
                id: "".into(),
            }),
            text("x"),
            End(TagEnd::Strong),
            End(TagEnd::Paragraph),
            text("after"),
            End(TagEnd::Link),
        ],
    ];
    for events in cases {
        let d = build(events);
        let _ = d.plain_text();
        let _ = d.dump();
    }
}

#[test]
fn stray_events_keep_their_text() {
    let d = build(vec![
        Event::Start(Tag::Item),
        text("loose item"),
        Event::End(TagEnd::Item),
        Event::Start(Tag::List(None)),
        Event::Start(Tag::Paragraph),
        text("no item"),
        Event::End(TagEnd::Paragraph),
        Event::End(TagEnd::List(false)),
    ]);
    assert_eq!(paras(&d), ["loose item", "no item"]);
}

#[test]
fn anchors_inside_footnotes_point_at_the_footnote_section() {
    let d = doc("Text[^f].\n\n[^f]: Note <a id=\"in-note\"></a> here.\n\n<a name=\"x\"></a>\n");
    let section = d.blocks.len() - 1;
    assert!(matches!(d.blocks[section], Block::FootnoteSection));
    assert_eq!(
        d.resolve_anchor("in-note"),
        Some(Anchor::Block(BlockId(section as u32)))
    );
    // Found by the random event test: an anchor in an unreferenced
    // definition must not point past the last block.
    let d = build(vec![
        text("日本"),
        Event::Start(Tag::FootnoteDefinition("f".into())),
        Event::Html("<a name=\"n\">".into()),
        text("日本"),
    ]);
    assert_eq!(d.resolve_anchor("n"), None);
}

#[test]
fn referenced_but_undefined_footnote_has_empty_body() {
    let d = build(vec![
        Event::Start(Tag::Paragraph),
        Event::FootnoteReference("ghost".into()),
        Event::End(TagEnd::Paragraph),
    ]);
    assert_eq!(d.footnotes.len(), 1);
    assert!(d.footnotes[0].body.is_empty());
}

#[test]
fn deep_nesting_is_kept_to_fifty_levels() {
    // Lists in quotes in lists, 50 levels: even levels are list items, odd
    // levels are quotes.
    let mut src = String::new();
    let mut prefix = String::new();
    for level in 0..50 {
        if level % 2 == 0 {
            src.push_str(&format!("{prefix}- level {level}\n"));
            prefix.push_str("  ");
        } else {
            src.push_str(&format!("{prefix}> level {level}\n"));
            prefix.push_str("> ");
        }
    }
    let d = doc(&src);
    fn depth(bs: &[Block]) -> usize {
        bs.iter()
            .map(|b| match b {
                Block::Quote { body, .. } => 1 + depth(body),
                Block::List(l) => 1 + l.items.iter().map(|i| depth(&i.body)).max().unwrap_or(0),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }
    assert_eq!(depth(&d.blocks), 50);
    assert!(d.plain_text().contains("level 49"));
    assert!(d.diagnostics.is_empty());
}

#[test]
fn extreme_nesting_is_flattened_without_overflow() {
    let quotes = format!("{} deep\n", ">".repeat(5000));
    let d = doc(&quotes);
    assert!(d.plain_text().contains("deep"));
    assert_eq!(d.diagnostics.len(), 1);
    let lists = format!("{}x\n", "- ".repeat(3000));
    assert!(doc(&lists).plain_text().contains('x'));
    let divs = format!("{}x{}", "<div>\n".repeat(3000), "</div>\n".repeat(3000));
    let _ = doc(&divs);
    let emph = format!("{}x{}", "*".repeat(3000), "*".repeat(3000));
    let _ = doc(&emph);
}

#[test]
fn corpus_of_nasty_inputs() {
    let inputs = [
        "",
        "\n\n\n",
        "#",
        "- ",
        "> ",
        "|",
        "| a |\n|---|",
        "[^]",
        "[^1]:",
        "$$",
        "$$$$",
        "$`$",
        "\\(",
        "\\)",
        "\\[\\]",
        "<",
        "<!--",
        "<details>",
        "</summary>",
        "<a href=",
        "<img>",
        "<img src>",
        "![",
        "![]()",
        "[]()",
        "***",
        "```",
        "```math",
        "~~~\n\t\u{0}\n",
        "---\n---",
        "+++",
        "- [ ]",
        "- [x]\n  ```\n  x",
        "<p align=center><p align=right><div align=left>",
        "a\u{2028}b\u{2029}c\u{85}d",
        "&#0;&#xFFFFFF;&#55296;",
        "\u{feff}# bom",
    ];
    for input in inputs {
        let _ = doc(input).dump();
    }
}

/// Events for the random-order property test.
fn arb_event() -> impl Strategy<Value = Event<'static>> {
    let words = prop::sample::select(vec![
        "word",
        " ",
        "",
        "日本",
        "a_b*c",
        "(",
        ")",
        "\\",
        "https://x.org/p",
        "\u{1b}",
        "\n",
    ]);
    let html = prop::sample::select(vec![
        "<b>",
        "</b>",
        "<br>",
        "<details>",
        "</details>",
        "<summary>",
        "</summary>",
        "<div align=\"center\">",
        "</div>",
        "<p>",
        "</p>",
        "<a href=\"u\">",
        "</a>",
        "<a name=\"n\">",
        "<img src=\"i.png\">",
        "<picture>",
        "<source srcset=\"s\">",
        "<pre>",
        "</pre>",
        "<script>",
        "<h2 align=\"center\">",
        "</h2>",
        "<code>",
        "<!--",
        "-->",
    ]);
    let tag = prop::sample::select(vec![
        Tag::Paragraph,
        Tag::Heading {
            level: HeadingLevel::H3,
            id: None,
            classes: vec![],
            attrs: vec![],
        },
        Tag::BlockQuote(None),
        Tag::BlockQuote(Some(BlockQuoteKind::Caution)),
        Tag::CodeBlock(CodeBlockKind::Fenced("math".into())),
        Tag::CodeBlock(CodeBlockKind::Indented),
        Tag::HtmlBlock,
        Tag::List(None),
        Tag::List(Some(2)),
        Tag::Item,
        Tag::FootnoteDefinition("f".into()),
        Tag::DefinitionList,
        Tag::DefinitionListTitle,
        Tag::DefinitionListDefinition,
        Tag::Table(vec![Alignment::None, Alignment::Right]),
        Tag::TableHead,
        Tag::TableRow,
        Tag::TableCell,
        Tag::Emphasis,
        Tag::Strong,
        Tag::Strikethrough,
        Tag::Superscript,
        Tag::Subscript,
        Tag::Link {
            link_type: LinkType::Autolink,
            dest_url: "https://a.b/c".into(),
            title: "".into(),
            id: "".into(),
        },
        Tag::Image {
            link_type: LinkType::Inline,
            dest_url: "i.png".into(),
            title: "t".into(),
            id: "".into(),
        },
        Tag::MetadataBlock(MetadataBlockKind::PlusesStyle),
    ]);
    prop_oneof![
        4 => words.clone().prop_map(|w| Event::Text(w.into())),
        1 => tag.clone().prop_map(Event::Start),
        1 => tag.prop_map(|t| Event::End(t.to_end())),
        1 => words.clone().prop_map(|w| Event::Code(w.into())),
        1 => words.clone().prop_map(|w| Event::InlineMath(w.into())),
        1 => words.prop_map(|w| Event::DisplayMath(w.into())),
        1 => html.clone().prop_map(|h| Event::Html(h.into())),
        1 => html.prop_map(|h| Event::InlineHtml(h.into())),
        1 => prop::sample::select(vec!["f", "g"]).prop_map(|l| Event::FootnoteReference(l.into())),
        1 => Just(Event::SoftBreak),
        1 => Just(Event::HardBreak),
        1 => Just(Event::Rule),
        1 => any::<bool>().prop_map(Event::TaskListMarker),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: ProptestConfig::default().cases.max(512),
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// Any sequence of events builds a valid document.
    #[test]
    fn random_event_orders_build_valid_documents(
        events in prop::collection::vec(arb_event(), 0..120)
    ) {
        let d = build(events);
        let _ = d.plain_text();
    }

    /// Random Markdown-ish text parses to a valid document, deterministically.
    #[test]
    fn random_markdown_parses(src in "[-*_`$\\\\()\\[\\]<>!#|:\\n a-z0-9日&;^~{}=+.]{0,200}") {
        let a = doc(&src);
        let b = parse(&src, &ParseOptions::default());
        prop_assert_eq!(a, b);
    }
}
