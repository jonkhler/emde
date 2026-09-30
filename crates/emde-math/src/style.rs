//! How atoms look: math alphabets, italics, bold and roles.
//!
//! * Letters. Latin letters and lowercase Greek are italic by default, as in
//!   TeX, and capital Greek is upright. With [`Letters::Italic`] an italic
//!   letter stays a plain character with [`MathRole::Var`] and the caller
//!   slants it. With [`Letters::UnicodeItalic`] it becomes a mathematical italic
//!   code point (ℎ for h). Those glyphs are already slanted, so they carry
//!   [`MathRole::Plain`], like every letter under [`Letters::Plain`].
//! * `\mathbb`, `\mathcal`, `\mathscr`, `\mathfrak` (and their bold forms)
//!   always use the Unicode alphabets, including the Letterlike holes.
//! * `\mathbf`, `\boldsymbol`, `\textbf` set the span's `bold` flag, or with
//!   [`Bold::Unicode`] use the bold alphabets where Unicode has the character.
//! * `\mathrm`, `\text`, function names and numbers are upright.

use crate::ast::{Atom, Font};
use crate::tables::{self, Alphabet};
use crate::{Bold, Letters, MathOptions, MathRole};

/// The rendered form of an atom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Look {
    pub(crate) text: String,
    pub(crate) role: MathRole,
    pub(crate) bold: bool,
}

/// How `atom` looks in `font`.
pub(crate) fn atom(atom: &Atom, font: Font, opts: &MathOptions) -> Look {
    let plain = |text: String, role: MathRole| Look {
        text,
        role,
        bold: font.is_bold(),
    };
    match atom {
        Atom::Ord(c) => letter(*c, font, opts),
        Atom::Num(n) => {
            let mut text = String::with_capacity(n.len());
            let mut all_mapped = true;
            for c in n.chars() {
                match digit_alphabet(font, opts).and_then(|a| tables::styled(a, c)) {
                    Some(s) => text.push(s),
                    None => {
                        all_mapped &= !c.is_ascii_digit();
                        text.push(c);
                    }
                }
            }
            Look {
                text,
                role: MathRole::Num,
                bold: font.is_bold() && !(all_mapped && bold_is_unicode(font, opts)),
            }
        }
        Atom::Func(name) => plain(name.clone(), MathRole::Func),
        Atom::Text(text) => plain(text.clone(), MathRole::Text),
        Atom::LargeOp(c) | Atom::Bin(c) => plain(c.to_string(), MathRole::Op),
        Atom::Rel(r) => plain(r.clone(), MathRole::Rel),
        Atom::Delim { ch, .. } => plain(ch.to_string(), MathRole::Delim),
        Atom::Punct(c) => plain(c.to_string(), MathRole::Plain),
    }
}

/// The kind of letter a character is, for italics.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Latin letters (with the dotless ı ȷ) and lowercase Greek: italic by
    /// default.
    Slanted,
    /// Capital Greek: upright by default, italic under `\mathit`.
    Upright,
    /// Anything else.
    Symbol,
}

fn kind(c: char) -> Kind {
    match c {
        'a'..='z' | 'A'..='Z' | 'ı' | 'ȷ' => Kind::Slanted,
        'α'..='ω' | 'ϵ' | 'ϑ' | 'ϰ' | 'ϕ' | 'ϱ' | 'ϖ' | 'ϝ' => Kind::Slanted,
        'Α'..='Ω' | 'ϴ' | 'Ϝ' => Kind::Upright,
        _ => Kind::Symbol,
    }
}

fn bold_is_unicode(font: Font, opts: &MathOptions) -> bool {
    font.is_bold() && opts.bold == Bold::Unicode
}

/// The alphabet digits use in `font`, if any.
fn digit_alphabet(font: Font, opts: &MathOptions) -> Option<Alphabet> {
    match font {
        Font::DoubleStruck => Some(Alphabet::DoubleStruck),
        _ if bold_is_unicode(font, opts) => Some(Alphabet::Bold),
        _ => None,
    }
}

