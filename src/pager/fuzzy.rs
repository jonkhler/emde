//! A small fzf-like fuzzy matcher for file paths.
//!
//! A query is split at spaces into terms that must all match (AND). A term
//! matches a path when its characters appear in it in order (a
//! subsequence); case is ignored unless the query has an uppercase letter
//! (smart case). Among the ways a term can match, the best one is found
//! with a small dynamic program (as fzf's v2 algorithm does) that rewards:
//!
//! * matches at the start of the path, after `/ _ - . space`, or at a
//!   camelCase or letter-to-digit boundary;
//! * runs of consecutive matches (a run keeps the bonus of its first
//!   character);
//! * matches inside the base name rather than the directories (strongly),
//!   and a term that is a prefix of the base name;
//!
//! and penalises gaps (more for opening one than for making it longer) and
//! long paths, a little. Before any of that, a 64-bit mask of the
//! characters a path contains ([`mask`], computed once per path) rejects
//! most paths that cannot match at all.

/// Points for every matched character.
const MATCH: i32 = 16;
/// Opening a gap between two matched characters.
const GAP_START: i32 = -3;
/// Every further character of a gap.
const GAP_EXTEND: i32 = -1;
/// A match at the start of the path or after a space.
const BONUS_WHITE: i32 = 12;
/// A match after `/` (a path component starts).
const BONUS_DELIMITER: i32 = 11;
/// A match after `_`, `-`, `.` or another non-word character.
const BONUS_BOUNDARY: i32 = 10;
/// A match at a camelCase or letter-to-digit boundary.
const BONUS_CAMEL: i32 = 9;
/// A match in the middle of a word that does not continue a run: initials
/// of words are what people type.
const SCATTERED: i32 = -4;
/// The least a character continuing a run of matches gets.
const BONUS_CONSECUTIVE: i32 = -(GAP_START + GAP_EXTEND);
/// The first character's bonus counts this many times.
const FIRST_MULTIPLIER: i32 = 2;
/// Every matched character inside the base name.
const BONUS_BASENAME: i32 = 8;
/// A term that is a prefix of the base name.
const BONUS_PREFIX: i32 = 24;
/// One point off per this many characters of path.
const LENGTH_DIVISOR: i32 = 16;
/// Scores below this are impossible.
const NONE: i32 = i32::MIN / 2;

/// The bit of the mask a (folded) character sets.
fn bit(c: char) -> u64 {
    let b = match c {
        'a'..='z' => c as u32 - 'a' as u32,
        '0'..='9' => 26 + (c as u32 - '0' as u32),
        c if c.is_ascii() => 36 + (c as u32 % 27),
        _ => 63,
    };
    1 << b
}

/// Lowercase `c` (the first character of its lowercase form).
fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// The characters `text` contains, folded, as a mask for [`Pattern`]'s
/// quick rejection.
pub(crate) fn mask(text: &str) -> u64 {
    if text.is_ascii() {
        text.bytes()
            .fold(0, |m, b| m | bit(char::from(b.to_ascii_lowercase())))
    } else {
        text.chars().fold(0, |m, c| m | bit(fold(c)))
    }
}

/// A match: its score and the byte offsets of the matched characters,
/// sorted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Match {
    pub(crate) score: i32,
    pub(crate) positions: Vec<u32>,
}

/// One way a term character can match: at `pos`, with the best score of
/// the term up to it, the bonus of the run it ends, and the cell of the
/// previous term character it came from.
#[derive(Clone, Copy, Debug)]
struct Cell {
    pos: u32,
    score: i32,
    run: i32,
    back: u32,
}

/// Reusable buffers for matching (no allocation per path).
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    folded_bytes: Vec<u8>,
    chars: Vec<char>,
    folded_chars: Vec<char>,
    work: Work,
}

/// The dynamic program's cells, row by row (one row per term character,
/// with a cell where that character can match only).
#[derive(Debug, Default)]
struct Work {
    found: Vec<u32>,
    cells: Vec<Cell>,
    rows: Vec<usize>,
}

/// A parsed query; see the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pattern {
    /// Its terms (folded when case is ignored).
    terms: Vec<Vec<char>>,
    /// The same as bytes, when they are all ASCII.
    ascii: Option<Vec<Vec<u8>>>,
    case_sensitive: bool,
    /// What a path must contain: [`mask`] of every term.
    mask: u64,
}

