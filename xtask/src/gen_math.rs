//! Generated math tables for `crates/emde-math` (super/subscripts, math
//! alphabets, NFC accent and negation pairs) from pinned Unicode data.
//!
//! The inputs are `UnicodeData.txt` and `CompositionExclusions.txt` from UCD
//! 17.0.0 (Unicode License v3). They are downloaded once with `curl`, cached
//! under `$CARGO_TARGET_DIR/ucd-17.0.0/` and verified against pinned SHA-256
//! digests. The output is three Rust modules in `crates/emde-math/src/gen/`:
//!
//! * `scripts.rs`: superscript and subscript forms (`<super>`/`<sub>`
//!   decompositions), split into a safe set and the Unicode 14+ additions;
//! * `alphabets.rs`: mathematical alphanumeric alphabets (`<font>`
//!   decompositions) with the 24 reserved holes filled from Letterlike Symbols;
//! * `compose.rs`: canonical (NFC) compositions of a letter with an accent mark,
//!   and the relations negated with U+0338 COMBINING LONG SOLIDUS OVERLAY.
//!
//! With `--check` nothing is written; the task fails if a checked-in file is
//! not exactly what the generator produces.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Pinned UCD version; the URL and the cache directory follow it.
const UCD_VERSION: &str = "17.0.0";
/// The only User-Agent sent with downloads.
const USER_AGENT: &str = "emde-dev";
/// Source files and their SHA-256 digests.
const SOURCES: [(&str, &str); 2] = [
    (
        "UnicodeData.txt",
        "2e1efc1dcb59c575eedf5ccae60f95229f706ee6d031835247d843c11d96470c",
    ),
    (
        "CompositionExclusions.txt",
        "2f239196ef3b5b61db5cc476e9bd80f534d15aa1b74e1be1dea5d042a344c85f",
    ),
];

/// Superscripts and subscripts at or above this code point (Latin
/// Extended-D/F modifier letters, Unicode 14.0 and later) have poor font
/// coverage, so they are only in the full set.
const SAFE_SCRIPT_LIMIT: u32 = 0xA700;
/// Ordinal indicators decompose to `<super> a`/`<super> o` but are underlined
/// in many fonts; `ᵃ` and `ᵒ` are the real superscripts.
const NOT_SCRIPTS: [char; 2] = ['ª', 'º'];
/// Greek letters without a superscript of their own borrow the superscript of
/// the Latin letter that was derived from them (IPA alpha, open e, iota).
const SCRIPT_LOOKALIKES: [(char, char); 3] = [('α', 'ɑ'), ('ε', 'ɛ'), ('ι', 'ɩ')];

/// Every combining mark the renderer attaches for accents (see
/// `emde_math::tables`); a unit test there checks the two lists agree.
const COMPOSING_MARKS: [u32; 20] = [
    0x0300, 0x0301, 0x0302, 0x0303, 0x0304, 0x0305, 0x0306, 0x0307, 0x0308, 0x030A, 0x030C, 0x0332,
    0x034D, 0x20D0, 0x20D1, 0x20D6, 0x20D7, 0x20E1, 0x20EE, 0x20EF,
];
/// U+0338 COMBINING LONG SOLIDUS OVERLAY, used by `\not`.
const NEGATION_MARK: u32 = 0x0338;

/// A mathematical alphabet emitted into `alphabets.rs`.
struct Style {
    /// The `Alphabet` variant name.
    variant: &'static str,
    /// The style as it appears in character names after `MATHEMATICAL `.
    ucd: &'static str,
    /// Which Letterlike Symbols fill this style's reserved holes.
    holes: Holes,
    /// Doc comment for the variant.
    doc: &'static str,
}

