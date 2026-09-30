//! TeX-style spacing between the items of a row.
//!
//! Both renderers space rows the same way, following TeX's inter-atom table
//! (TeXbook chapter 18) in a terminal-sized form: one column where TeX would
//! put a thin, medium or thick space, nothing elsewhere.
//!
//! * Binary operators and relations get a column on each side, except in
//!   *tight* contexts (scripts, fraction parts, radicands).
//! * A binary operator with nothing to operate on becomes ordinary (TeX rules
//!   5 and 6), so there is no space after a unary minus: `a − b = −c`.
//! * Large operators and function names are separated from what follows by a
//!   column in every context (`sin x`, `∑ᵢ i`). A function name hugs an
//!   opening delimiter (`sin(x)`); a large operator or an operator with limits
//!   does not (`lim_(x→0) (…)`, `∑ᵢ (…)`), which reads better in a terminal
//!   than TeX's tight setting.
//! * Punctuation is followed by a column outside tight contexts (`f(x, y)`).

use crate::ast::{Atom, Limits, Node, Side};

/// TeX's atom classes, as far as spacing needs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// Ordinary symbols, numbers, groups and constructions.
    Ord,
    /// Function names.
    Op,
    /// Large operators and operators with limits above and below.
    Large,
    /// Binary operators.
    Bin,
    /// Relations.
    Rel,
    /// Opening delimiters.
    Open,
    /// Closing delimiters.
    Close,
    /// Punctuation.
    Punct,
}

/// The spacing of one row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Spacing {
    /// Per item: its resolved `(left, right)` classes, or `None` for items
    /// that spacing looks through (explicit spaces, empty font switches).
    pub(crate) classes: Vec<Option<(Class, Class)>>,
    /// Per item: the columns to put before it.
    pub(crate) before: Vec<usize>,
    /// Columns after the last item, towards a virtual trailing neighbour.
    pub(crate) after: usize,
}

impl Spacing {
    /// Whether item `i` is a binary operator or relation after resolution,
    /// which is where a line may break.
    pub(crate) fn breaks_after(&self, i: usize) -> bool {
        matches!(
            self.classes.get(i),
            Some(Some((_, Class::Bin | Class::Rel)))
        )
    }
}

/// Space the items of a row.
///
/// `lead` and `trail` are the classes of virtual neighbours before the first
/// and after the last item. Aligned environments use them so that a cell
/// starting with `=` is spaced as if the previous cell's content preceded it.
pub(crate) fn space(
    items: &[Node],
    tight: bool,
    lead: Option<Class>,
    trail: Option<Class>,
) -> Spacing {
    let mut classes: Vec<Option<(Class, Class)>> = items.iter().map(edges).collect();
    let unary = resolve_binary(&mut classes, lead, trail);
    let mut before = vec![0; items.len()];
    // The previous item's right edge, and whether it was a unary sign.
    let mut prev = lead.map(|c| (c, false));
    for ((slot, class), &sign) in before.iter_mut().zip(&classes).zip(&unary) {
        if let Some((left, right)) = *class {
            if let Some((p, after_sign)) = prev {
                *slot = if after_sign { 0 } else { gap(p, left, tight) };
            }
            prev = Some((right, sign));
        }
    }
    let after = match (prev, trail) {
        (Some((p, _)), Some(t)) if !classes.iter().all(Option::is_none) => gap(p, t, tight),
        _ => 0,
    };
    Spacing {
        classes,
        before,
        after,
    }
}

/// The classes a node presents to its left and right neighbours.
pub(crate) fn edges(node: &Node) -> Option<(Class, Class)> {
    let ord = Some((Class::Ord, Class::Ord));
    match node {
        Node::Atom(atom) => {
            let c = atom_class(atom);
            Some((c, c))
        }
        Node::Space(_) => None,
        Node::Styled { body, .. } => {
            let items = body.items();
            let left = items.iter().find_map(edges)?.0;
            let right = items.iter().rev().find_map(edges)?.1;
            Some((left, right))
        }
        Node::Scripts {
            base,
            limits: Limits::Display,
            ..
        } if matches!(edges(base), Some((Class::Op, _))) => Some((Class::Large, Class::Large)),
        Node::Scripts { base, .. } | Node::OverUnder { base, .. } | Node::Not(base) => {
            edges(base).or(ord)
        }
        // `\left(…\right)` spaces like its delimiters: `f(x)`, `sin(x)`.
        Node::Fenced { open, close, .. } => Some((
            if open.is_some() {
                Class::Open
            } else {
                Class::Ord
            },
            if close.is_some() {
                Class::Close
            } else {
                Class::Ord
            },
        )),
        Node::Row(_)
        | Node::Frac { .. }
        | Node::Root { .. }
        | Node::Accent { .. }
        | Node::Grid(_) => ord,
    }
}

