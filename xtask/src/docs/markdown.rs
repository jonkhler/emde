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

/// Prose for a Markdown paragraph or table cell: characters that Markdown
/// (or GitHub's math and tables) would interpret are escaped, except inside
/// `` `code` `` spans, which are kept as they are.
pub(crate) fn text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for (i, part) in s.split('`').enumerate() {
        if i > 0 {
            out.push('`');
        }
        if i % 2 == 1 {
            // Inside a code span only a pipe needs escaping (in a table).
            out.push_str(&part.replace('|', "\\|"));
            continue;
        }
        for c in part.chars() {
            if matches!(
                c,
                '\\' | '*' | '_' | '$' | '|' | '<' | '>' | '[' | ']' | '~' | '&' | '#'
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
        assert_eq!(text("`a_b|c` and a_b"), r"`a_b\|c` and a\_b");
        assert_eq!(code("a|b"), r"`a\|b`");
        assert_eq!(code("x`y"), "`` x`y ``");
    }

    #[test]
    fn tables() {
        let t = table(&["A", "B"], &[vec!["1".into(), "2".into()]]);
        assert_eq!(t, "| A | B |\n|---|---|\n| 1 | 2 |\n");
    }
}