/// Where a style's reserved holes are filled from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Holes {
    /// The style has no holes.
    None,
    /// Letterlike characters whose names start with this prefix.
    Prefix(&'static str),
    /// The Letterlike character with exactly this name.
    Name(&'static str),
}

/// The alphabets the renderer uses, in `Alphabet` order.
const STYLES: [Style; 9] = [
    Style {
        variant: "Bold",
        ucd: "BOLD",
        holes: Holes::None,
        doc: "Bold (`\\mathbf` with `Bold::Unicode`): 𝐀 𝐚 𝟎 𝚨 𝛂.",
    },
    Style {
        variant: "Italic",
        ucd: "ITALIC",
        holes: Holes::Name("PLANCK CONSTANT"),
        doc: "Italic (`Letters::UnicodeItalic`): 𝐴 𝑎 𝛼, with ℎ for h.",
    },
    Style {
        variant: "BoldItalic",
        ucd: "BOLD ITALIC",
        holes: Holes::None,
        doc: "Bold italic (`\\boldsymbol` with `Bold::Unicode`): 𝑨 𝒂 𝜶.",
    },
    Style {
        variant: "Script",
        ucd: "SCRIPT",
        holes: Holes::Prefix("SCRIPT "),
        doc: "Script (`\\mathcal`, `\\mathscr`): 𝒜 ℬ 𝒶 ℯ.",
    },
    Style {
        variant: "BoldScript",
        ucd: "BOLD SCRIPT",
        holes: Holes::None,
        doc: "Bold script (`\\mathbfcal`): 𝓐 𝓪.",
    },
    Style {
        variant: "Fraktur",
        ucd: "FRAKTUR",
        holes: Holes::Prefix("BLACK-LETTER "),
        doc: "Fraktur (`\\mathfrak`): 𝔄 ℭ 𝔞.",
    },
    Style {
        variant: "BoldFraktur",
        ucd: "BOLD FRAKTUR",
        holes: Holes::None,
        doc: "Bold Fraktur (`\\mathbffrak`): 𝕬 𝖆.",
    },
    Style {
        variant: "DoubleStruck",
        ucd: "DOUBLE-STRUCK",
        holes: Holes::Prefix("DOUBLE-STRUCK "),
        doc: "Double-struck (`\\mathbb`): 𝔸 ℂ 𝕒 𝟙 ℾ.",
    },
    Style {
        variant: "DoubleStruckItalic",
        ucd: "DOUBLE-STRUCK ITALIC",
        holes: Holes::None,
        doc: "Double-struck italic (`\\mathbbit`): only ⅅ ⅆ ⅇ ⅈ ⅉ exist.",
    },
];

/// The number of reserved holes in the alphanumeric block (all filled from
/// Letterlike Symbols).
const EXPECTED_HOLES: usize = 24;

/// Regenerate (or with `check`, verify) the math tables.
pub(crate) fn run(check: bool) -> Result<(), String> {
    let ucd = Ucd::load(&cache_dir()?)?;
    let outputs = [
        ("scripts.rs", scripts(&ucd)?),
        ("alphabets.rs", alphabets(&ucd)?),
        ("compose.rs", compose(&ucd)?),
    ];
    let dir = workspace_root()?.join("crates/emde-math/src/gen");
    let mut stale = Vec::new();
    for (name, text) in &outputs {
        let path = dir.join(name);
        let current = fs::read_to_string(&path).unwrap_or_default();
        if current == *text {
            continue;
        }
        if check {
            stale.push(path.display().to_string());
        } else {
            fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            eprintln!("xtask: wrote {}", path.display());
        }
    }
    if stale.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "generated math tables are out of date (run `cargo xtask gen`): {}",
            stale.join(", ")
        ))
    }
}

fn workspace_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "xtask has no parent directory".to_string())
}

fn cache_dir() -> Result<PathBuf, String> {
    Ok(crate::target_dir()?.join(format!("ucd-{UCD_VERSION}")))
}

/// Read `name` from the cache in `dir`, downloading it first unless a copy
/// with the pinned digest is already there.
fn fetch(dir: &Path, name: &str, sha256: &str) -> Result<String, String> {
    let path = dir.join(name);
    let read = |p: &Path| fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    if path.is_file() && digest(&path)? == sha256 {
        return read(&path);
    }
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let url = format!("https://www.unicode.org/Public/{UCD_VERSION}/ucd/{name}");
    let partial = dir.join(format!("{name}.part"));
    eprintln!("xtask: downloading {url}");
    let status = Command::new("curl")
        .args(["--proto", "=https", "--fail", "--silent", "--show-error"])
        .args([
            "--location",
            "--max-time",
            "120",
            "--user-agent",
            USER_AGENT,
        ])
        .arg("--output")
        .arg(&partial)
        .arg(&url)
        .status()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !status.success() {
        return Err(format!("downloading {url} failed ({status})"));
    }
    let actual = digest(&partial)?;
    if actual != sha256 {
        return Err(format!(
            "{url}: SHA-256 {actual} does not match the pinned {sha256}"
        ));
    }
    fs::rename(&partial, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    read(&path)
}

/// SHA-256 of a file via the system tool (`sha256sum`, or `shasum` on macOS).
fn digest(path: &Path) -> Result<String, String> {
    let tools: [(&str, &[&str]); 2] = [("sha256sum", &[]), ("shasum", &["-a", "256"])];
    for (tool, args) in tools {
        let Ok(out) = Command::new(tool).args(args).arg(path).output() else {
            continue;
        };
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(hex) = text.split_whitespace().next() {
                return Ok(hex.to_ascii_lowercase());
            }
        }
    }
    Err("neither sha256sum nor shasum is available".into())
}

