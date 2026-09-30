//! Reading input (file or stdin) and sanitising it.
//!
//! [`Source::load`] is the first pipeline stage. Whatever the bytes are, it
//! produces text the rest of emde can trust:
//!
//! * at most [`MAX_INPUT_BYTES`] are read (a diagnostic says when the input
//!   was cut);
//! * a UTF-8 byte-order mark is dropped, and UTF-16 with a byte-order mark
//!   is decoded;
//! * invalid UTF-8 is replaced with `U+FFFD` (with a diagnostic);
//! * CRLF and lone CR become LF;
//! * control characters other than `\n` and `\t` become visible control
//!   pictures (`ESC` → `␛`, see [`crate::text::sanitize`]), so a document can
//!   never inject escape sequences.

use std::fmt;
use std::fs;
use std::io::{self, IsTerminal as _, Read};
use std::path::{Path, PathBuf};

use crate::text::sanitize::{is_unsafe_control, push_picture};

/// Largest input read, in bytes (256 MiB). Longer input is truncated.
pub const MAX_INPUT_BYTES: usize = 256 * 1024 * 1024;

/// Where a document came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A file (after resolving a directory to its README).
    File(PathBuf),
    /// Standard input.
    Stdin,
    /// Text supplied by the program (tests, `--print-default-config`, …).
    Memory,
}

impl Origin {
    /// Directory that relative links and images resolve against.
    pub fn base_dir(&self) -> Option<PathBuf> {
        match self {
            Origin::File(path) => Some(match path.parent() {
                Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
                _ => PathBuf::from("."),
            }),
            Origin::Stdin | Origin::Memory => None,
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::File(path) => write!(f, "{}", path.display()),
            Origin::Stdin => f.write_str("<stdin>"),
            Origin::Memory => f.write_str("<memory>"),
        }
    }
}

/// What kind of problem a [`Diagnostic`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagnosticKind {
    /// Bytes that were not valid UTF-8 (or UTF-16) were replaced.
    InvalidUtf8,
    /// Control characters were replaced with control pictures.
    ControlCharacters,
    /// The input exceeded [`MAX_INPUT_BYTES`] and was cut.
    Truncated,
    /// Markup emde could not represent faithfully (shown with a fallback).
    Markup,
}

/// A content problem emde worked around. Shown with `-v`; never fatal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// What went wrong.
    pub kind: DiagnosticKind,
    /// A one-line description for the user.
    pub message: String,
}

impl Diagnostic {
    /// A diagnostic of the given kind.
    pub fn new(kind: DiagnosticKind, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// A user-facing input error (exit status 1).
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// No file was given and standard input is a terminal.
    #[error("no input: pass a Markdown file, or pipe Markdown into standard input")]
    NoInput,
    /// Reading failed.
    #[error("{name}: {source}")]
    Io {
        /// The path, or `<stdin>`.
        name: String,
        #[source]
        source: io::Error,
    },
    /// A directory was given but it has no README.
    #[error("{}: directory has no README", .0.display())]
    NoReadme(PathBuf),
}

/// Which input to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// Standard input.
    Stdin,
    /// A file or a directory (shown through its README).
    Path(PathBuf),
}

impl Input {
    /// The input for a command-line argument: `-` means standard input, and
    /// no argument means standard input when it is not a terminal.
    pub fn from_arg(arg: Option<&Path>) -> Result<Input, SourceError> {
        Input::from_arg_with(arg, io::stdin().is_terminal())
    }

    /// [`Input::from_arg`] with the terminal check supplied (for tests).
    pub fn from_arg_with(
        arg: Option<&Path>,
        stdin_is_terminal: bool,
    ) -> Result<Input, SourceError> {
        match arg {
            Some(p) if p.as_os_str() == "-" => Ok(Input::Stdin),
            Some(p) => Ok(Input::Path(p.to_path_buf())),
            None if stdin_is_terminal => Err(SourceError::NoInput),
            None => Ok(Input::Stdin),
        }
    }
}

/// A decoded, sanitised document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The text: valid UTF-8, LF line endings, no control characters other
    /// than `\n` and `\t`.
    pub text: String,
    /// Where the text came from.
    pub origin: Origin,
    /// Problems found while decoding (never fatal).
    pub diagnostics: Vec<Diagnostic>,
}

