//! pulldown-latex events → [`ast`](crate::ast) nodes.
//!
//! pulldown-latex 0.8 reports a formula as a flat stream of events in prefix
//! order with fixed arity (see its `event` module):
//!
//! * `Visual::Fraction` is followed by two elements (numerator, denominator),
//!   `Visual::Root` by the radicand and then the index, `Visual::SquareRoot`
//!   and `Visual::Negation` by one;
//! * `Script { ty, position }` is followed by the base, then the subscript
//!   and/or the superscript — always in that order, whatever the source order
//!   (pinned by a test below: the crate's own documentation disagrees);
//! * `Begin(grouping) … End` counts as one element;
//! * inside environments, `EnvironmentFlow::Alignment` and `NewLine` separate
//!   cells and rows;
//! * a `StateChange` applies to the rest of its group (or cell).
//!
//! [`parse`] walks that stream with a recursive descent and builds an owned
//! tree. Any parser error — `ParserError` has no span, so there is nothing to
//! recover — any violation of the arity rules, nesting deeper than
//! [`MAX_DEPTH`] and any panic inside pulldown-latex all make the whole
//! formula fail, and the caller shows it raw.

use std::panic::{self, AssertUnwindSafe};

use pulldown_latex::event::{
    ArrayColumn, ColumnAlignment, Content, DelimiterType, Dimension, DimensionUnit,
    EnvironmentFlow, Event, Font as LatexFont, Grouping, Line, ScriptPosition, ScriptType,
    StateChange, Visual,
};
use pulldown_latex::{Parser, Storage};

use crate::ast::{
    Accent, Align, Atom, Brace, BraceShape, Column, Font, Grid, GridKind, Limits, Node, Side,
};
use crate::width::sanitize;

/// The deepest nesting of elements accepted; deeper formulas are shown raw
/// (it also bounds the renderers' recursion).
pub(crate) const MAX_DEPTH: usize = 128;

/// The widest explicit space kept, in columns.
const MAX_SPACE: i16 = 16;

/// Why a formula has no tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParseError {
    /// pulldown-latex rejected the TeX.
    Syntax,
    /// The events broke pulldown-latex's documented arity rules.
    Structure,
    /// Nested deeper than [`MAX_DEPTH`].
    TooDeep,
    /// pulldown-latex panicked.
    Panicked,
}

/// Parse TeX (already through the [pre-pass](crate::prepass)) into the
/// top-level row.
///
/// Panics inside pulldown-latex are caught here. The process panic hook still
/// runs first, so a caller with a hook of its own should treat this call as a
/// guarded section (emde wraps it in `panic::guarded`).
pub(crate) fn parse(tex: &str) -> Result<Node, ParseError> {
    panic::catch_unwind(AssertUnwindSafe(|| {
        // The parser borrows the storage, so it must be bound first.
        let storage = Storage::new();
        let events = Parser::new(tex, &storage)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ParseError::Syntax)?;
        build(&events)
    }))
    .unwrap_or(Err(ParseError::Panicked))
}

/// Build the tree for a whole event stream.
fn build(events: &[Event<'_>]) -> Result<Node, ParseError> {
    let mut builder = Builder {
        events,
        pos: 0,
        depth: 0,
    };
    let (items, _) = builder.sequence(Scope::Top)?;
    Ok(Node::Row(items))
}

/// Where a sequence of elements ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// The whole formula: ends with the stream.
    Top,
    /// A group: ends at `End`.
    Group,
    /// A grid cell: ends at `End`, `Alignment` or `NewLine`.
    Cell,
}

/// What ended a sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    /// The end of the stream.
    Eof,
    /// `End`.
    End,
    /// `&`.
    Cell,
    /// `\\`, with the number of `\hline`s after it.
    Row(u8),
}

/// A recursive-descent cursor over the events.
struct Builder<'e, 'a> {
    events: &'e [Event<'a>],
    pos: usize,
    depth: usize,
}