/// One `UnicodeData.txt` record.
struct Record {
    cp: u32,
    name: String,
    /// Canonical combining class.
    ccc: u8,
    /// Decomposition tag (`super`, `font`, …); `None` for canonical ones.
    tag: Option<String>,
    decomposition: Vec<u32>,
}

/// The parsed UCD files.
struct Ucd {
    /// Records in code point order.
    records: Vec<Record>,
    /// Code points listed in `CompositionExclusions.txt`.
    exclusions: HashSet<u32>,
}

impl Ucd {
    fn load(dir: &Path) -> Result<Ucd, String> {
        let [(data_name, data_sha), (excl_name, excl_sha)] = SOURCES;
        let data = fetch(dir, data_name, data_sha)?;
        let exclusions = fetch(dir, excl_name, excl_sha)?;
        let ucd = Ucd {
            records: data.lines().map(parse_record).collect::<Result<_, _>>()?,
            exclusions: exclusions
                .lines()
                .filter_map(|l| l.split('#').next())
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| u32::from_str_radix(l, 16).map_err(|e| format!("{l}: {e}")))
                .collect::<Result<_, _>>()?,
        };
        // Guard against a truncated or wrong file.
        if ucd.records.len() < 40_000 || ucd.exclusions.len() < 60 {
            return Err("the UCD files look truncated".into());
        }
        Ok(ucd)
    }

    fn ccc(&self, cp: u32) -> u8 {
        self.records
            .binary_search_by_key(&cp, |r| r.cp)
            .ok()
            .and_then(|i| self.records.get(i))
            .map_or(0, |r| r.ccc)
    }
}

fn parse_record(line: &str) -> Result<Record, String> {
    let fields: Vec<&str> = line.split(';').collect();
    let [cp, name, _category, ccc, _bidi, decomposition, ..] = fields.as_slice() else {
        return Err(format!("malformed UnicodeData line: {line}"));
    };
    let cp = u32::from_str_radix(cp, 16).map_err(|e| format!("{line}: {e}"))?;
    let ccc = ccc.parse().map_err(|e| format!("{line}: {e}"))?;
    let (tag, parts) = match decomposition.strip_prefix('<') {
        Some(rest) => {
            let (tag, parts) = rest
                .split_once('>')
                .ok_or_else(|| format!("malformed decomposition: {line}"))?;
            (Some(tag.to_string()), parts)
        }
        None => (None, *decomposition),
    };
    let decomposition = parts
        .split_whitespace()
        .map(|p| u32::from_str_radix(p, 16).map_err(|e| format!("{line}: {e}")))
        .collect::<Result<_, _>>()?;
    Ok(Record {
        cp,
        name: name.to_string(),
        ccc,
        tag,
        decomposition,
    })
}

fn char_of(cp: u32) -> Result<char, String> {
    char::from_u32(cp).ok_or_else(|| format!("U+{cp:04X} is not a scalar value"))
}

/// A Rust char literal; marks, spaces and invisible characters are escaped.
fn lit(c: char, ucd: &Ucd) -> String {
    let invisible = c.is_whitespace()
        || c.is_control()
        || ucd.ccc(c as u32) != 0
        || matches!(c as u32, 0x0300..=0x036F | 0x20D0..=0x20FF | 0xFE00..=0xFE0F);
    match c {
        '\'' => "'\\''".into(),
        '\\' => "'\\\\'".into(),
        _ if invisible => format!("'\\u{{{:x}}}'", c as u32),
        _ => format!("'{c}'"),
    }
}

