//! Character tables and lookups.
//!
//! The Unicode-derived tables (scripts, alphabets, compositions) are
//! generated into `src/gen/` ([`generated`](crate::generated)); this module
//! adds the small hand-written ones — accent marks, delimiter pieces, vulgar
//! fractions and `\overset` composites — and the lookup functions both
//! renderers use.

use crate::ScriptSet;
use crate::ast::Accent;
pub(crate) use crate::generated::alphabets::Alphabet;
use crate::generated::{compose, scripts};

/// Look `key` up in a table of pairs sorted by key.
fn lookup<K: Ord + Copy, V: Copy>(table: &[(K, V)], key: K) -> Option<V> {
    table
        .binary_search_by_key(&key, |&(k, _)| k)
        .ok()
        .and_then(|i| table.get(i))
        .map(|&(_, v)| v)
}

/// Superscript and subscript forms treat `-` like the minus sign `−`.
fn script_key(c: char) -> char {
    if c == '-' { '−' } else { c }
}

/// Characters that are already superscript-shaped and stand for themselves in
/// superscripts: primes, daggers, `*` and the degree sign (`90^\circ` is
/// `90°`).
fn superscript_symbol(c: char) -> Option<char> {
    match c {
        '′' | '″' | '‴' | '⁗' | '†' | '‡' | '°' | '*' => Some(c),
        '∘' => Some('°'),
        '∗' => Some('*'),
        _ => None,
    }
}

/// The superscript form of `c`, if `set` has one.
pub(crate) fn superscript(c: char, set: ScriptSet) -> Option<char> {
    let key = script_key(c);
    lookup(&scripts::SUPERSCRIPTS, key)
        .or_else(|| match set {
            ScriptSet::Full => lookup(&scripts::SUPERSCRIPTS_FULL, key),
            ScriptSet::Safe => None,
        })
        .or_else(|| superscript_symbol(c))
}

/// The subscript form of `c`, if `set` has one.
pub(crate) fn subscript(c: char, set: ScriptSet) -> Option<char> {
    let key = script_key(c);
    lookup(&scripts::SUBSCRIPTS, key).or_else(|| match set {
        ScriptSet::Full => lookup(&scripts::SUBSCRIPTS_FULL, key),
        ScriptSet::Safe => None,
    })
}

/// `c` in a mathematical alphabet, if Unicode has that letter.
pub(crate) fn styled(alphabet: Alphabet, c: char) -> Option<char> {
    lookup(alphabet.table(), c)
}

/// The plain character behind a math italic, bold or bold italic one (the
/// alphabets that [`Letters::UnicodeItalic`](crate::Letters::UnicodeItalic)
/// and [`Bold::Unicode`](crate::Bold::Unicode) produce), and whether it is
/// bold: `𝑥` is `('x', false)`, `𝐱` is `('x', true)`.
pub(crate) fn unstyled(c: char) -> Option<(char, bool)> {
    // Everything these alphabets hold is at ℎ U+210E or above.
    if u32::from(c) < 0x210E {
        return None;
    }
    [
        (Alphabet::Italic, false),
        (Alphabet::Bold, true),
        (Alphabet::BoldItalic, true),
    ]
    .into_iter()
    .find_map(|(alphabet, bold)| {
        alphabet
            .table()
            .iter()
            .find(|&&(_, styled)| styled == c)
            .map(|&(base, _)| (base, bold))
    })
}

/// The precomposed form of `base` followed by the combining `mark`.
pub(crate) fn compose(base: char, mark: char) -> Option<char> {
    compose::ACCENTED
        .binary_search_by_key(&(base, mark), |&(b, m, _)| (b, m))
        .ok()
        .and_then(|i| compose::ACCENTED.get(i))
        .map(|&(_, _, c)| c)
}

/// The precomposed negation of a relation (`=` → `≠`, `∈` → `∉`).
pub(crate) fn negate(c: char) -> Option<char> {
    lookup(&compose::NEGATED, c)
}

/// U+0338 COMBINING LONG SOLIDUS OVERLAY, for negations without a
/// precomposed form.
pub(crate) const NEGATION_MARK: char = '\u{338}';