impl<'e, 'a> Builder<'e, 'a> {
    fn peek(&self) -> Option<&'e Event<'a>> {
        self.events.get(self.pos)
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err(ParseError::TooDeep)
        } else {
            Ok(())
        }
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Elements up to the end of `scope`. A font change wraps the rest of the
    /// sequence; colour and style changes are dropped (the output has no
    /// colour channel, and sizes do not exist in a terminal).
    fn sequence(&mut self, scope: Scope) -> Result<(Vec<Node>, Stop), ParseError> {
        self.enter()?;
        let mut items = Vec::new();
        let stop = loop {
            let Some(event) = self.peek() else {
                if scope == Scope::Top {
                    break Stop::Eof;
                }
                return Err(ParseError::Structure);
            };
            match event {
                Event::End if scope == Scope::Top => return Err(ParseError::Structure),
                Event::End => {
                    self.pos += 1;
                    break Stop::End;
                }
                Event::EnvironmentFlow(flow) => {
                    if scope != Scope::Cell {
                        return Err(ParseError::Structure);
                    }
                    self.pos += 1;
                    match flow {
                        EnvironmentFlow::Alignment => break Stop::Cell,
                        EnvironmentFlow::NewLine {
                            horizontal_lines, ..
                        } => break Stop::Row(count(horizontal_lines)),
                        EnvironmentFlow::StartLines { .. } => {}
                    }
                }
                Event::StateChange(StateChange::Font(font)) => {
                    let font = convert_font(*font);
                    self.pos += 1;
                    let (rest, stop) = self.sequence(scope)?;
                    items.push(Node::Styled {
                        font,
                        body: Box::new(Node::Row(rest)),
                    });
                    break stop;
                }
                Event::StateChange(_) => self.pos += 1,
                Event::Space { .. } if self.is_pmod() => {
                    self.pos += 1;
                    items.push(Node::Space(1));
                }
                _ => items.push(self.element()?),
            }
        };
        self.leave();
        Ok((items, stop))
    }

    /// Whether the next events are `\pmod`'s: a 1em space, then a group
    /// opening with `(` and `mod`. It gets one column of space, not a quad.
    fn is_pmod(&self) -> bool {
        let Some(rest) = self.events.get(self.pos..) else {
            return false;
        };
        matches!(
            rest,
            [
                Event::Space {
                    width: Some(Dimension {
                        unit: DimensionUnit::Em,
                        ..
                    }),
                    ..
                },
                Event::Begin(Grouping::Normal),
                Event::Content(Content::Delimiter { content: '(', .. }),
                Event::Content(Content::Function("mod")),
                ..
            ]
        )
    }

    /// Exactly one element.
    fn element(&mut self) -> Result<Node, ParseError> {
        self.enter()?;
        let event = self.peek().ok_or(ParseError::Structure)?;
        self.pos += 1;
        let node = match event {
            Event::Content(content) => content_node(*content),
            Event::Begin(grouping) => self.group(grouping)?,
            Event::End | Event::EnvironmentFlow(_) => return Err(ParseError::Structure),
            Event::Visual(visual) => self.visual(*visual)?,
            Event::Script { ty, position } => self.script(*ty, *position)?,
            Event::Space { width, .. } => Node::Space(columns(*width)),
            // A state change where an element belongs (`x^\bf`): nothing.
            Event::StateChange(_) => Node::empty(),
        };
        self.leave();
        Ok(node)
    }

    fn group(&mut self, grouping: &Grouping) -> Result<Node, ParseError> {
        let align = |a: &ColumnAlignment| match a {
            ColumnAlignment::Left => Align::Left,
            ColumnAlignment::Center => Align::Center,
            ColumnAlignment::Right => Align::Right,
        };
        Ok(match grouping {
            Grouping::Normal => Node::Row(self.sequence(Scope::Group)?.0),
            Grouping::LeftRight(open, close) => Node::Fenced {
                open: *open,
                close: *close,
                body: Box::new(Node::Row(self.sequence(Scope::Group)?.0)),
            },
            Grouping::Matrix { alignment } => {
                self.grid(GridKind::Matrix(align(alignment)), Vec::new())?
            }
            Grouping::Cases { left } => self.grid(GridKind::Cases { left: *left }, Vec::new())?,
            Grouping::Equation { .. }
            | Grouping::Gather { .. }
            | Grouping::Gathered
            | Grouping::Multline => self.grid(GridKind::Gathered, Vec::new())?,
            Grouping::Align { .. }
            | Grouping::Aligned
            | Grouping::Alignat { .. }
            | Grouping::Alignedat { .. }
            | Grouping::Split => self.grid(GridKind::Aligned, Vec::new())?,
            Grouping::Array(spec) => {
                let columns = spec
                    .iter()
                    .map(|c| match c {
                        ArrayColumn::Column(a) => Column::Cells(align(a)),
                        ArrayColumn::Separator(line) => Column::Rule {
                            dashed: *line == Line::Dashed,
                        },
                    })
                    .collect();
                self.grid(GridKind::Array, columns)?
            }
            Grouping::SubArray { alignment } => {
                self.grid(GridKind::Substack(align(alignment)), Vec::new())?
            }
        })
    }

