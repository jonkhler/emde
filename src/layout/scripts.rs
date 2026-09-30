//! Unicode super- and subscripts for `<sup>`/`<sub>` text and footnote
//! numbers.
//!
//! Mapping is all-or-nothing: text becomes Unicode scripts only when every
//! character has one (`x<sup>2</sup>` → `x²`, `H<sub>2</sub>O` → `H₂O`,
//! `1<sup>st</sup>` → `1ˢᵗ`); otherwise it stays as written. The tables
//! only use characters with good font coverage (the Superscripts and
//! Subscripts block, spacing modifier letters and phonetic extensions).

/// The superscript form of a character.
fn sup(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' | '−' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'a' => 'ᵃ',
        'b' => 'ᵇ',
        'c' => 'ᶜ',
        'd' => 'ᵈ',
        'e' => 'ᵉ',
        'f' => 'ᶠ',
        'g' => 'ᵍ',
        'h' => 'ʰ',
        'i' => 'ⁱ',
        'j' => 'ʲ',
        'k' => 'ᵏ',
        'l' => 'ˡ',
        'm' => 'ᵐ',
        'n' => 'ⁿ',
        'o' => 'ᵒ',
        'p' => 'ᵖ',
        'r' => 'ʳ',
        's' => 'ˢ',
        't' => 'ᵗ',
        'u' => 'ᵘ',
        'v' => 'ᵛ',
        'w' => 'ʷ',
        'x' => 'ˣ',
        'y' => 'ʸ',
        'z' => 'ᶻ',
        'A' => 'ᴬ',
        'B' => 'ᴮ',
        'D' => 'ᴰ',
        'E' => 'ᴱ',
        'G' => 'ᴳ',
        'H' => 'ᴴ',
        'I' => 'ᴵ',
        'J' => 'ᴶ',
        'K' => 'ᴷ',
        'L' => 'ᴸ',
        'M' => 'ᴹ',
        'N' => 'ᴺ',
        'O' => 'ᴼ',
        'P' => 'ᴾ',
        'R' => 'ᴿ',
        'T' => 'ᵀ',
        'U' => 'ᵁ',
        'W' => 'ᵂ',
        _ => return None,
    })
}

/// The subscript form of a character.
fn sub(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' | '−' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'v' => 'ᵥ',
        'x' => 'ₓ',
        _ => return None,
    })
}

fn map_all(s: &str, f: fn(char) -> Option<char>) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    s.chars().map(f).collect()
}

/// `s` in superscript characters, if every character has one.
pub(super) fn superscript(s: &str) -> Option<String> {
    map_all(s, sup)
}

/// `s` in subscript characters, if every character has one.
pub(super) fn subscript(s: &str) -> Option<String> {
    map_all(s, sub)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_and_signs() {
        assert_eq!(superscript("2").as_deref(), Some("²"));
        assert_eq!(superscript("n+1").as_deref(), Some("ⁿ⁺¹"));
        assert_eq!(superscript("-10").as_deref(), Some("⁻¹⁰"));
        assert_eq!(subscript("2").as_deref(), Some("₂"));
        assert_eq!(subscript("i=0").as_deref(), Some("ᵢ₌₀"));
    }

    #[test]
    fn ordinals() {
        for (s, want) in [("st", "ˢᵗ"), ("nd", "ⁿᵈ"), ("rd", "ʳᵈ"), ("th", "ᵗʰ")] {
            assert_eq!(superscript(s).as_deref(), Some(want));
        }
    }

    #[test]
    fn all_or_nothing() {
        assert_eq!(superscript("[1]"), None);
        assert_eq!(superscript("q"), None, "no superscript q in the safe set");
        assert_eq!(subscript("b"), None);
        assert_eq!(superscript(""), None);
        assert_eq!(subscript("x y"), None);
    }

    #[test]
    fn mapped_characters_are_one_column() {
        let all: String = ('!'..='~')
            .filter_map(sup)
            .chain(('!'..='~').filter_map(sub))
            .collect();
        for c in all.chars() {
            assert_eq!(
                crate::text::str_width(&c.to_string(), false),
                1,
                "{c:?} U+{:04X}",
                u32::from(c)
            );
        }
    }
}