/// The accent characters pulldown-latex reports, with their combining marks
/// (in source order, not sorted).
pub(crate) static ACCENT_MARKS: [(char, char); 12] = [
    ('´', '\u{301}'),  // \acute
    ('‾', '\u{304}'),  // \bar (U+0305 when over several characters)
    ('˘', '\u{306}'),  // \breve
    ('ˇ', '\u{30C}'),  // \check
    ('˙', '\u{307}'),  // \dot
    ('¨', '\u{308}'),  // \ddot
    ('`', '\u{300}'),  // \grave
    ('^', '\u{302}'),  // \hat
    ('~', '\u{303}'),  // \tilde
    ('→', '\u{20D7}'), // \vec
    ('˚', '\u{30A}'),  // \mathring
    ('_', '\u{332}'),  // \underline
];

/// Combining overline, for `\overline` and `\bar` over several characters.
const OVERLINE: char = '\u{305}';

/// The combining mark for `accent` on a base of `clusters` characters, if
/// there is one. A combining mark typed after a symbol is its own mark.
pub(crate) fn accent_mark(accent: Accent, clusters: usize) -> Option<char> {
    match (accent.ch, accent.under) {
        (c, _) if crate::width::is_zero_width(c) => Some(c),
        ('‾', false) if clusters > 1 => Some(OVERLINE),
        ('_', _) => Some('\u{332}'),
        ('→', true) => Some('\u{20EF}'),
        ('←', true) => Some('\u{20EE}'),
        ('↔', true) => Some('\u{34D}'),
        ('←', false) => Some('\u{20D6}'),
        ('↔', false) => Some('\u{20E1}'),
        ('↼', false) => Some('\u{20D0}'),
        ('⇀', false) => Some('\u{20D1}'),
        // No combining double arrow exists; `\Overrightarrow` uses the single one.
        ('⇒', false) => Some('\u{20D7}'),
        (c, false) => ACCENT_MARKS.iter().find(|&&(a, _)| a == c).map(|&(_, m)| m),
        _ => None,
    }
}

/// The row drawn over (or under) a base that is too wide or too tall for a
/// combining mark, `w` columns wide.
pub(crate) fn accent_row(accent: Accent, w: usize) -> Vec<char> {
    let w = w.max(1);
    let line = |fill: char, left: Option<char>, right: Option<char>| {
        let mut row = vec![fill; w];
        if let (Some(l), Some(first)) = (left, row.first_mut()) {
            *first = l;
        }
        if let (Some(r), Some(last)) = (right, row.last_mut()) {
            *last = r;
        }
        row
    };
    match accent.ch {
        '‾' => vec!['▁'; w],
        '_' => vec!['▔'; w],
        '→' => line('─', None, Some('→')),
        '←' => line('─', Some('←'), None),
        '↔' if w == 1 => vec!['↔'],
        '↔' => line('─', Some('←'), Some('→')),
        '⇒' => line('═', None, Some('⇒')),
        '⇀' => line('─', None, Some('⇀')),
        '↼' => line('─', Some('↼'), None),
        c => {
            let mut row = vec![' '; w];
            if let Some(mid) = row.get_mut((w - 1) / 2) {
                *mid = c;
            }
            row
        }
    }
}

/// The pieces of a tall delimiter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pieces {
    pub(crate) top: char,
    pub(crate) extender: char,
    pub(crate) bottom: char,
    /// The middle piece of a brace, on the middle row.
    pub(crate) middle: Option<char>,
    /// Top and bottom for a delimiter exactly two rows tall, when they differ
    /// from `top`/`bottom` (braces use `⎰⎱`).
    pub(crate) two: Option<(char, char)>,
}

impl Pieces {
    const fn new(top: char, extender: char, bottom: char) -> Pieces {
        Pieces {
            top,
            extender,
            bottom,
            middle: None,
            two: None,
        }
    }

    /// The column of characters for a delimiter `h` rows tall (`h ≥ 2`).
    pub(crate) fn column(&self, h: usize) -> Vec<char> {
        if let (2, Some((top, bottom))) = (h, self.two) {
            return vec![top, bottom];
        }
        let mut col = vec![self.extender; h];
        if let Some(first) = col.first_mut() {
            *first = self.top;
        }
        if let Some(last) = col.last_mut() {
            *last = self.bottom;
        }
        if let (Some(m), true) = (self.middle, h >= 3)
            && let Some(mid) = col.get_mut((h - 1) / 2)
        {
            *mid = m;
        }
        col
    }
}