fn atom_class(atom: &Atom) -> Class {
    match atom {
        Atom::Ord(_) | Atom::Num(_) | Atom::Text(_) => Class::Ord,
        Atom::Func(_) => Class::Op,
        Atom::LargeOp(_) => Class::Large,
        Atom::Bin(_) => Class::Bin,
        Atom::Rel(_) => Class::Rel,
        Atom::Delim { side, .. } => match side {
            Side::Open => Class::Open,
            Side::Close => Class::Close,
            Side::Middle => Class::Ord,
        },
        Atom::Punct(_) => Class::Punct,
    }
}

/// TeX rules 5 and 6: a binary operator that does not stand between two
/// operands (at the start, after another operator, a relation, an opening
/// delimiter or punctuation, or before a relation, closing delimiter,
/// punctuation or the end) is ordinary. Returns which items became unary
/// signs (the first case), which are never followed by space.
fn resolve_binary(
    classes: &mut [Option<(Class, Class)>],
    lead: Option<Class>,
    trail: Option<Class>,
) -> Vec<bool> {
    let mut unary = vec![false; classes.len()];
    let mut prev: Option<(usize, Class)> = None;
    for i in 0..classes.len() {
        let Some(Some((left, right))) = classes.get(i).copied() else {
            continue;
        };
        let prev_class = prev.map(|(_, c)| c).or(lead);
        let (mut left, mut right) = (left, right);
        if left == Class::Bin
            && matches!(
                prev_class,
                None | Some(
                    Class::Bin | Class::Op | Class::Large | Class::Rel | Class::Open | Class::Punct
                )
            )
        {
            left = Class::Ord;
            if right == Class::Bin {
                right = Class::Ord;
            }
            if let Some(u) = unary.get_mut(i) {
                *u = true;
            }
        }
        if matches!(left, Class::Rel | Class::Close | Class::Punct)
            && let Some((j, Class::Bin)) = prev
        {
            demote_right(classes, j);
        }
        if let Some(slot) = classes.get_mut(i) {
            *slot = Some((left, right));
        }
        prev = Some((i, right));
    }
    if let Some((j, Class::Bin)) = prev
        && matches!(trail, None | Some(Class::Rel | Class::Close | Class::Punct))
    {
        demote_right(classes, j);
    }
    unary
}

/// Turn the right edge of item `j` (a binary operator) into an ordinary one.
fn demote_right(classes: &mut [Option<(Class, Class)>], j: usize) {
    if let Some(Some((left, right))) = classes.get_mut(j) {
        *right = Class::Ord;
        if *left == Class::Bin {
            *left = Class::Ord;
        }
    }
}

