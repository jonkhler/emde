//! Generated graphics tables (kitty placeholder diacritics, sextant and
//! octant glyphs) from pinned Unicode data.
//!
//! `cargo xtask gen` downloads each pinned `UnicodeData.txt` once (with
//! `curl`) into `$CARGO_TARGET_DIR/ucd/<version>/`, checks its SHA-256 and
//! writes:
//!
//! * `src/gfx/gen/diacritics.rs`: the 297 row/column diacritics of kitty's
//!   Unicode placeholders. kitty derives them from Unicode **6.0.0** (see its
//!   `gen/rowcolumn-diacritics.txt`): every character of general category
//!   `Mn` with combining class 230, bidi class `NSM` and no decomposition
//!   mapping, minus 19 marks that NFC may fuse with a base letter. Newer UCD
//!   versions interleave more marks, so the version is part of the protocol.
//! * `src/gfx/gen/octants.rs`: a glyph for each of the 256 octant masks: the
//!   230 `BLOCK OCTANT-…` characters of Unicode 17.0.0 plus 26 older block
//!   elements whose shapes are octant patterns.
//! * `src/gfx/gen/sextants.rs`: the 64 sextant glyphs (`BLOCK SEXTANT-…` plus
//!   space and the half/full blocks), the oracle for emde's sextant formula.
//!
//! `--check` regenerates the files in memory and fails if a checked-in file
//! differs.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A pinned `UnicodeData.txt`.
struct Ucd {
    version: &'static str,
    sha256: &'static str,
}

impl Ucd {
    fn url(&self) -> String {
        format!(
            "https://www.unicode.org/Public/{}/ucd/UnicodeData.txt",
            self.version
        )
    }
}

/// The UCD version kitty's placeholder diacritics are defined against.
const UCD_DIACRITICS: Ucd = Ucd {
    version: "6.0.0",
    sha256: "90b45b777346bef027556f9c6cb3ea5d7d745bd60d6762855c08d8a22d34f771",
};

/// The UCD version the block glyph tables come from.
const UCD_BLOCKS: Ucd = Ucd {
    version: "17.0.0",
    sha256: "2e1efc1dcb59c575eedf5ccae60f95229f706ee6d031835247d843c11d96470c",
};

/// Marks with combining class 230 that kitty leaves out because NFC may
/// compose them with a preceding letter (e.g. `A` + U+0300 → `À`).
const DIACRITIC_EXCLUSIONS: [u32; 19] = [
    0x0300, 0x0301, 0x0302, 0x0303, 0x0304, 0x0306, 0x0307, 0x0308, 0x0309, 0x030A, 0x030B, 0x030C,
    0x030F, 0x0311, 0x0313, 0x0314, 0x0342, 0x0653, 0x0654,
];

/// Number of row/column diacritics defined by the kitty protocol.
const DIACRITIC_COUNT: usize = 297;

/// The first three diacritics (numbers 0, 1 and 2), as the kitty docs list them.
const DIACRITIC_START: [u32; 3] = [0x0305, 0x030D, 0x030E];

/// Octant patterns drawn by characters outside the `BLOCK OCTANT-…` range:
/// (mask, code point, expected UCD name). Bit `n − 1` is octant `n`, counted
/// left to right, then top to bottom.
const OCTANT_EXTRAS: [(u8, u32, &str); 26] = [
    (0x00, 0x0020, "SPACE"),
    (0xFF, 0x2588, "FULL BLOCK"),
    (0x0F, 0x2580, "UPPER HALF BLOCK"),
    (0xF0, 0x2584, "LOWER HALF BLOCK"),
    (0x55, 0x258C, "LEFT HALF BLOCK"),
    (0xAA, 0x2590, "RIGHT HALF BLOCK"),
    (0x50, 0x2596, "QUADRANT LOWER LEFT"),
    (0xA0, 0x2597, "QUADRANT LOWER RIGHT"),
    (0x05, 0x2598, "QUADRANT UPPER LEFT"),
    (
        0xF5,
        0x2599,
        "QUADRANT UPPER LEFT AND LOWER LEFT AND LOWER RIGHT",
    ),
    (0xA5, 0x259A, "QUADRANT UPPER LEFT AND LOWER RIGHT"),
    (
        0x5F,
        0x259B,
        "QUADRANT UPPER LEFT AND UPPER RIGHT AND LOWER LEFT",
    ),
    (
        0xAF,
        0x259C,
        "QUADRANT UPPER LEFT AND UPPER RIGHT AND LOWER RIGHT",
    ),
    (0x0A, 0x259D, "QUADRANT UPPER RIGHT"),
    (0x5A, 0x259E, "QUADRANT UPPER RIGHT AND LOWER LEFT"),
    (
        0xFA,
        0x259F,
        "QUADRANT UPPER RIGHT AND LOWER LEFT AND LOWER RIGHT",
    ),
    (0x03, 0x1FB82, "UPPER ONE QUARTER BLOCK"),
    (0xC0, 0x2582, "LOWER ONE QUARTER BLOCK"),
    (0x3F, 0x1FB85, "UPPER THREE QUARTERS BLOCK"),
    (0xFC, 0x2586, "LOWER THREE QUARTERS BLOCK"),
    (0x01, 0x1CEA8, "LEFT HALF UPPER ONE QUARTER BLOCK"),
    (0x02, 0x1CEAB, "RIGHT HALF UPPER ONE QUARTER BLOCK"),
    (0x40, 0x1CEA3, "LEFT HALF LOWER ONE QUARTER BLOCK"),
    (0x80, 0x1CEA0, "RIGHT HALF LOWER ONE QUARTER BLOCK"),
    (0x14, 0x1FBE6, "MIDDLE LEFT ONE QUARTER BLOCK"),
    (0x28, 0x1FBE7, "MIDDLE RIGHT ONE QUARTER BLOCK"),
];

