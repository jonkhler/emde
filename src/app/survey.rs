//! What documents contain that needs work beyond text: a prescan of the
//! raw text before parsing ([`Hints`]) and a survey of the parsed document
//! ([`Survey`]).

use std::collections::HashSet;

use crate::ir::{Block, CodeBlock, Document, ImageId};

/// What a document might contain, from a scan of its text that takes
/// microseconds: it never misses anything, and sometimes sees what is not
/// there (the parser has the last word). It decides what starts before
/// parsing: loading the syntax set, and whether the probe asks about
/// graphics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Hints {
    /// A fence (```` ``` ```` or `~~~`): code in some language, perhaps.
    pub(super) code: bool,
    /// `![` or an `<img` tag.
    pub(super) images: bool,
    /// `$`, `\(` or `\[`.
    pub(super) math: bool,
}

impl Hints {
    /// Scan one document's text.
    pub(super) fn of(text: &str) -> Hints {
        let b = text.as_bytes();
        let has = |needle: &[u8]| memchr::memmem::find(b, needle).is_some();
        let img_tag = || {
            memchr::memchr_iter(b'<', b).any(|i| {
                b.get(i + 1..i + 4)
                    .is_some_and(|t| t.eq_ignore_ascii_case(b"img"))
            })
        };
        Hints {
            code: has(b"```") || has(b"~~~"),
            images: has(b"![") || img_tag(),
            math: memchr::memchr(b'$', b).is_some() || has(b"\\(") || has(b"\\["),
        }
    }

    /// Whatever either may contain.
    pub(super) fn union(self, other: Hints) -> Hints {
        Hints {
            code: self.code || other.code,
            images: self.images || other.images,
            math: self.math || other.math,
        }
    }
}

/// Call `f` for every block of `doc` in reading order: nested blocks
/// (quotes, list items, details, aligned sections, definitions) and
/// footnotes included.
pub(super) fn each_block<'d>(doc: &'d Document, f: &mut dyn FnMut(&'d Block)) {
    fn walk<'d>(blocks: &'d [Block], f: &mut dyn FnMut(&'d Block)) {
        for block in blocks {
            f(block);
            match block {
                Block::Quote { body, .. }
                | Block::Details { body, .. }
                | Block::Align { body, .. } => {
                    walk(body, f);
                }
                Block::List(list) => {
                    for item in &list.items {
                        walk(&item.body, f);
                    }
                }
                Block::DefList(items) => {
                    for def in items.iter().flat_map(|item| &item.defs) {
                        walk(def, f);
                    }
                }
                _ => {}
            }
        }
    }
    walk(&doc.blocks, f);
    for note in &doc.footnotes {
        walk(&note.body, f);
    }
}

/// The code blocks of `doc` that name a language, in reading order.
pub(super) fn code_blocks(doc: &Document) -> Vec<&CodeBlock> {
    let mut out = Vec::new();
    each_block(doc, &mut |block| {
        if let Block::Code(cb) = block
            && cb.lang.is_some()
        {
            out.push(cb);
        }
    });
    out
}

/// What a parsed document uses beyond text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Survey {
    /// Fence languages of its code blocks, in order of first use.
    pub(super) langs: Vec<Box<str>>,
    /// The images of its figures, sorted.
    pub(super) figures: Vec<ImageId>,
}

impl Survey {
    pub(super) fn of(doc: &Document) -> Survey {
        let mut survey = Survey::default();
        let mut seen = HashSet::new();
        each_block(doc, &mut |block| match block {
            Block::Code(cb) => {
                if let Some(lang) = cb.lang.as_deref()
                    && seen.insert(lang)
                {
                    survey.langs.push(lang.into());
                }
            }
            Block::Figure(f) => survey.figures.push(f.image),
            _ => {}
        });
        survey.figures.sort_unstable();
        survey.figures.dedup();
        survey
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{ParseOptions, parse};

    #[test]
    fn hints_find_what_may_be_there() {
        let h = Hints::of("# Title\n\n```rust\nfn x() {}\n```\n");
        assert_eq!(
            h,
            Hints {
                code: true,
                images: false,
                math: false
            }
        );
        assert!(Hints::of("~~~\nx\n~~~").code);
        assert!(Hints::of("see ![alt](a.png)").images);
        assert!(Hints::of("<IMG src=a.png>").images);
        assert!(Hints::of("<img src=a.png>").images);
        assert!(!Hints::of("<i>mg</i> and < img").images);
        assert!(Hints::of("costs $5").math);
        assert!(Hints::of("\\(x\\)").math);
        assert!(Hints::of("\\[x\\]").math);
        assert_eq!(Hints::of("plain prose, nothing else."), Hints::default());
        assert_eq!(Hints::of(""), Hints::default());
        // A tag cut off at the end of the text.
        assert!(!Hints::of("text <im").images);
        let both = Hints::of("```").union(Hints::of("![x](y)"));
        assert!(both.code && both.images && !both.math);
    }

    #[test]
    fn hints_never_miss_what_the_parser_finds() {
        for md in [
            "![a](b.png)",
            "[![badge](b.svg)](https://ci)",
            "<p align=center><img src=\"logo.png\" width=200></p>",
            "<picture><source srcset=d.png media=\"(prefers-color-scheme: dark)\"><img src=l.png></picture>",
        ] {
            let doc = parse(md, &ParseOptions::default());
            if !doc.images.is_empty() {
                assert!(Hints::of(md).images, "{md}");
            }
        }
    }

    const NESTED: &str = "\
```rust
fn a() {}
```

> ```python
> x = 1
> ```
>
> ![quoted](q.png)

- item

  ```rust
  fn b() {}
  ```

  ![listed](l.png)

<details><summary>More</summary>

```bash
echo hi
```

</details>

![top](t.png)

    indented code has no language

Text[^n].

[^n]: ![noted](n.png)
";

    #[test]
    fn surveys_look_inside_every_container() {
        let doc = parse(NESTED, &ParseOptions::default());
        let survey = Survey::of(&doc);
        let langs: Vec<&str> = survey.langs.iter().map(AsRef::as_ref).collect();
        assert_eq!(langs, ["rust", "python", "bash"]);
        let figures: Vec<&str> = survey
            .figures
            .iter()
            .filter_map(|&id| doc.image(id))
            .map(|img| img.alt.as_ref())
            .collect();
        assert_eq!(figures, ["quoted", "listed", "top", "noted"]);
        let code: Vec<&str> = code_blocks(&doc)
            .iter()
            .map(|cb| cb.code.as_str())
            .collect();
        assert_eq!(code, ["fn a() {}", "x = 1", "fn b() {}", "echo hi"]);
    }

    #[test]
    fn empty_documents_need_nothing() {
        let doc = parse("just text", &ParseOptions::default());
        assert_eq!(Survey::of(&doc), Survey::default());
        assert!(code_blocks(&doc).is_empty());
    }
}