/// What sort of character `c` is, for the boundary bonuses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    White,
    Delimiter,
    NonWord,
    Lower,
    Upper,
    Digit,
    Other,
}

fn class(c: char) -> Class {
    match c {
        ' ' | '\t' => Class::White,
        '/' | ':' | ';' | ',' | '|' => Class::Delimiter,
        'a'..='z' => Class::Lower,
        'A'..='Z' => Class::Upper,
        '0'..='9' => Class::Digit,
        c if c.is_ascii() => Class::NonWord,
        c if c.is_lowercase() => Class::Lower,
        c if c.is_uppercase() => Class::Upper,
        c if c.is_alphanumeric() => Class::Other,
        _ => Class::NonWord,
    }
}

/// The bonus for a match on a `cur` character after a `prev` one.
fn bonus(prev: Class, cur: Class) -> i32 {
    match (prev, cur) {
        (_, Class::White | Class::Delimiter | Class::NonWord) => 0,
        (Class::White, _) => BONUS_WHITE,
        (Class::Delimiter, _) => BONUS_DELIMITER,
        (Class::NonWord, _) => BONUS_BOUNDARY,
        (Class::Lower, Class::Upper)
        | (Class::Lower | Class::Upper | Class::Other, Class::Digit) => BONUS_CAMEL,
        _ => 0,
    }
}

/// A unit of text: an ASCII byte (the fast path) or a character.
trait Unit: Copy + Eq {
    fn class(self) -> Class;
    /// The indexes of `needle` in `hay`, in order, into `out`.
    fn find(hay: &[Self], needle: Self, out: &mut Vec<u32>);
}

/// The class of every ASCII character.
const ASCII_CLASSES: [Class; 128] = {
    let mut t = [Class::NonWord; 128];
    let mut i = 0;
    while i < 128 {
        t[i] = match i as u8 {
            b' ' | b'\t' => Class::White,
            b'/' | b':' | b';' | b',' | b'|' => Class::Delimiter,
            b'a'..=b'z' => Class::Lower,
            b'A'..=b'Z' => Class::Upper,
            b'0'..=b'9' => Class::Digit,
            _ => Class::NonWord,
        };
        i += 1;
    }
    t
};

impl Unit for u8 {
    fn class(self) -> Class {
        ASCII_CLASSES
            .get(usize::from(self))
            .copied()
            .unwrap_or(Class::NonWord)
    }

    fn find(hay: &[u8], needle: u8, out: &mut Vec<u32>) {
        // Paths are short: a plain loop beats memchr's setup.
        for (i, &b) in hay.iter().enumerate() {
            if b == needle {
                out.push(i as u32);
            }
        }
    }
}

impl Unit for char {
    fn class(self) -> Class {
        class(self)
    }

    fn find(hay: &[char], needle: char, out: &mut Vec<u32>) {
        out.extend(
            hay.iter()
                .enumerate()
                .filter(|&(_, &c)| c == needle)
                .map(|(i, _)| u32::try_from(i).unwrap_or(u32::MAX)),
        );
    }
}

impl Pattern {
    /// The pattern for `query` (see the module docs).
    pub(crate) fn new(query: &str) -> Pattern {
        let case_sensitive = query.chars().any(char::is_uppercase);
        let terms: Vec<Vec<char>> = query
            .split_whitespace()
            .map(|t| {
                t.chars()
                    .map(|c| if case_sensitive { c } else { fold(c) })
                    .collect()
            })
            .collect();
        let ascii = terms.iter().flatten().all(char::is_ascii).then(|| {
            terms
                .iter()
                .map(|t| t.iter().map(|&c| c as u8).collect())
                .collect()
        });
        let mask = terms.iter().flatten().fold(0, |m, &c| m | bit(fold(c)));
        Pattern {
            terms,
            ascii,
            case_sensitive,
            mask,
        }
    }

    /// Whether the pattern has no terms (everything matches).
    pub(crate) fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// The score of `text` (whose base name starts at byte `base`; its
    /// [`mask`] is `text_mask`), or `None` when it does not match.
    pub(crate) fn score(
        &self,
        text: &str,
        text_mask: u64,
        base: usize,
        scratch: &mut Scratch,
    ) -> Option<i32> {
        if text_mask & self.mask != self.mask {
            return None;
        }
        self.run(text, base, scratch, false).map(|m| m.score)
    }

