//! TeX rewrites applied before parsing.
//!
//! pulldown-latex parses one formula, not a whole amsmath document, and has
//! a few gaps. The pre-pass bridges them on the source text:
//!
//! * `\operatorname*{op}` becomes `\operatorname{op}\limits` (the starred form
//!   is unsupported; `\limits` gives it limits above and below).
//! * `\tag{…}` and `\tag*{…}` are removed and returned (the first one wins);
//!   `\label{…}`, `\nonumber` and `\notag` are dropped.
//! * A bare top-level `\\` wraps the formula in `\begin{gathered}…
//!   \end{gathered}`, or in `aligned` when it also has a top-level `&`, as
//!   LLM-written display math often does.
//! * `\mathscr` becomes `\mathcal`: Unicode has a single script alphabet.
//! * `\sqrt[…]` gets its index braced, `\sqrt[{…}]`: pulldown-latex emits a
//!   multi-token index as loose elements, which breaks the root's arity.
//! * A one-character argument of `\text` (and `\textbf` …), `\operatorname`,
//!   `\colorbox` and `\fcolorbox` is braced, `\text{a}`: pulldown-latex
//!   takes all of the formula after an unbraced one for the argument, then
//!   reads that rest again.
//! * `%` comments are removed (up to the end of the line), so that one at the
//!   end cannot swallow the `\end{gathered}` added after it.
//! * The result is trimmed.
//!
//! Input over [`MAX_INPUT`] bytes is rejected, and so is a run of more than
//! [`MAX_CHAIN`] control words: pulldown-latex recurses once for every
//! command that takes another command as its argument (`\hat\hat\hat x`), and
//! a stack overflow cannot be caught. Nesting deeper than [`MAX_NESTING`]
//! groups, environments and `\left…\right` pairs is rejected before parsing:
//! the adapter would refuse it anyway, and pulldown-latex takes quadratic
//! time to scan it.

use crate::width::sanitize;

/// Formulas longer than this (in bytes) are shown raw.
pub(crate) const MAX_INPUT: usize = 4096;

/// The longest run of control words accepted. Real formulas stay in single
/// digits; pulldown-latex overflows a 2 MiB stack at about 130 nested
/// arguments in unoptimised builds.
pub(crate) const MAX_CHAIN: usize = 64;

/// The deepest nesting of groups, environments and `\left…\right` pairs
/// accepted. Each level takes two of the adapter's
/// [`MAX_DEPTH`](crate::adapter::MAX_DEPTH) levels, so anything deeper could
/// not be built anyway.
pub(crate) const MAX_NESTING: usize = crate::adapter::MAX_DEPTH / 2;

/// Why the pre-pass refused a formula.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rejected {
    /// Longer than [`MAX_INPUT`].
    TooLong,
    /// Nested deeper than the parser can take safely.
    TooDeep,
}

/// A formula ready for the parser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prepared {
    /// The rewritten TeX.
    pub(crate) tex: String,
    /// The formatted tag: `(1)` for `\tag{1}`, `A` for `\tag*{A}`.
    pub(crate) tag: Option<String>,
    /// The whole formula was one `\boxed{…}` (unwrapped in `tex`).
    pub(crate) boxed: bool,
}

/// Rewrite `tex` for the parser.
pub(crate) fn prepare(tex: &str) -> Result<Prepared, Rejected> {
    if tex.len() > MAX_INPUT {
        return Err(Rejected::TooLong);
    }
    if longest_chain(tex) > MAX_CHAIN {
        return Err(Rejected::TooDeep);
    }
    let (tex, boxed) = match whole_box(tex) {
        Some(inner) => (inner, true),
        None => (tex, false),
    };
    let mut rw = Rewriter::default();
    let body = rw.rewrite(tex, true, 0);
    if rw.deepest > MAX_NESTING {
        return Err(Rejected::TooDeep);
    }
    let body = body.trim();
    let tex = match (rw.top_newline, rw.top_alignment) {
        (_, true) => format!("\\begin{{aligned}}{body}\\end{{aligned}}"),
        (true, false) => format!("\\begin{{gathered}}{body}\\end{{gathered}}"),
        (false, false) => body.to_string(),
    };
    Ok(Prepared {
        tex,
        tag: rw.tag,
        boxed,
    })
}

