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
        text.bytes().fold(0, |m, b| m | bit(char::from(b.to_ascii_lowercase())))
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

/// Reusable buffers for matching (no allocation per path).
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    text: Vec<char>,
    folded: Vec<char>,
    bonus: Vec<i32>,
    prev: Vec<(i32, i32)>,
    cur: Vec<(i32, i32)>,
    /// For each term character and window position: where the previous
    /// term character matched (`u32::MAX`: no match here).
    back: Vec<u32>,
}

/// A parsed query; see the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pattern {
    /// Its terms (folded when case is ignored).
    terms: Vec<Vec<char>>,
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
        (Class::Lower, Class::Upper) | (Class::Lower | Class::Upper | Class::Other, Class::Digit) => {
            BONUS_CAMEL
        }
        _ => 0,
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
        let mask = terms
            .iter()
            .flatten()
            .fold(0, |m, &c| m | bit(fold(c)));
        Pattern {
            terms,
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
        s.text.clear();
        s.text.extend(text.chars());
        s.folded.clear();
        if self.case_sensitive {
            s.folded.extend_from_slice(&s.text);
        } else {
            s.folded.extend(s.text.iter().map(|&c| fold(c)));
        }
        let base = text.get(..base).map_or(0, |b| b.chars().count());
        let len = i32::try_from(s.text.len()).unwrap_or(i32::MAX);
        let mut out = Match {
            score: -(len / LENGTH_DIVISOR),
            positions: Vec::new(),
        };
        for t in 0..self.terms.len() {
            let term = self.terms.get(t)?;
            let (score, at) = best(term, base, s, positions)?;
            out.score = out.score.saturating_add(score);
            out.positions.extend(at);
        }
        if positions {
            // Character indexes to byte offsets.
            out.positions.sort_unstable();
            out.positions.dedup();
            if !text.is_ascii() {
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

/// The best way `term` matches the text in `s` (base name from character
/// `base`): its score and, with `positions`, the character indexes.
fn best(term: &[char], base: usize, s: &mut Scratch, positions: bool) -> Option<(i32, Vec<u32>)> {
    let (first, last) = (*term.first()?, *term.last()?);
    let f = &s.folded;
    // The window: from the first place the term can start to the last
    // place it can end; and it must be a subsequence at all.
    let lo = f.iter().position(|&c| c == first)?;
    let mut k = 0;
    for &c in f.get(lo..)? {
        if term.get(k) == Some(&c) {
            k += 1;
            if k == term.len() {
                break;
            }
        }
    }
    if k < term.len() {
        return None;
    }
    let hi = f.iter().rposition(|&c| c == last)?;
    let w = hi + 1 - lo;
    s.bonus.clear();
    s.bonus.extend((lo..=hi).map(|i| {
        let prev = if i == 0 {
            Class::White
        } else {
            s.text.get(i - 1).map_or(Class::White, |&c| class(c))
        };
        bonus(prev, s.text.get(i).map_or(Class::Other, |&c| class(c)))
    }));
    s.prev.clear();
    s.prev.resize(w, (NONE, 0));
    s.cur.clear();
    s.cur.resize(w, (NONE, 0));
    if positions {
        s.back.clear();
        s.back.resize(w * term.len(), u32::MAX);
    }
    let gain = |i: usize| MATCH + if lo + i >= base { BONUS_BASENAME } else { 0 };
    for (j, &pc) in term.iter().enumerate() {
        // The best score arriving at `i` over a gap from the previous row,
        // and where that came from.
        let mut gap = (NONE, u32::MAX);
        for i in 0..w {
            if j > 0 && i >= 2 {
                let from = s.prev.get(i - 2).map_or(NONE, |p| p.0);
                let extended = gap.0.saturating_add(GAP_EXTEND);
                let opened = from.saturating_add(GAP_START);
                gap = if opened >= extended {
                    (opened, u32::try_from(i - 2).unwrap_or(u32::MAX))
                } else {
                    (extended, gap.1)
                };
            }
            let cell = &mut s.cur[i];
            *cell = (NONE, 0);
            if f.get(lo + i) != Some(&pc) {
                continue;
            }
            let own = s.bonus.get(i).copied().unwrap_or(0);
            let lone = if own == 0 { SCATTERED } else { own };
            if j == 0 {
                let first = if own == 0 { SCATTERED } else { own * FIRST_MULTIPLIER };
                *cell = (gain(i) + first, own);
                continue;
            }
            // A run: the character after the previous one.
            let mut choice = (NONE, 0, u32::MAX);
            if let Some(&(p, run)) = i.checked_sub(1).and_then(|k| s.prev.get(k))
                && p > NONE
            {
                let run = if own >= BONUS_BOUNDARY { own } else { run };
                let b = own.max(run).max(BONUS_CONSECUTIVE);
                choice = (p + gain(i) + b, run, u32::try_from(i - 1).unwrap_or(u32::MAX));
            }
            if gap.0 > NONE && gap.0 + gain(i) + lone > choice.0 {
                choice = (gap.0 + gain(i) + lone, own, gap.1);
            }
            *cell = (choice.0, choice.1);
            if positions && let Some(b) = s.back.get_mut(j * w + i) {
                *b = choice.2;
            }
        }
        std::mem::swap(&mut s.prev, &mut s.cur);
    }
    let (end, score) = s
        .prev
        .iter()
        .enumerate()
        .max_by_key(|&(i, &(score, _))| (score, std::cmp::Reverse(i)))
        .map(|(i, &(score, _))| (i, score))?;
    if score <= NONE {
        return None;
    }
    let prefix = f.get(base..).is_some_and(|b| b.starts_with(term));
    let score = score + if prefix { BONUS_PREFIX } else { 0 };
    let mut at = Vec::new();
    if positions {
        let mut i = end;
        for j in (0..term.len()).rev() {
            at.push(u32::try_from(lo + i).unwrap_or(u32::MAX));
            let b = s.back.get(j * w + i).copied().unwrap_or(u32::MAX);
            if j > 0 {
                i = b as usize;
            }
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
        assert!(score("Readme", "README.md").is_none(), "an uppercase letter: exact case");
        assert!(score("Readme", "docs/Readme.md").is_some());
    }

    #[test]
    fn terms_must_all_match() {
        assert!(score("pager upd", "src/pager/update.rs").is_some());
        assert!(score("upd pager", "src/pager/update.rs").is_some(), "in any order");
        assert!(score("pager zzz", "src/pager/update.rs").is_none());
        assert_eq!(
            positions("src rs", "src/x.rs"),
            Some(vec![0, 1, 2, 6, 7])
        );
    }

    #[test]
    fn positions_are_where_the_best_match_is() {
        // Word starts: `all`, `atom`, the `m` of `md` (not of `atom`), `results`.
        let p = "ala2_all_atom_md_results.md";
        assert_eq!(positions("aamr", p), Some(vec![5, 9, 14, 17]));
        assert_eq!(positions("upd", "src/pager/update.rs"), Some(vec![10, 11, 12]));
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
        assert!(p.score("abc", mask("abc"), 0, &mut Scratch::default()).is_none());
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
