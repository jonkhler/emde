//! The owned math AST.
//!
//! [`adapter`](crate::adapter) builds it from pulldown-latex events; the
//! [`linear`](crate::linear) and [`display`](mod@crate::display) renderers
//! consume it. It records what the TeX *means* (a fraction, a script, a
//! delimiter) and leaves every presentation decision to the renderers. Nothing
//! here depends on the parser, so a different front end could produce it.

/// A parsed formula: its body and the `\tag` extracted by the pre-pass.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Formula {
    /// The top-level sequence (always a [`Node::Row`]).
    pub(crate) body: Node,
    /// The equation tag, already formatted (`(1)` for `\tag{1}`, `A` for
    /// `\tag*{A}`).
    pub(crate) tag: Option<String>,
}

/// One node of the math tree.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    /// A leaf symbol.
    Atom(Atom),
    /// A sequence. A braced group `{…}` is one of these, and like in TeX it
    /// spaces as an ordinary symbol from the outside.
    Row(Vec<Node>),
    /// `\frac`, `\binom` (inside a [`Node::Fenced`]) and friends. `bar` is
    /// false for bar-less stacks such as `\binom`.
    Frac {
        num: Box<Node>,
        den: Box<Node>,
        bar: bool,
    },
    /// `\sqrt[index]{radicand}`.
    Root {
        radicand: Box<Node>,
        index: Option<Box<Node>>,
    },
    /// A base with a subscript and/or superscript.
    Scripts {
        base: Box<Node>,
        sub: Option<Box<Node>>,
        sup: Option<Box<Node>>,
        limits: Limits,
    },
    /// An accent over (or, for `\underline`, under) its base.
    Accent { base: Box<Node>, accent: Accent },
    /// `\left … \right` (and `\begin{pmatrix}`'s implicit delimiters).
    /// `None` is the invisible `\left.` delimiter.
    Fenced {
        open: Option<char>,
        close: Option<char>,
        body: Box<Node>,
    },
    /// A matrix-like environment.
    Grid(Grid),
    /// A font change that applies to `body` (`\mathbb{…}`, `\bf …`).
    Styled { font: Font, body: Box<Node> },
    /// Explicit horizontal space in columns; negative values (`\!`) take
    /// space away.
    Space(i16),
    /// `\not` (and `\cancel`): the negated relation or struck-out content.
    Not(Box<Node>),
    /// Something set over and/or under a base: `\overset`, `\underset`,
    /// `\stackrel`, `\xrightarrow`, and horizontal braces with their labels.
    OverUnder {
        base: Box<Node>,
        over: Option<Box<Node>>,
        under: Option<Box<Node>>,
        brace: Option<Brace>,
    },
}

/// A leaf symbol. pulldown-latex already turned commands into Unicode (`\alpha`
/// is `α`, `-` is `−`), so atoms carry final characters.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Atom {
    /// An ordinary symbol: a letter, a Greek letter or another symbol.
    Ord(char),
    /// A number such as `12` or `3.5`.
    Num(String),
    /// An upright operator name (`sin`, `lim`, `mod`, `\operatorname{…}`).
    Func(String),
    /// Text-mode content (`\text{…}`), spaces preserved.
    Text(String),
    /// A large operator (`∑`, `∫`).
    LargeOp(char),
    /// A binary operator (`+`, `×`).
    Bin(char),
    /// A relation; some are two characters (`≔` spelled `:−`).
    Rel(String),
    /// A delimiter. `sized` is set for `\big(`-style delimiters, which the 2D
    /// renderer stretches to the height of the surrounding row.
    Delim { ch: char, side: Side, sized: bool },
    /// Punctuation (`,`, `;`, `\colon`).
    Punct(char),
}

/// Which side a delimiter opens or closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Open,
    Close,
    /// A fence that is neither, such as `|` or `\middle|`.
    Middle,
}

/// How scripts attach to a base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Limits {
    /// To the right of the base (`x^2`, `\int_0^1`).
    Right,
    /// Above and below the base in display math, to the right inline
    /// (`\sum`, `\lim`; also `\limits`, which TeX keeps above/below even
    /// inline but which reads better linearly here).
    Display,
}

