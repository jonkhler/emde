//! UAX #14 line break opportunities for printable ASCII, fast.
//!
//! [`unicode_linebreak::linebreaks`] decodes every character, looks up its
//! class in a trie and steps a pair-table state machine through a chain of
//! iterators: about 6–7 ns per byte, half the cost of laying out prose. For
//! printable ASCII (`0x20..=0x7E`, the bulk of real documents) the rules of
//! UAX #14 (Unicode 15.0, as the crate implements them) reduce to a table of
//! 14 classes and a two-part state: the class of the last non-space
//! character and whether spaces followed it. `ascii_breaks` implements
//! exactly that; tests compare it with the crate on every class sequence up
//! to length 6 and on random text, so the two never disagree.
//!
//! The rules involved, by UAX #14 number:
//!
//! * LB7 `× SP` — never before a space; LB18 `SP ÷` — after spaces, except
//!   LB13 (`× CL`, `× CP`, `× EX`, `× IS`, `× SY` even after spaces), LB14
//!   (`OP SP* ×`) and LB15 (`QU SP* × OP`);
//! * between two non-spaces: LB13, LB14, LB15, LB19 (`× QU`, `QU ×`), LB21
//!   (`× BA`, `× HY`), LB23 (letters and digits), LB24 (prefix/postfix and
//!   letters), LB25 (numbers), LB28 (`AL × AL`), LB29 (`IS × AL`), LB30
//!   (`(AL|NU) × OP`, `CP × (AL|NU)`), else LB31 `÷`.

/// Line break classes of printable ASCII characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Class {
    /// Letters and ordinary symbols.
    Al,
    /// Digits.
    Nu,
    /// Space.
    Sp,
    /// `, . : ;`
    Is,
    /// `! ?`
    Ex,
    /// `" '`
    Qu,
    /// `( [ {`
    Op,
    /// `) ]`
    Cp,
    /// `}`
    Cl,
    /// `-`
    Hy,
    /// `|`
    Ba,
    /// `/`
    Sy,
    /// `$ + \`
    Pr,
    /// `%`
    Po,
}

use Class::{Al, Ba, Cl, Cp, Ex, Hy, Is, Nu, Op, Po, Pr, Qu, Sp, Sy};

/// The class of a printable ASCII byte (anything else is treated as `Al`;
/// callers only pass printable ASCII).
const fn class(b: u8) -> Class {
    match b {
        b' ' => Sp,
        b'0'..=b'9' => Nu,
        b',' | b'.' | b':' | b';' => Is,
        b'!' | b'?' => Ex,
        b'"' | b'\'' => Qu,
        b'(' | b'[' | b'{' => Op,
        b')' | b']' => Cp,
        b'}' => Cl,
        b'-' => Hy,
        b'|' => Ba,
        b'/' => Sy,
        b'$' | b'+' | b'\\' => Pr,
        b'%' => Po,
        _ => Al,
    }
}

/// Classes by byte.
static CLASSES: [Class; 128] = {
    let mut t = [Al; 128];
    let mut b = 0;
    while b < 128 {
        t[b] = class(b as u8);
        b += 1;
    }
    t
};

/// Whether a break is prohibited between two adjacent non-space classes.
const fn no_break(before: Class, after: Class) -> bool {
    match (before, after) {
        // LB13: × CL, × CP, × EX, × IS, × SY
        (_, Cl | Cp | Ex | Is | Sy) => true,
        // LB14: OP ×
        (Op, _) => true,
        // LB19: × QU, QU × (LB15 QU × OP is included)
        (_, Qu) | (Qu, _) => true,
        // LB21: × BA, × HY
        (_, Ba | Hy) => true,
        // LB23: AL × NU, NU × AL
        (Al, Nu) | (Nu, Al) => true,
        // LB24: (PR | PO) × AL, AL × (PR | PO)
        (Pr | Po, Al) | (Al, Pr | Po) => true,
        // LB25: numbers
        (Cl | Cp, Po | Pr) | (Nu, Po | Pr) | (Po | Pr, Op | Nu) | (Hy | Is | Nu | Sy, Nu) => true,
        // LB28: AL × AL; LB29: IS × AL
        (Al | Is, Al) => true,
        // LB30: (AL | NU) × OP, CP × (AL | NU)
        (Al | Nu, Op) | (Cp, Al | Nu) => true,
        // LB31: break everywhere else.
        _ => false,
    }
}

