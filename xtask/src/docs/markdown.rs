//! Markdown building blocks for the generated documentation: templates,
//! generated blocks in hand-written files, escaping and tables.

/// Fill a template: every line that is exactly `{{name}}` (surrounding
/// spaces allowed) becomes the generated part of that name, which ends with
/// one newline. An unknown name, or `{{` anywhere else, is an error.
pub(crate) fn fill_template(template: &str, parts: &[(&str, String)]) -> Result<String, String> {
    let mut out = String::with_capacity(template.len() * 4);
    for (n, line) in template.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(name) = trimmed
            .strip_prefix("{{")
            .and_then(|rest| rest.strip_suffix("}}"))
        {
            let (_, text) = parts
                .iter()
                .find(|(part, _)| *part == name)
                .ok_or_else(|| format!("line {}: unknown placeholder `{trimmed}`", n + 1))?;
            out.push_str(text.trim_end_matches('\n'));
            out.push('\n');
        } else if line.contains("{{") {
            return Err(format!(
                "line {}: a placeholder must be a line of its own",
                n + 1
            ));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    Ok(out)
}

/// Replace the body of the generated block `name` of a hand-written file:
/// the lines between `<!-- BEGIN GENERATED name … -->` and
/// `<!-- END GENERATED name -->` become `content`.
pub(crate) fn replace_generated(text: &str, name: &str, content: &str) -> Result<String, String> {
    let begin = format!("<!-- BEGIN GENERATED {name} ");
    let end = format!("<!-- END GENERATED {name} -->");
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let find = |pred: &dyn Fn(&str) -> bool| -> Vec<usize> {
        lines
            .iter()
            .enumerate()
            .filter(|(_, l)| pred(l.trim_end()))
            .map(|(i, _)| i)
            .collect()
    };
    let starts = find(&|l| l.starts_with(&begin) && l.ends_with("-->"));
    let ends = find(&|l| l == end);
    let (&[start], &[stop]) = (starts.as_slice(), ends.as_slice()) else {
        return Err(format!(
            "expected one `{begin}… -->` line and one `{end}` line"
        ));
    };
    if stop < start {
        return Err(format!("`{end}` comes before its BEGIN line"));
    }
    let mut out = String::with_capacity(text.len() + content.len());
    out.extend(lines[..=start].iter().copied());
    out.push('\n');
    out.push_str(content.trim_matches('\n'));
    out.push_str("\n\n");
    out.extend(lines[stop..].iter().copied());
    Ok(out)
}

/// Prose for a Markdown table cell: characters that Markdown (or GitHub's
/// math and tables) would interpret are escaped, except inside `` `code` ``
/// spans, where only a pipe is (a table takes `\|` as part of the cell
/// everywhere).
///
/// `[` is not escaped: emde, like other readers of LLM-written Markdown,
/// takes `\[` and `\(` after an odd number of backslashes for TeX math, so
/// the escaped bracket in `\\\[` (a literal `\[`) would turn the text up to
/// the next `\]` into a formula. An escaped `]` is enough to keep brackets
/// from making links, images or footnote references.
pub(crate) fn text(s: &str) -> String {
    escape(s, true)
}

/// Prose for a Markdown paragraph: as [`text`], but code spans are kept
/// exactly as they are (outside a table, `\|` in one shows the backslash).
pub(crate) fn paragraph(s: &str) -> String {
    escape(s, false)
}

/// [`text`], escaping pipes in code spans or not.
fn escape(s: &str, in_table: bool) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for (i, part) in s.split('`').enumerate() {
        if i > 0 {
            out.push('`');
        }
        if i % 2 == 1 {
            if in_table {
                out.push_str(&part.replace('|', "\\|"));
            } else {
                out.push_str(part);
            }
            continue;
        }
        for c in part.chars() {
            if matches!(
                c,
                '\\' | '*' | '_' | '$' | '|' | '<' | '>' | ']' | '~' | '&' | '#'
            ) {
                out.push('\\');
            }
            out.push(c);
        }
    }
    out
}

/// `s` as a code span (safe in a table cell).
pub(crate) fn code(s: &str) -> String {
    let fence = if s.contains('`') { "`` " } else { "`" };
    let close = if s.contains('`') { " ``" } else { "`" };
    format!("{fence}{}{close}", s.replace('|', "\\|"))
}

