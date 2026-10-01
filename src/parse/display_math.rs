//! Multi-line display math blocks, protected from block parsing.
//!
//! CommonMark parses block structure before it ever looks at inline math,
//! so a display formula written over several lines can be torn apart by
//! its own content: a line holding only `=` turns the lines above it into
//! a setext heading, `- x` starts a list, `> y` a quote. Written as
//!
//! ```text
//! \[
//! a
//! =
//! b
//! \]
//! ```
//!
//! the formula became the heading "\[ a" with "b \]" below it.
//!
//! [`fence`] rewrites every such block, before parsing, into a ```` ```math ````
//! fence (which the builder shows as display math and whose content no
//! block rule can touch). It recognises a block when
//!
//! * a line starts with `\[` (with `tex_delimiters`) or `$$` (with math on),
//!   after the line's container prefix (spaces, tabs and `>` markers);
//! * a later line, before any blank line and with the same prefix, ends
//!   with the matching `\]` or `$$`;
//! * it is not inside a fenced code block and does not look like indented
//!   code.
//!
//! Single-line formulas (`\[ x \]`, `$$x$$`) are left alone: nothing inside
//! them can start a block. Anything else that does not match is left alone
//! too and parsed as before.

use std::borrow::Cow;

/// The most lines a display block may span.
const MAX_LINES: usize = 400;

/// The two delimiter pairs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Delim {
    /// `\[ … \]`.
    Bracket,
    /// `$$ … $$`.
    Dollars,
}

impl Delim {
    fn open(self) -> &'static str {
        match self {
            Delim::Bracket => "\\[",
            Delim::Dollars => "$$",
        }
    }

    /// Where the closing delimiter starts if `content` ends with it.
    fn closes_at(self, content: &str) -> Option<usize> {
        match self {
            Delim::Bracket => {
                let body = content.strip_suffix(']')?;
                // `\]` needs an odd run of backslashes before the `]`.
                let slashes = body.len() - body.trim_end_matches('\\').len();
                (slashes % 2 == 1).then(|| body.len() - 1)
            }
            Delim::Dollars => content.strip_suffix("$$").map(str::len),
        }
    }
}

/// One line of the source, split into its container prefix and content.
struct Line<'a> {
    /// `[start, end)` in the source, without the line break.
    start: usize,
    /// The leading spaces, tabs and `>` markers.
    prefix: &'a str,
    /// The rest, without trailing whitespace.
    content: &'a str,
}

fn lines(src: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for raw in src.split_inclusive('\n') {
        let text = raw.strip_suffix('\n').unwrap_or(raw);
        let text = text.strip_suffix('\r').unwrap_or(text);
        let split = text
            .find(|c: char| !matches!(c, ' ' | '\t' | '>'))
            .unwrap_or(text.len());
        let (prefix, content) = text.split_at(split);
        out.push(Line {
            start,
            prefix,
            content: content.trim_end(),
        });
        start += raw.len();
    }
    out
}

/// Columns of indentation at the end of a prefix (after its last `>`).
fn indent(prefix: &str) -> usize {
    let tail = prefix.rsplit('>').next().unwrap_or(prefix);
    tail.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

/// A code fence opener or closer: its character and length.
fn code_fence(content: &str) -> Option<(char, usize)> {
    let c = content.chars().next().filter(|&c| c == '`' || c == '~')?;
    let n = content.len() - content.trim_start_matches(c).len();
    (n >= 3).then_some((c, n))
}

/// Rewrite multi-line display math blocks into ```` ```math ```` fences.
/// `brackets` enables `\[ … \]`, `dollars` enables `$$ … $$`. Returns the
/// source unchanged (borrowed) when there is nothing to rewrite.
pub(crate) fn fence(src: &str, brackets: bool, dollars: bool) -> Cow<'_, str> {
    if !(brackets && src.contains("\\[") || dollars && src.contains("$$")) {
        return Cow::Borrowed(src);
    }
    let lines = lines(src);
    let mut out = String::new();
    let mut copied = 0;
    let mut in_code: Option<(char, usize)> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        if let Some((c, n)) = in_code {
            if code_fence(line.content).is_some_and(|(c2, n2)| c2 == c && n2 >= n) {
                in_code = None;
            }
            i += 1;
            continue;
        }
        if let Some(f) = code_fence(line.content) {
            in_code = Some(f);
            i += 1;
            continue;
        }
        let delim = [(brackets, Delim::Bracket), (dollars, Delim::Dollars)]
            .into_iter()
            .find(|&(on, d)| on && line.content.starts_with(d.open()))
            .map(|(_, d)| d);
        let looks_like_code =
            indent(line.prefix) >= 4 && (i == 0 || lines[i - 1].content.is_empty());
        let block = match delim {
            Some(d) if !looks_like_code => block_end(&lines, i, d).map(|end| (d, end)),
            _ => None,
        };
        let Some((d, end)) = block else {
            i += 1;
            continue;
        };
        out.push_str(&src[copied..line.start]);
        write_fence(src, &lines[i..=end], d, &mut out);
        copied = lines.get(end + 1).map_or(src.len(), |l| l.start);
        i = end + 1;
    }
    if copied == 0 {
        return Cow::Borrowed(src);
    }
    out.push_str(&src[copied..]);
    Cow::Owned(out)
}

