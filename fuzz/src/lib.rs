//! Code shared by emde's fuzz targets (`fuzz_targets/`).
//!
//! * [`init`] sets the panic policy: every panic is a finding, except one
//!   raised inside pulldown-latex, which emde contains on purpose (the
//!   formula is shown as TeX) and which is not emde's to fix.
//! * [`header`] splits an input into option bytes and the payload.
//! * [`check_escapes`] checks that rendered bytes only hold emde's own
//!   escape sequences, with every OSC 8 link closed on its line and its URI
//!   made safe.

use std::panic::{self, PanicHookInfo};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

/// Third-party code whose panics emde catches and turns into a fallback.
const CONTAINED: &[&str] = &["/pulldown-latex-", "/pulldown_latex/"];

/// Install the panic policy (once per process).
///
/// libFuzzer's hook aborts on every panic, before unwinding, so a panic
/// that emde catches with `catch_unwind` would still end the run. Panics in
/// [`CONTAINED`] code return from the hook instead and unwind into emde's
/// guard; the first one is reported on standard error. Everything else goes
/// to libFuzzer's hook: a crash.
pub fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let libfuzzer = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if contained(info) {
                report_contained(info);
                return;
            }
            libfuzzer(info);
        }));
    });
}

/// Whether a panic was raised in code emde contains.
fn contained(info: &PanicHookInfo<'_>) -> bool {
    info.location()
        .is_some_and(|l| CONTAINED.iter().any(|c| l.file().contains(c)))
}

/// Say once per process that a contained panic happened.
fn report_contained(info: &PanicHookInfo<'_>) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if !REPORTED.swap(true, Ordering::Relaxed) {
        let location = info
            .location()
            .map_or_else(String::new, |l| format!(" at {l}"));
        // Shown once; libFuzzer's output is on standard error too.
        #[allow(clippy::print_stderr)]
        {
            eprintln!("emde-fuzz: contained third-party panic{location} (not reported again)");
        }
    }
}

/// The first `N` bytes of `data` (zero when it is shorter) and the rest.
pub fn header<const N: usize>(data: &[u8]) -> ([u8; N], &[u8]) {
    let mut head = [0u8; N];
    let n = data.len().min(N);
    head[..n].copy_from_slice(&data[..n]);
    (head, &data[n..])
}

/// What a line of rendered output shows, once its escape sequences are
/// taken out.
///
/// The only escapes allowed are SGR (`ESC [ params m`, with digits, `;`
/// and `:`) and OSC 8 hyperlinks (`ESC ] 8 ; params ; URI ESC \`) whose
/// parameters and URI hold no control characters, each opened link closed
/// on the same line. Any other escape, or a control character anywhere,
/// came from the document: an error.
pub fn check_escapes(line: &str) -> Result<String, String> {
    let mut visible = String::with_capacity(line.len());
    let mut link_open = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c.is_control() {
                return Err(format!("control character {c:?} in {line:?}"));
            }
            visible.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut ok = false;
                for p in chars.by_ref() {
                    if p == 'm' {
                        ok = true;
                        break;
                    }
                    if !(p.is_ascii_digit() || p == ';' || p == ':') {
                        break;
                    }
                }
                if !ok {
                    return Err(format!("an escape that is not SGR in {line:?}"));
                }
            }
            Some(']') => {
                let mut body = String::new();
                let mut terminated = false;
                while let Some(p) = chars.next() {
                    if p == '\u{1b}' {
                        terminated = chars.next() == Some('\\');
                        break;
                    }
                    if p.is_control() {
                        return Err(format!("control character {p:?} in an OSC in {line:?}"));
                    }
                    body.push(p);
                }
                let uri = body
                    .strip_prefix("8;")
                    .and_then(|rest| rest.split_once(';'))
                    .map(|(_, uri)| uri);
                match (terminated, uri) {
                    (true, Some("")) if link_open => link_open = false,
                    (true, Some(uri)) if !uri.is_empty() && !link_open => {
                        check_uri(uri)?;
                        link_open = true;
                    }
                    _ => return Err(format!("a bad or unbalanced OSC 8 in {line:?}")),
                }
            }
            _ => return Err(format!("a raw ESC in {line:?}")),
        }
    }
    if link_open {
        return Err(format!("an OSC 8 link left open in {line:?}"));
    }
    Ok(visible)
}

/// The longest URI emde links (`render::osc8::MAX_URL`).
const MAX_URI: usize = emde::render::osc8::MAX_URL;

/// Schemes emde never links: they run code when opened.
const REFUSED_SCHEMES: [&str; 3] = ["javascript", "vbscript", "data"];

/// An OSC 8 URI as emde promises it: printable ASCII only (everything else
/// percent-encoded), at most [`MAX_URI`] bytes, and no scheme that runs code.
fn check_uri(uri: &str) -> Result<(), String> {
    if uri.len() > MAX_URI {
        return Err(format!("an OSC 8 URI of {} bytes", uri.len()));
    }
    if !uri.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(format!("an OSC 8 URI with a byte to encode: {uri:?}"));
    }
    let scheme = uri.split_once(':').map_or("", |(scheme, _)| scheme);
    if REFUSED_SCHEMES
        .iter()
        .any(|r| scheme.eq_ignore_ascii_case(r))
    {
        return Err(format!("a linked {scheme}: URI"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers() {
        let (h, rest) = header::<3>(b"ab");
        assert_eq!((h, rest), ([b'a', b'b', 0], &b""[..]));
        let (h, rest) = header::<1>(b"xyz");
        assert_eq!((h, rest), (*b"x", &b"yz"[..]));
    }

    #[test]
    fn escapes() {
        let ok = "\u{1b}[1;38:2::1:2:3mhi\u{1b}]8;id=e1-0;https://x.org\u{1b}\\link\
                  \u{1b}]8;;\u{1b}\\\u{1b}[0m";
        assert_eq!(check_escapes(ok).unwrap(), "hilink");
        for bad in [
            "\u{1b}[2J",
            "a\u{1b}b",
            "\u{1b}]8;;https://x\u{1b}\\",
            "\u{1b}]8;;\u{1b}\\",
            "\u{1b}]8;;https://x\u{7}y\u{1b}\\\u{1b}]8;;\u{1b}\\",
            "\u{1b}]0;title\u{1b}\\",
            "\u{1b}]8;;JavaScript:x\u{1b}\\a\u{1b}]8;;\u{1b}\\",
            "\u{1b}]8;;https://x/é\u{1b}\\a\u{1b}]8;;\u{1b}\\",
            "\u{9b}2J",
            "tab\there",
        ] {
            assert!(check_escapes(bad).is_err(), "{bad:?}");
        }
    }
}