/// Sextant patterns outside the `BLOCK SEXTANT-…` range (bit `n − 1` is
/// sextant `n`).
const SEXTANT_EXTRAS: [(u8, u32, &str); 4] = [
    (0, 0x0020, "SPACE"),
    (21, 0x258C, "LEFT HALF BLOCK"),
    (42, 0x2590, "RIGHT HALF BLOCK"),
    (63, 0x2588, "FULL BLOCK"),
];

/// Regenerate (or with `check`, verify) the graphics tables.
pub(crate) fn run(check: bool) -> Result<(), String> {
    let root = repo_root()?;
    let cache = crate::target_dir()?.join("ucd");
    let old = load(&UCD_DIACRITICS, &cache)?;
    let new = load(&UCD_BLOCKS, &cache)?;
    let old = UnicodeData::parse(&old)?;
    let new = UnicodeData::parse(&new)?;

    let outputs = [
        (
            "src/gfx/gen/diacritics.rs",
            render_diacritics(&diacritics(&old)?),
        ),
        ("src/gfx/gen/octants.rs", render_octants(&octants(&new)?)),
        ("src/gfx/gen/sextants.rs", render_sextants(&sextants(&new)?)),
    ];
    let mut stale = Vec::new();
    for (rel, text) in &outputs {
        let path = root.join(rel);
        let current = fs::read_to_string(&path).ok();
        if current.as_deref() == Some(text.as_str()) {
            continue;
        }
        if check {
            stale.push(*rel);
        } else {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            eprintln!("xtask: wrote {rel}");
        }
    }
    if stale.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "generated graphics tables are out of date: {} (run `cargo xtask gen`)",
            stale.join(", ")
        ))
    }
}

/// The repository root (the parent of the xtask crate).
fn repo_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot locate the repository root".to_string())
}

/// Read a pinned UCD file from the cache, downloading it on first use.
fn load(ucd: &Ucd, cache: &Path) -> Result<String, String> {
    let dir = cache.join(ucd.version);
    let path = dir.join("UnicodeData.txt");
    if !path.exists() {
        download(ucd, &dir, &path)?;
    }
    let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let digest = hex(&sha256(&bytes));
    if digest != ucd.sha256 {
        return Err(format!(
            "{} has SHA-256 {digest}, expected {} (delete it to download it again)",
            path.display(),
            ucd.sha256
        ));
    }
    String::from_utf8(bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Fetch a UCD file with `curl` into `path` (via a temporary file, so an
/// interrupted download never leaves a truncated cache entry).
fn download(ucd: &Ucd, dir: &Path, path: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let partial = dir.join("UnicodeData.txt.part");
    let url = ucd.url();
    eprintln!("xtask: downloading {url}");
    let status = Command::new("curl")
        .args(["--proto", "=https", "--silent", "--show-error", "--fail"])
        .args(["--location", "--retry", "2", "--user-agent", "emde-dev"])
        .arg("--output")
        .arg(&partial)
        .arg(&url)
        .status()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !status.success() {
        let _ = fs::remove_file(&partial);
        return Err(format!("downloading {url} failed ({status})"));
    }
    fs::rename(&partial, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// One record of `UnicodeData.txt` (the fields the generators use).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Record<'a> {
    code: u32,
    name: &'a str,
    category: &'a str,
    combining_class: &'a str,
    bidi: &'a str,
    decomposition: &'a str,
}

/// The records of one `UnicodeData.txt`, in file order.
struct UnicodeData<'a> {
    records: Vec<Record<'a>>,
    by_code: HashMap<u32, usize>,
}

impl<'a> UnicodeData<'a> {
    fn parse(text: &'a str) -> Result<Self, String> {
        let mut records = Vec::new();
        for (n, line) in text.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split(';').collect();
            let [
                code,
                name,
                category,
                combining_class,
                bidi,
                decomposition,
                ..,
            ] = fields[..]
            else {
                return Err(format!("UnicodeData.txt:{}: malformed record", n + 1));
            };
            let code = u32::from_str_radix(code, 16)
                .map_err(|e| format!("UnicodeData.txt:{}: {e}", n + 1))?;
            records.push(Record {
                code,
                name,
                category,
                combining_class,
                bidi,
                decomposition,
            });
        }
        let by_code = records
            .iter()
            .enumerate()
            .map(|(i, r)| (r.code, i))
            .collect();
        Ok(UnicodeData { records, by_code })
    }

    fn name(&self, code: u32) -> Option<&'a str> {
        self.by_code
            .get(&code)
            .and_then(|&i| self.records.get(i))
            .map(|r| r.name)
    }
}

/// A generated table entry: the character and its UCD name.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Glyph {
    code: u32,
    name: String,
}

