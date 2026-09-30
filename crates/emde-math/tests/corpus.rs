//! The math corpus: about 150 formulas, each rendered inline and as display
//! math, snapshotted by topic. The renderings approved in the plan are also
//! asserted exactly.
//!
//! Snapshot format, per formula:
//!
//! ```text
//! ── \frac{a+b}{c}          the TeX
//! inline  (a+b)/c           inline(); `[raw]` marks the error fallback
//!   │a+b│                   display(): one line per row between │ … │,
//! ▸ │───│                   which show the padding; ▸ marks the baseline
//!   │ c │
//! ```
//!
//! Display falls back to `lines` (the linear form, wrapped) or `raw`.

use emde_math::{MathDisplay, MathLine, MathOptions, MathRole, display, inline};

/// The display width used for the corpus.
const WIDTH: u16 = 80;

fn render_one(tex: &str, opts: &MathOptions, avail: u16) -> String {
    let line = inline(tex, opts);
    let mut out = format!("── {tex}\ninline  {}", line.text);
    if !line.ok {
        out.push_str("   [raw]");
    }
    out.push('\n');
    match display(tex, opts, avail) {
        MathDisplay::Box(b) => {
            for (i, row) in b.rows.iter().enumerate() {
                let mark = if i == usize::from(b.baseline) {
                    '▸'
                } else {
                    ' '
                };
                out.push_str(&format!("{mark} │{}│\n", row.text));
            }
        }
        MathDisplay::Lines(lines) => {
            out.push_str("lines\n");
            for l in &lines {
                out.push_str(&format!("  │{}│\n", l.text));
            }
        }
        MathDisplay::Raw(l) => out.push_str(&format!("raw     {}\n", l.text)),
    }
    out
}

fn render_all(formulas: &[&str], opts: &MathOptions, avail: u16) -> String {
    formulas
        .iter()
        .map(|tex| render_one(tex, opts, avail))
        .collect::<Vec<_>>()
        .join("\n")
}

macro_rules! corpus {
    ($name:ident, [$($tex:expr),* $(,)?]) => {
        #[test]
        fn $name() {
            let formulas: &[&str] = &[$($tex),*];
            insta::assert_snapshot!(
                stringify!($name),
                render_all(formulas, &MathOptions::default(), WIDTH)
            );
        }
    };
}

fn trimmed_rows(tex: &str) -> Vec<String> {
    match display(tex, &MathOptions::default(), WIDTH) {
        MathDisplay::Box(b) => b
            .rows
            .iter()
            .map(|r| r.text.trim_end().to_string())
            .collect(),
        other => panic!("{tex}: expected a box, got {other:?}"),
    }
}

#[test]
fn approved_renderings() {
    let tex = r"\sum_{i=1}^{n} i^2 = \frac{n(n+1)(2n+1)}{6}";
    let MathDisplay::Box(b) = display(tex, &MathOptions::default(), WIDTH) else {
        panic!("sum: expected a box");
    };
    assert_eq!((b.width, b.height, b.baseline), (21, 3, 1));
    for row in &b.rows {
        assert_eq!(row.width, 21, "rows are padded to the box width");
    }
    assert_eq!(
        trimmed_rows(tex),
        [
            " n       n(n+1)(2n+1)",
            " ∑  i² = ────────────",
            "i=1           6"
        ]
    );
    // The 6 is centred under the 12-wide bar that starts at column 9.
    assert_eq!(b.rows[2].text.find('6'), Some(14));

    assert_eq!(
        trimmed_rows(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}"),
        ["⎛a  b⎞", "⎝c  d⎠"]
    );
    assert_eq!(
        trimmed_rows(r"A = \begin{pmatrix} a & b \\ c & d \end{pmatrix}"),
        ["A = ⎛a  b⎞", "    ⎝c  d⎠"]
    );
    assert_eq!(
        trimmed_rows(r"f(x) = \begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}"),
        ["       ⎧ 1  x > 0", "f(x) = ⎨", "       ⎩ 0  otherwise"]
    );
    assert_eq!(
        inline(r"\alpha^2 + \frac{a}{b}", &MathOptions::default()).text,
        "α² + a/b"
    );
}

