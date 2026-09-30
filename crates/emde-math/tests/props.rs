//! Property tests: `inline` and `display` never panic, and their output keeps
//! the documented invariants for any input, width and options.
//!
//! * Every line's spans are non-empty, contiguous and cover its text, and its
//!   `width` is `unicode-width`'s width of the text; breaks are increasing
//!   character boundaries.
//! * Every box has `height` rows and `baseline < height`, fits `avail`
//!   columns and `max_height` rows, and every row is exactly `width` columns
//!   by `unicode-width`.
//!
//! Inputs are arbitrary strings, random soups of TeX tokens (mostly parse
//! errors, some odd but valid formulas), and generated valid formulas that
//! reach every 2D construction.

use emde_math::{
    Bold, Fractions, Letters, MathDisplay, MathLine, MathOptions, ScriptSet, display, inline,
};
use proptest::prelude::*;
use unicode_width::UnicodeWidthStr;

fn measure(s: &str, opts: &MathOptions) -> usize {
    if opts.ambiguous_wide {
        s.width_cjk()
    } else {
        s.width()
    }
}

fn check_line(line: &MathLine, opts: &MathOptions) -> Result<(), TestCaseError> {
    let len = line.text.len();
    let mut prev = 0usize;
    for span in &line.spans {
        let end = span.end as usize;
        prop_assert!(end > prev, "empty or unordered span in {:?}", line);
        prop_assert!(
            line.text.is_char_boundary(end),
            "span end inside a char: {:?}",
            line
        );
        prev = end;
    }
    prop_assert_eq!(prev, len, "spans do not cover the text: {:?}", line);
    let mut prev = 0usize;
    for &b in &line.breaks {
        let b = b as usize;
        prop_assert!(b > prev && b <= len, "bad break {} in {:?}", b, line);
        prop_assert!(line.text.is_char_boundary(b));
        prev = b;
    }
    prop_assert_eq!(
        usize::from(line.width),
        measure(&line.text, opts),
        "width of {:?}",
        line.text
    );
    Ok(())
}

fn check_display(
    result: &MathDisplay,
    opts: &MathOptions,
    avail: u16,
) -> Result<(), TestCaseError> {
    match result {
        MathDisplay::Box(b) => {
            prop_assert!(b.width <= avail, "box wider than {}", avail);
            prop_assert!(b.height <= opts.max_height, "box taller than the limit");
            prop_assert!(b.height >= 1);
            prop_assert!(b.baseline < b.height);
            prop_assert_eq!(b.rows.len(), usize::from(b.height));
            for row in &b.rows {
                prop_assert!(row.ok);
                prop_assert_eq!(row.width, b.width);
                check_line(row, opts)?;
            }
        }
        MathDisplay::Lines(lines) => {
            prop_assert!(!lines.is_empty());
            for line in lines {
                prop_assert!(line.ok);
                check_line(line, opts)?;
            }
        }
        MathDisplay::Raw(line) => {
            prop_assert!(!line.ok);
            check_line(line, opts)?;
        }
    }
    Ok(())
}

fn check(tex: &str, opts: &MathOptions, avail: u16) -> Result<(), TestCaseError> {
    let line = inline(tex, opts);
    check_line(&line, opts)?;
    check_display(&display(tex, opts, avail), opts, avail)
}

fn options() -> impl Strategy<Value = MathOptions> {
    (
        prop_oneof![
            Just(Letters::Italic),
            Just(Letters::UnicodeItalic),
            Just(Letters::Plain)
        ],
        prop_oneof![Just(ScriptSet::Safe), Just(ScriptSet::Full)],
        prop_oneof![Just(Fractions::Vulgar), Just(Fractions::Slash)],
        prop_oneof![Just(Bold::Sgr), Just(Bold::Unicode)],
        any::<bool>(),
        0u16..20,
    )
        .prop_map(
            |(letters, scripts, fractions, bold, ambiguous_wide, max_height)| MathOptions {
                letters,
                scripts,
                fractions,
                bold,
                ambiguous_wide,
                max_height,
            },
        )
}