    /// [`Pattern::score`] with the positions of the matched characters.
    pub(crate) fn matches(&self, text: &str, base: usize, scratch: &mut Scratch) -> Option<Match> {
        if mask(text) & self.mask != self.mask {
            return None;
        }
        self.run(text, base, scratch, true)
    }

    fn run(&self, text: &str, base: usize, s: &mut Scratch, positions: bool) -> Option<Match> {
        if self.terms.is_empty() {
            return Some(Match::default());
        }
        let len = i32::try_from(text.len()).unwrap_or(i32::MAX);
        let mut out = Match {
            score: -(len / LENGTH_DIVISOR),
            positions: Vec::new(),
        };
        let mut add = |found: (i32, Vec<u32>)| {
            out.score = out.score.saturating_add(found.0);
            out.positions.extend(found.1);
        };
        match &self.ascii {
            Some(terms) if text.is_ascii() => {
                // Bytes are characters.
                let orig = text.as_bytes();
                s.folded_bytes.clear();
                s.folded_bytes.extend_from_slice(orig);
                if !self.case_sensitive {
                    s.folded_bytes.make_ascii_lowercase();
                }
                for term in terms {
                    add(best(
                        orig,
                        &s.folded_bytes,
                        term,
                        base,
                        &mut s.work,
                        positions,
                    )?);
                }
            }
            _ => {
                s.chars.clear();
                s.chars.extend(text.chars());
                s.folded_chars.clear();
                if self.case_sensitive {
                    s.folded_chars.extend_from_slice(&s.chars);
                } else {
                    s.folded_chars.extend(s.chars.iter().map(|&c| fold(c)));
                }
                let base = text.get(..base).map_or(0, |b| b.chars().count());
                for term in &self.terms {
                    add(best(
                        &s.chars,
                        &s.folded_chars,
                        term,
                        base,
                        &mut s.work,
                        positions,
                    )?);
                }
            }
        }
        if positions {
            out.positions.sort_unstable();
            out.positions.dedup();
            if !text.is_ascii() {
                // Character indexes to byte offsets.
                let offsets: Vec<u32> = text
                    .char_indices()
                    .map(|(b, _)| u32::try_from(b).unwrap_or(u32::MAX))
                    .collect();
                for p in &mut out.positions {
                    *p = offsets.get(*p as usize).copied().unwrap_or(u32::MAX);
                }
            }
        }
        Some(out)
    }
}

/// The bonus for a match at `i` of `text`.
fn bonus_at<T: Unit>(text: &[T], i: usize) -> i32 {
    let prev = match i.checked_sub(1).and_then(|k| text.get(k)) {
        Some(c) => c.class(),
        None => Class::White,
    };
    bonus(prev, text.get(i).map_or(Class::Other, |c| c.class()))
}

