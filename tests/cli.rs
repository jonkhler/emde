//! The binary end to end: output modes, exit statuses, standard input,
//! several files, dumps, and a reader that goes away.

use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};

fn emde() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_emde"));
    // Deterministic: no inherited colour settings or terminal hints.
    for var in emde::term::env::VARS {
        cmd.env_remove(var);
    }
    cmd.env("TERM", "xterm-256color");
    cmd
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/md/{name}.md", env!("CARGO_MANIFEST_DIR"))
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("run emde")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn piped_output_has_no_escapes() {
    let o = run(emde().arg(fixture("kitchen-sink")));
    assert!(o.status.success(), "{o:?}");
    let out = stdout(&o);
    assert!(!out.contains('\u{1b}'), "escapes in piped output");
    assert!(out.contains("Kitchen Sink"));
    // Numbered references stand in for OSC 8.
    assert!(out.contains("[1]: https://example.com"), "{out}");
    for line in out.lines() {
        assert!(emde::text::str_width(line, false) <= 80, "{line:?}");
    }
}

#[test]
fn forced_colour_and_links() {
    let o = run(emde()
        .args(["--color=always", "--hyperlinks=always"])
        .arg(fixture("links")));
    assert!(o.status.success());
    let out = stdout(&o);
    assert!(out.contains("\u{1b}[38;5;"), "256 colours from TERM: {out}");
    assert!(out.contains("\u{1b}]8;id=e"), "OSC 8 links");
    let o = run(emde().args(["--color=truecolor"]).arg(fixture("links")));
    assert!(stdout(&o).contains("\u{1b}[4;38;2;"));
    let o = run(emde().args(["--color=never"]).arg(fixture("links")));
    assert!(!stdout(&o).contains('\u{1b}'));
}

#[test]
fn no_color_keeps_attributes_on_a_forced_terminal_only() {
    // NO_COLOR with output to a pipe: nothing at all.
    let o = run(emde().env("NO_COLOR", "1").arg(fixture("kitchen-sink")));
    assert!(!stdout(&o).contains('\u{1b}'));
    // FORCE_COLOR=3 forces 24-bit colour even when piped.
    let o = run(emde().env("FORCE_COLOR", "3").arg(fixture("kitchen-sink")));
    assert!(stdout(&o).contains("\u{1b}[38;2;"));
}

#[test]
fn width_flag() {
    let o = run(emde().args(["-w", "30"]).arg(fixture("regressions")));
    assert!(o.status.success());
    let out = stdout(&o);
    for line in out.lines() {
        assert!(emde::text::str_width(line, false) <= 30, "{line:?}");
    }
    // $COLUMNS is used when there is no terminal and no flag.
    let o = run(emde().env("COLUMNS", "44").arg(fixture("regressions")));
    let widest = stdout(&o)
        .lines()
        .map(|l| emde::text::str_width(l, false))
        .max()
        .unwrap_or(0);
    assert!(widest <= 44 && widest > 30, "{widest}");
}

#[test]
fn standard_input() {
    let mut child = emde()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"# From stdin\n\nhello")
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success());
    assert_eq!(
        stdout(&o),
        "From stdin\n════════════════════════════════════════════════════════════════════════════\n\nhello\n"
    );
    // `-` is standard input too.
    let mut child = emde()
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"x").unwrap();
    assert_eq!(stdout(&child.wait_with_output().unwrap()), "x\n");
}

#[test]
fn several_files_in_sequence() {
    let o = run(emde().arg(fixture("headings")).arg(fixture("footnotes")));
    assert!(o.status.success());
    let out = stdout(&o);
    let a = out.find("Heading level one").unwrap();
    let b = out.find("A sentence with a footnote").unwrap();
    assert!(a < b);
}

#[test]
fn missing_files_fail_with_one_but_the_rest_is_shown() {
    let o = run(emde()
        .arg("/nonexistent/emde-test.md")
        .arg(fixture("headings")));
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("/nonexistent/emde-test.md"));
    assert!(stdout(&o).contains("Heading level one"));
}

#[test]
fn usage_errors_exit_with_two() {
    let o = run(emde().arg("--no-such-flag"));
    assert_eq!(o.status.code(), Some(2));
    let o = run(emde().args(["--color", "rainbow"]));
    assert_eq!(o.status.code(), Some(2));
    let o = run(emde().arg("--help"));
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("--max-width"));
    assert!(!stdout(&o).contains("--dump"), "--dump is hidden");
    let o = run(emde().arg("--version"));
    assert_eq!(o.status.code(), Some(0));
}

#[test]
fn dumps() {
    let o = run(emde().args(["--dump", "ir"]).arg(fixture("headings")));
    assert!(
        stdout(&o).starts_with("h1 H0: \"Heading level one\""),
        "{}",
        stdout(&o)
    );
    let o = run(emde()
        .args(["--dump", "lines", "-w", "40"])
        .arg(fixture("headings")));
    let out = stdout(&o);
    assert!(
        out.lines().next().unwrap().contains("|Heading level one|"),
        "{out}"
    );
}

#[test]
fn a_closed_pipe_is_a_normal_end() {
    // A large document, read a few bytes, then close the pipe.
    let mut child = emde()
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let para = "lorem ipsum dolor sit amet consectetur adipiscing elit\n\n";
        let doc = para.repeat(100_000);
        let _ = stdin.write_all(doc.as_bytes());
    });
    let mut out = child.stdout.take().unwrap();
    let mut buf = [0u8; 64];
    out.read_exact(&mut buf).unwrap();
    drop(out);
    writer.join().unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(err.is_empty(), "{err}");
}

#[test]
fn documents_cannot_inject_escapes() {
    let mut child = emde()
        .arg("--color=always")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            b"evil \x1b]52;c;AAAA\x07 and \x1b[2J and \xc2\x9b31m\n\n[x](https://a.b/\x1b]8;;)",
        )
        .unwrap();
    let o = child.wait_with_output().unwrap();
    let out = stdout(&o);
    assert!(!out.contains("\u{1b}]52"), "{out:?}");
    assert!(!out.contains("\u{1b}[2J"), "{out:?}");
    assert!(!out.contains('\u{9b}'), "{out:?}");
    assert!(!out.contains('\u{7}'), "{out:?}");
    assert!(out.contains("␛]52"), "shown as control pictures");
}