/// The column of characters for `delim` drawn `h ≥ 2` rows tall, if it can
/// be drawn that way.
pub(crate) fn tall_delimiter(delim: char, h: usize) -> Option<Vec<char>> {
    // Angle brackets: the upper half slants one way, the lower half the
    // other, with the bracket itself on the middle row of an odd height.
    let angle = match delim {
        '⟨' => Some(('╱', '╲')),
        '⟩' => Some(('╲', '╱')),
        _ => None,
    };
    if let Some((upper, lower)) = angle {
        let column = (0..h)
            .map(|row| match (2 * row + 1).cmp(&h) {
                std::cmp::Ordering::Less => upper,
                std::cmp::Ordering::Equal => delim,
                std::cmp::Ordering::Greater => lower,
            })
            .collect();
        return Some(column);
    }
    pieces(delim).map(|p| p.column(h))
}

/// The pieces for a delimiter taller than one row, if it has any.
fn pieces(delim: char) -> Option<Pieces> {
    Some(match delim {
        '(' => Pieces::new('⎛', '⎜', '⎝'),
        ')' => Pieces::new('⎞', '⎟', '⎠'),
        '[' => Pieces::new('⎡', '⎢', '⎣'),
        ']' => Pieces::new('⎤', '⎥', '⎦'),
        '{' => Pieces {
            middle: Some('⎨'),
            two: Some(('⎰', '⎱')),
            ..Pieces::new('⎧', '⎪', '⎩')
        },
        '}' => Pieces {
            middle: Some('⎬'),
            two: Some(('⎱', '⎰')),
            ..Pieces::new('⎫', '⎪', '⎭')
        },
        '⌊' => Pieces::new('⎢', '⎢', '⎣'),
        '⌋' => Pieces::new('⎥', '⎥', '⎦'),
        '⌈' => Pieces::new('⎡', '⎢', '⎢'),
        '⌉' => Pieces::new('⎤', '⎥', '⎥'),
        '|' | '∣' => Pieces::new('│', '│', '│'),
        '‖' | '∥' => Pieces::new('║', '║', '║'),
        '↑' => Pieces::new('↑', '│', '│'),
        '↓' => Pieces::new('│', '│', '↓'),
        '↕' => Pieces::new('↑', '│', '↓'),
        '⇑' => Pieces::new('⇑', '║', '║'),
        '⇓' => Pieces::new('║', '║', '⇓'),
        '⇕' => Pieces::new('⇑', '║', '⇓'),
        _ => return None,
    })
}

/// The pieces of a tall integral sign: `⌠`, `⎮`, `⌡`.
pub(crate) const INTEGRAL: Pieces = Pieces::new('⌠', '⎮', '⌡');

/// How many integral signs a multiple integral stands for (`∬` is 2).
pub(crate) fn integral_count(op: char) -> Option<usize> {
    match op {
        '∫' => Some(1),
        '∬' => Some(2),
        '∭' => Some(3),
        '⨌' => Some(4),
        _ => None,
    }
}

/// A horizontal brace row, `w` columns wide: `╭─┴─╮` over a base, `╰─┬─╯`
/// under it (square brackets and parentheses have no tip).
pub(crate) fn brace_row(shape: crate::ast::BraceShape, over: bool, w: usize) -> Vec<char> {
    use crate::ast::BraceShape;
    let (left, right, tip) = match (shape, over) {
        (BraceShape::Brace, true) => ('╭', '╮', Some('┴')),
        (BraceShape::Brace, false) => ('╰', '╯', Some('┬')),
        (BraceShape::Bracket, true) => ('┌', '┐', None),
        (BraceShape::Bracket, false) => ('└', '┘', None),
        (BraceShape::Paren, true) => ('╭', '╮', None),
        (BraceShape::Paren, false) => ('╰', '╯', None),
    };
    let w = w.max(1);
    if w == 1 {
        return vec![tip.unwrap_or('─')];
    }
    let mut row = vec!['─'; w];
    if let Some(first) = row.first_mut() {
        *first = left;
    }
    if let Some(last) = row.last_mut() {
        *last = right;
    }
    if let (Some(t), true) = (tip, w >= 3)
        && let Some(mid) = row.get_mut((w - 1) / 2)
    {
        *mid = t;
    }
    row
}