    /// The cells of an environment, up to its `End`.
    fn grid(&mut self, kind: GridKind, columns: Vec<Column>) -> Result<Node, ParseError> {
        // Size changes (`smallmatrix`, `dcases`) come first; then `\hline`s.
        while let Some(Event::StateChange(StateChange::Style(_))) = self.peek() {
            self.pos += 1;
        }
        let leading = match self.peek() {
            Some(Event::EnvironmentFlow(EnvironmentFlow::StartLines { lines })) => {
                self.pos += 1;
                count(lines)
            }
            _ => 0,
        };
        let mut hlines = vec![leading];
        let mut rows = Vec::new();
        let mut row = Vec::new();
        loop {
            let (cell, stop) = self.sequence(Scope::Cell)?;
            row.push(Node::Row(cell));
            match stop {
                Stop::Cell => {}
                Stop::Row(lines) => {
                    rows.push(std::mem::take(&mut row));
                    hlines.push(lines);
                }
                Stop::End => {
                    rows.push(row);
                    hlines.push(0);
                    break;
                }
                Stop::Eof => return Err(ParseError::Structure),
            }
        }
        // A trailing `\\` leaves an empty last row.
        let empty_last = rows
            .last()
            .is_some_and(|r| r.iter().all(Node::is_empty) && r.len() == 1);
        if rows.len() > 1 && empty_last {
            rows.pop();
            hlines.pop();
        }
        Ok(Node::Grid(Grid {
            kind,
            rows,
            columns,
            hlines,
        }))
    }

    fn visual(&mut self, visual: Visual) -> Result<Node, ParseError> {
        Ok(match visual {
            Visual::Fraction(bar) => {
                let num = Box::new(self.element()?);
                let den = Box::new(self.element()?);
                Node::Frac {
                    num,
                    den,
                    bar: bar.is_none_or(|d| d.value.abs() > f32::EPSILON),
                }
            }
            Visual::SquareRoot => Node::Root {
                radicand: Box::new(self.element()?),
                index: None,
            },
            Visual::Root => {
                let radicand = Box::new(self.element()?);
                let index = Some(Box::new(self.element()?));
                Node::Root { radicand, index }
            }
            Visual::Negation => Node::Not(Box::new(self.element()?)),
        })
    }

    /// The bare `Ordinary` character an element would start with, if it is
    /// one (accents and braces arrive as scripts of such characters).
    fn peek_ordinary(&self) -> Option<(char, bool)> {
        match self.peek() {
            Some(Event::Content(Content::Ordinary { content, stretchy })) => {
                Some((*content, *stretchy))
            }
            _ => None,
        }
    }

    fn script(&mut self, ty: ScriptType, position: ScriptPosition) -> Result<Node, ParseError> {
        let base = self.element()?;
        let (sub, sup, mark) = match ty {
            ScriptType::Subscript => {
                let mark = self.peek_ordinary();
                (Some(self.element()?), None, mark)
            }
            ScriptType::Superscript => {
                let mark = self.peek_ordinary();
                (None, Some(self.element()?), mark)
            }
            ScriptType::SubSuperscript => {
                let sub = self.element()?;
                (Some(sub), Some(self.element()?), None)
            }
        };
        Ok(classify_script(base, sub, sup, position, mark))
    }
}