/// The best way `term` matches `text` (`folded` as compared; base name
/// from unit `base`): its score and, with `positions`, the indexes.
///
/// The dynamic program only has cells where a term character matches:
/// row `j` holds the places where `term[j]` can end the best match of
/// `term[..=j]`. A cell continues a run from the cell right before it in
/// the row above, or follows the best cell further left over a gap (the
/// gap's cost grows by [`GAP_EXTEND`] per character, so the best one is a
/// running maximum of `score - GAP_EXTEND * pos`).
fn best<T: Unit>(
    text: &[T],
    folded: &[T],
    term: &[T],
    base: usize,
    w: &mut Work,
    positions: bool,
) -> Option<(i32, Vec<u32>)> {
    let (first, last) = (*term.first()?, *term.last()?);
    // The window: from the first place the term can start to the last
    // place it can end.
    let lo = folded.iter().position(|&c| c == first)?;
    let hi = folded.iter().rposition(|&c| c == last)?;
    let window = folded.get(lo..=hi)?;
    let gain = |i: u32| {
        MATCH
            + if i as usize >= base {
                BONUS_BASENAME
            } else {
                0
            }
    };
    let Work { found, cells, rows } = w;
    cells.clear();
    rows.clear();
    for (j, &unit) in term.iter().enumerate() {
        found.clear();
        T::find(window, unit, found);
        let start = cells.len();
        rows.push(start);
        if j == 0 {
            for &r in found.iter() {
                let i = r + lo as u32;
                let own = bonus_at(text, i as usize);
                let first = if own == 0 {
                    SCATTERED
                } else {
                    own * FIRST_MULTIPLIER
                };
                cells.push(Cell {
                    pos: i,
                    score: gain(i) + first,
                    run: own,
                    back: u32::MAX,
                });
            }
            continue;
        }
        let prev_start = rows.get(j - 1).copied().unwrap_or(0);
        let mut k = prev_start;
        // The best cell of the row above at least two places left, as
        // `score - GAP_EXTEND * pos`, and which it is.
        let mut gap = (NONE, u32::MAX);
        for &r in found.iter() {
            let i = r + lo as u32;
            while let Some(c) = cells.get(k).filter(|c| k < start && c.pos + 2 <= i) {
                let v = c.score - GAP_EXTEND * c.pos as i32;
                if v > gap.0 {
                    gap = (v, k as u32);
                }
                k += 1;
            }
            let own = bonus_at(text, i as usize);
            let mut choice = (NONE, 0, u32::MAX);
            if let Some(c) = cells.get(k).filter(|c| k < start && c.pos + 1 == i) {
                let run = if own >= BONUS_BOUNDARY { own } else { c.run };
                let b = own.max(run).max(BONUS_CONSECUTIVE);
                choice = (c.score + gain(i) + b, run, k as u32);
            }
            if gap.0 > NONE {
                let lone = if own == 0 { SCATTERED } else { own };
                let g = gap.0 + GAP_START + GAP_EXTEND * (i as i32 - 2) + gain(i) + lone;
                if g > choice.0 {
                    choice = (g, own, gap.1);
                }
            }
            if choice.0 > NONE {
                cells.push(Cell {
                    pos: i,
                    score: choice.0,
                    run: choice.1,
                    back: choice.2,
                });
            }
        }
        if cells.len() == start {
            return None;
        }
    }
    let last_row = *rows.last()?;
    let (end, cell) = cells
        .iter()
        .enumerate()
        .skip(last_row)
        .max_by_key(|&(_, c)| (c.score, std::cmp::Reverse(c.pos)))?;
    let prefix = folded.get(base..).is_some_and(|b| b.starts_with(term));
    let score = cell.score + if prefix { BONUS_PREFIX } else { 0 };
    let mut at = Vec::new();
    if positions {
        let mut k = end;
        while let Some(c) = cells.get(k) {
            at.push(c.pos);
            k = c.back as usize;
        }
        at.reverse();
    }
    Some((score, at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The byte offset of `path`'s base name.
    fn base(path: &str) -> usize {
        path.rfind('/').map_or(0, |i| i + 1)
    }

    fn score(q: &str, path: &str) -> Option<i32> {
        Pattern::new(q).score(path, mask(path), base(path), &mut Scratch::default())
    }

    fn positions(q: &str, path: &str) -> Option<Vec<u32>> {
        Pattern::new(q)
            .matches(path, base(path), &mut Scratch::default())
            .map(|m| m.positions)
    }

    /// `paths` ordered best first for `q` (only those that match).
    fn ranked<'a>(q: &str, paths: &[&'a str]) -> Vec<&'a str> {
        let mut out: Vec<(i32, &str)> = paths
            .iter()
            .filter_map(|p| score(q, p).map(|s| (s, *p)))
            .collect();
        out.sort_by_key(|&(s, p)| (std::cmp::Reverse(s), p.len()));
        out.into_iter().map(|(_, p)| p).collect()
    }

    #[test]
    fn a_subsequence_is_required() {
        assert!(score("abc", "a/b/c.md").is_some());
        assert!(score("acb", "a/b/c.md").is_none());
        assert!(score("x", "a/b/c.md").is_none());
        assert!(score("", "anything").is_some(), "no terms: everything");
        assert!(score("aa", "a").is_none(), "each character once");
    }

    #[test]
    fn smart_case() {
        assert!(score("readme", "README.md").is_some());
        assert!(score("README", "README.md").is_some());
        assert!(
            score("Readme", "README.md").is_none(),
            "an uppercase letter: exact case"
        );
        assert!(score("Readme", "docs/Readme.md").is_some());
    }

    #[test]
    fn terms_must_all_match() {
        assert!(score("pager upd", "src/pager/update.rs").is_some());
        assert!(
            score("upd pager", "src/pager/update.rs").is_some(),
            "in any order"
        );
        assert!(score("pager zzz", "src/pager/update.rs").is_none());
        assert_eq!(positions("src rs", "src/x.rs"), Some(vec![0, 1, 2, 6, 7]));
    }

    #[test]
    fn positions_are_where_the_best_match_is() {
        // Word starts: `all`, `atom`, the `m` of `md` (not of `atom`), `results`.
        let p = "ala2_all_atom_md_results.md";
        assert_eq!(positions("aamr", p), Some(vec![5, 9, 14, 17]));
        assert_eq!(
            positions("upd", "src/pager/update.rs"),
            Some(vec![10, 11, 12])
        );
        // Byte offsets in text that is not ASCII.
        assert_eq!(positions("nb", "énb.md"), Some(vec![2, 3]));
    }

    #[test]
    fn boundaries_and_base_names_rank_first() {
        // Names from a real research repository.
        let decoys = [
            "additive_bias_results.md",
            "ala2_all_atom_encoder_results.md",
            "ala2_all_atom_gpu_results.md",
            "ala2_all_atom_md_protocol.md",
            "ala2_md_recovery_results.md",
            "ala2_mh_results.md",
            "annealed_target_results.md",
            "analysis/archive/model_runs.md",
            "data/alanine/amber_md/run.md",
            "ala2_all_atom_md_results.md",
            "ala2_all_atom_md_results/report-001/report.md",
        ];
        let got = ranked("aamr", &decoys);
        assert_eq!(got.first(), Some(&"ala2_all_atom_md_results.md"), "{got:?}");
        // The base name beats the directories.
        let got = ranked("update", &["update/notes.md", "src/pager/update.rs"]);
        assert_eq!(got, ["src/pager/update.rs", "update/notes.md"]);
        // A word start beats the middle of a word.
        let got = ranked("map", &["src/bitmap.rs", "src/map.rs"]);
        assert_eq!(got, ["src/map.rs", "src/bitmap.rs"]);
        // camelCase.
        let got = ranked("fb", &["src/fooBar.rs", "src/fxxbxx.rs"]);
        assert_eq!(got.first(), Some(&"src/fooBar.rs"));
        // Consecutive beats scattered.
        let got = ranked("conf", &["c/o/n/f.md", "config.md"]);
        assert_eq!(got.first(), Some(&"config.md"));
        // A prefix of the base name.
        let got = ranked("read", &["docs/thread_safety.md", "docs/README.md"]);
        assert_eq!(got.first(), Some(&"docs/README.md"));
        // The shorter path, all else equal.
        let got = ranked("x", &["a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/x.md", "x.md"]);
        assert_eq!(got.first(), Some(&"x.md"));
    }

    #[test]
    fn masks() {
        assert_eq!(mask("ab") & mask("BA"), mask("ab"));
        assert_ne!(mask("é") & bit('é'), 0);
        let p = Pattern::new("xyz");
        assert!(
            p.score("abc", mask("abc"), 0, &mut Scratch::default())
                .is_none()
        );
    }

    proptest! {
        #[test]
        fn matches_are_subsequences(text in "[a-cA-C/_.é ]{0,24}", query in "[a-cé]{1,4}") {
            let found = positions(&query, &text);
            let folded: Vec<char> = text.chars().map(fold).collect();
            let is_sub = {
                let mut it = folded.iter();
                query.chars().all(|q| it.any(|&c| c == q))
            };
            prop_assert_eq!(found.is_some(), is_sub);
            if let Some(at) = found {
                // Sorted, one per query character, each on that character.
                prop_assert!(at.windows(2).all(|w| w[0] < w[1]));
                prop_assert_eq!(at.len(), query.chars().count());
                for (&b, q) in at.iter().zip(query.chars()) {
                    let c = text.get(b as usize..).and_then(|s| s.chars().next());
                    prop_assert_eq!(c.map(fold), Some(q));
                }
                prop_assert_eq!(
                    score(&query, &text),
                    Pattern::new(&query)
                        .matches(&text, base(&text), &mut Scratch::default())
                        .map(|m| m.score)
                );
            }
        }
    }
}