/// Fragments of TeX, valid and otherwise.
fn token() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "x",
        "y",
        "2",
        "10",
        "+",
        "-",
        "=",
        "<",
        ",",
        ".",
        "'",
        "(",
        ")",
        "[",
        "]",
        "|",
        "{",
        "}",
        "^",
        "_",
        "&",
        r"\\",
        " ",
        "%",
        "~",
        r"\frac",
        r"\sqrt",
        r"\sqrt[3]",
        r"\sqrt[n+1]",
        r"\left(",
        r"\right)",
        r"\left.",
        r"\right|",
        r"\middle|",
        r"\sum",
        r"\int",
        r"\iint",
        r"\lim",
        r"\sin",
        r"\alpha",
        r"\infty",
        r"\hat",
        r"\vec",
        r"\overline",
        r"\underline",
        r"\not",
        r"\mathbb",
        r"\mathbf",
        r"\mathcal",
        r"\boldsymbol",
        r"\text{a b}",
        r"\begin{pmatrix}",
        r"\end{pmatrix}",
        r"\begin{cases}",
        r"\end{cases}",
        r"\begin{aligned}",
        r"\end{aligned}",
        r"\begin{array}{c|c}",
        r"\end{array}",
        r"\hline",
        r"\overbrace",
        r"\underbrace",
        r"\overset",
        r"\underset",
        r"\binom",
        r"\tag{1}",
        r"\label{x}",
        r"\quad",
        r"\,",
        r"\!",
        r"\to",
        r"\le",
        r"\operatorname*{op}",
        r"\Big(",
        r"\Big)",
        r"\color{red}",
        r"\cdot",
        "日",
        "é",
        "\u{301}",
        "\u{1F468}\u{200D}",
        "\u{1F469}",
        "\u{644}",
        "\u{627}",
        r"\substack{a\\b}",
        r"\xrightarrow",
        r"\{",
        r"\}",
        r"\langle",
        r"\rangle",
        r"\lfloor",
        r"\rceil",
        r"\pmod",
        r"\bmod",
        r"\genfrac",
        r"\def",
        r"\hspace{1e9em}",
        r"\kern-3em",
    ])
}

fn soup() -> impl Strategy<Value = String> {
    prop::collection::vec(token(), 0..40).prop_map(|t| t.concat())
}

/// Valid formulas: a generated expression, possibly with a tag or split
/// into rows by a bare `\\`.
fn formula() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => expression(),
        1 => expression().prop_map(|a| format!(r"{a} \tag{{1}}")),
        1 => (expression(), expression()).prop_map(|(a, b)| format!(r"{a} \\ {b}")),
    ]
}