/// Turn a script event into a node. `mark` is the bare `Ordinary` character
/// (and its stretchy flag) when the only script was one: accents, braces and
/// under-arrows are encoded as over/under scripts of such characters.
fn classify_script(
    base: Node,
    sub: Option<Node>,
    sup: Option<Node>,
    position: ScriptPosition,
    mark: Option<(char, bool)>,
) -> Node {
    let boxed = |n: Option<Node>| n.map(Box::new);
    match position {
        ScriptPosition::Right => {
            return Node::Scripts {
                base: Box::new(base),
                sub: boxed(sub),
                sup: boxed(sup),
                limits: Limits::Right,
            };
        }
        ScriptPosition::Movable => {
            return Node::Scripts {
                base: Box::new(base),
                sub: boxed(sub),
                sup: boxed(sup),
                limits: Limits::Display,
            };
        }
        ScriptPosition::AboveBelow => {}
    }
    let under = sup.is_none();
    if let Some((ch, stretchy)) = mark {
        if let Some(shape) = brace_shape(ch) {
            return Node::OverUnder {
                base: Box::new(base),
                over: None,
                under: None,
                brace: Some(Brace {
                    shape,
                    over: !under,
                }),
            };
        }
        if is_accent(ch, stretchy, under) {
            return Node::Accent {
                base: Box::new(base),
                accent: Accent {
                    ch,
                    wide: stretchy,
                    under,
                },
            };
        }
    }
    match base {
        // The label of `\overbrace{…}^{…}` / `\underbrace{…}_{…}`.
        Node::OverUnder {
            base,
            over: None,
            under: None,
            brace: Some(brace),
        } => Node::OverUnder {
            base,
            over: boxed(sup),
            under: boxed(sub),
            brace: Some(brace),
        },
        // `\sum\limits`, `\operatorname*{…}`, `\underset{…}{\min}`.
        base if matches!(base.as_atom(), Some(Atom::LargeOp(_) | Atom::Func(_))) => Node::Scripts {
            base: Box::new(base),
            sub: boxed(sub),
            sup: boxed(sup),
            limits: Limits::Display,
        },
        // `\overset`, `\underset`, `\stackrel`, `\xrightarrow`.
        base => Node::OverUnder {
            base: Box::new(base),
            over: boxed(sup),
            under: boxed(sub),
            brace: None,
        },
    }
}

/// The brace shape for the stretchy characters of `\overbrace` and friends.
fn brace_shape(ch: char) -> Option<BraceShape> {
    match ch {
        '⏞' | '⏟' => Some(BraceShape::Brace),
        '⎴' | '⎵' => Some(BraceShape::Bracket),
        '⏜' | '⏝' | '⏠' | '⏡' => Some(BraceShape::Paren),
        _ => None,
    }
}

/// Whether `ch` is one of the characters pulldown-latex uses for accents
/// (`\hat` is `^`, `\widehat` a stretchy `^`, `\underline` a stretchy `_`).
fn is_accent(ch: char, stretchy: bool, under: bool) -> bool {
    match (under, stretchy) {
        (false, false) => "´‾˘ˇ˙¨`^~→˚".contains(ch),
        (false, true) => "^~ˇ←→↔⇒↼⇀".contains(ch),
        (true, true) => "_←→↔".contains(ch),
        (true, false) => false,
    }
}

/// A content event as a node: `~` and `\ ` arrive as the text `&nbsp;` and
/// are plain spaces.
fn content_node(content: Content<'_>) -> Node {
    match content {
        Content::Text("&nbsp;") => Node::Space(1),
        other => Node::Atom(atom(other)),
    }
}

/// A character from the parser, with controls (which would reach the
/// terminal) replaced.
fn visible(c: char) -> char {
    if c.is_control() { '\u{FFFD}' } else { c }
}