/// The 19 vulgar fraction characters, by `(numerator, denominator)`.
pub(crate) static VULGAR_FRACTIONS: [((u8, u8), char); 19] = [
    ((0, 3), '↉'),
    ((1, 2), '½'),
    ((1, 3), '⅓'),
    ((1, 4), '¼'),
    ((1, 5), '⅕'),
    ((1, 6), '⅙'),
    ((1, 7), '⅐'),
    ((1, 8), '⅛'),
    ((1, 9), '⅑'),
    ((1, 10), '⅒'),
    ((2, 3), '⅔'),
    ((2, 5), '⅖'),
    ((3, 4), '¾'),
    ((3, 5), '⅗'),
    ((3, 8), '⅜'),
    ((4, 5), '⅘'),
    ((5, 6), '⅚'),
    ((5, 8), '⅝'),
    ((7, 8), '⅞'),
];

/// U+215F FRACTION NUMERATOR ONE, written before a denominator that has no
/// vulgar fraction (`⅟16`).
pub(crate) const NUMERATOR_ONE: char = '⅟';

/// The vulgar fraction for the digit strings `num`/`den`.
pub(crate) fn vulgar_fraction(num: &str, den: &str) -> Option<char> {
    let n: u8 = num.parse().ok()?;
    let d: u8 = den.parse().ok()?;
    lookup(&VULGAR_FRACTIONS, (n, d))
}

/// `\overset` composites: the text set over a relation, the relation, and
/// the single character that means both.
pub(crate) static OVERSET_COMPOSITES: [(&str, char, char); 9] = [
    ("def", '=', '≝'),
    ("?", '=', '≟'),
    ("△", '=', '≜'),
    ("Δ", '=', '≜'),
    ("∘", '=', '≗'),
    ("⋆", '=', '≛'),
    ("m", '=', '≞'),
    ("∧", '=', '≙'),
    ("∨", '=', '≚'),
];

