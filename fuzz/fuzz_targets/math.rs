//! `emde_math::inline` and `emde_math::display` on arbitrary TeX.
//!
//! Input: three option bytes, then the TeX (lossy UTF-8).
//!
//! | byte | meaning |
//! |---|---|
//! | 0 | bits 0–1 letters, 2 full script set, 3 slash fractions, 4 Unicode bold, 5 wide ambiguous characters |
//! | 1 | the columns available to display math, 0–255 |
//! | 2 | the height limit of 2D boxes, 0–19 |
//!
//! Checked, as the crate documents:
//! * every line's spans are non-empty, contiguous and cover its text, on
//!   character boundaries; its breaks are increasing character boundaries
//!   inside the text; it has no control characters; its `width` is the
//!   text's width;
//! * a box has `height ≥ 1` rows, `baseline < height`, fits the available
//!   columns and the height limit, and every row is exactly `width` columns
//!   wide;
//! * lines of a linear fallback parsed; raw TeX did not;
//! * the same input gives the same output.

#![no_main]

use emde_fuzz::header;
use emde_math::{
    Bold, Fractions, Letters, MathDisplay, MathLine, MathOptions, ScriptSet, display, inline,
};
use libfuzzer_sys::fuzz_target;
use unicode_width::UnicodeWidthStr;

fuzz_target!(init: emde_fuzz::init(), |data: &[u8]| {
    let ([flags, avail, height], body) = header::<3>(data);
    let tex = String::from_utf8_lossy(body);
    let opts = options(flags, height);
    let avail = u16::from(avail);

    let line = inline(&tex, &opts);
    check_line(&line, &opts);
    let shown = display(&tex, &opts, avail);
    check_display(&shown, &opts, avail);

    assert_eq!(inline(&tex, &opts), line, "inline is not deterministic");
    assert_eq!(display(&tex, &opts, avail), shown, "display is not deterministic");
});

fn options(flags: u8, height: u8) -> MathOptions {
    let bit = |n: u8| flags & (1 << n) != 0;
    MathOptions {
        letters: [Letters::Italic, Letters::UnicodeItalic, Letters::Plain]
            [usize::from(flags & 3) % 3],
        scripts: if bit(2) {
            ScriptSet::Full
        } else {
            ScriptSet::Safe
        },
        fractions: if bit(3) {
            Fractions::Slash
        } else {
            Fractions::Vulgar
        },
        bold: if bit(4) { Bold::Unicode } else { Bold::Sgr },
        ambiguous_wide: bit(5),
        max_height: u16::from(height % 20),
    }
}

/// The width the crate measures with.
fn measure(s: &str, opts: &MathOptions) -> usize {
    if opts.ambiguous_wide {
        s.width_cjk()
    } else {
        s.width()
    }
}

fn check_line(line: &MathLine, opts: &MathOptions) {
    let len = line.text.len();
    let mut prev = 0usize;
    for span in &line.spans {
        let end = span.end as usize;
        assert!(end > prev, "an empty or unordered span: {line:?}");
        assert!(
            line.text.is_char_boundary(end),
            "a span ends inside a character: {line:?}"
        );
        prev = end;
    }
    assert_eq!(prev, len, "the spans do not cover the text: {line:?}");
    assert!(
        !line.text.chars().any(char::is_control),
        "a control character in {:?}",
        line.text
    );
    let mut prev = 0usize;
    for &b in &line.breaks {
        let b = b as usize;
        assert!(b > prev && b <= len, "bad break {b} in {line:?}");
        assert!(
            line.text.is_char_boundary(b),
            "a break inside a character: {line:?}"
        );
        prev = b;
    }
    assert_eq!(
        usize::from(line.width),
        measure(&line.text, opts),
        "the width of {:?}",
        line.text
    );
}

fn check_display(shown: &MathDisplay, opts: &MathOptions, avail: u16) {
    match shown {
        MathDisplay::Box(b) => {
            assert!(b.width <= avail, "a box wider than {avail}: {b:?}");
            assert!(
                b.height <= opts.max_height,
                "a box taller than the limit: {b:?}"
            );
            assert!(b.height >= 1, "an empty box");
            assert!(
                b.baseline < b.height,
                "the baseline is outside the box: {b:?}"
            );
            assert_eq!(b.rows.len(), usize::from(b.height), "rows ≠ height: {b:?}");
            for row in &b.rows {
                assert!(row.ok, "a box row from TeX that did not parse");
                assert!(row.breaks.is_empty(), "a box row with break hints: {row:?}");
                assert_eq!(row.width, b.width, "a row of another width: {b:?}");
                check_line(row, opts);
            }
        }
        MathDisplay::Lines(lines) => {
            assert!(!lines.is_empty(), "no lines");
            for line in lines {
                assert!(line.ok, "a linear line from TeX that did not parse");
                check_line(line, opts);
            }
        }
        MathDisplay::Raw(line) => {
            assert!(!line.ok, "raw TeX that parsed");
            check_line(line, opts);
        }
    }
}