/// The line closing a block opened on line `open`, if any.
fn block_end(lines: &[Line<'_>], open: usize, d: Delim) -> Option<usize> {
    let first = &lines[open];
    let after_open = &first.content[d.open().len()..];
    // A formula closed on its own line needs no protection.
    if d.closes_at(after_open).is_some() {
        return None;
    }
    let last = lines.len().min(open + 1 + MAX_LINES);
    for (j, line) in lines.iter().enumerate().take(last).skip(open + 1) {
        if line.content.is_empty() || line.prefix != first.prefix {
            return None;
        }
        if d.closes_at(line.content).is_some() {
            return Some(j);
        }
    }
    None
}

/// Write `block` (opener to closer line) as a math fence.
fn write_fence(src: &str, block: &[Line<'_>], d: Delim, out: &mut String) {
    let (Some(first), Some(last)) = (block.first(), block.last()) else {
        return;
    };
    let prefix = first.prefix;
    let fence = "`".repeat(longest_backtick_run(block).max(2) + 1);
    let put = |out: &mut String, text: &str| {
        out.push_str(prefix);
        out.push_str(text);
        out.push('\n');
    };
    // The text of a closing line before its delimiter.
    let before_close = |content: &str| {
        let end = d.closes_at(content).unwrap_or(content.len());
        content[..end].trim_end().to_string()
    };
    put(out, &format!("{fence}math"));
    let opening = &first.content[d.open().len()..];
    if block.len() == 1 {
        put(out, &before_close(opening));
    } else {
        if !opening.trim().is_empty() {
            put(out, opening.trim_start());
        }
        for line in &block[1..block.len() - 1] {
            out.push_str(&src[line.start..line.start + line.prefix.len() + line.content.len()]);
            out.push('\n');
        }
        let tail = before_close(last.content);
        if !tail.is_empty() {
            put(out, &tail);
        }
    }
    put(out, &fence);
}

fn longest_backtick_run(block: &[Line<'_>]) -> usize {
    block
        .iter()
        .flat_map(|l| l.content.split(|c| c != '`'))
        .map(str::len)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(src: &str) -> String {
        fence(src, true, true).into_owned()
    }

    #[test]
    fn a_relation_on_its_own_line_is_protected() {
        assert_eq!(
            f("Write it as\n\\[\na\n=\nb.\n\\]\nThen more.\n"),
            "Write it as\n```math\na\n=\nb.\n```\nThen more.\n"
        );
        assert_eq!(f("$$\n- x\n$$\n"), "```math\n- x\n```\n");
    }

    #[test]
    fn content_on_the_delimiter_lines_is_kept() {
        assert_eq!(f("\\[ a =\nb \\]\n"), "```math\na =\nb\n```\n");
        assert_eq!(f("$$a\n= b$$"), "```math\na\n= b\n```\n");
    }

    #[test]
    fn containers_keep_their_prefix() {
        assert_eq!(
            f("> \\[\n> a\n> =\n> \\]\n"),
            "> ```math\n> a\n> =\n> ```\n"
        );
        assert_eq!(
            f("- item\n  \\[\n  x\n  \\]\n"),
            "- item\n  ```math\n  x\n  ```\n"
        );
    }

    #[test]
    fn what_needs_no_protection_is_untouched() {
        for src in [
            "plain text",
            "\\[ x = y \\]\n",
            "$$x$$\n",
            "\\[\nunclosed\n\nparagraph\n\\]\n",
            "```\n\\[\nx\n\\]\n```\n",
            "~~~~\n$$\n=\n$$\n~~~~\n",
            "text\n\n    \\[\n    x\n    \\]\n",
            "> \\[\nx\n\\]\n",
            "a \\\\[\nb\n\\]\n",
            "\\[\nx\n\\\\]\n",
        ] {
            assert!(
                matches!(fence(src, true, true), Cow::Borrowed(_)),
                "{src:?}"
            );
        }
    }

    #[test]
    fn the_switches_are_respected() {
        let src = "\\[\na\n=\n\\]\n$$\nb\n=\n$$\n";
        assert_eq!(
            fence(src, false, true),
            "\\[\na\n=\n\\]\n```math\nb\n=\n```\n"
        );
        assert_eq!(
            fence(src, true, false),
            "```math\na\n=\n```\n$$\nb\n=\n$$\n"
        );
    }

    #[test]
    fn backticks_in_the_formula_lengthen_the_fence() {
        assert_eq!(f("\\[\n```x\n\\]\n"), "````math\n```x\n````\n");
    }

    #[test]
    fn crlf_and_odd_input_never_panic() {
        assert_eq!(f("\\[\r\na\r\n=\r\n\\]\r\n"), "```math\na\n=\n```\n");
        for src in [
            "\\[",
            "$$",
            "\\[\n",
            "$$\n$",
            "\\[\n\\]",
            ">\\[\n>\\]",
            "\u{a0}\\[\nx\n\\]",
        ] {
            let _ = f(src);
        }
    }
}