#[test]
fn plan_linear_examples() {
    let cases = [
        (r"a-b=-c", "a − b = −c"),
        (r"\frac{a+b}{c}", "(a+b)/c"),
        (r"\frac12", "½"),
        (r"A^{-1}", "A⁻¹"),
        (r"x^{n+q}", "x^(n+q)"),
        (r"\sqrt{x^2+1}", "√(x²+1)"),
        (r"\sum_{i=1}^n", "∑ᵢ₌₁ⁿ"),
        (r"\int_0^\infty", "∫₀^∞"),
        (r"\lim_{x\to 0}", "lim_(x→0)"),
        (r"\mathbb{R}^n", "ℝⁿ"),
        (r"\hat a", "â"),
        (r"\not\in", "∉"),
        (r"\binom{n}{k}", "(n choose k)"),
        (r"a\equiv b\pmod n", "a ≡ b (mod n)"),
        (
            r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
            "(a b; c d)",
        ),
        (
            r"\begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}",
            "{1, x > 0; 0, otherwise}",
        ),
        (r"\phi \ne \varphi", "ϕ ≠ φ"),
    ];
    for (tex, want) in cases {
        assert_eq!(inline(tex, &MathOptions::default()).text, want, "{tex}");
    }
}

/// Spans as `text⟨role⟩`, with `*` for bold and `~` for dim.
fn roles(line: &MathLine) -> String {
    let mut out = String::new();
    let mut start = 0;
    for span in &line.spans {
        let end = span.end as usize;
        let text = &line.text[start..end];
        start = end;
        let role = match span.role {
            MathRole::Plain => "plain",
            MathRole::Var => "var",
            MathRole::Num => "num",
            MathRole::Op => "op",
            MathRole::Rel => "rel",
            MathRole::Func => "func",
            MathRole::Text => "text",
            MathRole::Delim => "delim",
            MathRole::Error => "error",
        };
        let flags = format!(
            "{}{}",
            if span.bold { "*" } else { "" },
            if span.dim { "~" } else { "" }
        );
        out.push_str(&format!("{text:?}⟨{role}{flags}⟩ "));
    }
    out.trim_end().to_string()
}

#[test]
fn span_roles() {
    let formulas = [
        r"\alpha^2 + \frac{a}{b}",
        r"x^{n+q} = y",
        r"\sin x \le 1",
        r"\mathbf{v} \cdot \boldsymbol{\omega}",
        r"\text{if } n > 0",
        r"\left( a \right)",
        r"\sqrt{2}",
        r"\frac{x",
    ];
    let out: Vec<String> = formulas
        .iter()
        .map(|tex| format!("{tex}\n  {}", roles(&inline(tex, &MathOptions::default()))))
        .collect();
    insta::assert_snapshot!("span_roles", out.join("\n"));
}

corpus!(
    fractions,
    [
        r"\frac{a+b}{c}",
        r"\frac{1}{2}",
        r"\frac{3}{4} + \frac{1}{16}",
        r"\frac{n(n+1)}{2}",
        r"\dfrac{a}{b} = \tfrac{a}{b}",
        r"\frac{1}{1+\frac{1}{x}}",
        r"\frac{\frac{a}{b}}{\frac{c}{d}}",
        r"\cfrac{1}{1+\cfrac{1}{1+\cfrac{1}{x}}}",
        r"\frac{\partial f}{\partial x}",
        r"\frac{d^2 y}{dx^2}",
        r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}",
        r"\binom{n}{k} = \frac{n!}{k!(n-k)!}",
        r"\frac{a}{b} \cdot \frac{c}{d} = \frac{ac}{bd}",
    ]
);

corpus!(
    scripts,
    [
        r"x^2 + y^2 = z^2",
        r"x_i^2",
        r"x^{2n}",
        r"A^{-1} A = I",
        r"A^T = A^\dagger",
        r"e^{i\pi} + 1 = 0",
        r"e^{-x^2}",
        r"2^{2^n}",
        r"x_{i,j}",
        r"a_{n+1} = a_n + a_{n-1}",
        r"f'(x) + f''(x)",
        r"f^{(n)}(x)",
        r"90^\circ",
        r"\mathbb{R}^{n \times n}",
        r"T_{\mu\nu} = g_{\mu\nu}",
        r"\Gamma^\lambda_{\mu\nu}",
        r"x_N + y_{\max}",
        r"x^{1/2}",
        r"\nabla_\theta J(\theta)",
        r"x_\alpha y_\beta",
        r"x_N^2",
        r"\frac{a}{b}^2",
        r"\sqrt{x}^2",
    ]
);

