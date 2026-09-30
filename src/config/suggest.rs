//! "Did you mean …?" suggestions for misspelt keys and values.

/// Largest edit distance for which a suggestion is offered.
pub(crate) const MAX_DISTANCE: usize = 2;

/// Case-insensitive optimal string alignment distance: the Levenshtein
/// distance where swapping two adjacent characters also costs one edit.
pub(crate) fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().flat_map(char::to_lowercase).collect();
    let b: Vec<char> = b.chars().flat_map(char::to_lowercase).collect();
    // Three rolling rows: two back (for transpositions), previous, current.
    let mut two_back: Vec<usize> = vec![0; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let mut d = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
            if i > 0 && j > 0 && a.get(i - 1) == Some(&cb) && b.get(j - 1) == Some(&ca) {
                d = d.min(two_back[j - 1] + 1);
            }
            cur[j + 1] = d;
        }
        std::mem::swap(&mut two_back, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The candidate closest to `word`, if it is within [`MAX_DISTANCE`] edits
/// and closer than rewriting the whole word. Ties go to the earlier candidate.
pub(crate) fn did_you_mean<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let len = word.chars().count();
    // Lengths as `distance` compares the words (lowercased).
    let lower_len = |s: &str| s.chars().flat_map(char::to_lowercase).count();
    let word_len = lower_len(word);
    let mut best: Option<(usize, &'a str)> = None;
    for cand in candidates {
        // Words whose lengths differ by more than the limit are further
        // apart than it: skip them without the quadratic distance.
        if cand == word || lower_len(cand).abs_diff(word_len) > MAX_DISTANCE {
            continue;
        }
        let d = distance(word, cand);
        if d <= MAX_DISTANCE && d < len.max(1) && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, cand));
        }
    }
    best.map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances() {
        assert_eq!(distance("", ""), 0);
        assert_eq!(distance("abc", ""), 3);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("max_widht", "max_width"), 1, "transposition");
        assert_eq!(distance("Render", "render"), 0, "case-insensitive");
        assert_eq!(distance("rendr", "render"), 1);
        assert_eq!(distance("ab", "ba"), 1);
        assert_eq!(distance("ca", "abc"), 3);
        assert_eq!(
            distance("héllo", "hello"),
            1,
            "counts characters, not bytes"
        );
    }

    #[test]
    fn suggestions() {
        let keys = ["max_width", "margin", "align", "width"];
        assert_eq!(did_you_mean("max_widht", keys), Some("max_width"));
        assert_eq!(did_you_mean("allign", keys), Some("align"));
        assert_eq!(did_you_mean("colour", keys), None);
        // Nearest wins; ties keep candidate order.
        assert_eq!(did_you_mean("h7", ["h1", "h2"]), Some("h1"));
        assert_eq!(did_you_mean("widht", keys), Some("width"));
        // Very short words need a closer match than a full rewrite.
        assert_eq!(did_you_mean("ab", ["fg"]), None);
        assert_eq!(did_you_mean("gb", ["bg"]), Some("bg"));
        // An exact match is not a suggestion.
        assert_eq!(did_you_mean("align", ["align"]), None);
        // The length filter compares lowercased lengths, as `distance` does.
        assert_eq!(did_you_mean("MAX_WIDHT", keys), Some("max_width"));
        assert_eq!(did_you_mean("abc", ["abcdef", "abcde"]), Some("abcde"));
    }

    /// The length filter never hides a candidate `distance` would accept.
    #[test]
    fn length_filter_agrees_with_distance() {
        let words = [
            "", "a", "ab", "abc", "abcd", "abcde", "İi", "ǅx", "straße", "STRASSE",
        ];
        for w in words {
            for c in words {
                let expected = w != c
                    && distance(w, c) <= MAX_DISTANCE
                    && distance(w, c) < w.chars().count().max(1);
                assert_eq!(did_you_mean(w, [c]).is_some(), expected, "{w:?} {c:?}");
            }
        }
    }
}