/// The content of a formula that is one `\boxed{…}` and nothing else
/// (so the frame can go around the whole display), else `None`.
fn whole_box(tex: &str) -> Option<&str> {
    let t = tex.trim();
    let rest = t.strip_prefix("\\boxed")?;
    if rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let off = t.len() - rest.len();
    let open = skip_space(t, off);
    if !t[open..].starts_with('{') {
        return None;
    }
    let close = matching(t, open, '{', '}')?;
    (t[close + 1..].trim().is_empty()).then(|| &t[open + 1..close])
}

/// State of one rewrite pass.
#[derive(Default)]
struct Rewriter {
    /// Formatted tag of the first `\tag`.
    tag: Option<String>,
    /// A `\\` outside any group, environment or `\left…\right`.
    top_newline: bool,
    /// A `&` in the same position.
    top_alignment: bool,
    /// The deepest nesting seen (see [`MAX_NESTING`]).
    deepest: usize,
}

/// Nesting while scanning, to tell whether `\\` and `&` are at the top level.
#[derive(Default)]
struct Depth {
    /// The nesting around the piece being scanned (a root index).
    outer: usize,
    braces: usize,
    environments: usize,
    fences: usize,
}

impl Depth {
    /// Outside any group, environment or `\left…\right` of this piece.
    fn top(&self) -> bool {
        self.braces + self.environments + self.fences == 0
    }

    /// The nesting of the formula at this point.
    fn total(&self) -> usize {
        self.outer + self.braces + self.environments + self.fences
    }
}

impl Rewriter {
    /// Rewrite `src`, which sits `outer` levels deep. `top` is false for
    /// nested pieces (a root index), whose `\\` and `&` never count as
    /// top-level.
    fn rewrite(&mut self, src: &str, top: bool, outer: usize) -> String {
        let mut out = String::with_capacity(src.len() + 16);
        let mut depth = Depth {
            outer,
            ..Depth::default()
        };
        let mut pos = 0;
        while let Some(c) = src.get(pos..).and_then(|s| s.chars().next()) {
            match c {
                // A comment runs to the end of the line; the line break stays
                // as whitespace, so the tokens around it stay apart.
                '%' => pos = src[pos..].find('\n').map_or(src.len(), |i| pos + i),
                '{' => {
                    depth.braces += 1;
                    self.deepest = self.deepest.max(depth.total());
                    out.push(c);
                    pos += 1;
                }
                '}' => {
                    depth.braces = depth.braces.saturating_sub(1);
                    out.push(c);
                    pos += 1;
                }
                '&' => {
                    self.top_alignment |= top && depth.top();
                    out.push(c);
                    pos += 1;
                }
                '\\' => pos = self.command(src, pos, top, &mut depth, &mut out),
                _ => {
                    out.push(c);
                    pos += c.len_utf8();
                }
            }
        }
        out
    }