/// kitty's row/column diacritics, in number order.
fn diacritics(ucd: &UnicodeData<'_>) -> Result<Vec<Glyph>, String> {
    let list: Vec<Glyph> = ucd
        .records
        .iter()
        .filter(|r| {
            r.category == "Mn"
                && r.combining_class == "230"
                && r.bidi == "NSM"
                && r.decomposition.is_empty()
                && !DIACRITIC_EXCLUSIONS.contains(&r.code)
        })
        .map(|r| Glyph {
            code: r.code,
            name: r.name.to_string(),
        })
        .collect();
    if list.len() != DIACRITIC_COUNT {
        return Err(format!(
            "expected {DIACRITIC_COUNT} placeholder diacritics, found {}",
            list.len()
        ));
    }
    let start: Vec<u32> = list.iter().take(3).map(|g| g.code).collect();
    if start != DIACRITIC_START {
        return Err(format!(
            "placeholder diacritics start {start:04X?}, expected {DIACRITIC_START:04X?}"
        ));
    }
    Ok(list)
}

/// Parse the cell list of a `BLOCK OCTANT-1357` / `BLOCK SEXTANT-12` name into
/// a mask (bit `n − 1` for cell `n`). Cells must be strictly increasing and
/// at most `cells`.
fn cell_mask(digits: &str, cells: u32) -> Option<u8> {
    let mut mask = 0u8;
    let mut last = 0;
    for c in digits.chars() {
        let n = c.to_digit(10)?;
        if n <= last || n > cells {
            return None;
        }
        last = n;
        mask |= 1 << (n - 1);
    }
    (mask != 0).then_some(mask)
}

/// Build a mask-indexed glyph table from the named `<prefix>…` characters
/// plus the listed extras, checking that every mask is covered exactly once.
fn block_table(
    ucd: &UnicodeData<'_>,
    prefix: &str,
    cells: u32,
    expected_named: usize,
    extras: &[(u8, u32, &str)],
) -> Result<Vec<Glyph>, String> {
    let mut table: Vec<Option<Glyph>> = vec![None; 1 << cells];
    let mut put = |mask: u8, glyph: Glyph| -> Result<(), String> {
        let slot = table
            .get_mut(usize::from(mask))
            .ok_or_else(|| format!("{prefix} mask {mask:#04x} is out of range"))?;
        if let Some(prev) = slot {
            return Err(format!(
                "{prefix} mask {mask:#04x} is drawn by both U+{:04X} and U+{:04X}",
                prev.code, glyph.code
            ));
        }
        *slot = Some(glyph);
        Ok(())
    };
    let mut named = 0;
    for r in &ucd.records {
        let Some(digits) = r.name.strip_prefix(prefix) else {
            continue;
        };
        let mask = cell_mask(digits, cells)
            .ok_or_else(|| format!("U+{:04X}: unexpected name {}", r.code, r.name))?;
        let glyph = Glyph {
            code: r.code,
            name: r.name.to_string(),
        };
        put(mask, glyph)?;
        named += 1;
    }
    if named != expected_named {
        return Err(format!(
            "expected {expected_named} `{prefix}…` characters, found {named}"
        ));
    }
    for &(mask, code, name) in extras {
        match ucd.name(code) {
            Some(actual) if actual == name => {}
            actual => {
                return Err(format!(
                    "U+{code:04X} is {actual:?} in the UCD, expected {name:?}"
                ));
            }
        }
        let glyph = Glyph {
            code,
            name: name.to_string(),
        };
        put(mask, glyph)?;
    }
    table
        .into_iter()
        .enumerate()
        .map(|(mask, g)| g.ok_or_else(|| format!("{prefix} mask {mask:#04x} has no glyph")))
        .collect()
}

