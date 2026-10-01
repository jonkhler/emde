//! The binary end to end: output modes, exit statuses, standard input,
//! several files, dumps, a reader that goes away, colours, highlighting and
//! the flags that change what is shown.

use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};

/// The binary with a controlled environment: no inherited colour settings,
/// terminal hints or user configuration.
fn emde() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_emde"));
    for var in emde::term::env::VARS {
        cmd.env_remove(var);
    }
    for var in ["EMDE_CONFIG", "XDG_CONFIG_HOME", "EMDE_TRACE"] {
        cmd.env_remove(var);
    }
    cmd.env("HOME", "/nonexistent/emde-test-home");
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

/// Run with `md` on standard input.
fn with_input(cmd: &mut Command, md: &str) -> Output {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(md.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const RUST: &str = "```rust\nfn main() { let x = \"hi\"; }\n```\n";

#[test]
fn code_is_highlighted_with_the_theme_code_theme() {
    let o = with_input(emde().arg("--color=truecolor"), RUST);
    assert!(o.status.success());
    let out = stdout(&o);
    if cfg!(feature = "highlight") {
        // OneHalfDark, the `emde` theme's dark code theme: keywords #c678dd,
        // strings #98c379.
        assert!(out.contains("\u{1b}[38;2;198;120;221mfn"), "{out:?}");
        assert!(out.contains("\u{1b}[38;2;152;195;121m\"hi\""), "{out:?}");
        // The code panel and its language label.
        assert!(out.contains("rust"), "{out:?}");
        let o = with_input(
            emde().args(["--color=truecolor", "--code-theme", "Nord"]),
            RUST,
        );
        assert!(
            stdout(&o).contains("\u{1b}[38;2;129;161;193mfn"),
            "{}",
            stdout(&o)
        );
    }
    // Piped without colour: the code, and nothing else.
    let o = with_input(&mut emde(), RUST);
    assert!(stdout(&o).contains("fn main() { let x = \"hi\"; }"));
    assert!(!stdout(&o).contains('\u{1b}'));
}

#[test]
fn an_unknown_code_theme_warns_once_and_falls_back() {
    let o = with_input(
        emde().args(["--color=truecolor", "--code-theme", "Nrod"]),
        RUST,
    );
    assert!(o.status.success());
    let err = stderr(&o);
    assert_eq!(err.matches("unknown code theme").count(), 1, "{err}");
    if cfg!(feature = "highlight") {
        assert!(err.contains("did you mean `Nord`?"), "{err}");
        // The `ansi` code theme: the terminal's own colours.
        assert!(stdout(&o).contains("\u{1b}[35mfn"), "{}", stdout(&o));
    }
}

#[test]
fn several_languages_are_all_highlighted() {
    let md = "```rust\nfn a() {}\n```\n\n```python\ndef b(): pass\n```\n\n\
              ```bash\necho hi\n```\n\n```rust\nfn c() {}\n```\n";
    let o = with_input(emde().arg("--color=truecolor"), md);
    let out = stdout(&o);
    if cfg!(feature = "highlight") {
        for keyword in ["fn", "def", "echo"] {
            assert!(
                out.contains(&format!("m{keyword}")),
                "{keyword} is highlighted: {out:?}"
            );
        }
        assert_eq!(out.matches("\u{1b}[38;2;198;120;221mfn").count(), 2);
    }
}

#[test]
fn no_color_and_forced_colour() {
    // NO_COLOR on a pipe: nothing.
    let o = run(emde().env("NO_COLOR", "1").arg(fixture("headings")));
    assert!(!stdout(&o).contains('\u{1b}'));
    // `--color` comes first: always means colour, at the detected depth.
    let o = run(emde()
        .env("NO_COLOR", "1")
        .arg("--color=always")
        .arg(fixture("headings")));
    assert!(stdout(&o).contains("\u{1b}[38;5;"), "{}", stdout(&o));
    // `--color=16` uses the `ansi` theme's 16 colours.
    let o = run(emde().arg("--color=16").arg(fixture("headings")));
    let out = stdout(&o);
    assert!(out.contains('\u{1b}') && !out.contains("38;5;") && !out.contains("38;2;"));
}

#[test]
fn themes_and_backgrounds() {
    let doc = "# Title\n\n## Section\n";
    let h2 = |args: &[&str]| {
        let o = with_input(emde().arg("--color=truecolor").args(args), doc);
        assert!(o.status.success(), "{o:?}");
        stdout(&o)
    };
    // The `emde` theme's h2 is mauve: Mocha on dark, Latte on light.
    assert!(h2(&[]).contains("38;2;203;166;247"), "{}", h2(&[]));
    assert!(h2(&["--background", "light"]).contains("38;2;136;57;239"));
    // `mono` has no colours, only attributes.
    let mono = h2(&["-t", "mono"]);
    assert!(
        !mono.contains("38;2;") && mono.contains("\u{1b}[1m"),
        "{mono:?}"
    );
    // A theme that does not exist warns and keeps the default.
    let o = with_input(emde().args(["--color=truecolor", "-t", "nonesuch"]), doc);
    assert!(o.status.success());
    assert!(stderr(&o).contains("nonesuch"), "{}", stderr(&o));
    assert!(stdout(&o).contains("38;2;203;166;247"));
}

#[test]
fn content_flags() {
    let code = "```\nlet a_rather_long_line_of_code = 1234567890 + 1234567890 + 1234567890;\n```\n";
    let o = with_input(emde().args(["-w", "40", "--line-numbers"]), code);
    assert!(stdout(&o).contains("1 let"), "{}", stdout(&o));
    let o = with_input(emde().args(["-w", "40", "--no-wrap-code"]), code);
    assert!(stdout(&o).contains('›'), "clipped: {}", stdout(&o));
    let o = with_input(emde().args(["-w", "40"]), code);
    assert!(stdout(&o).contains('↳'), "wrapped: {}", stdout(&o));
    let o = with_input(emde().args(["--ascii"]), "- item\n\n---\n");
    assert!(stdout(&o).is_ascii(), "{}", stdout(&o));
    let o = with_input(emde().args(["--math", "raw"]), "$\\alpha^2$\n");
    assert!(stdout(&o).contains("\\alpha^2"), "{}", stdout(&o));
    let o = with_input(&mut emde(), "$\\alpha^2$\n");
    assert!(stdout(&o).contains("α²"), "{}", stdout(&o));
    let o = with_input(
        emde().args(["--link-refs", "never"]),
        "[a](https://example.com)\n",
    );
    assert!(!stdout(&o).contains("[1]"), "{}", stdout(&o));
    let o = with_input(emde().args(["-m", "20", "-w", "100"]), &"word ".repeat(40));
    let widest = stdout(&o)
        .lines()
        .map(|l| emde::text::str_width(l, false))
        .max()
        .unwrap_or(0);
    assert!(widest <= 20, "{widest}");
}

#[test]
fn pager_flags_are_accepted_in_stream_mode() {
    let doc = fixture("headings");
    let anchored = format!("{doc}#heading-level-two");
    for args in [
        vec!["--paging=always", doc.as_str()],
        vec!["--paging=never", "--toc", doc.as_str()],
        vec!["--anchor", "heading-level-two", doc.as_str()],
        vec![anchored.as_str()],
        vec!["-p", doc.as_str()],
    ] {
        let o = run(emde().args(&args));
        assert!(o.status.success(), "{args:?}: {o:?}");
        assert!(stdout(&o).contains("Heading level one"), "{args:?}");
    }
}

#[test]
fn verbose_shows_content_problems() {
    let o = with_input(&mut emde(), "bad \u{7} bell\n");
    assert!(o.status.success());
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
    let o = with_input(emde().arg("-v"), "bad \u{7} bell\n");
    assert!(
        stderr(&o).contains("<stdin>: 1 control character shown as control pictures"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn directories_show_their_readme() {
    let dir = std::env::temp_dir().join(format!("emde-cli-readme-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("README.md"), "# Read me first\n").unwrap();
    let o = run(emde().arg(&dir));
    assert!(stdout(&o).contains("Read me first"));
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let o = run(emde().arg(&empty));
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("directory has no README"));
    let _ = std::fs::remove_dir_all(&dir);
}