/// The module docs shared by all generated files.
fn header(title: &str, body: &str) -> String {
    format!(
        "//! {title}\n//!\n{body}//!\n//! Generated by `cargo xtask gen` from UCD {UCD_VERSION}; do not edit.\n\n"
    )
}

/// A `#[rustfmt::skip]` static array, with as many entries per line as fit
/// in 100 columns.
fn table(out: &mut String, doc: &str, vis: &str, name: &str, ty: &str, entries: &[String]) {
    let _ = writeln!(out, "/// {doc}");
    let _ = writeln!(out, "#[rustfmt::skip]");
    if entries.is_empty() {
        let _ = writeln!(out, "{vis}static {name}: [{ty}; 0] = [];");
        return;
    }
    let _ = writeln!(out, "{vis}static {name}: [{ty}; {}] = [", entries.len());
    let mut line = String::new();
    for entry in entries {
        if !line.is_empty() && 4 + line.chars().count() + entry.chars().count() + 2 > 100 {
            let _ = writeln!(out, "    {}", line.trim_end());
            line.clear();
        }
        line.push_str(entry);
        line.push_str(", ");
    }
    let _ = writeln!(out, "    {}", line.trim_end());
    let _ = writeln!(out, "];");
}

// ── Superscripts and subscripts ─────────────────────────────────────────────

/// Characters worth mapping into scripts: ASCII digits and letters, Greek
/// letters and `+ − = ( )`.
fn script_source(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '+' | '−' | '=' | '(' | ')')
        || matches!(c, 'Α'..='Ω' | 'α'..='ω' | 'ϑ' | 'ϕ' | 'ϵ' | 'ϰ' | 'ϱ' | 'ϖ')
}

/// `(base, script, code point)` for one decomposition tag, sorted by base.
fn script_pairs(ucd: &Ucd, tag: &str) -> Result<Vec<(char, char, u32)>, String> {
    let mut pairs: BTreeMap<char, (char, u32)> = BTreeMap::new();
    for r in &ucd.records {
        let (Some(t), [base]) = (&r.tag, r.decomposition.as_slice()) else {
            continue;
        };
        let script = char_of(r.cp)?;
        if t != tag || NOT_SCRIPTS.contains(&script) {
            continue;
        }
        let base = char_of(*base)?;
        let base = SCRIPT_LOOKALIKES
            .iter()
            .find(|&&(_, latin)| latin == base)
            .map_or(base, |&(greek, _)| greek);
        if !script_source(base) {
            continue;
        }
        if let Some((other, _)) = pairs.insert(base, (script, r.cp)) {
            return Err(format!(
                "two {tag} forms of {base:?}: {other:?} and {script:?}"
            ));
        }
    }
    Ok(pairs.into_iter().map(|(b, (s, cp))| (b, s, cp)).collect())
}

fn scripts(ucd: &Ucd) -> Result<String, String> {
    let mut out = header(
        "Unicode superscript and subscript forms.",
        "//! From the `<super>` and `<sub>` decompositions in UnicodeData.txt, for\n\
         //! digits, Latin and Greek letters and `+ − = ( )`. The safe set leaves out\n\
         //! the Unicode 14+ modifier letters (code points from U+A700), which few\n\
         //! fonts cover. The ordinal indicators ª º are never used, and α ε ι borrow\n\
         //! the superscripts of their Latin look-alikes ɑ ɛ ɩ.\n",
    );
    let sets = [
        ("super", "SUPERSCRIPTS", "Superscript"),
        ("sub", "SUBSCRIPTS", "Subscript"),
    ];
    for (i, (tag, name, what)) in sets.into_iter().enumerate() {
        let pairs = script_pairs(ucd, tag)?;
        let entry = |p: &&(char, char, u32)| format!("({}, {})", lit(p.0, ucd), lit(p.1, ucd));
        let safe: Vec<String> = pairs
            .iter()
            .filter(|p| p.2 < SAFE_SCRIPT_LIMIT)
            .map(|p| entry(&p))
            .collect();
        let full: Vec<String> = pairs
            .iter()
            .filter(|p| p.2 >= SAFE_SCRIPT_LIMIT)
            .map(|p| entry(&p))
            .collect();
        if i > 0 {
            out.push('\n');
        }
        table(
            &mut out,
            &format!("{what} forms in the safe set, `(base, {tag}script)` sorted by base."),
            "pub(crate) ",
            name,
            "(char, char)",
            &safe,
        );
        out.push('\n');
        table(
            &mut out,
            &format!("{what} forms only in the full set (Unicode 14+), sorted by base."),
            "pub(crate) ",
            &format!("{name}_FULL"),
            "(char, char)",
            &full,
        );
    }
    Ok(out)
}