/// An accent. The renderers look up the combining mark in
/// [`tables`](crate::tables).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Accent {
    /// The spacing character pulldown-latex reports (`^` for `\hat`, `‾` for
    /// `\bar`, `→` for `\vec`, `_` for `\underline`).
    pub(crate) ch: char,
    /// Wide variants (`\widehat`, `\overrightarrow`) that span their base.
    pub(crate) wide: bool,
    /// Accents set under the base (`\underline`, `\underrightarrow`).
    pub(crate) under: bool,
}

/// A horizontal brace drawn over or under a base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Brace {
    pub(crate) shape: BraceShape,
    /// Above the base (`\overbrace`) rather than below (`\underbrace`).
    pub(crate) over: bool,
}

/// The shape of a horizontal brace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BraceShape {
    /// `\overbrace`, `\underbrace`: `╭─┴─╮`.
    Brace,
    /// `\overbracket`, `\underbracket`: `┌───┐`.
    Bracket,
    /// `\overparen`, `\overgroup` and their under forms: `╭───╮`.
    Paren,
}

/// A math alphabet or font switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Font {
    /// Back to the default math font (`\mathnormal`).
    Normal,
    /// Upright letters (`\mathrm`, `\mathsf`, `\mathtt`, `\text…`).
    Upright,
    /// Italic letters (`\mathit`).
    Italic,
    /// Upright bold (`\mathbf`).
    Bold,
    /// Bold italic (`\mathbfit`).
    BoldItalic,
    /// `\boldsymbol`: bold italic letters and lowercase Greek, bold upright
    /// digits and capital Greek, and bold symbols.
    BoldSymbol,
    /// `\mathcal`, `\mathscr`.
    Script,
    /// `\mathbfcal`.
    BoldScript,
    /// `\mathfrak`.
    Fraktur,
    /// `\mathbffrak`.
    BoldFraktur,
    /// `\mathbb`.
    DoubleStruck,
    /// `\mathbbit`.
    DoubleStruckItalic,
}

impl Font {
    /// Fonts shown with the bold attribute when bold is not Unicode.
    pub(crate) fn is_bold(self) -> bool {
        matches!(self, Font::Bold | Font::BoldItalic | Font::BoldSymbol)
    }
}

/// A matrix-like environment: rows of cells.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Grid {
    pub(crate) kind: GridKind,
    /// Rows of cells; rows may have different lengths.
    pub(crate) rows: Vec<Vec<Node>>,
    /// `array` column specification (empty for other environments).
    pub(crate) columns: Vec<Column>,
    /// Horizontal rules: `hlines[i]` is the number of `\hline`s before row
    /// `i`; `hlines[rows.len()]` those after the last row.
    pub(crate) hlines: Vec<u8>,
}

/// The environment behind a [`Grid`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GridKind {
    /// `matrix` and friends (the delimiters are a surrounding
    /// [`Node::Fenced`]).
    Matrix(Align),
    /// `cases` (`left`) and `rcases`.
    Cases { left: bool },
    /// `aligned`, `align`, `alignat`, `split`: right/left column pairs.
    Aligned,
    /// `gathered`, `gather`, `equation`, `multline`: centred rows.
    Gathered,
    /// `array` with its column specification.
    Array,
    /// `\substack` and `subarray`: stacked rows in limits.
    Substack(Align),
}

/// Horizontal alignment of a grid column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Center,
    Right,
}

/// One entry of an `array` column specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Column {
    /// A content column.
    Cells(Align),
    /// A vertical rule (`|`, or `:` when `dashed`).
    Rule { dashed: bool },
}

impl Node {
    /// An empty row, used for missing content.
    pub(crate) fn empty() -> Node {
        Node::Row(Vec::new())
    }

    /// Whether the node renders as nothing.
    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Node::Row(items) => items.iter().all(Node::is_empty),
            Node::Styled { body, .. } => body.is_empty(),
            Node::Space(n) => *n == 0,
            _ => false,
        }
    }

    /// The items of a row, or the node itself as a one-item sequence.
    pub(crate) fn items(&self) -> &[Node] {
        match self {
            Node::Row(items) => items,
            other => std::slice::from_ref(other),
        }
    }

    /// The single atom this node boils down to, looking through one-item rows
    /// and font changes.
    pub(crate) fn as_atom(&self) -> Option<&Atom> {
        match self {
            Node::Atom(a) => Some(a),
            Node::Row(items) => match items.as_slice() {
                [only] => only.as_atom(),
                _ => None,
            },
            Node::Styled { body, .. } => body.as_atom(),
            _ => None,
        }
    }
}