corpus!(
    roots,
    [
        r"\sqrt{x}",
        r"\sqrt{2}",
        r"\sqrt{x^2+1}",
        r"\sqrt[3]{x}",
        r"\sqrt[4]{x+1}",
        r"\sqrt[n]{x}",
        r"\sqrt[n+1]{x}",
        r"\sqrt{\frac{a}{b}}",
        r"\sqrt{1+\sqrt{2}}",
        r"\sqrt{x_1^2 + x_2^2}",
        r"\sqrt[\pi]{x}",
        r"\sqrt{2}\pi",
        r"\sqrt{(a+b)}",
    ]
);

corpus!(
    big_operators,
    [
        r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}",
        r"\sum_{k=0}^\infty \frac{x^k}{k!}",
        r"\prod_{p} \frac{1}{1-p^{-s}}",
        r"\int_0^1 x\,dx",
        r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
        r"\iint_D f\,dA",
        r"\oint_C \mathbf{F} \cdot d\mathbf{r}",
        r"\lim_{x \to 0} \frac{\sin x}{x} = 1",
        r"\lim_{n\to\infty} \left(1+\frac{1}{n}\right)^n = e",
        r"\max_{x \in S} f(x)",
        r"\operatorname*{argmin}_\theta L(\theta)",
        r"\bigcup_{i=1}^n A_i",
        r"\sum\nolimits_i x_i",
        r"\int\limits_a^b f",
        r"\sum_{\substack{0<i<m\\0<j<n}} P(i,j)",
        r"\liminf_{n} a_n",
    ]
);

corpus!(
    functions,
    [
        r"\sin x",
        r"\sin^2 x + \cos^2 x = 1",
        r"\log_2 n",
        r"\ln(1+x)",
        r"\exp(x) = e^x",
        r"\det(A) \ne 0",
        r"\gcd(a, b)",
        r"\operatorname{sgn}(x)",
        r"\tan\theta = \frac{\sin\theta}{\cos\theta}",
        r"a \bmod b",
        r"a \equiv b \pmod{n}",
    ]
);

corpus!(
    alphabets,
    [
        r"\mathbb{R}",
        r"\mathbb{N} \subset \mathbb{Z} \subset \mathbb{Q} \subset \mathbb{R} \subset \mathbb{C}",
        r"\mathcal{L}(\theta)",
        r"\mathcal{O}(n \log n)",
        r"\mathscr{H}",
        r"\mathfrak{g}",
        r"\mathbf{x} \cdot \mathbf{y}",
        r"\boldsymbol{\alpha}",
        r"\mathrm{d}x",
        r"\text{if } x > 0",
        r"\mathbb{1}",
        r"\mathit{AB}",
    ]
);

corpus!(
    accents,
    [
        r"\hat{x} \hat{a}",
        r"\bar{x} \bar{a}",
        r"\vec{v}",
        r"\dot{x} \ddot{x}",
        r"\tilde{n} \check{c} \breve{u}",
        r"\acute{e} \grave{a} \mathring{A}",
        r"\overline{AB}",
        r"\underline{x}",
        r"\widehat{xy} \widetilde{abc}",
        r"\overrightarrow{AB}",
        r"\hat{\mathbf{n}}",
        r"\overline{\frac{a}{b}}",
    ]
);

corpus!(
    relations,
    [
        r"\not\in",
        r"a \not= b",
        r"a \neq b",
        r"a \not< b",
        r"A \not\subset B",
        r"a \not\equiv b",
        r"a \not\preceq b",
        r"x \notin A",
        r"a \le b \ge c",
        r"a \approx b \sim c \simeq d \cong e \equiv f",
        r"A \subseteq B",
        r"p \implies q \iff r",
        r"\forall x \exists y",
    ]
);

corpus!(
    delimiters,
    [
        r"\left( \frac{a}{b} \right)",
        r"\left[ x \right]",
        r"\left\{ \frac{1}{2} \right\}",
        r"\left| x \right|",
        r"\left\| v \right\|",
        r"\left\lfloor x \right\rfloor",
        r"\left\lceil \frac{n}{2} \right\rceil",
        r"\left\langle \frac{a}{b} \right\rangle",
        r"\left. \frac{df}{dx} \right|_{x=0}",
        r"\Big( \frac{a}{b} \Big)",
        r"\bigl[ x \bigr]",
        r"|x|",
        r"\{x \mid x > 0\}",
        r"\left( \begin{matrix} a \\ b \end{matrix} \right)",
    ]
);