fn atom(content: Content<'_>) -> Atom {
    match content {
        Content::Text(t) => Atom::Text(clean_text(t)),
        Content::Number(n) => Atom::Num(sanitize(n)),
        Content::Function(f) => Atom::Func(clean_text(f)),
        // `\colon` is punctuation (`:` alone is a relation).
        Content::Ordinary { content: ':', .. } => Atom::Punct(':'),
        // `\mu` arrives as the micro sign; math alphabets know the Greek mu.
        Content::Ordinary { content: 'µ', .. } => Atom::Ord('μ'),
        Content::Ordinary { content, .. } => Atom::Ord(visible(content)),
        Content::LargeOp { content, .. } => Atom::LargeOp(visible(content)),
        // pulldown-latex treats `/` as binary; TeX spaces it as ordinary.
        Content::BinaryOp {
            content: c @ ('/' | '\\'),
            ..
        } => Atom::Ord(c),
        Content::BinaryOp { content, .. } => Atom::Bin(visible(content)),
        Content::Relation { content, .. } => {
            let mut buf = [0; 8];
            let text = String::from_utf8_lossy(content.encode_utf8_to_buf(&mut buf));
            Atom::Rel(text.chars().map(visible).collect())
        }
        Content::Delimiter { content, size, ty } => Atom::Delim {
            ch: visible(content),
            side: match ty {
                DelimiterType::Open => Side::Open,
                DelimiterType::Close => Side::Close,
                DelimiterType::Fence => Side::Middle,
            },
            sized: size.is_some(),
        },
        // A decimal point is ordinary; `,` and `;` are punctuation.
        Content::Punctuation('.') => Atom::Ord('.'),
        Content::Punctuation(c) => Atom::Punct(visible(c)),
    }
}

/// Text-mode content, which pulldown-latex passes through verbatim: `~` and
/// `\ `-style spaces become spaces, `$` signs go, escaped specials (`\%`)
/// lose their backslash; anything else is kept as written.
fn clean_text(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '~' => out.push(' '),
            '$' => {}
            '\\' => match chars.peek().copied() {
                Some(n @ ('{' | '}' | '$' | '%' | '&' | '#' | '_')) => {
                    chars.next();
                    out.push(n);
                }
                Some(' ' | ',' | ';' | ':' | '\\') => {
                    chars.next();
                    out.push(' ');
                }
                Some('!') => {
                    chars.next();
                }
                _ => out.push('\\'),
            },
            c => out.push(c),
        }
    }
    sanitize(&out)
}

fn convert_font(font: Option<LatexFont>) -> Font {
    match font {
        None => Font::Normal,
        Some(LatexFont::UpRight | LatexFont::SansSerif | LatexFont::Monospace) => Font::Upright,
        Some(LatexFont::Italic | LatexFont::SansSerifItalic) => Font::Italic,
        Some(LatexFont::Bold | LatexFont::BoldSansSerif) => Font::Bold,
        Some(LatexFont::BoldItalic | LatexFont::SansSerifBoldItalic) => Font::BoldItalic,
        Some(LatexFont::BoldSymbol) => Font::BoldSymbol,
        Some(LatexFont::Script) => Font::Script,
        Some(LatexFont::BoldScript) => Font::BoldScript,
        Some(LatexFont::Fraktur) => Font::Fraktur,
        Some(LatexFont::BoldFraktur) => Font::BoldFraktur,
        Some(LatexFont::DoubleStruck) => Font::DoubleStruck,
        Some(LatexFont::DoubleStruckItalic) => Font::DoubleStruckItalic,
    }
}

/// A TeX dimension in columns: one per thin, medium or thick space and two
/// per em (`\quad` is 2, `\qquad` 4); negative widths take columns away
/// (`\!` is −1).
fn columns(width: Option<Dimension>) -> i16 {
    let Some(d) = width else {
        return 0;
    };
    // TeX units per em, assuming a 10pt font.
    let per_em = match d.unit {
        DimensionUnit::Em => 1.0,
        DimensionUnit::Mu => 18.0,
        DimensionUnit::Ex => 2.3,
        DimensionUnit::Pt => 10.0,
        DimensionUnit::Pc => 10.0 / 12.0,
        DimensionUnit::In => 10.0 / 72.27,
        DimensionUnit::Bp => 10.0 * 72.0 / 72.27,
        DimensionUnit::Cm => 10.0 / 28.45,
        DimensionUnit::Mm => 10.0 / 2.845,
        DimensionUnit::Dd => 10.0 / 1.07,
        DimensionUnit::Cc => 10.0 / 12.84,
        DimensionUnit::Sp => 10.0 * 65536.0,
    };
    let cols = (2.0 * d.value / per_em).round();
    if !cols.is_finite() {
        return 0;
    }
    let cols = cols.clamp(f32::from(-MAX_SPACE), f32::from(MAX_SPACE)) as i16;
    match cols {
        0 if d.value > 0.0 => 1,
        0 if d.value < 0.0 => -1,
        n => n,
    }
}