/// Columns between an item whose right edge is `a` and one whose left edge is
/// `b`.
pub(crate) fn gap(a: Class, b: Class, tight: bool) -> usize {
    use Class::*;
    match (a, b) {
        (Bin, _) | (_, Bin) => usize::from(!tight),
        (Rel, Rel) | (Rel, Close) | (Rel, Punct) | (Open, Rel) => 0,
        (Rel, _) | (_, Rel) => usize::from(!tight),
        (Op | Large, Ord | Op | Large) | (Ord | Close, Op | Large) | (Large, Open) => 1,
        (Punct, _) => usize::from(!tight),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ord(c: char) -> Node {
        Node::Atom(Atom::Ord(c))
    }
    fn bin(c: char) -> Node {
        Node::Atom(Atom::Bin(c))
    }
    fn rel(s: &str) -> Node {
        Node::Atom(Atom::Rel(s.into()))
    }
    fn func(s: &str) -> Node {
        Node::Atom(Atom::Func(s.into()))
    }
    fn open() -> Node {
        Node::Atom(Atom::Delim {
            ch: '(',
            side: Side::Open,
            sized: false,
        })
    }

    #[test]
    fn binary_and_relation_spacing() {
        // a − b = −c
        let row = [ord('a'), bin('−'), ord('b'), rel("="), bin('−'), ord('c')];
        let s = space(&row, false, None, None);
        assert_eq!(s.before, [0, 1, 1, 1, 1, 0]);
        assert_eq!(s.classes[4], Some((Class::Ord, Class::Ord)));
        assert!(s.breaks_after(1) && s.breaks_after(3) && !s.breaks_after(4));
    }

    #[test]
    fn tight_contexts_drop_operator_spacing() {
        let row = [ord('i'), rel("="), Node::Atom(Atom::Num("1".into()))];
        assert_eq!(space(&row, true, None, None).before, [0, 0, 0]);
    }

    #[test]
    fn leading_and_trailing_binary_operators_are_ordinary() {
        let row = [bin('−'), ord('x'), bin('+')];
        let s = space(&row, false, None, None);
        assert_eq!(s.before, [0, 0, 0]);
        assert_eq!(s.classes[2], Some((Class::Ord, Class::Ord)));
        // Before a relation too.
        let row = [ord('x'), bin('+'), rel("=")];
        assert_eq!(space(&row, false, None, None).before, [0, 0, 1]);
    }

    #[test]
    fn functions_space_before_undelimited_arguments() {
        assert_eq!(
            space(&[func("sin"), ord('x')], true, None, None).before,
            [0, 1]
        );
        assert_eq!(
            space(&[func("sin"), open()], false, None, None).before,
            [0, 0]
        );
        assert_eq!(
            space(&[ord('2'), func("sin")], true, None, None).before,
            [0, 1]
        );
    }

    #[test]
    fn punctuation_spacing() {
        let row = [ord('x'), Node::Atom(Atom::Punct(',')), ord('y')];
        assert_eq!(space(&row, false, None, None).before, [0, 0, 1]);
        assert_eq!(space(&row, true, None, None).before, [0, 0, 0]);
    }

    #[test]
    fn explicit_spaces_are_transparent() {
        let row = [ord('a'), Node::Space(1), bin('+'), ord('b')];
        let s = space(&row, false, None, None);
        assert_eq!(s.classes[1], None);
        assert_eq!(s.before, [0, 0, 1, 1]);
    }

    #[test]
    fn virtual_neighbours_space_aligned_cells() {
        // The `&= b` cell of `a &= b`.
        let cell = [rel("="), ord('b')];
        assert_eq!(space(&cell, false, Some(Class::Ord), None).before, [1, 1]);
        // The `a =` cell of `a = & b`.
        let cell = [ord('a'), rel("=")];
        assert_eq!(space(&cell, false, None, Some(Class::Ord)).after, 1);
    }

    #[test]
    fn no_space_after_a_unary_sign() {
        // −∑ᵢ, not − ∑ᵢ.
        let row = [rel("="), bin('−'), Node::Atom(Atom::LargeOp('∑'))];
        assert_eq!(space(&row, false, None, None).before, [0, 1, 0]);
    }

    #[test]
    fn operators_with_limits_keep_a_column_before_delimiters() {
        let lim = Node::Scripts {
            base: Box::new(func("lim")),
            sub: Some(Box::new(ord('x'))),
            sup: None,
            limits: Limits::Display,
        };
        assert_eq!(space(&[lim, open()], false, None, None).before, [0, 1]);
        let sum = Node::Atom(Atom::LargeOp('∑'));
        assert_eq!(space(&[sum, open()], true, None, None).before, [0, 1]);
        assert_eq!(
            space(&[func("log"), open()], true, None, None).before,
            [0, 0]
        );
    }

    #[test]
    fn fences_space_like_their_delimiters() {
        let fenced = Node::Fenced {
            open: Some('('),
            close: Some(')'),
            body: Box::new(Node::Row(vec![ord('x')])),
        };
        assert_eq!(
            space(&[func("sin"), fenced.clone()], false, None, None).before,
            [0, 0]
        );
        assert_eq!(space(&[fenced, ord('y')], false, None, None).before, [0, 0]);
    }

    #[test]
    fn groups_are_ordinary() {
        let row = [ord('a'), Node::Row(vec![rel("=")]), ord('b')];
        assert_eq!(space(&row, false, None, None).before, [0, 0, 0]);
    }
}