/// Whether a break is prohibited before `after` when spaces follow
/// `before` (`None`: the start of the text).
const fn no_break_after_spaces(before: Option<Class>, after: Class) -> bool {
    match (before, after) {
        // LB13 holds even after spaces.
        (_, Cl | Cp | Ex | Is | Sy) => true,
        // LB14: OP SP* ×; LB15: QU SP* × OP
        (Some(Op), _) | (Some(Qu), Op) => true,
        // LB18: SP ÷
        _ => false,
    }
}

/// Number of classes.
const N: usize = 14;
/// The state "start of text" (no character before).
const SOT: usize = N;

/// Every class, in discriminant order.
const ALL: [Class; N] = [Al, Nu, Sp, Is, Ex, Qu, Op, Cp, Cl, Hy, Ba, Sy, Pr, Po];

/// `ALLOWED[spaces][before][after]`: whether a line may start before a
/// character of class `after` when `before` (or [`SOT`]) was the last
/// non-space, with (`1`) or without (`0`) spaces since. Never before a
/// space (LB7).
static ALLOWED: [[[bool; N]; N + 1]; 2] = {
    let mut t = [[[false; N]; N + 1]; 2];
    let mut b = 0;
    while b <= N {
        let before = if b == SOT { None } else { Some(ALL[b]) };
        let mut a = 0;
        while a < N {
            let after = ALL[a];
            if a != Sp as usize {
                // LB2: never at the start of the text (without spaces).
                t[0][b][a] = match before {
                    Some(p) => !no_break(p, after),
                    None => false,
                };
                t[1][b][a] = !no_break_after_spaces(before, after);
            }
            a += 1;
        }
        b += 1;
    }
    t
};

/// Append the UAX #14 break opportunities of `text` (printable ASCII only;
/// see [`is_printable_ascii`]) to `out`: byte offsets where a line may
/// start, ending with `text.len()`, like [`unicode_linebreak::linebreaks`].
pub(crate) fn ascii_breaks(text: &[u8], out: &mut Vec<u32>) {
    // Indices are in range by construction: bytes are masked to 7 bits
    // (128 classes), classes are below `N`, `before` is a class or `SOT`.
    // The state is the last non-space class and whether spaces followed
    // it; it comes straight from the class, so iterations do not wait on
    // each other's table lookups.
    let sp = Sp as usize;
    let al = Al as usize;
    let mut before = SOT;
    let mut spaces = 0usize;
    for (i, &b) in text.iter().enumerate() {
        // Inside a word nothing changes and nothing breaks (LB28).
        if before == al && spaces == 0 && (b | 0x20).wrapping_sub(b'a') < 26 {
            continue;
        }
        let c = CLASSES[usize::from(b & 0x7f)] as usize;
        if ALLOWED[spaces][before][c] {
            out.push(i as u32);
        }
        let is_space = c == sp;
        before = if is_space { before } else { c };
        spaces = usize::from(is_space);
    }
    out.push(u32::try_from(text.len()).unwrap_or(u32::MAX));
}