/// The 256 octant glyphs, indexed by mask.
fn octants(ucd: &UnicodeData<'_>) -> Result<Vec<Glyph>, String> {
    block_table(ucd, "BLOCK OCTANT-", 8, 230, &OCTANT_EXTRAS)
}

/// The 64 sextant glyphs, indexed by mask.
fn sextants(ucd: &UnicodeData<'_>) -> Result<Vec<Glyph>, String> {
    block_table(ucd, "BLOCK SEXTANT-", 6, 60, &SEXTANT_EXTRAS)
}

/// A Rust char literal for a code point: ASCII stays literal, everything else
/// is a `\u{…}` escape so the generated source is plain ASCII.
fn char_literal(code: u32) -> String {
    match char::from_u32(code) {
        Some(c) if c.is_ascii_graphic() || c == ' ' => format!("'{c}'"),
        _ => format!("'\\u{{{code:X}}}'"),
    }
}

/// Render a module with one `const NAME: [char; N]` table, one commented
/// entry per line. Comments are aligned the way rustfmt aligns them, so the
/// output is already formatted.
fn render_table(
    module_doc: &str,
    item_doc: &str,
    name: &str,
    glyphs: &[Glyph],
    label: impl Fn(usize) -> String,
) -> String {
    let mut out = String::new();
    for line in module_doc.lines() {
        if line.is_empty() {
            out.push_str("//!\n");
        } else {
            let _ = writeln!(out, "//! {line}");
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "/// {item_doc}");
    let _ = writeln!(out, "pub(crate) const {name}: [char; {}] = [", glyphs.len());
    let items: Vec<String> = glyphs
        .iter()
        .map(|g| format!("{},", char_literal(g.code)))
        .collect();
    let width = items.iter().map(String::len).max().unwrap_or(0);
    for (i, (item, g)) in items.iter().zip(glyphs).enumerate() {
        let _ = writeln!(out, "    {item:<width$} // {} {}", label(i), g.name);
    }
    out.push_str("];\n");
    out
}

fn render_diacritics(glyphs: &[Glyph]) -> String {
    let v = UCD_DIACRITICS.version;
    let doc = format!(
        "Row/column diacritics of kitty's Unicode placeholders: `DIACRITICS[n]`
is the combining mark that encodes the number `n`.

kitty's rule (its `gen/rowcolumn-diacritics.txt`): every character of
Unicode {v} with general category `Mn`, combining class 230, bidi class
`NSM` and no decomposition mapping, except 19 marks that NFC may fuse
with a preceding letter.

Generated by `cargo xtask gen` from the Unicode {v} `UnicodeData.txt`;
do not edit by hand."
    );
    render_table(
        &doc,
        "Placeholder diacritics in number order.",
        "DIACRITICS",
        glyphs,
        |i| i.to_string(),
    )
}

fn render_octants(glyphs: &[Glyph]) -> String {
    let v = UCD_BLOCKS.version;
    let doc = format!(
        "Octant glyphs for text-mode images, indexed by sub-pixel mask.

Bit `n − 1` of a mask is octant `n` in Unicode's numbering: left to
right, then top to bottom, so bit 0 is the top-left eighth of the cell
and bit 7 the bottom-right one. 230 masks are `BLOCK OCTANT-…`
characters; the other 26 are older block elements of the same shape.

Generated by `cargo xtask gen` from the Unicode {v} `UnicodeData.txt`;
do not edit by hand."
    );
    render_table(
        &doc,
        "The glyph that inks exactly the octants set in the mask.",
        "OCTANTS",
        glyphs,
        |i| format!("{i:#010b}"),
    )
}

fn render_sextants(glyphs: &[Glyph]) -> String {
    let v = UCD_BLOCKS.version;
    let doc = format!(
        "Sextant glyphs for text-mode images, indexed by sub-pixel mask.

Bit `n − 1` of a mask is sextant `n` in Unicode's numbering: left to
right, then top to bottom (bit 0 top-left, bit 5 bottom-right). emde
computes sextant glyphs with a formula; this table is its test oracle.

Generated by `cargo xtask gen` from the Unicode {v} `UnicodeData.txt`;
do not edit by hand."
    );
    render_table(
        &doc,
        "The glyph that inks exactly the sextants set in the mask.",
        "SEXTANTS",
        glyphs,
        |i| format!("{i:#08b}"),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256 (FIPS 180-4), used to pin the downloaded UCD files.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bits = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (k, wi) in K.iter().zip(w) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(wi);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(v);
        }
    }
    let mut out = [0u8; 32];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *chunk = word.to_be_bytes();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // A full 64-byte block forces an extra, padding-only block.
        assert_eq!(
            hex(&sha256(&[b'a'; 64])),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn cell_masks() {
        assert_eq!(cell_mask("1", 8), Some(0x01));
        assert_eq!(cell_mask("1357", 8), Some(0x55));
        assert_eq!(cell_mask("2345678", 8), Some(0xFE));
        assert_eq!(cell_mask("23456", 6), Some(0x3E));
        assert_eq!(cell_mask("21", 8), None, "cells must increase");
        assert_eq!(cell_mask("7", 6), None, "sextants have six cells");
        assert_eq!(cell_mask("", 8), None);
        assert_eq!(cell_mask("1a", 8), None);
    }

    #[test]
    fn char_literals() {
        assert_eq!(char_literal(0x20), "' '");
        assert_eq!(char_literal(0x41), "'A'");
        assert_eq!(char_literal(0x305), "'\\u{305}'");
        assert_eq!(char_literal(0x1CD00), "'\\u{1CD00}'");
    }

    /// The diacritic rule keeps only NSM marks of class 230 without a
    /// decomposition that are not on the exclusion list.
    #[test]
    fn diacritic_rule() {
        let text = "\
0300;COMBINING GRAVE ACCENT;Mn;230;NSM;;;;;N;;;;;
0305;COMBINING OVERLINE;Mn;230;NSM;;;;;N;;;;;
0316;COMBINING GRAVE ACCENT BELOW;Mn;220;NSM;;;;;N;;;;;
0340;COMBINING GRAVE TONE MARK;Mn;230;NSM;0300;;;;N;;;;;
0041;LATIN CAPITAL LETTER A;Lu;0;L;;;;;N;;;;0061;
";
        let ucd = UnicodeData::parse(text).unwrap();
        let err = diacritics(&ucd).unwrap_err();
        assert!(err.contains("found 1"), "{err}");
    }

    #[test]
    fn block_table_checks_coverage() {
        let text = "\
0020;SPACE;Zs;0;WS;;;;;N;;;;;
2580;UPPER HALF BLOCK;So;0;ON;;;;;N;;;;;
2584;LOWER HALF BLOCK;So;0;ON;;;;;N;;;;;
2588;FULL BLOCK;So;0;ON;;;;;N;;;;;
";
        let ucd = UnicodeData::parse(text).unwrap();
        // A two-cell (half block) table built from extras only.
        let extras = [
            (0, 0x20, "SPACE"),
            (1, 0x2580, "UPPER HALF BLOCK"),
            (2, 0x2584, "LOWER HALF BLOCK"),
            (3, 0x2588, "FULL BLOCK"),
        ];
        let table = block_table(&ucd, "BLOCK HALF-", 2, 0, &extras).unwrap();
        let codes: Vec<u32> = table.iter().map(|g| g.code).collect();
        assert_eq!(codes, [0x20, 0x2580, 0x2584, 0x2588]);

        let missing = block_table(&ucd, "BLOCK HALF-", 2, 0, &extras[..3]).unwrap_err();
        assert!(missing.contains("has no glyph"), "{missing}");
        let clash = [(0, 0x20, "SPACE"), (0, 0x2588, "FULL BLOCK")];
        let clash = block_table(&ucd, "BLOCK HALF-", 2, 0, &clash).unwrap_err();
        assert!(clash.contains("both"), "{clash}");
        let renamed = [(0, 0x20, "NOT SPACE")];
        let renamed = block_table(&ucd, "BLOCK HALF-", 2, 0, &renamed).unwrap_err();
        assert!(renamed.contains("expected"), "{renamed}");
        let counted = block_table(&ucd, "BLOCK HALF-", 2, 1, &extras).unwrap_err();
        assert!(counted.contains("found 0"), "{counted}");
    }

    #[test]
    fn malformed_records_are_errors() {
        assert!(UnicodeData::parse("0041;A\n").is_err());
        assert!(UnicodeData::parse("XYZ;A;Lu;0;L;;;;;N;;;;;\n").is_err());
    }
}