/// A count of `\hline`s, saturated.
fn count(lines: &[Line]) -> u8 {
    u8::try_from(lines.len()).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prepass::prepare;

    fn tree(tex: &str) -> Node {
        let prepared = prepare(tex).expect("prepass");
        parse(&prepared.tex).expect("parse")
    }

    /// The single top-level item.
    fn only(tex: &str) -> Node {
        match tree(tex) {
            Node::Row(mut items) if items.len() == 1 => items.remove(0),
            other => panic!("{tex}: {other:?}"),
        }
    }

    fn ord(c: char) -> Node {
        Node::Atom(Atom::Ord(c))
    }
    fn row(items: Vec<Node>) -> Node {
        Node::Row(items)
    }
    fn b(n: Node) -> Option<Box<Node>> {
        Some(Box::new(n))
    }

    #[test]
    fn scripts_are_base_then_sub_then_sup_in_either_source_order() {
        // pulldown-latex documents different orders in different places;
        // this pins what 0.8 actually emits.
        let want = Node::Scripts {
            base: Box::new(ord('x')),
            sub: b(ord('a')),
            sup: b(ord('b')),
            limits: Limits::Right,
        };
        assert_eq!(only("x_a^b"), want);
        assert_eq!(only("x^b_a"), want);
        let raw = |tex: &str| {
            let storage = Storage::new();
            Parser::new(tex, &storage)
                .map(|e| format!("{:?}", e.expect("event")))
                .collect::<Vec<_>>()
        };
        let events = raw("x^b_a");
        assert!(events[0].contains("SubSuperscript"));
        assert!(events[1].contains("'x'"));
        assert!(events[2].contains("'a'"), "subscript second: {events:?}");
        assert!(events[3].contains("'b'"), "superscript last: {events:?}");
    }

    #[test]
    fn fractions_roots_and_negations_take_their_arity() {
        assert_eq!(
            only(r"\frac{a}{b}"),
            Node::Frac {
                num: Box::new(row(vec![ord('a')])),
                den: Box::new(row(vec![ord('b')])),
                bar: true,
            }
        );
        assert_eq!(
            tree(r"\frac12x"),
            row(vec![
                Node::Frac {
                    num: Box::new(Node::Atom(Atom::Num("1".into()))),
                    den: Box::new(Node::Atom(Atom::Num("2".into()))),
                    bar: true,
                },
                ord('x'),
            ])
        );
        // The radicand comes first, then the (pre-pass braced) index.
        assert_eq!(
            only(r"\sqrt[n+1]{x}"),
            Node::Root {
                radicand: Box::new(row(vec![ord('x')])),
                index: b(row(vec![
                    ord('n'),
                    Node::Atom(Atom::Bin('+')),
                    Node::Atom(Atom::Num("1".into())),
                ])),
            }
        );
        assert_eq!(
            only(r"\not="),
            Node::Not(Box::new(Node::Atom(Atom::Rel("=".into()))))
        );
        assert!(
            matches!(only(r"\binom{n}{k}"), Node::Fenced { open: Some('('), close: Some(')'), body }
            if matches!(body.items(), [Node::Frac { bar: false, .. }]))
        );
    }

    #[test]
    fn accents_braces_and_oversets() {
        assert_eq!(
            only(r"\hat a"),
            Node::Accent {
                base: Box::new(ord('a')),
                accent: Accent {
                    ch: '^',
                    wide: false,
                    under: false
                },
            }
        );
        assert!(matches!(
            only(r"\overrightarrow{AB}"),
            Node::Accent {
                accent: Accent {
                    ch: '→',
                    wide: true,
                    under: false
                },
                ..
            }
        ));
        assert!(matches!(
            only(r"\underline{x}"),
            Node::Accent {
                accent: Accent {
                    ch: '_',
                    under: true,
                    ..
                },
                ..
            }
        ));
        let Node::OverUnder {
            over,
            under,
            brace: Some(brace),
            ..
        } = only(r"\overbrace{a+b}^{n}")
        else {
            panic!("overbrace");
        };
        assert_eq!((brace.shape, brace.over), (BraceShape::Brace, true));
        assert_eq!(over, b(row(vec![ord('n')])));
        assert_eq!(under, None);
        let Node::OverUnder {
            under: Some(label),
            brace: Some(brace),
            ..
        } = only(r"\underbrace{x}_{k}")
        else {
            panic!("underbrace");
        };
        assert!(!brace.over);
        assert_eq!(*label, row(vec![ord('k')]));
        assert!(matches!(
            only(r"\overset{a}{=}"),
            Node::OverUnder {
                brace: None,
                over: Some(_),
                under: None,
                ..
            }
        ));
        assert!(matches!(
            only(r"\xrightarrow{f}"),
            Node::OverUnder {
                brace: None,
                over: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn limits() {
        let limits = |tex: &str| match only(tex) {
            Node::Scripts { limits, .. } => limits,
            other => panic!("{tex}: {other:?}"),
        };
        assert_eq!(limits(r"\sum_{i=1}^n"), Limits::Display);
        assert_eq!(limits(r"\lim_{x\to 0}"), Limits::Display);
        assert_eq!(limits(r"\int_0^1"), Limits::Right);
        assert_eq!(limits(r"\sum\nolimits_i"), Limits::Right);
        assert_eq!(limits(r"\int\limits_0^1"), Limits::Display);
        assert_eq!(limits(r"\operatorname*{argmax}_x"), Limits::Display);
    }

    #[test]
    fn environments_split_cells_and_rows() {
        let Node::Fenced {
            open: Some('('),
            close: Some(')'),
            body,
        } = only(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}")
        else {
            panic!("pmatrix");
        };
        let [Node::Grid(grid)] = body.items() else {
            panic!("grid");
        };
        assert_eq!(grid.kind, GridKind::Matrix(Align::Center));
        let cells: Vec<Vec<Node>> = ["ab", "cd"]
            .iter()
            .map(|r| r.chars().map(|c| row(vec![ord(c)])).collect())
            .collect();
        assert_eq!(grid.rows, cells);
        assert_eq!(grid.hlines, [0, 0, 0]);

        let Node::Grid(cases) = only(r"\begin{cases} 1 & x > 0 \\ 0 & \text{else} \\ \end{cases}")
        else {
            panic!("cases");
        };
        assert_eq!(cases.kind, GridKind::Cases { left: true });
        assert_eq!(cases.rows.len(), 2, "trailing \\\\ dropped");

        let Node::Grid(array) =
            only(r"\begin{array}{c|l} \hline a & b \\ \hline c & d \end{array}")
        else {
            panic!("array");
        };
        assert_eq!(
            array.columns,
            [
                Column::Cells(Align::Center),
                Column::Rule { dashed: false },
                Column::Cells(Align::Left)
            ]
        );
        assert_eq!(array.hlines, [1, 1, 0]);
        assert!(matches!(
            only(r"a \\ b"),
            Node::Grid(Grid {
                kind: GridKind::Gathered,
                ..
            })
        ));
        assert!(matches!(
            only(r"a &= b"),
            Node::Grid(Grid {
                kind: GridKind::Aligned,
                ..
            })
        ));
    }

    #[test]
    fn font_changes_scope_to_their_group_or_cell() {
        assert_eq!(
            tree(r"{\bf a} b"),
            row(vec![
                row(vec![Node::Styled {
                    font: Font::Bold,
                    body: Box::new(row(vec![ord('a')])),
                }]),
                ord('b'),
            ])
        );
        assert_eq!(
            only(r"\mathbb{R}"),
            row(vec![Node::Styled {
                font: Font::DoubleStruck,
                body: Box::new(row(vec![ord('R')])),
            }])
        );
        let Node::Grid(grid) = only(r"\begin{matrix} \bf a & b \end{matrix}") else {
            panic!("matrix");
        };
        assert!(matches!(grid.rows[0][0].items(), [Node::Styled { .. }]));
        assert_eq!(grid.rows[0][1], row(vec![ord('b')]));
        // Colours are dropped.
        assert_eq!(tree(r"\color{red} x"), row(vec![ord('x')]));
    }

    #[test]
    fn atoms_and_spaces() {
        assert_eq!(
            tree(r"a~b\ c\,d\quad e\!f"),
            row(vec![
                ord('a'),
                Node::Space(1),
                ord('b'),
                Node::Space(1),
                ord('c'),
                Node::Space(1),
                ord('d'),
                Node::Space(2),
                ord('e'),
                Node::Space(-1),
                ord('f'),
            ])
        );
        assert_eq!(tree(r"a/b").items()[1], ord('/'));
        assert_eq!(tree(r"f\colon").items()[1], Node::Atom(Atom::Punct(':')));
        assert_eq!(
            tree(r"\text{if $x$~is\%}"),
            row(vec![Node::Atom(Atom::Text("if x is%".into()))])
        );
        assert_eq!(
            tree(r"\operatorname{arg\,max}"),
            row(vec![Node::Atom(Atom::Func("arg max".into()))])
        );
        assert_eq!(tree(r"\phi\varphi").items(), [ord('ϕ'), ord('φ')]);
    }

    #[test]
    fn pmod_gets_one_column_of_space() {
        let items = tree(r"a \pmod{n}");
        assert_eq!(items.items()[1], Node::Space(1));
        assert_eq!(tree(r"a \quad b").items()[1], Node::Space(2));
    }

    #[test]
    fn dimensions_to_columns() {
        let dim = |value, unit| columns(Some(Dimension { value, unit }));
        assert_eq!(dim(3.0 / 18.0, DimensionUnit::Em), 1);
        assert_eq!(dim(1.0, DimensionUnit::Em), 2);
        assert_eq!(dim(2.0, DimensionUnit::Em), 4);
        assert_eq!(dim(18.0, DimensionUnit::Mu), 2);
        assert_eq!(dim(-3.0 / 18.0, DimensionUnit::Em), -1);
        assert_eq!(dim(1e9, DimensionUnit::Em), MAX_SPACE);
        assert_eq!(dim(f32::NAN, DimensionUnit::Em), 0);
        assert_eq!(dim(0.0, DimensionUnit::Pt), 0);
        assert_eq!(columns(None), 0);
    }

    #[test]
    fn errors() {
        assert_eq!(parse(r"\frac{a}"), Err(ParseError::Syntax));
        assert_eq!(parse(r"\unknowncommand"), Err(ParseError::Syntax));
        assert_eq!(parse("x^"), Err(ParseError::Syntax));
        let deep = format!("{}x{}", "{".repeat(MAX_DEPTH), "}".repeat(MAX_DEPTH));
        assert_eq!(parse(&deep), Err(ParseError::TooDeep));
        let fine = format!("{}x{}", "{".repeat(20), "}".repeat(20));
        assert!(parse(&fine).is_ok());
        assert_eq!(parse(""), Ok(Node::Row(Vec::new())));
    }

    #[test]
    fn arity_violations_are_errors() {
        let a = Event::Content(Content::Ordinary {
            content: 'a',
            stretchy: false,
        });
        let frac = Event::Visual(Visual::Fraction(None));
        assert_eq!(
            build(&[frac.clone(), a.clone()]),
            Err(ParseError::Structure)
        );
        assert_eq!(build(&[Event::End]), Err(ParseError::Structure));
        assert_eq!(
            build(&[Event::Begin(Grouping::Normal), a.clone()]),
            Err(ParseError::Structure)
        );
        assert_eq!(
            build(&[Event::EnvironmentFlow(EnvironmentFlow::Alignment)]),
            Err(ParseError::Structure)
        );
        assert!(build(&[frac, a.clone(), a]).is_ok());
    }
}