    /// Handle the control sequence at `pos` (a backslash); returns the
    /// position after it and any arguments it consumed.
    fn command(
        &mut self,
        src: &str,
        pos: usize,
        top: bool,
        depth: &mut Depth,
        out: &mut String,
    ) -> usize {
        let (name, mut next) = control_sequence(src, pos);
        match name {
            "operatorname" if peek_star(src, next).is_some() => {
                next = peek_star(src, next).unwrap_or(next);
                let (arg, end) = argument(src, next);
                // `\operatorname*x` has a one-token argument; braced, it cannot
                // run into the command name.
                out.push_str("\\operatorname");
                if arg.starts_with('{') {
                    out.push_str(arg);
                } else {
                    out.push('{');
                    out.push_str(arg);
                    out.push('}');
                }
                out.push_str("\\limits ");
                end
            }
            "tag" => {
                let star = peek_star(src, next);
                let (arg, end) = argument(src, star.unwrap_or(next));
                if self.tag.is_none() {
                    self.tag = format_tag(arg, star.is_some());
                }
                out.push(' ');
                end
            }
            "label" => {
                out.push(' ');
                argument(src, next).1
            }
            "nonumber" | "notag" => {
                out.push(' ');
                next
            }
            // A box inside a formula: its content, unframed (only a whole
            // boxed display gets a frame, see `whole_box`).
            "boxed" => next,
            "mathscr" => {
                out.push_str("\\mathcal");
                next
            }
            "sqrt" => {
                out.push_str("\\sqrt");
                self.root_index(src, next, depth.total(), out)
            }
            // pulldown-latex 0.8 reads a one-character argument of these as
            // everything that follows (its character token keeps the rest of
            // the input) and then reads the rest again: `\text a b` showed
            // as "a bb", and a formula full of them grew quadratically.
            "text" | "textrm" | "textbf" | "textit" | "textsf" | "texttt" | "operatorname" => {
                out.push_str(&src[pos..next]);
                brace_char_argument(src, next, out)
            }
            "colorbox" | "fcolorbox" => {
                let colours = if name == "colorbox" { 1 } else { 2 };
                let Some(end) = groups(src, next, colours) else {
                    out.push_str(&src[pos..next]);
                    return next;
                };
                out.push_str(&src[pos..end]);
                brace_char_argument(src, end, out)
            }
            _ => {
                match name {
                    "begin" => depth.environments += 1,
                    "end" => depth.environments = depth.environments.saturating_sub(1),
                    "left" => depth.fences += 1,
                    "right" => depth.fences = depth.fences.saturating_sub(1),
                    "\\" | "cr" => self.top_newline |= top && depth.top(),
                    _ => {}
                }
                self.deepest = self.deepest.max(depth.total());
                out.push_str(&src[pos..next]);
                next
            }
        }
    }

    /// After `\sqrt`, `outer` levels deep: brace a bracketed index, rewriting
    /// inside it too.
    fn root_index(&mut self, src: &str, pos: usize, outer: usize, out: &mut String) -> usize {
        let start = skip_space(src, pos);
        if !src[start..].starts_with('[') {
            return pos;
        }
        let Some(close) = matching(src, start, '[', ']') else {
            return pos;
        };
        let index = self.rewrite(&src[start + 1..close], false, outer + 1);
        let trimmed = index.trim();
        let braced =
            trimmed.starts_with('{') && matching(trimmed, 0, '{', '}') == Some(trimmed.len() - 1);
        if braced {
            out.push('[');
            out.push_str(trimmed);
            out.push(']');
        } else {
            out.push_str("[{");
            out.push_str(trimmed);
            out.push_str("}]");
        }
        close + 1
    }
}

/// The control sequence at `pos` (a backslash): its name (letters, or one
/// other character) and the position after it.
fn control_sequence(src: &str, pos: usize) -> (&str, usize) {
    let start = pos + 1;
    let rest = src.get(start..).unwrap_or("");
    let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    let len = if letters > 0 {
        letters
    } else {
        rest.chars().next().map_or(0, char::len_utf8)
    };
    (&src[start..start + len], start + len)
}

/// The position after a `*` that follows `pos` (after optional spaces).
fn peek_star(src: &str, pos: usize) -> Option<usize> {
    let at = skip_space(src, pos);
    src[at..].starts_with('*').then_some(at + 1)
}

fn skip_space(src: &str, pos: usize) -> usize {
    let rest = &src[pos..];
    pos + (rest.len() - rest.trim_start().len())
}

/// A macro argument at `pos`: a braced group (returned with its braces), a
/// control sequence or one character. Returns it and the position after it.
fn argument(src: &str, pos: usize) -> (&str, usize) {
    let start = skip_space(src, pos);
    let rest = &src[start..];
    let end = match rest.chars().next() {
        None => start,
        Some('{') => matching(src, start, '{', '}').map_or(src.len(), |close| close + 1),
        Some('\\') => control_sequence(src, start).1,
        Some(c) => start + c.len_utf8(),
    };
    (&src[start..end], end)
}

/// The position after `n` braced arguments at `pos`, or `None` when one of
/// them is no group.
fn groups(src: &str, pos: usize, n: usize) -> Option<usize> {
    (0..n).try_fold(pos, |at, _| {
        let (arg, end) = argument(src, at);
        arg.starts_with('{').then_some(end)
    })
}