/// How an ordinary character looks in `font`.
fn letter(c: char, font: Font, opts: &MathOptions) -> Look {
    let styled = |alphabet: Alphabet| tables::styled(alphabet, c);
    let fixed = match font {
        Font::Script => styled(Alphabet::Script),
        Font::BoldScript => styled(Alphabet::BoldScript),
        Font::Fraktur => styled(Alphabet::Fraktur),
        Font::BoldFraktur => styled(Alphabet::BoldFraktur),
        Font::DoubleStruck => styled(Alphabet::DoubleStruck),
        Font::DoubleStruckItalic => {
            styled(Alphabet::DoubleStruckItalic).or_else(|| styled(Alphabet::DoubleStruck))
        }
        _ => None,
    };
    if let Some(s) = fixed {
        return Look {
            text: s.to_string(),
            role: MathRole::Plain,
            bold: false,
        };
    }
    let kind = kind(c);
    let italic = match font {
        Font::Upright | Font::Bold => false,
        Font::Italic | Font::BoldItalic => kind != Kind::Symbol,
        // `\mathnormal`, `\boldsymbol` and alphabets without this character.
        _ => kind == Kind::Slanted,
    };
    if bold_is_unicode(font, opts) {
        let alphabet = if italic {
            Alphabet::BoldItalic
        } else {
            Alphabet::Bold
        };
        if let Some(s) = styled(alphabet) {
            return Look {
                text: s.to_string(),
                role: MathRole::Plain,
                bold: false,
            };
        }
    }
    let bold = font.is_bold();
    let (text, role) = match (italic, opts.letters) {
        (true, Letters::Italic) => (c, MathRole::Var),
        (true, Letters::UnicodeItalic) => (styled(Alphabet::Italic).unwrap_or(c), MathRole::Plain),
        _ => (c, MathRole::Plain),
    };
    Look {
        text: text.to_string(),
        role,
        bold,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(a: Atom, font: Font, opts: &MathOptions) -> (String, MathRole, bool) {
        let l = atom(&a, font, opts);
        (l.text, l.role, l.bold)
    }

    fn opts() -> MathOptions {
        MathOptions::default()
    }

    #[test]
    fn default_letters_are_variables() {
        let o = opts();
        let var = |s: &str| (s.to_string(), MathRole::Var, false);
        assert_eq!(look(Atom::Ord('x'), Font::Normal, &o), var("x"));
        assert_eq!(look(Atom::Ord('α'), Font::Normal, &o), var("α"));
        assert_eq!(
            look(Atom::Ord('Γ'), Font::Normal, &o),
            ("Γ".into(), MathRole::Plain, false)
        );
        assert_eq!(look(Atom::Ord('Γ'), Font::Italic, &o), var("Γ"));
        assert_eq!(
            look(Atom::Ord('∞'), Font::Normal, &o),
            ("∞".into(), MathRole::Plain, false)
        );
        assert_eq!(
            look(Atom::Ord('d'), Font::Upright, &o),
            ("d".into(), MathRole::Plain, false)
        );
    }

    #[test]
    fn unicode_italic_and_plain_letters() {
        let mut o = opts();
        o.letters = Letters::UnicodeItalic;
        assert_eq!(look(Atom::Ord('h'), Font::Normal, &o).0, "ℎ");
        assert_eq!(look(Atom::Ord('x'), Font::Normal, &o).0, "𝑥");
        assert_eq!(look(Atom::Ord('x'), Font::Normal, &o).1, MathRole::Plain);
        assert_eq!(look(Atom::Ord('Γ'), Font::Normal, &o).0, "Γ");
        o.letters = Letters::Plain;
        assert_eq!(
            look(Atom::Ord('x'), Font::Normal, &o),
            ("x".into(), MathRole::Plain, false)
        );
    }

    #[test]
    fn alphabets_always_use_unicode() {
        let o = opts();
        assert_eq!(look(Atom::Ord('R'), Font::DoubleStruck, &o).0, "ℝ");
        assert_eq!(look(Atom::Ord('L'), Font::Script, &o).0, "ℒ");
        assert_eq!(look(Atom::Ord('g'), Font::Fraktur, &o).0, "𝔤");
        assert_eq!(look(Atom::Num("1".into()), Font::DoubleStruck, &o).0, "𝟙");
        assert_eq!(look(Atom::Ord('d'), Font::DoubleStruckItalic, &o).0, "ⅆ");
        assert_eq!(look(Atom::Ord('x'), Font::DoubleStruckItalic, &o).0, "𝕩");
        // No double-struck alpha: the default look.
        assert_eq!(
            look(Atom::Ord('α'), Font::DoubleStruck, &o).1,
            MathRole::Var
        );
    }

    #[test]
    fn bold_is_a_flag_or_unicode() {
        let mut o = opts();
        assert_eq!(
            look(Atom::Ord('x'), Font::Bold, &o),
            ("x".into(), MathRole::Plain, true)
        );
        assert_eq!(
            look(Atom::Ord('x'), Font::BoldSymbol, &o),
            ("x".into(), MathRole::Var, true)
        );
        assert_eq!(
            look(Atom::Text("abc".into()), Font::Bold, &o),
            ("abc".into(), MathRole::Text, true)
        );
        o.bold = Bold::Unicode;
        assert_eq!(
            look(Atom::Ord('x'), Font::Bold, &o),
            ("𝐱".into(), MathRole::Plain, false)
        );
        assert_eq!(look(Atom::Ord('x'), Font::BoldSymbol, &o).0, "𝒙");
        assert_eq!(look(Atom::Ord('Γ'), Font::BoldSymbol, &o).0, "𝚪");
        assert_eq!(look(Atom::Num("12".into()), Font::Bold, &o).0, "𝟏𝟐");
        // Operators have no bold alphabet: they keep the flag.
        assert_eq!(
            look(Atom::Bin('+'), Font::Bold, &o),
            ("+".into(), MathRole::Op, true)
        );
    }

    #[test]
    fn roles() {
        let o = opts();
        let role = |a| look(a, Font::Normal, &o).1;
        assert_eq!(role(Atom::Num("3".into())), MathRole::Num);
        assert_eq!(role(Atom::Func("sin".into())), MathRole::Func);
        assert_eq!(role(Atom::LargeOp('∑')), MathRole::Op);
        assert_eq!(role(Atom::Bin('+')), MathRole::Op);
        assert_eq!(role(Atom::Rel("=".into())), MathRole::Rel);
        assert_eq!(role(Atom::Text("if".into())), MathRole::Text);
        assert_eq!(role(Atom::Punct(',')), MathRole::Plain);
    }
}