corpus!(
    matrices,
    [
        r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
        r"\begin{bmatrix} 1 & 0 \\ 0 & 1 \end{bmatrix}",
        r"\begin{Bmatrix} x \\ y \end{Bmatrix}",
        r"\begin{vmatrix} a & b \\ c & d \end{vmatrix} = ad - bc",
        r"\begin{Vmatrix} x \end{Vmatrix}",
        r"\begin{matrix} a & b \\ c & d \end{matrix}",
        r"\begin{pmatrix} 1 & 2 & 3 \\ 4 & 5 & 6 \\ 7 & 8 & 9 \end{pmatrix}",
        r"\begin{pmatrix} \frac{1}{2} & 0 \\ 0 & \frac{1}{3} \end{pmatrix}",
        r"\begin{smallmatrix} a & b \\ c & d \end{smallmatrix}",
        r"A = \begin{pmatrix} a & b \\ c & d \end{pmatrix}",
        r"\begin{array}{c|c} a & b \\ \hline c & d \end{array}",
        r"\left[\begin{array}{cc|c} 1 & 2 & 3 \\ 4 & 5 & 6 \end{array}\right]",
        r"\begin{pmatrix} a+b & c \\ d & e \end{pmatrix}",
        r"\begin{array}{c||c} a & b \\ \hline c & d \end{array}",
    ]
);

corpus!(
    environments,
    [
        r"f(x) = \begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}",
        r"|x| = \begin{cases} x & x \ge 0 \\ -x & x < 0 \end{cases}",
        r"\operatorname{sgn}(x) = \begin{cases} 1 & x > 0 \\ 0 & x = 0 \\ -1 & x < 0 \end{cases}",
        r"\begin{aligned} f(x) &= (x+1)^2 \\ &= x^2 + 2x + 1 \end{aligned}",
        r"\begin{align*} a &= b \\ c &= d \end{align*}",
        r"\begin{gathered} a = b \\ c = d \end{gathered}",
        r"a = b \\ c = d",
        r"x &= 1 \\ y &= 2",
        r"\begin{equation} E = mc^2 \end{equation}",
        r"\begin{split} a &= b + c \\ &= d \end{split}",
        r"\begin{aligned} x &= 1 & y &= 2 \end{aligned}",
        r"\begin{aligned} a + b &= c \\ &\quad + d \end{aligned}",
        r"a = b \\ c = d % trailing comment",
    ]
);

corpus!(
    spacing,
    [
        r"a\,b",
        r"a\:b",
        r"a\;b",
        r"a~b",
        r"a\ b",
        r"a\quad b",
        r"a\qquad b",
        r"a\!b",
        r"f(x)\,dx",
        r"a\hspace{1em}b",
    ]
);

corpus!(
    greek,
    [
        r"\alpha \beta \gamma \delta",
        r"\epsilon \varepsilon",
        r"\theta \vartheta \phi \varphi",
        r"\Gamma \Delta \Theta \Lambda \Xi \Pi \Sigma \Phi \Psi \Omega",
        r"\pi r^2",
        r"\mu \nu \xi \rho \sigma \tau \omega",
    ]
);

corpus!(
    symbols,
    [
        r"a \pm b \mp c",
        r"a \times b \div c",
        r"a \cdot b \circ c",
        r"\nabla f = \partial_x f",
        r"\infty \emptyset \aleph_0",
        r"\hbar \omega",
        r"1, 2, \ldots, n",
        r"a_1 + \cdots + a_n",
        r"A \cup B \cap C \setminus D",
        r"\neg p \land q \lor r",
    ]
);

corpus!(
    science,
    [
        r"E = mc^2",
        r"F = ma",
        r"e^{i\theta} = \cos\theta + i\sin\theta",
        r"a^2 + b^2 = c^2",
        r"\sigma(z)_i = \frac{e^{z_i}}{\sum_{j=1}^K e^{z_j}}",
        r"P(A \mid B) = \frac{P(B \mid A)\,P(A)}{P(B)}",
        r"f(x) = \frac{1}{\sigma\sqrt{2\pi}} e^{-\frac{(x-\mu)^2}{2\sigma^2}}",
        r"\nabla \cdot \mathbf{E} = \frac{\rho}{\varepsilon_0}",
        r"\nabla \times \mathbf{B} = \mu_0 \mathbf{J} + \mu_0 \varepsilon_0 \frac{\partial \mathbf{E}}{\partial t}",
        r"i\hbar \frac{\partial}{\partial t} \Psi = \hat{H} \Psi",
        r"\mathcal{L} = -\sum_{i} y_i \log \hat{y}_i",
        r"\operatorname{Attention}(Q, K, V) = \operatorname{softmax}\left(\frac{QK^T}{\sqrt{d_k}}\right) V",
        r"\theta \leftarrow \theta - \eta \nabla_\theta J(\theta)",
        r"D_{\mathrm{KL}}(P \parallel Q) = \sum_x P(x) \log \frac{P(x)}{Q(x)}",
        r"\operatorname{Var}(X) = \mathbb{E}[X^2] - \mathbb{E}[X]^2",
    ]
);