/// Whether every byte is printable ASCII (`0x20..=0x7E`). Checked 16
/// bytes at a time without early exits inside a block, so it vectorises.
pub(crate) fn is_printable_ascii(bytes: &[u8]) -> bool {
    let printable = |b: u8| b.wrapping_sub(0x20) < 0x5f;
    let (blocks, tail) = bytes.as_chunks::<16>();
    blocks
        .iter()
        .all(|block| block.iter().fold(true, |ok, &b| ok & printable(b)))
        && tail.iter().all(|&b| printable(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crate's break positions.
    fn reference(s: &str) -> Vec<u32> {
        unicode_linebreak::linebreaks(s)
            .map(|(i, _)| i as u32)
            .collect()
    }

    fn fast(s: &str) -> Vec<u32> {
        let mut out = Vec::new();
        ascii_breaks(s.as_bytes(), &mut out);
        out
    }

    /// One character per class.
    const REPRESENTATIVES: &[u8] = b"a1 .!\"(})-|/$%";

    #[test]
    fn classes_match_the_unicode_data() {
        use unicode_linebreak::BreakClass as B;
        for b in 0x20u8..0x7f {
            let want = match unicode_linebreak::break_property(u32::from(b)) {
                B::Alphabetic => Al,
                B::Numeric => Nu,
                B::Space => Sp,
                B::InfixSeparator => Is,
                B::Exclamation => Ex,
                B::Quotation => Qu,
                B::OpenPunctuation => Op,
                B::CloseParenthesis => Cp,
                B::ClosePunctuation => Cl,
                B::Hyphen => Hy,
                B::After => Ba,
                B::Symbol => Sy,
                B::Prefix => Pr,
                B::Postfix => Po,
                other => panic!("{:?} has class {other:?}", char::from(b)),
            };
            assert_eq!(class(b), want, "{:?}", char::from(b));
        }
        // The representatives cover every class once.
        let mut seen: Vec<Class> = REPRESENTATIVES.iter().map(|&b| class(b)).collect();
        seen.dedup();
        assert_eq!(seen.len(), 14);
    }

    #[test]
    fn every_class_sequence_up_to_six_matches() {
        // Length 6 (7.5 million strings) takes seconds unoptimised.
        let longest = if cfg!(debug_assertions) { 5 } else { 6 };
        let n = REPRESENTATIVES.len();
        let mut buf = Vec::with_capacity(6);
        for len in 1..=longest {
            for mut code in 0..n.pow(len) {
                buf.clear();
                for _ in 0..len {
                    buf.push(REPRESENTATIVES[code % n]);
                    code /= n;
                }
                let s = std::str::from_utf8(&buf).unwrap();
                assert_eq!(fast(s), reference(s), "{s:?}");
            }
        }
    }

    #[test]
    fn printable_ascii_detection() {
        for len in 0..40 {
            let s = "a".repeat(len);
            assert!(is_printable_ascii(s.as_bytes()));
            for at in 0..len {
                for bad in [0x1fu8, 0x7f, 0x80, 0xff, b'\n'] {
                    let mut b = s.clone().into_bytes();
                    b[at] = bad;
                    assert!(!is_printable_ascii(&b), "{len} {at} {bad}");
                }
            }
        }
        assert!(is_printable_ascii(b" ~"));
    }

    #[test]
    fn examples() {
        assert_eq!(fast("Hello world!"), [6, 12]);
        assert_eq!(fast("well-known fact"), [5, 11, 15]);
        assert_eq!(fast("  lead"), [2, 6]);
        assert_eq!(fast("a/b"), [2, 3]);
        assert_eq!(fast("3.14 and ($5)"), [5, 9, 13]);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig {
            cases: proptest::prelude::ProptestConfig::default().cases.max(2000),
            failure_persistence: None,
            ..proptest::prelude::ProptestConfig::default()
        })]

        #[test]
        fn random_ascii_matches(s in "[ -~]{0,80}") {
            if !s.is_empty() {
                proptest::prop_assert_eq!(fast(&s), reference(&s));
            }
        }

        #[test]
        fn prose_like_ascii_matches(s in "([a-z]{1,8}[ ,.;:!?'\"()\\-/]{0,3}){0,20}") {
            if !s.is_empty() {
                proptest::prop_assert_eq!(fast(&s), reference(&s));
            }
        }
    }
}