// ── Mathematical alphanumeric alphabets ─────────────────────────────────────

/// Words that start the letter part of a `MATHEMATICAL <STYLE> …` name.
const LETTER_WORDS: [&str; 12] = [
    "CAPITAL", "SMALL", "DIGIT", "NABLA", "PARTIAL", "EPSILON", "THETA", "KAPPA", "PHI", "RHO",
    "PI", "DOTLESS",
];

/// Whether `name` belongs to the alphanumeric style `style` (and not to a
/// longer style such as `BOLD ITALIC` when looking for `BOLD`).
fn in_style(name: &str, style: &str) -> bool {
    name.strip_prefix("MATHEMATICAL ")
        .and_then(|n| n.strip_prefix(style))
        .and_then(|n| n.strip_prefix(' '))
        .and_then(|n| n.split(' ').next())
        .is_some_and(|word| LETTER_WORDS.contains(&word))
}

fn is_letterlike(cp: u32) -> bool {
    (0x2100..=0x214F).contains(&cp)
}

/// Whether the Letterlike record `r` supplies characters for `style`.
fn fills(style: &Style, r: &Record) -> bool {
    if !is_letterlike(r.cp) {
        return false;
    }
    match style.holes {
        Holes::None => false,
        Holes::Name(n) => r.name == n,
        Holes::Prefix(p) => r.name.starts_with(p) && !r.name.starts_with("DOUBLE-STRUCK ITALIC"),
    }
}

fn alphabets(ucd: &Ucd) -> Result<String, String> {
    let mut out = header(
        "Mathematical alphanumeric alphabets (`\\mathbb`, `\\mathcal`, …).",
        "//! From the `<font>` decompositions in UnicodeData.txt. The 24 reserved\n\
         //! holes in the Mathematical Alphanumeric Symbols block (ℎ ℬ ℰ ℱ ℋ ℐ ℒ ℳ ℛ\n\
         //! ℯ ℊ ℴ ℭ ℌ ℑ ℜ ℨ ℂ ℍ ℕ ℙ ℚ ℝ ℤ) are filled from Letterlike Symbols, as\n\
         //! are the double-struck Greek letters ℾ ℿ ℽ ℼ and ⅅ ⅆ ⅇ ⅈ ⅉ.\n",
    );
    let fonts: Vec<&Record> = ucd
        .records
        .iter()
        .filter(|r| r.tag.as_deref() == Some("font") && r.decomposition.len() == 1)
        .collect();
    let mut holes = 0;
    let mut tables = Vec::new();
    for style in &STYLES {
        let mut map: BTreeMap<char, char> = BTreeMap::new();
        for r in fonts.iter().filter(|r| in_style(&r.name, style.ucd)) {
            map.insert(char_of(r.decomposition[0])?, char_of(r.cp)?);
        }
        for r in fonts.iter().filter(|r| fills(style, r)) {
            let base = char_of(r.decomposition[0])?;
            if base.is_ascii_alphabetic() {
                if map.contains_key(&base) {
                    continue; // not a hole (ℓ next to 𝓁)
                }
                holes += 1;
            }
            map.insert(base, char_of(r.cp)?);
        }
        if style.variant == "DoubleStruckItalic" {
            for r in fonts
                .iter()
                .filter(|r| is_letterlike(r.cp) && r.name.starts_with("DOUBLE-STRUCK ITALIC "))
            {
                map.insert(char_of(r.decomposition[0])?, char_of(r.cp)?);
            }
        }
        if map.is_empty() {
            return Err(format!("no characters for the {} alphabet", style.ucd));
        }
        tables.push((style, map));
    }
    if holes != EXPECTED_HOLES {
        return Err(format!(
            "filled {holes} alphanumeric holes, expected {EXPECTED_HOLES}"
        ));
    }

    out.push_str("/// A mathematical alphanumeric style.\n");
    out.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n");
    out.push_str("pub(crate) enum Alphabet {\n");
    for style in &STYLES {
        let _ = writeln!(out, "    /// {}", style.doc);
        let _ = writeln!(out, "    {},", style.variant);
    }
    out.push_str("}\n\nimpl Alphabet {\n");
    out.push_str("    /// Every alphabet, in declaration order.\n");
    out.push_str("    #[cfg(test)]\n");
    let _ = writeln!(
        out,
        "    pub(crate) const ALL: [Alphabet; {}] = [",
        STYLES.len()
    );
    for style in &STYLES {
        let _ = writeln!(out, "        Alphabet::{},", style.variant);
    }
    out.push_str("    ];\n\n");
    out.push_str("    /// `(base, styled)` pairs sorted by base.\n");
    out.push_str("    pub(crate) fn table(self) -> &'static [(char, char)] {\n");
    out.push_str("        match self {\n");
    for style in &STYLES {
        let _ = writeln!(
            out,
            "            Alphabet::{} => &{},",
            style.variant,
            const_name(style.variant)
        );
    }
    out.push_str("        }\n    }\n}\n");
    for (style, map) in &tables {
        let entries: Vec<String> = map
            .iter()
            .map(|(&b, &s)| format!("({}, {})", lit(b, ucd), lit(s, ucd)))
            .collect();
        out.push('\n');
        table(
            &mut out,
            style.doc,
            "",
            &const_name(style.variant),
            "(char, char)",
            &entries,
        );
    }
    Ok(out)
}