/// Write the argument at `pos` braced when it is one character (after
/// spaces and comments, as TeX reads it), and return the position after it.
/// A group, a control sequence or nothing is left to the scan: `pos`.
fn brace_char_argument(src: &str, pos: usize, out: &mut String) -> usize {
    let mut start = skip_space(src, pos);
    while src[start..].starts_with('%') {
        let eol = src[start..].find('\n').map_or(src.len(), |i| start + i);
        start = skip_space(src, eol);
    }
    match src[start..].chars().next() {
        Some(c) if !matches!(c, '{' | '}' | '\\') => {
            out.push('{');
            out.push(c);
            out.push('}');
            start + c.len_utf8()
        }
        _ => pos,
    }
}

/// The position of the delimiter closing the one at `open` (which must be
/// `left`), skipping escaped characters and nested pairs.
fn matching(src: &str, open: usize, left: char, right: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut chars = src.get(open..)?.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if c == left {
            depth += 1;
        } else if c == right {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(open + i);
            }
        }
    }
    None
}

/// `(1)` for `\tag{1}`, `A` for `\tag*{A}`; `None` when empty. The tag is
/// shown as written (without `$` signs), with whitespace collapsed and
/// control characters replaced.
fn format_tag(arg: &str, star: bool) -> Option<String> {
    let inner = arg
        .strip_prefix('{')
        .and_then(|a| a.strip_suffix('}'))
        .unwrap_or(arg);
    let text = sanitize(&inner.replace('$', ""));
    let text = text.trim();
    if text.is_empty() {
        None
    } else if star {
        Some(text.to_string())
    } else {
        Some(format!("({text})"))
    }
}

