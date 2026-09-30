//! GitHub-compatible heading slugs.
//!
//! This follows github-slugger, which reproduces the anchors GitHub gives
//! headings: lowercase the text, remove every character that is not
//! alphabetic, a mark, a decimal digit, connector punctuation (`_`), `-` or a
//! space, then turn each space into `-`. Nothing is trimmed or collapsed, so
//! `Foo & Bar` becomes `foo--bar` and `🎉 Features` becomes `-features`.
//! Repeated slugs get `-1`, `-2`, … suffixes ([`Slugger`]).

use std::cmp::Ordering;
use std::collections::HashMap;

use super::slug_table::WORD_EXTRA;

/// The slug of a heading text, without de-duplication.
pub fn slug(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    for c in lower.chars() {
        if c == ' ' {
            out.push('-');
        } else if c == '-' || keeps(c) {
            out.push(c);
        }
    }
    out
}

/// Whether a character survives in a slug (besides `-` and space).
fn keeps(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    if c.is_alphabetic() {
        return true;
    }
    let cp = u32::from(c);
    WORD_EXTRA
        .binary_search_by(|&(lo, hi)| {
            if hi < cp {
                Ordering::Less
            } else if lo > cp {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// Generates unique slugs the way github-slugger does.
#[derive(Clone, Debug, Default)]
pub struct Slugger {
    occurrences: HashMap<String, u32>,
}

impl Slugger {
    /// The unique slug for a heading text.
    pub fn unique(&mut self, text: &str) -> String {
        let base = slug(text);
        let mut result = base.clone();
        while self.occurrences.contains_key(&result) {
            let n = self.occurrences.entry(base.clone()).or_insert(0);
            *n += 1;
            result = format!("{base}-{n}");
        }
        self.occurrences.insert(result.clone(), 0);
        result
    }

    /// Mark an explicit anchor as taken, so a later generated slug does not
    /// collide with it.
    pub fn reserve(&mut self, anchor: &str) {
        self.occurrences.entry(anchor.to_string()).or_insert(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_cases() {
        let cases = [
            ("Hello World", "hello-world"),
            ("foo & bar", "foo--bar"),
            ("foo!@#$%^&*()bar", "foobar"),
            ("C++ Programming", "c-programming"),
            ("snake_case and kebab-case", "snake_case-and-kebab-case"),
            ("1.0.0 Release", "100-release"),
            ("What's new?", "whats-new"),
            ("  spaced  out  ", "--spaced--out--"),
            ("`code` in heading", "code-in-heading"),
            ("E = mc²", "e--mc"),
            ("Tabs\tand\u{a0}NBSP", "tabsandnbsp"),
            ("", ""),
        ];
        for (text, want) in cases {
            assert_eq!(slug(text), want, "{text:?}");
        }
    }

    #[test]
    fn unicode_letters_and_marks() {
        assert_eq!(slug("Übung macht den Meister"), "übung-macht-den-meister");
        assert_eq!(slug("Привет non-latin 你好"), "привет-non-latin-你好");
        assert_eq!(slug("日本語のテキスト"), "日本語のテキスト");
        // A decomposed accent keeps its combining mark.
        assert_eq!(slug("Cafe\u{301}"), "cafe\u{301}");
        // Devanagari vowel signs are marks.
        assert_eq!(slug("हिन्दी"), "हिन्दी");
        // Arabic-Indic digits are decimal numbers.
        assert_eq!(slug("رقم ١٢٣"), "رقم-١٢٣");
        // Fullwidth underscore is connector punctuation.
        assert_eq!(slug("a＿b"), "a＿b");
    }

    #[test]
    fn emoji_and_symbols_are_removed() {
        assert_eq!(slug("🎉 Features"), "-features");
        assert_eq!(slug("Family 👨\u{200d}👩\u{200d}👧"), "family-");
        assert_eq!(slug("→ Next ←"), "-next-");
        assert_eq!(slug("Price: 5 €"), "price-5-");
        // Circled letters are alphabetic symbols and survive.
        assert_eq!(slug("Ⓐ"), "ⓐ");
    }

    #[test]
    fn duplicates_get_suffixes() {
        let mut s = Slugger::default();
        let got: Vec<String> = ["Foo", "foo", "Foo-1", "FOO", "Bar", "Foo 1"]
            .iter()
            .map(|t| s.unique(t))
            .collect();
        assert_eq!(got, ["foo", "foo-1", "foo-1-1", "foo-2", "bar", "foo-1-2"]);
    }

    #[test]
    fn reserved_anchors_are_skipped() {
        let mut s = Slugger::default();
        s.reserve("install");
        assert_eq!(s.unique("Install"), "install-1");
    }

    #[test]
    fn table_is_sorted_and_disjoint() {
        for w in WORD_EXTRA.windows(2) {
            assert!(w[0].0 <= w[0].1 && w[0].1 < w[1].0, "{w:?}");
        }
    }
}