/// `DoubleStruckItalic` → `DOUBLE_STRUCK_ITALIC`.
fn const_name(variant: &str) -> String {
    let mut name = String::new();
    for (i, c) in variant.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            name.push('_');
        }
        name.push(c.to_ascii_uppercase());
    }
    name
}

// ── Canonical compositions ─────────────────────────────────────────────────

fn compose(ucd: &Ucd) -> Result<String, String> {
    let mut out = header(
        "Canonical (NFC) compositions used by accents and `\\not`.",
        "//! Pairs from the canonical decompositions in UnicodeData.txt that NFC\n\
         //! recomposes: composition exclusions (CompositionExclusions.txt, such as\n\
         //! U+2ADC FORKING) and non-starter decompositions are left out.\n",
    );
    let marks: BTreeSet<u32> = COMPOSING_MARKS.into_iter().collect();
    let mut accented = Vec::new();
    let mut negated = Vec::new();
    for r in &ucd.records {
        let (None, &[base, mark]) = (&r.tag, r.decomposition.as_slice()) else {
            continue;
        };
        if ucd.exclusions.contains(&r.cp) || ucd.ccc(base) != 0 || r.ccc != 0 {
            continue;
        }
        let (b, m, c) = (char_of(base)?, char_of(mark)?, char_of(r.cp)?);
        if mark == NEGATION_MARK {
            negated.push((b, c));
        } else if marks.contains(&mark) {
            accented.push((b, m, c));
        }
    }
    accented.sort_unstable();
    negated.sort_unstable();

    let mark_list: Vec<String> = COMPOSING_MARKS
        .iter()
        .map(|&m| char_of(m).map(|c| lit(c, ucd)))
        .collect::<Result<_, _>>()?;
    out.push_str("/// The combining marks whose compositions [`ACCENTED`] lists.\n");
    let _ = writeln!(
        out,
        "#[cfg(test)]\n#[rustfmt::skip]\npub(crate) static COMPOSING_MARKS: [char; {}] = [",
        mark_list.len()
    );
    for chunk in mark_list.chunks(8) {
        let _ = writeln!(out, "    {},", chunk.join(", "));
    }
    out.push_str("];\n\n");
    let entries: Vec<String> = accented
        .iter()
        .map(|&(b, m, c)| format!("({}, {}, {})", lit(b, ucd), lit(m, ucd), lit(c, ucd)))
        .collect();
    table(
        &mut out,
        "`(base, mark, composed)` sorted by `(base, mark)`.",
        "pub(crate) ",
        "ACCENTED",
        "(char, char, char)",
        &entries,
    );
    out.push('\n');
    let entries: Vec<String> = negated
        .iter()
        .map(|&(b, c)| format!("({}, {})", lit(b, ucd), lit(c, ucd)))
        .collect();
    table(
        &mut out,
        "`(relation, negated)`: the relation followed by U+0338, sorted.",
        "pub(crate) ",
        "NEGATED",
        "(char, char)",
        &entries,
    );
    Ok(out)
}