/// The longest run of consecutive control words, an upper bound on how deep
/// pulldown-latex recurses. Whitespace and comments do not interrupt a run;
/// any other character or control symbol does. A group `{…}` or bracket
/// `[…]` is skipped by the run around it, and its content starts a run of its
/// own (pulldown-latex parses group content lazily). `\genfrac`'s four
/// leading arguments (delimiters, bar, style) are skipped because its
/// fraction parts can continue the chain.
fn longest_chain(src: &str) -> usize {
    let mut longest = 0;
    let mut run = 0;
    // The runs around the open groups and brackets.
    let mut outer: Vec<(char, usize)> = Vec::new();
    let mut pos = 0;
    while let Some(c) = src.get(pos..).and_then(|s| s.chars().next()) {
        match c {
            '\\' => {
                let (name, mut next) = control_sequence(src, pos);
                if name.bytes().next().is_some_and(|b| b.is_ascii_alphabetic()) {
                    run += 1;
                    longest = longest.max(run);
                    if name == "genfrac" {
                        for _ in 0..4 {
                            next = argument(src, next).1;
                        }
                    }
                } else {
                    run = 0;
                }
                pos = next;
                continue;
            }
            '{' | '[' => {
                outer.push((c, run));
                run = 0;
            }
            // Unmatched brackets inside the group are dropped with it.
            '}' => {
                run = 0;
                while let Some((open, before)) = outer.pop() {
                    if open == '{' {
                        run = before;
                        break;
                    }
                }
            }
            ']' => match outer.last() {
                Some(&('[', before)) => {
                    outer.pop();
                    run = before;
                }
                _ => run = 0,
            },
            '%' => {
                pos = src[pos..].find('\n').map_or(src.len(), |i| pos + i);
                continue;
            }
            c if c.is_whitespace() => {}
            _ => run = 0,
        }
        pos += c.len_utf8();
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tex(src: &str) -> String {
        prepare(src).map(|p| p.tex).unwrap_or_default()
    }

    fn tag(src: &str) -> Option<String> {
        prepare(src).ok().and_then(|p| p.tag)
    }

    #[test]
    fn operatorname_star_gets_limits() {
        assert_eq!(
            tex(r"\operatorname*{argmax}_x f"),
            r"\operatorname{argmax}\limits _x f"
        );
        assert_eq!(tex(r"\operatorname{sgn} x"), r"\operatorname{sgn} x");
    }

    #[test]
    fn tags_are_extracted() {
        assert_eq!(tex(r"E = mc^2 \tag{1}"), "E = mc^2");
        assert_eq!(tag(r"E = mc^2 \tag{1}"), Some("(1)".into()));
        assert_eq!(tag(r"x \tag*{A.1}"), Some("A.1".into()));
        assert_eq!(tag(r"x \tag{1} \tag{2}"), Some("(1)".into()));
        assert_eq!(tag(r"x \tag{ $\ast$ }"), Some(r"(\ast)".into()));
        assert_eq!(tag(r"x \tag{}"), None);
        assert_eq!(tag(r"x \tag"), None);
        // Control characters cannot reach the output.
        assert_eq!(
            tag("x \\tag{a\u{1b}[31m\tb}"),
            Some("(a\u{FFFD}[31m b)".into())
        );
    }

    #[test]
    fn labels_and_numbering_are_dropped() {
        assert_eq!(tex(r"a \label{eq:a} = b \nonumber"), "a   = b");
        assert_eq!(tex(r"a\notag"), "a");
        // A removed command still separates the tokens around it.
        assert_eq!(tex(r"\alpha\label{x}b"), r"\alpha b");
    }

    #[test]
    fn bare_newlines_wrap_in_gathered_or_aligned() {
        assert_eq!(tex(r"a \\ b"), r"\begin{gathered}a \\ b\end{gathered}");
        assert_eq!(
            tex(r"a &= b \\ &= c"),
            r"\begin{aligned}a &= b \\ &= c\end{aligned}"
        );
        // Not at the top level: left alone.
        let env = r"\begin{pmatrix} a \\ b \end{pmatrix}";
        assert_eq!(tex(env), env);
        assert_eq!(tex(r"\text{a \\ b}"), r"\text{a \\ b}");
        assert_eq!(tex(r"\{ a \}"), r"\{ a \}");
    }

    #[test]
    fn mathscr_is_mathcal() {
        assert_eq!(tex(r"\mathscr{L}"), r"\mathcal{L}");
    }

    #[test]
    fn root_indices_are_braced() {
        assert_eq!(tex(r"\sqrt[n+1]{x}"), r"\sqrt[{n+1}]{x}");
        assert_eq!(tex(r"\sqrt[{3}]{x}"), r"\sqrt[{3}]{x}");
        assert_eq!(tex(r"\sqrt{x}"), r"\sqrt{x}");
        assert_eq!(tex(r"\sqrt [\sqrt[3]{2}]{x}"), r"\sqrt[{\sqrt[{3}]{2}}]{x}");
        // Unbalanced: left for the parser to reject.
        assert_eq!(tex(r"\sqrt[3"), r"\sqrt[3");
    }

    #[test]
    fn comments_are_removed() {
        // Nothing in a comment counts: no newline, alignment or tag.
        assert_eq!(tex("a % \\\\ & \\tag{9}\n+ b"), "a \n+ b");
        assert_eq!(tag("a % \\tag{9}\n"), None);
        // The line break stays, so the tokens around it stay apart.
        assert_eq!(tex("\\alpha%\nb"), "\\alpha\nb");
        assert_eq!(tex(r"50\% x"), r"50\% x");
        // A comment at the end cannot swallow the added `\end{gathered}`.
        assert_eq!(
            tex(r"a \\ b % note"),
            r"\begin{gathered}a \\ b\end{gathered}"
        );
    }

    /// Found by the `math` fuzz target: pulldown-latex took everything after
    /// an unbraced one-character argument of `\text` for the argument and
    /// then read it again, so 2 KiB of TeX with a hundred `\text`s came out
    /// 94,000 columns wide.
    #[test]
    fn one_character_arguments_are_braced() {
        assert_eq!(tex(r"\text a b"), r"\text{a} b");
        assert_eq!(
            tex(r"\textbf x+\operatorname f(x)"),
            r"\textbf{x}+\operatorname{f}(x)"
        );
        assert_eq!(tex("\\textit % note\n  é."), r"\textit{é}.");
        assert_eq!(
            tex(r"\colorbox{red}x \fcolorbox{red}{blue} y"),
            r"\colorbox{red}{x} \fcolorbox{red}{blue}{y}"
        );
        // Groups, commands, other commands and missing arguments stay.
        for src in [
            r"\text{a} b",
            r"\text \alpha",
            r"\textcolor{red}x",
            r"\colorbox x",
            r"\fcolorbox{red}x",
            r"{\text}",
            r"\text",
            "\\text %",
        ] {
            assert_eq!(tex(src), src.trim_end_matches(" %"), "{src}");
        }
    }

    #[test]
    fn text_arguments_are_read_once() {
        let opts = crate::MathOptions::default();
        assert_eq!(crate::inline(r"\text a b", &opts).text, "ab");
        assert_eq!(crate::inline(r"\operatorname f x", &opts).text, "f x");
        let tex = r"\text > ".repeat(MAX_INPUT / 8);
        let line = crate::inline(&tex, &opts);
        assert!(line.ok);
        assert_eq!(line.text, ">".repeat(MAX_INPUT / 8));
    }

    #[test]
    fn operatorname_star_braces_a_bare_argument() {
        assert_eq!(tex(r"\operatorname*a_x"), r"\operatorname{a}\limits _x");
        assert_eq!(
            tex(r"\operatorname* {op}_x"),
            r"\operatorname{op}\limits _x"
        );
    }

    #[test]
    fn deep_nesting_is_rejected_before_parsing() {
        let nested = |n: usize| format!("{}x{}", "{".repeat(n), "}".repeat(n));
        assert!(prepare(&nested(MAX_NESTING)).is_ok());
        assert_eq!(prepare(&nested(MAX_NESTING + 1)), Err(Rejected::TooDeep));
        let fences = |n: usize| format!("{}x{}", r"\left(".repeat(n), r"\right)".repeat(n));
        assert_eq!(prepare(&fences(MAX_NESTING + 1)), Err(Rejected::TooDeep));
        let envs = |n: usize| {
            format!(
                "{}x{}",
                r"\begin{matrix}".repeat(n),
                r"\end{matrix}".repeat(n)
            )
        };
        assert_eq!(prepare(&envs(MAX_NESTING + 1)), Err(Rejected::TooDeep));
        // Root indices count with what surrounds them.
        let roots = |n: usize| format!("{}x{}", r"\sqrt[".repeat(n), "]{y}".repeat(n));
        assert_eq!(prepare(&roots(MAX_NESTING + 1)), Err(Rejected::TooDeep));
        // Sequential groups are not nested.
        assert!(prepare(&"{x}".repeat(MAX_NESTING * 4)).is_ok());
    }

    #[test]
    fn trims_and_rejects_oversized_input() {
        assert_eq!(tex("  x \n"), "x");
        assert_eq!(prepare(&"x".repeat(MAX_INPUT)).map(|_| ()), Ok(()));
        assert_eq!(prepare(&"x".repeat(MAX_INPUT + 1)), Err(Rejected::TooLong));
    }

    #[test]
    fn deep_argument_chains_are_rejected() {
        assert_eq!(longest_chain(r"\hat\hat\hat x"), 3);
        assert_eq!(longest_chain(r"\frac{\alpha}{\beta} + \sqrt[3]{x}"), 1);
        assert_eq!(longest_chain(&format!("{}x", r"\sqrt[3]".repeat(5))), 5);
        assert_eq!(longest_chain(&format!("{}x", r"\overset{[}".repeat(5))), 5);
        assert_eq!(longest_chain(r"[0, 1) \alpha ] \beta"), 1);
        assert_eq!(longest_chain(r"\textcolor{red}\hat x"), 2);
        assert_eq!(longest_chain(r"\alpha + \beta"), 1);
        assert_eq!(longest_chain(r"\,\,\,"), 0);
        let genfracs = r"\genfrac(]{0pt}{2}".repeat(3);
        assert_eq!(longest_chain(&genfracs), 3);
        let deep = format!("{}x", r"\hat".repeat(MAX_CHAIN + 1));
        assert_eq!(prepare(&deep), Err(Rejected::TooDeep));
        let ok = format!("{}x", r"\hat".repeat(MAX_CHAIN));
        assert!(prepare(&ok).is_ok());
    }

    #[test]
    fn total_on_odd_input() {
        for src in [
            "\\",
            "{",
            "}",
            "\\sqrt[",
            "\\tag{",
            "\\operatorname*",
            "é\\",
            "%",
        ] {
            let _ = prepare(src);
        }
        assert_eq!(tex("\\"), "\\");
    }
}