impl Source {
    /// Read an input. A directory is shown through its README.
    pub fn load(input: &Input) -> Result<Source, SourceError> {
        match input {
            Input::Stdin => {
                let (bytes, truncated) = read_capped(io::stdin().lock(), 0, MAX_INPUT_BYTES)
                    .map_err(|source| SourceError::Io {
                        name: Origin::Stdin.to_string(),
                        source,
                    })?;
                Ok(Source::decode(bytes, truncated, Origin::Stdin))
            }
            Input::Path(path) => {
                let path = if path.is_dir() {
                    find_readme(path).ok_or_else(|| SourceError::NoReadme(path.clone()))?
                } else {
                    path.clone()
                };
                let io_err = |source| SourceError::Io {
                    name: path.display().to_string(),
                    source,
                };
                let file = fs::File::open(&path).map_err(io_err)?;
                let hint = file
                    .metadata()
                    .ok()
                    .and_then(|m| usize::try_from(m.len()).ok())
                    .unwrap_or(0);
                let (bytes, truncated) =
                    read_capped(file, hint, MAX_INPUT_BYTES).map_err(io_err)?;
                Ok(Source::decode(bytes, truncated, Origin::File(path)))
            }
        }
    }

    /// Decode and sanitise raw bytes (at most [`MAX_INPUT_BYTES`] are kept).
    pub fn from_bytes(mut bytes: Vec<u8>, origin: Origin) -> Source {
        let truncated = bytes.len() > MAX_INPUT_BYTES;
        bytes.truncate(MAX_INPUT_BYTES);
        Source::decode(bytes, truncated, origin)
    }

    /// Sanitise text supplied by the program.
    pub fn from_text(text: &str) -> Source {
        Source::decode(text.as_bytes().to_vec(), false, Origin::Memory)
    }

    /// Directory that relative links and images resolve against.
    pub fn base_dir(&self) -> Option<PathBuf> {
        self.origin.base_dir()
    }