corpus!(
    over_under,
    [
        r"\overbrace{a+b+c}^{n}",
        r"\underbrace{1+\cdots+1}_{n\text{ times}}",
        r"\overset{\text{def}}{=}",
        r"\stackrel{?}{=}",
        r"\overset{\triangle}{=}",
        r"\underset{x}{\arg\min}",
        r"\xrightarrow{f}",
        r"\xleftarrow[g]{f}",
        r"\overset{a}{b}",
    ]
);

corpus!(
    tags,
    [
        r"E = mc^2 \tag{1}",
        r"a = b \tag*{(A)}",
        r"x \label{eq:x} = 1",
        r"y = 2 \nonumber",
    ]
);

corpus!(
    malformed,
    [
        r"\frac{a}",
        r"x^",
        r"{",
        r"}",
        r"\left( x",
        r"\begin{foo} x \end{foo}",
        r"\unknowncommand",
        r"x^{a}^{b}",
        r"\\",
        r"\sqrt[",
        r"",
        r"   ",
        r"\text{unclosed",
        "a\u{7}b",
    ]
);

corpus!(
    unicode_input,
    [
        r"\text{日本語} = x",
        r"x = \text{größer}",
        r"α + β = γ",
        r"\text{naïve} \cdot 2",
        "x\u{301} = e\u{301}",
        "a =\u{338} b",
    ]
);

#[test]
fn narrow_fallbacks() {
    let cases: &[(&str, u16)] = &[
        (r"a + b = c + d = e + f", 13),
        (r"a + b = c + d = e + f", 12),
        (r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}", 12),
        (r"\sum_{i=1}^{n} i^2 = \frac{n(n+1)(2n+1)}{6}", 16),
        (r"f(x) = a_0 + a_1 x + a_2 x^2 + a_3 x^3 + a_4 x^4", 20),
        (r"\frac{aaaa+bbbb}{c} + d", 6),
        (r"E = mc^2 \tag{1}", 9),
        (
            r"\begin{aligned} f(x) &= a+b+c+d \\ &= e+f+g+h \end{aligned}",
            12,
        ),
        ("aaaa+\\char\"A0", 3),
    ];
    let out: Vec<String> = cases
        .iter()
        .map(|&(tex, avail)| {
            format!(
                "@ width {avail}\n{}",
                render_one(tex, &MathOptions::default(), avail)
            )
        })
        .collect();
    insta::assert_snapshot!("narrow_fallbacks", out.join("\n"));
}

#[test]
fn options() {
    use emde_math::{Bold, Fractions, Letters, ScriptSet};
    let variants: Vec<(&str, MathOptions)> = vec![
        (
            "unicode italic",
            MathOptions {
                letters: Letters::UnicodeItalic,
                ..MathOptions::default()
            },
        ),
        (
            "plain letters",
            MathOptions {
                letters: Letters::Plain,
                ..MathOptions::default()
            },
        ),
        (
            "full scripts",
            MathOptions {
                scripts: ScriptSet::Full,
                ..MathOptions::default()
            },
        ),
        (
            "slash fractions",
            MathOptions {
                fractions: Fractions::Slash,
                ..MathOptions::default()
            },
        ),
        (
            "unicode bold",
            MathOptions {
                bold: Bold::Unicode,
                ..MathOptions::default()
            },
        ),
        (
            "ambiguous wide",
            MathOptions {
                ambiguous_wide: true,
                ..MathOptions::default()
            },
        ),
    ];
    let formulas = [
        r"h(x) = x^q + \frac{1}{2}",
        r"\mathbf{F} = m\mathbf{a}",
        r"\sum_{i=1}^n x_i",
    ];
    let out: Vec<String> = variants
        .iter()
        .map(|(name, opts)| format!("@ {name}\n{}", render_all(&formulas, opts, WIDTH)))
        .collect();
    insta::assert_snapshot!("options", out.join("\n"));
}