/// The composite for `over` set over the relation `base`.
pub(crate) fn overset_composite(over: &str, base: &str) -> Option<char> {
    let mut chars = base.chars();
    let (Some(b), None) = (chars.next(), chars.next()) else {
        return None;
    };
    OVERSET_COMPOSITES
        .iter()
        .find(|&&(o, r, _)| o == over && r == b)
        .map(|&(_, _, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::BraceShape;

    fn sorted_unique<K: Ord, V>(table: &[(K, V)]) -> bool {
        table.windows(2).all(|w| w[0].0 < w[1].0)
    }

    #[test]
    fn generated_tables_are_sorted_without_duplicates() {
        assert!(sorted_unique(&scripts::SUPERSCRIPTS));
        assert!(sorted_unique(&scripts::SUPERSCRIPTS_FULL));
        assert!(sorted_unique(&scripts::SUBSCRIPTS));
        assert!(sorted_unique(&compose::NEGATED));
        assert!(
            compose::ACCENTED
                .windows(2)
                .all(|w| (w[0].0, w[0].1) < (w[1].0, w[1].1))
        );
        for alphabet in Alphabet::ALL {
            assert!(sorted_unique(alphabet.table()), "{alphabet:?}");
        }
        let mut vulgar = VULGAR_FRACTIONS.to_vec();
        vulgar.sort_unstable();
        assert_eq!(vulgar, VULGAR_FRACTIONS);
    }

    #[test]
    fn script_set_sizes() {
        // 68 safe superscripts (digits, + − = ( ), 25 lowercase letters, 19
        // capitals, 6 Greek letters and 3 look-alikes); ~40 subscripts.
        assert_eq!(scripts::SUPERSCRIPTS.len(), 68);
        assert_eq!(scripts::SUBSCRIPTS.len(), 37);
        assert_eq!(superscript('q', ScriptSet::Safe), None);
        assert_eq!(superscript('q', ScriptSet::Full), Some('𐞥'));
        assert_eq!(superscript('S', ScriptSet::Full), Some('꟱'));
        assert_eq!(superscript('X', ScriptSet::Full), None);
    }

    #[test]
    fn script_lookups() {
        assert_eq!(superscript('2', ScriptSet::Safe), Some('²'));
        assert_eq!(superscript('-', ScriptSet::Safe), Some('⁻'));
        assert_eq!(superscript('−', ScriptSet::Safe), Some('⁻'));
        assert_eq!(superscript('α', ScriptSet::Safe), Some('ᵅ'));
        assert_eq!(superscript('a', ScriptSet::Safe), Some('ᵃ'));
        assert_eq!(superscript('∘', ScriptSet::Safe), Some('°'));
        assert_eq!(superscript('′', ScriptSet::Safe), Some('′'));
        assert_eq!(subscript('1', ScriptSet::Safe), Some('₁'));
        assert_eq!(subscript('x', ScriptSet::Safe), Some('ₓ'));
        assert_eq!(subscript('y', ScriptSet::Full), None);
        assert_eq!(subscript('∘', ScriptSet::Safe), None);
    }

    #[test]
    fn alphabets_fill_the_24_letterlike_holes() {
        let holes = [
            (Alphabet::Italic, 'h', 'ℎ'),
            (Alphabet::Script, 'B', 'ℬ'),
            (Alphabet::Script, 'E', 'ℰ'),
            (Alphabet::Script, 'F', 'ℱ'),
            (Alphabet::Script, 'H', 'ℋ'),
            (Alphabet::Script, 'I', 'ℐ'),
            (Alphabet::Script, 'L', 'ℒ'),
            (Alphabet::Script, 'M', 'ℳ'),
            (Alphabet::Script, 'R', 'ℛ'),
            (Alphabet::Script, 'e', 'ℯ'),
            (Alphabet::Script, 'g', 'ℊ'),
            (Alphabet::Script, 'o', 'ℴ'),
            (Alphabet::Fraktur, 'C', 'ℭ'),
            (Alphabet::Fraktur, 'H', 'ℌ'),
            (Alphabet::Fraktur, 'I', 'ℑ'),
            (Alphabet::Fraktur, 'R', 'ℜ'),
            (Alphabet::Fraktur, 'Z', 'ℨ'),
            (Alphabet::DoubleStruck, 'C', 'ℂ'),
            (Alphabet::DoubleStruck, 'H', 'ℍ'),
            (Alphabet::DoubleStruck, 'N', 'ℕ'),
            (Alphabet::DoubleStruck, 'P', 'ℙ'),
            (Alphabet::DoubleStruck, 'Q', 'ℚ'),
            (Alphabet::DoubleStruck, 'R', 'ℝ'),
            (Alphabet::DoubleStruck, 'Z', 'ℤ'),
        ];
        for (alphabet, base, want) in holes {
            assert_eq!(styled(alphabet, base), Some(want), "{alphabet:?} {base}");
        }
        assert_eq!(styled(Alphabet::Script, 'l'), Some('𝓁'));
        assert_eq!(styled(Alphabet::DoubleStruck, 'A'), Some('𝔸'));
        assert_eq!(styled(Alphabet::DoubleStruck, '1'), Some('𝟙'));
        assert_eq!(styled(Alphabet::DoubleStruck, 'Γ'), Some('ℾ'));
        assert_eq!(styled(Alphabet::Bold, 'x'), Some('𝐱'));
        assert_eq!(styled(Alphabet::Bold, '∇'), Some('𝛁'));
        assert_eq!(styled(Alphabet::Italic, 'α'), Some('𝛼'));
        assert_eq!(styled(Alphabet::DoubleStruckItalic, 'd'), Some('ⅆ'));
        assert_eq!(styled(Alphabet::DoubleStruckItalic, 'x'), None);
        assert_eq!(styled(Alphabet::Fraktur, '1'), None);
        // Every Latin letter exists in every full alphabet.
        for alphabet in Alphabet::ALL {
            if alphabet == Alphabet::DoubleStruckItalic {
                continue;
            }
            for c in ('A'..='Z').chain('a'..='z') {
                assert!(styled(alphabet, c).is_some(), "{alphabet:?} {c}");
            }
        }
        assert_eq!(Alphabet::ALL.len(), 9);
    }

    #[test]
    fn unstyled_reverses_the_option_alphabets() {
        assert_eq!(unstyled('𝑥'), Some(('x', false)));
        assert_eq!(unstyled('ℎ'), Some(('h', false)));
        assert_eq!(unstyled('𝛼'), Some(('α', false)));
        assert_eq!(unstyled('𝐱'), Some(('x', true)));
        assert_eq!(unstyled('𝟐'), Some(('2', true)));
        assert_eq!(unstyled('𝒙'), Some(('x', true)));
        // Font alphabets are not options: they have no scripts.
        assert_eq!(unstyled('ℝ'), None);
        assert_eq!(unstyled('𝒜'), None);
        assert_eq!(unstyled('x'), None);
        assert_eq!(unstyled('→'), None);
    }

    #[test]
    fn compositions() {
        assert_eq!(compose('a', '\u{302}'), Some('â'));
        assert_eq!(compose('x', '\u{307}'), Some('ẋ'));
        assert_eq!(compose('x', '\u{302}'), None);
        assert_eq!(compose('α', '\u{301}'), Some('ά'));
        assert_eq!(negate('='), Some('≠'));
        assert_eq!(negate('∈'), Some('∉'));
        assert_eq!(negate('⪯'), None);
        // U+2ADC FORKING is a composition exclusion.
        assert_eq!(negate('⫝'), None);
        assert_eq!(compose::NEGATED.len(), 44);
    }

    #[test]
    fn accent_marks_are_covered_by_the_generator() {
        let accents = ACCENT_MARKS
            .iter()
            .map(|&(ch, _)| Accent {
                ch,
                wide: false,
                under: ch == '_',
            })
            .chain("→←↔↼⇀".chars().flat_map(|ch| {
                [false, true].map(|under| Accent {
                    ch,
                    wide: true,
                    under,
                })
            }));
        for accent in accents {
            for n in [1, 2] {
                if let Some(mark) = accent_mark(accent, n) {
                    assert!(
                        compose::COMPOSING_MARKS.contains(&mark),
                        "{accent:?} → U+{:04X}",
                        mark as u32
                    );
                }
            }
        }
        assert_eq!(
            accent_mark(
                Accent {
                    ch: '‾',
                    wide: false,
                    under: false
                },
                2
            ),
            Some('\u{305}')
        );
    }

    #[test]
    fn delimiter_columns() {
        let col = |d, h| tall_delimiter(d, h).unwrap();
        assert_eq!(col('(', 2), ['⎛', '⎝']);
        assert_eq!(col('(', 4), ['⎛', '⎜', '⎜', '⎝']);
        assert_eq!(col('{', 2), ['⎰', '⎱']);
        assert_eq!(col('{', 3), ['⎧', '⎨', '⎩']);
        assert_eq!(col('{', 5), ['⎧', '⎪', '⎨', '⎪', '⎩']);
        assert_eq!(col('}', 2), ['⎱', '⎰']);
        assert_eq!(col('⌊', 3), ['⎢', '⎢', '⎣']);
        assert_eq!(col('⌉', 2), ['⎤', '⎥']);
        assert_eq!(col('|', 3), ['│', '│', '│']);
        assert_eq!(col('⟨', 3), ['╱', '⟨', '╲']);
        assert_eq!(col('⟨', 4), ['╱', '╱', '╲', '╲']);
        assert_eq!(col('⟩', 2), ['╲', '╱']);
        assert_eq!(tall_delimiter('x', 3), None);
        assert_eq!(INTEGRAL.column(3), ['⌠', '⎮', '⌡']);
    }

    #[test]
    fn braces_and_accent_rows() {
        assert_eq!(
            brace_row(BraceShape::Brace, true, 5),
            ['╭', '─', '┴', '─', '╮']
        );
        assert_eq!(brace_row(BraceShape::Brace, false, 4), ['╰', '┬', '─', '╯']);
        assert_eq!(brace_row(BraceShape::Bracket, true, 3), ['┌', '─', '┐']);
        assert_eq!(brace_row(BraceShape::Brace, true, 1), ['┴']);
        let vec = Accent {
            ch: '→',
            wide: true,
            under: false,
        };
        assert_eq!(accent_row(vec, 3), ['─', '─', '→']);
        let hat = Accent {
            ch: '^',
            wide: true,
            under: false,
        };
        assert_eq!(accent_row(hat, 3), [' ', '^', ' ']);
    }

    #[test]
    fn fractions_and_composites() {
        assert_eq!(vulgar_fraction("1", "2"), Some('½'));
        assert_eq!(vulgar_fraction("7", "8"), Some('⅞'));
        assert_eq!(vulgar_fraction("0", "3"), Some('↉'));
        assert_eq!(vulgar_fraction("2", "4"), None);
        assert_eq!(vulgar_fraction("1", "300"), None);
        assert_eq!(overset_composite("def", "="), Some('≝'));
        assert_eq!(overset_composite("?", "="), Some('≟'));
        assert_eq!(overset_composite("△", "="), Some('≜'));
        assert_eq!(overset_composite("def", "≤"), None);
    }
}