    fn decode(mut bytes: Vec<u8>, truncated: bool, origin: Origin) -> Source {
        let mut diagnostics = Vec::new();
        if truncated {
            // Do not report a multi-byte character cut in half as invalid.
            let keep = complete_utf8_prefix(&bytes);
            bytes.truncate(keep);
            diagnostics.push(Diagnostic::new(
                DiagnosticKind::Truncated,
                format!(
                    "input is larger than {} MiB; showing only the beginning",
                    MAX_INPUT_BYTES >> 20
                ),
            ));
        }
        let (text, invalid) = decode_text(bytes);
        if invalid > 0 {
            diagnostics.push(Diagnostic::new(
                DiagnosticKind::InvalidUtf8,
                format!(
                    "{invalid} invalid byte sequence{} replaced with U+FFFD",
                    plural(invalid)
                ),
            ));
        }
        let (text, controls) = normalize(text);
        if controls > 0 {
            diagnostics.push(Diagnostic::new(
                DiagnosticKind::ControlCharacters,
                format!(
                    "{controls} control character{} shown as control pictures",
                    plural(controls)
                ),
            ));
        }
        Source {
            text,
            origin,
            diagnostics,
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Find the README of a directory: `README.md` first, then (ignoring case)
/// `readme.md`, `readme.markdown`, `readme.mdown`, `readme.mkd`, `readme.txt`
/// and `readme`. Ties are broken by name so the choice is deterministic.
pub fn find_readme(dir: &Path) -> Option<PathBuf> {
    const RANKED: &[&str] = &[
        "readme.md",
        "readme.markdown",
        "readme.mdown",
        "readme.mkd",
        "readme.txt",
        "readme",
    ];
    let exact = dir.join("README.md");
    if exact.is_file() {
        return Some(exact);
    }
    let mut best: Option<(usize, String, PathBuf)> = None;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let lower = name.to_ascii_lowercase();
        let Some(rank) = RANKED.iter().position(|&r| r == lower) else {
            continue;
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let better = best
            .as_ref()
            .is_none_or(|(r, n, _)| (rank, name.as_str()) < (*r, n.as_str()));
        if better {
            best = Some((rank, name, path));
        }
    }
    best.map(|(_, _, path)| path)
}

/// Read at most `cap` bytes; the flag says whether more were available.
fn read_capped(reader: impl Read, size_hint: usize, cap: usize) -> io::Result<(Vec<u8>, bool)> {
    let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::with_capacity(size_hint.min(cap).saturating_add(1));
    reader.take(limit).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > cap;
    bytes.truncate(cap);
    Ok((bytes, truncated))
}

/// Length of the longest prefix of `bytes` that does not end inside a
/// multi-byte UTF-8 sequence.
fn complete_utf8_prefix(bytes: &[u8]) -> usize {
    let len = bytes.len();
    // A sequence is at most 4 bytes: look at the last 3 for its lead byte.
    for back in 1..=3.min(len) {
        let i = len - back;
        let Some(&b) = bytes.get(i) else { break };
        if b & 0xc0 == 0x80 {
            continue; // continuation byte
        }
        let need = match b {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        return if back < need { i } else { len };
    }
    len
}

/// Decode bytes as UTF-8 (or UTF-16 with a byte-order mark), dropping a
/// UTF-8 byte-order mark. Returns the text and the number of replaced
/// invalid sequences.
fn decode_text(mut bytes: Vec<u8>) -> (String, usize) {
    match bytes.as_slice() {
        [0xef, 0xbb, 0xbf, ..] => {
            bytes.drain(..3);
        }
        [0xff, 0xfe, rest @ ..] => return decode_utf16(rest, u16::from_le_bytes),
        [0xfe, 0xff, rest @ ..] => return decode_utf16(rest, u16::from_be_bytes),
        _ => {}
    }
    match String::from_utf8(bytes) {
        Ok(text) => (text, 0),
        Err(err) => {
            let bytes = err.into_bytes();
            let mut text = String::with_capacity(bytes.len());
            let mut invalid = 0;
            for chunk in bytes.utf8_chunks() {
                text.push_str(chunk.valid());
                if !chunk.invalid().is_empty() {
                    text.push(char::REPLACEMENT_CHARACTER);
                    invalid += 1;
                }
            }
            (text, invalid)
        }
    }
}

fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> (String, usize) {
    let (pairs, odd) = bytes.as_chunks::<2>();
    let mut invalid = usize::from(!odd.is_empty());
    let mut text = String::with_capacity(bytes.len());
    for r in char::decode_utf16(pairs.iter().map(|&pair| unit(pair))) {
        text.push(r.unwrap_or_else(|_| {
            invalid += 1;
            char::REPLACEMENT_CHARACTER
        }));
    }
    if !odd.is_empty() {
        text.push(char::REPLACEMENT_CHARACTER);
    }
    (text, invalid)
}

/// Normalise line endings to LF and replace unsafe control characters.
/// Returns the text and the number of replaced control characters.
fn normalize(text: String) -> (String, usize) {
    let needs_work = text
        .bytes()
        .any(|b| (b < 0x20 && b != b'\n' && b != b'\t') || b == 0x7f || b == 0xc2);
    if !needs_work {
        return (text, 0);
    }
    let mut out = String::with_capacity(text.len());
    let mut controls = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            // CRLF → LF, and a lone CR is a line ending too (CommonMark).
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else if is_unsafe_control(c) {
            push_picture(&mut out, c);
            controls += 1;
        } else {
            out.push(c);
        }
    }
    (out, controls)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(bytes: &[u8]) -> Source {
        Source::from_bytes(bytes.to_vec(), Origin::Memory)
    }

    fn kinds(s: &Source) -> Vec<DiagnosticKind> {
        s.diagnostics.iter().map(|d| d.kind).collect()
    }

    #[test]
    fn clean_input_is_unchanged() {
        let s = src("# Title\n\nsome *text*\twith tab\n".as_bytes());
        assert_eq!(s.text, "# Title\n\nsome *text*\twith tab\n");
        assert!(s.diagnostics.is_empty());
    }

    #[test]
    fn strips_utf8_bom() {
        assert_eq!(src(b"\xef\xbb\xbfhi").text, "hi");
        // Only a leading BOM is removed.
        assert_eq!(src("a\u{feff}b".as_bytes()).text, "a\u{feff}b");
    }

    #[test]
    fn decodes_utf16_with_bom() {
        assert_eq!(src(b"\xff\xfeh\x00i\x00").text, "hi");
        assert_eq!(src(b"\xfe\xff\x00h\x00i").text, "hi");
        let odd = src(b"\xff\xfeh\x00i");
        assert_eq!(odd.text, "h\u{fffd}");
        assert_eq!(kinds(&odd), [DiagnosticKind::InvalidUtf8]);
    }

    #[test]
    fn normalizes_line_endings() {
        assert_eq!(src(b"a\r\nb\rc\n").text, "a\nb\nc\n");
        assert_eq!(src(b"\r\r\n").text, "\n\n");
        assert!(src(b"a\r\nb").diagnostics.is_empty());
    }

    #[test]
    fn invalid_utf8_is_replaced_with_a_diagnostic() {
        let s = src(b"ok \xff\xfe bad \xc3");
        assert_eq!(s.text, "ok \u{fffd}\u{fffd} bad \u{fffd}");
        assert_eq!(kinds(&s), [DiagnosticKind::InvalidUtf8]);
        assert!(s.diagnostics[0].message.starts_with("3 invalid"));
    }

    #[test]
    fn controls_become_pictures() {
        let s = src(b"\x1b[31mred\x1b[0m \x07 \x7f \xc2\x9b2J");
        assert_eq!(s.text, "␛[31mred␛[0m ␇ ␡ ␛[2J");
        assert_eq!(kinds(&s), [DiagnosticKind::ControlCharacters]);
        assert!(s.diagnostics[0].message.starts_with("5 control characters"));
        assert!(!s.text.contains('\x1b'));
    }

    #[test]
    fn truncation_keeps_whole_characters() {
        let (bytes, truncated) = read_capped(&b"abcdef"[..], 0, 4).unwrap();
        assert_eq!((bytes.as_slice(), truncated), (&b"abcd"[..], true));
        let (bytes, truncated) = read_capped(&b"abcd"[..], 0, 4).unwrap();
        assert_eq!((bytes.as_slice(), truncated), (&b"abcd"[..], false));
        // "aé" cut after the first byte of é.
        let s = Source::decode(b"a\xc3".to_vec(), true, Origin::Memory);
        assert_eq!(s.text, "a");
        assert_eq!(kinds(&s), [DiagnosticKind::Truncated]);
        assert_eq!(complete_utf8_prefix("a€".as_bytes()), 4);
        assert_eq!(complete_utf8_prefix(&"a€".as_bytes()[..3]), 1);
        assert_eq!(complete_utf8_prefix(&[0xf0, 0x9f, 0x98]), 0);
        assert_eq!(complete_utf8_prefix(b""), 0);
    }

    #[test]
    fn input_selection() {
        assert_eq!(
            Input::from_arg_with(Some(Path::new("-")), true).unwrap(),
            Input::Stdin
        );
        assert_eq!(Input::from_arg_with(None, false).unwrap(), Input::Stdin);
        assert!(matches!(
            Input::from_arg_with(None, true),
            Err(SourceError::NoInput)
        ));
        assert_eq!(
            Input::from_arg_with(Some(Path::new("x.md")), true).unwrap(),
            Input::Path("x.md".into())
        );
    }

    #[test]
    fn base_dir_of_origins() {
        assert_eq!(
            Origin::File("docs/a.md".into()).base_dir(),
            Some(PathBuf::from("docs"))
        );
        assert_eq!(
            Origin::File("a.md".into()).base_dir(),
            Some(PathBuf::from("."))
        );
        assert_eq!(Origin::Stdin.base_dir(), None);
    }

    /// A unique scratch directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let dir =
                std::env::temp_dir().join(format!("emde-source-test-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn loads_files_and_directory_readmes() {
        let tmp = TempDir::new("load");
        let file = tmp.0.join("doc.md");
        fs::write(&file, b"# Hi\r\n").unwrap();
        let s = Source::load(&Input::Path(file.clone())).unwrap();
        assert_eq!(s.text, "# Hi\n");
        assert_eq!(s.origin, Origin::File(file));

        assert!(matches!(
            Source::load(&Input::Path(tmp.0.clone())),
            Err(SourceError::NoReadme(_))
        ));
        fs::write(tmp.0.join("readme.txt"), b"txt").unwrap();
        fs::write(tmp.0.join("Readme.md"), b"md").unwrap();
        let s = Source::load(&Input::Path(tmp.0.clone())).unwrap();
        assert_eq!(s.text, "md");

        let missing = tmp.0.join("missing.md");
        let err = Source::load(&Input::Path(missing)).unwrap_err();
        assert!(matches!(err, SourceError::Io { .. }));
        assert!(err.to_string().contains("missing.md"));
    }
}