/// A table with a header row; cells are inserted as they are.
pub(crate) fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut out = format!("| {} |\n|", header.join(" | "));
    for _ in header {
        out.push_str("---|");
    }
    out.push('\n');
    for row in rows {
        out.push_str("| ");
        out.push_str(&row.join(" | "));
        out.push_str(" |\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates() {
        let parts = [("a", "A1\nA2\n".to_owned()), ("b", "B".to_owned())];
        assert_eq!(
            fill_template("x\n{{a}}\n  {{b}}  \ny", &parts).unwrap(),
            "x\nA1\nA2\nB\ny\n"
        );
        assert!(
            fill_template("{{c}}", &parts)
                .unwrap_err()
                .contains("{{c}}")
        );
        assert!(fill_template("see {{a}} here", &parts).is_err());
    }

    #[test]
    fn generated_blocks() {
        let text = "intro\n<!-- BEGIN GENERATED keys (by xtask) -->\nold\n\
                    <!-- END GENERATED keys -->\nrest\n";
        let new = replace_generated(text, "keys", "| a |\n").unwrap();
        assert_eq!(
            new,
            "intro\n<!-- BEGIN GENERATED keys (by xtask) -->\n\n| a |\n\n\
             <!-- END GENERATED keys -->\nrest\n"
        );
        // Idempotent.
        assert_eq!(replace_generated(&new, "keys", "| a |\n").unwrap(), new);
        assert!(replace_generated("no markers", "keys", "x").is_err());
        let backwards = "<!-- END GENERATED keys -->\n<!-- BEGIN GENERATED keys x -->\n";
        assert!(replace_generated(backwards, "keys", "x").is_err());
        let twice = format!("{text}{text}");
        assert!(replace_generated(&twice, "keys", "x").is_err());
    }

    #[test]
    fn escaping() {
        assert_eq!(
            text(r"$...$ and \( x \), a|b, *not* <em>"),
            r"\$...\$ and \\( x \\), a\|b, \*not\* \<em\>"
        );
        assert_eq!(text(r"\[ x \] and [1]"), r"\\[ x \\\] and [1\]");
        assert_eq!(text("`a_b|c` and a_b"), r"`a_b\|c` and a\_b");
        assert_eq!(paragraph("`a_b|c` and a|b"), r"`a_b|c` and a\|b");
        assert_eq!(code("a|b"), r"`a\|b`");
        assert_eq!(code("x`y"), "`` x`y ``");
    }

    /// Prose that Markdown, GitHub or emde could take for markup: TeX
    /// delimiters, math, links, footnotes, HTML, entities, emphasis.
    const TRICKY: &[&str] = &[
        r"Recognise \( ... \) and \[ ... \] as math.",
        r"\mathbf: sgr (bold text) or unicode (𝐱).",
        "$...$ and $$...$$ math, $5 and $10.",
        "Numbered link references ([1]) where hyperlinks are off.",
        "When your [style.h1] sets no background; [a_b] and [x^2 + 1].",
        "[text](https://example.org) and ![alt](x.png), [^1] and [ref].",
        "<b>bold</b> &amp; ~~struck~~ *em* _em_ #hash a|b \\ end",
        "`code with | and \\(x\\)` and `[1]`",
    ];

    /// How emde shows `markdown`, as plain text without trailing space.
    fn shown(markdown: &str) -> String {
        use emde::highlight::PlainHighlighter;
        use emde::layout::{NoImages, layout};
        use emde::options::{RenderOptions, When};
        use emde::parse::{ParseOptions, parse_source};
        use emde::render::plain_text;
        use emde::source::{Origin, Source};
        use emde::term::Caps;
        use emde::theme::Theme;

        let opts = RenderOptions {
            max_width: 0,
            link_refs: When::Never,
            ..RenderOptions::default()
        };
        let source = Source::from_bytes(markdown.as_bytes().to_vec(), Origin::Memory);
        let doc = parse_source(&source, &ParseOptions::from(&opts));
        let caps = Caps::plain();
        let l = layout(
            &doc,
            400,
            &Theme::test(),
            &caps,
            &opts,
            &PlainHighlighter,
            &NoImages,
        );
        let text = plain_text(&doc, &l);
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        lines.join("\n").trim().to_owned()
    }

    /// Escaped prose reads in emde as it was written, in a paragraph and in
    /// a table cell (where emde draws the borders around it).
    #[test]
    fn escaped_text_reads_as_written_in_emde() {
        for &s in TRICKY {
            assert_eq!(shown(&paragraph(s)), s, "{:?}", paragraph(s));
            let cell = shown(&table(&["x"], &[vec![text(s)]]));
            assert!(cell.contains(&format!(" {s} ")), "{s:?}:\n{cell}");
        }
    }

    /// The same for every description the settings reference shows.
    #[test]
    fn settings_read_as_written_in_emde() {
        let defaults = include_str!("../../../assets/default.toml");
        for section in super::super::settings::parse(defaults).unwrap() {
            for setting in &section.keys {
                let row = shown(&table(&["x"], &[vec![text(&setting.doc)]]));
                let one_line: String = row.split_whitespace().collect::<Vec<_>>().join(" ");
                let doc = setting.doc.split_whitespace().collect::<Vec<_>>().join(" ");
                assert!(
                    one_line.contains(&doc),
                    "{}.{}: {doc:?} shows as\n{row}",
                    section.name,
                    setting.key
                );
            }
        }
    }

    #[test]
    fn tables() {
        let t = table(&["A", "B"], &[vec!["1".into(), "2".into()]]);
        assert_eq!(t, "| A | B |\n|---|---|\n| 1 | 2 |\n");
    }
}