/// Valid expressions built from every construction the renderers know.
fn expression() -> impl Strategy<Value = String> {
    let leaf = prop::sample::select(vec![
        "x",
        "y",
        "2",
        "10",
        r"\alpha",
        r"\infty",
        r"\pi",
        r"\text{ab}",
        "日",
        r"\nabla",
        "-x",
        r"\sin x",
        r"\ldots",
        "f(x)",
    ])
    .prop_map(String::from);
    leaf.prop_recursive(5, 96, 4, |inner| {
        let two = || (inner.clone(), inner.clone());
        prop_oneof![
            two().prop_map(|(a, b)| format!(r"\frac{{{a}}}{{{b}}}")),
            two().prop_map(|(a, b)| format!("{{{a}}}^{{{b}}}")),
            two().prop_map(|(a, b)| format!("{{{a}}}_{{{b}}}")),
            (inner.clone(), inner.clone(), inner.clone())
                .prop_map(|(a, b, c)| format!("{{{a}}}_{{{b}}}^{{{c}}}")),
            inner.clone().prop_map(|a| format!(r"\sqrt{{{a}}}")),
            two().prop_map(|(a, b)| format!(r"\sqrt[{a}]{{{b}}}")),
            inner.clone().prop_map(|a| format!(r"\left({a}\right)")),
            inner.clone().prop_map(|a| format!(r"\left\{{{a}\right\}}")),
            inner
                .clone()
                .prop_map(|a| format!(r"\left\langle {a} \right|")),
            two().prop_map(|(a, b)| format!("{a} + {b}")),
            two().prop_map(|(a, b)| format!("{a} = {b}")),
            two().prop_map(|(a, b)| format!("{a} {b}")),
            (inner.clone(), inner.clone(), inner.clone(), inner.clone()).prop_map(
                |(a, b, c, d)| format!(r"\begin{{pmatrix}} {a} & {b} \\ {c} & {d} \end{{pmatrix}}")
            ),
            two().prop_map(|(a, b)| format!(
                r"\begin{{cases}} {a} & {b} \\ {b} & \text{{else}} \end{{cases}}"
            )),
            two().prop_map(|(a, b)| format!(
                r"\begin{{aligned}} {a} &= {b} \\ &= {a} \end{{aligned}}"
            )),
            two().prop_map(|(a, b)| format!(
                r"\begin{{array}}{{c|c}} {a} & {b} \\ \hline {b} & {a} \end{{array}}"
            )),
            two().prop_map(|(a, b)| format!(r"\sum_{{{a}}}^{{{b}}} {a}")),
            two().prop_map(|(a, b)| format!(r"\int_{{{a}}}^{{{b}}} {b}\,dx")),
            inner
                .clone()
                .prop_map(|a| format!(r"\lim_{{x \to {a}}} {a}")),
            inner.clone().prop_map(|a| format!(r"\hat{{{a}}}")),
            inner.clone().prop_map(|a| format!(r"\overline{{{a}}}")),
            inner
                .clone()
                .prop_map(|a| format!(r"\overrightarrow{{{a}}}")),
            two().prop_map(|(a, b)| format!(r"\overbrace{{{a}}}^{{{b}}}")),
            two().prop_map(|(a, b)| format!(r"\underbrace{{{a}}}_{{{b}}}")),
            two().prop_map(|(a, b)| format!(r"\overset{{{a}}}{{{b}}}")),
            inner.clone().prop_map(|a| format!(r"\xrightarrow{{{a}}}")),
            inner.clone().prop_map(|a| format!(r"\mathbb{{{a}}}")),
            inner.clone().prop_map(|a| format!(r"\mathbf{{{a}}}")),
            inner.clone().prop_map(|a| format!(r"\not{{{a}}}")),
            two().prop_map(|(a, b)| format!(r"\binom{{{a}}}{{{b}}}")),
            inner.clone().prop_map(|a| format!(r"\Big( {a} \Big)")),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        // At least 512 cases; `PROPTEST_CASES` can ask for more.
        cases: ProptestConfig::default().cases.max(512),
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
        )),
        ..ProptestConfig::default()
    })]

    #[test]
    fn arbitrary_strings(tex in any::<String>(), avail in 0u16..200) {
        check(&tex, &MathOptions::default(), avail)?;
    }

    #[test]
    fn tex_token_soup(tex in soup(), opts in options(), avail in 0u16..160) {
        check(&tex, &opts, avail)?;
    }

    #[test]
    fn valid_formulas(tex in formula(), opts in options(), avail in 0u16..160) {
        check(&tex, &opts, avail)?;
    }

    // Without a tag (which widens the box to `avail`) and without a bare
    // `\\`, a valid formula always has a 2D box when size is no object.
    #[test]
    fn valid_formulas_render_in_2d_when_there_is_room(tex in expression()) {
        let opts = MathOptions { max_height: u16::MAX, ..MathOptions::default() };
        match display(&tex, &opts, u16::MAX) {
            MathDisplay::Box(b) => check_display(&MathDisplay::Box(b), &opts, u16::MAX)?,
            other => prop_assert!(false, "{} gave {:?}", tex, other),
        }
    }
}
