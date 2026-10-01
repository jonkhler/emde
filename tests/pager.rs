//! The pager end to end: scripted keys on a [`FakeTerminal`], the bytes it
//! wrote replayed into a `vt100` screen, and the screen snapshotted.
//!
//! In snapshots each row is shown between `│`s, followed (when it has any)
//! by a row of markers for the cells that are highlighted: `^` the current
//! search match, `~` another match, `=` reverse video (the focused link,
//! the outline selection, the prompt cursor).

mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{FakeHighlighter, fixture};
use emde::config::PagerOptions;
use emde::options::RenderOptions;
use emde::pager::term::{
    Button, Chunk, ENTER, EXIT, FakeTerminal, Key, KeyCode, MOUSE_OFF, MOUSE_ON, Mouse, MouseKind,
    Step,
};
use emde::pager::{
    Ctx, PagerDoc, PagerExit, PagerSession, SYNC_OFF, SYNC_ON, Screen, Settings, Signals, State,
    run_on, view,
};
use emde::parse::ParseOptions;
use emde::render::RenderConfig;
use emde::source::{Origin, Source};
use emde::term::Caps;
use emde::term::env::Env;
use emde::theme::Theme;

/// A session for `md` (not from a file) with the test theme and the fake
/// highlighter.
fn session_for(doc: PagerDoc) -> PagerSession {
    let opts = RenderOptions::default();
    let mut s = PagerSession::new(doc, Theme::test(), Caps::full(), opts);
    s.highlighter = Arc::new(FakeHighlighter);
    s.env = Env::default();
    s
}

fn memory(md: &str) -> PagerDoc {
    let source = Source::from_bytes(md.as_bytes().to_vec(), Origin::Memory);
    PagerDoc::parse(source, &ParseOptions::default())
}

fn session(md: &str) -> PagerSession {
    session_for(memory(md))
}

/// Run the script to its end (it must quit).
fn run(mut term: FakeTerminal, session: PagerSession) -> (FakeTerminal, PagerExit) {
    let exit = run_on(&mut term, session, &Signals::new()).expect("the pager runs");
    (term, exit)
}

/// The screen as it was at mark `name` (or at the end without one).
fn screen_at(term: &FakeTerminal, cols: u16, rows: u16, name: Option<&str>) -> vt100::Parser {
    let mut p = vt100::Parser::new(rows, cols, 0);
    for chunk in term.chunks() {
        match chunk {
            Chunk::Write(bytes) => p.process(bytes),
            Chunk::Resize(c, r) => p.screen_mut().set_size(*r, *c),
            Chunk::Mark(m) if Some(m.as_str()) == name => break,
            Chunk::Mark(_) | Chunk::Suspend => {}
        }
    }
    p
}

const PEACH: vt100::Color = vt100::Color::Rgb(0xfa, 0xb3, 0x87);
const YELLOW: vt100::Color = vt100::Color::Rgb(0xf9, 0xe2, 0xaf);
const BLUE: vt100::Color = vt100::Color::Rgb(0x89, 0xb4, 0xfa);

/// The screen as text; see the module docs.
fn render(p: &vt100::Parser) -> String {
    render_with(p, false)
}

/// [`render`], with `#` under cells on the hint colour when `hints`.
fn render_with(p: &vt100::Parser, hints: bool) -> String {
    let screen = p.screen();
    let (rows, cols) = screen.size();
    let mut out = String::new();
    for r in 0..rows {
        let mut text = String::new();
        let mut marks = String::new();
        for c in 0..cols {
            let Some(cell) = screen.cell(r, c) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let width = if cell.is_wide() { 2 } else { 1 };
            let contents = cell.contents();
            text.push_str(if contents.is_empty() { " " } else { contents });
            let mark = if cell.bgcolor() == PEACH {
                '^'
            } else if cell.bgcolor() == YELLOW {
                '~'
            } else if cell.inverse() {
                '='
            } else if hints && cell.bgcolor() == BLUE {
                '#'
            } else {
                ' '
            };
            for _ in 0..width {
                marks.push(mark);
            }
        }
        out.push_str(&format!("{:>2}│{text}│\n", r + 1));
        if marks.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("  │{}│\n", marks.trim_end()));
    }
    out
}

fn screen_text(term: &FakeTerminal, cols: u16, rows: u16, name: &str) -> String {
    render(&screen_at(term, cols, rows, Some(name)))
}

/// The text of screen row `row` (0-based) at mark `name`.
fn row_text(term: &FakeTerminal, cols: u16, rows: u16, name: &str, row: u16) -> String {
    let p = screen_at(term, cols, rows, Some(name));
    p.screen()
        .rows(0, cols)
        .nth(usize::from(row))
        .unwrap_or_default()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Keys typed one by one, with a pause after each (the pager takes keys
/// that arrive together in one go, and paints once).
fn typed(mut term: FakeTerminal, text: &str) -> FakeTerminal {
    let mut buf = [0u8; 4];
    for c in text.chars() {
        term = term.keys(c.encode_utf8(&mut buf)).wait(ms(5));
    }
    term
}

/// A fresh directory under the target's temporary directory.
fn temp_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("pager-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn file_session(path: &Path) -> PagerSession {
    session_for(PagerDoc::load(path, &ParseOptions::default()).unwrap())
}

/// A long document with numbered sections.
fn long_doc(sections: usize) -> String {
    let mut md = String::from("# Long document\n\n");
    for s in 1..=sections {
        md.push_str(&format!(
            "## Section {s}\n\nParagraph {s} has a few words of text in it.\n\n"
        ));
    }
    md
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

#[test]
fn first_frame_and_scrolling() {
    let term = FakeTerminal::new(80, 24)
        .mark("first")
        .keys("jjj")
        .mark("down three")
        .keys(" ")
        .mark("a page")
        .keys("G")
        .mark("bottom")
        .keys("q");
    let (term, exit) = run(term, session(&fixture("kitchen-sink")));
    assert_eq!(exit, PagerExit::Quit);
    insta::assert_snapshot!("first-frame", screen_text(&term, 80, 24, "first"));
    insta::assert_snapshot!("down-three", screen_text(&term, 80, 24, "down three"));
    insta::assert_snapshot!("a-page", screen_text(&term, 80, 24, "a page"));
    insta::assert_snapshot!("bottom", screen_text(&term, 80, 24, "bottom"));
}

#[test]
fn search_highlights_matches() {
    let term = FakeTerminal::new(80, 24)
        .keys("/footnote")
        .mark("typing")
        .keys("\n")
        .keys("n")
        .mark("next")
        .keys("\x1b")
        .mark("cleared")
        .keys("q");
    let (term, _) = run(term, session(&fixture("kitchen-sink")));
    insta::assert_snapshot!("search-typing", screen_text(&term, 80, 24, "typing"));
    insta::assert_snapshot!("search-next", screen_text(&term, 80, 24, "next"));
    let cleared = screen_text(&term, 80, 24, "cleared");
    assert!(
        !cleared.contains('^') && !cleared.contains('~'),
        "{cleared}"
    );
}

#[test]
fn search_matches_across_wrapped_lines() {
    let md = "Some words here and the quick brown fox jumps over the lazy dog many times.";
    // "the" ends the first line, "quick brown" starts the second.
    let term = FakeTerminal::new(30, 8)
        .keys("/the quick brown\n")
        .mark("found")
        .keys("q");
    let (term, _) = run(term, session(md));
    insta::assert_snapshot!("search-wrapped", screen_text(&term, 30, 8, "found"));
}

#[test]
fn outline_overlay() {
    let term = FakeTerminal::new(80, 24)
        .keys("]]")
        .keys("t")
        .mark("open")
        .keys("ta")
        .mark("filtered")
        .keys("\n")
        .mark("jumped")
        .keys("q");
    let (term, _) = run(term, session(&fixture("kitchen-sink")));
    insta::assert_snapshot!("outline-open", screen_text(&term, 80, 24, "open"));
    insta::assert_snapshot!("outline-filtered", screen_text(&term, 80, 24, "filtered"));
    assert_eq!(row_text(&term, 80, 24, "jumped", 0).trim(), "Table");
}

#[test]
fn help_overlay() {
    let term = FakeTerminal::new(80, 24)
        .keys("h")
        .mark("help")
        .keys("q")
        .mark("closed")
        .keys("q");
    let (term, _) = run(term, session(&fixture("kitchen-sink")));
    insta::assert_snapshot!("help", screen_text(&term, 80, 24, "help"));
    assert!(!screen_text(&term, 80, 24, "closed").contains("Keys"));
}

#[test]
fn link_focus_follow_and_back() {
    let md = format!(
        "# Links\n\nGo to [the end](#the-end) or [elsewhere](https://example.com).\n\n\
         {filler}## The end\n\nDone.\n\n{filler}",
        filler = "filler\n\n".repeat(30)
    );
    let term = FakeTerminal::new(60, 12)
        .code(KeyCode::Tab)
        .mark("focused")
        .code(KeyCode::Enter)
        .mark("followed")
        .code(KeyCode::Backspace)
        .mark("back")
        .keys("L")
        .mark("forward")
        .keys("q");
    let (term, _) = run(term, session(&md));
    insta::assert_snapshot!("link-focused", screen_text(&term, 60, 12, "focused"));
    assert_eq!(row_text(&term, 60, 12, "followed", 0).trim(), "The end");
    assert_eq!(
        screen_text(&term, 60, 12, "back"),
        screen_text(&term, 60, 12, "focused"),
        "back where it was, focus included"
    );
    assert_eq!(row_text(&term, 60, 12, "forward", 0).trim(), "The end");
}

#[test]
fn link_hints_label_visible_links() {
    let md = "One [alpha](#b), two [beta](#b), three [gamma](https://example.com).\n\n## B";
    let term = FakeTerminal::new(70, 8)
        .keys("o")
        .mark("hints")
        .keys("\x1b")
        .keys("q");
    let (term, _) = run(term, session(md));
    let screen = screen_at(&term, 70, 8, Some("hints"));
    insta::assert_snapshot!("hints", render_with(&screen, true));
}

#[test]
fn following_a_local_document_and_back() {
    let dir = temp_dir("follow");
    std::fs::write(
        dir.join("a.md"),
        "# Page A\n\nSee [page B](b.md#second), [the docs](docs/) and [again](docs).\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b.md"),
        format!(
            "# Page B\n\n{}## Second\n\nB text.\n",
            "filler\n\n".repeat(20)
        ),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::write(dir.join("docs/README.md"), "# Docs readme\n").unwrap();
    let term = FakeTerminal::new(60, 10)
        .code(KeyCode::Tab)
        .code(KeyCode::Enter)
        .mark("b")
        .code(KeyCode::Backspace)
        .mark("a")
        .code(KeyCode::Tab)
        .code(KeyCode::Tab)
        .code(KeyCode::Enter)
        .mark("docs")
        .keys("q");
    let (term, _) = run(term, file_session(&dir.join("a.md")));
    insta::assert_snapshot!("local-b", screen_text(&term, 60, 10, "b"));
    let a = screen_text(&term, 60, 10, "a");
    assert!(a.contains("Page A") && a.contains(" a.md "), "{a}");
    let docs = screen_text(&term, 60, 10, "docs");
    assert!(
        docs.contains("Docs readme") && docs.contains("README.md"),
        "{docs}"
    );
    // A directory linked without the trailing slash is a local file link:
    // still shown through its README.
    let term = FakeTerminal::new(60, 10)
        .code(KeyCode::BackTab)
        .code(KeyCode::Enter)
        .mark("dir")
        .keys("q");
    let (term, _) = run(term, file_session(&dir.join("a.md")));
    assert!(screen_text(&term, 60, 10, "dir").contains("Docs readme"));
}

#[test]
fn session_options() {
    let md = long_doc(20);
    // An anchor to start at, and a first layout made for another width
    // (which the pager must not use).
    let mut s = session(&md);
    s.anchor = Some("section-12".into());
    let doc = memory(&md);
    s.layout = Some(emde::layout::layout(
        &doc.doc,
        40,
        &Theme::test(),
        &Caps::full(),
        &RenderOptions::default(),
        &FakeHighlighter,
        &emde::layout::NoImages,
    ));
    let (term, _) = run(FakeTerminal::new(80, 12).mark("start").keys("q"), s);
    assert_eq!(row_text(&term, 80, 12, "start", 0).trim(), "Section 12");
    let text = screen_text(&term, 80, 12, "start");
    assert!(
        text.contains("Paragraph 12 has a few words of text in it."),
        "laid out for 80 columns: {text}"
    );
    // The outline right away.
    let mut s = session(&md);
    s.open_toc = true;
    let (term, _) = run(FakeTerminal::new(80, 12).mark("toc").keys("\x1bq"), s);
    assert!(screen_text(&term, 80, 12, "toc").contains("Outline"));
    // A missing anchor says so and starts at the top.
    let mut s = session(&md);
    s.anchor = Some("#nowhere".into());
    let (term, _) = run(FakeTerminal::new(80, 12).mark("missing").keys("q"), s);
    let text = screen_text(&term, 80, 12, "missing");
    assert!(text.contains("no anchor #nowhere in text"), "{text}");
}

#[test]
fn resize_re_anchors_on_the_same_text() {
    let term = FakeTerminal::new(80, 20)
        .keys("]]]]]]")
        .mark("before")
        .resize(40, 16)
        .mark("narrow")
        .resize(40, 30)
        .mark("taller")
        .keys("q");
    let (term, _) = run(term, session(&long_doc(20)));
    let before = row_text(&term, 80, 20, "before", 0);
    assert_eq!(before.trim(), "Section 6");
    assert_eq!(row_text(&term, 80, 20, "narrow", 0).trim(), "Section 6");
    insta::assert_snapshot!("resized-narrow", screen_text(&term, 80, 20, "narrow"));
    insta::assert_snapshot!("resized-taller", screen_text(&term, 80, 20, "taller"));
}

#[test]
fn resizes_are_debounced() {
    // A burst of resizes: one layout, for the last size.
    let term = FakeTerminal::new(80, 20)
        .resize(70, 20)
        .resize(60, 20)
        .resize(50, 20)
        .mark("settled")
        .keys("q");
    let (term, _) = run(term, session(&long_doc(10)));
    let frames = term
        .writes()
        .iter()
        .filter(|w| w.windows(SYNC_ON.len()).any(|x| x == SYNC_ON))
        .count();
    assert_eq!(frames, 2, "the first frame and one after the resizes");
    let p = screen_at(&term, 80, 20, Some("settled"));
    assert_eq!(p.screen().size(), (20, 50));
    assert!(render(&p).contains("Long document"));
}

#[test]
fn reload_when_the_file_changes() {
    let dir = temp_dir("reload");
    let path = dir.join("doc.md");
    std::fs::write(&path, "# Watched\n\nOld text.\n").unwrap();
    let writer = path.clone();
    let term = FakeTerminal::new(50, 8)
        .mark("before")
        .run(move || {
            // A different size, so the change is seen even when the clock
            // keeps the same modification time.
            std::fs::write(&writer, "# Watched\n\nNew text, a bit longer.\n").unwrap();
        })
        .wait(ms(800))
        .mark("after")
        .keys("q");
    let (term, _) = run(term, file_session(&path));
    assert!(screen_text(&term, 50, 8, "before").contains("Old text."));
    insta::assert_snapshot!("reloaded", screen_text(&term, 50, 8, "after"));
}

#[test]
fn a_document_changed_while_away_is_reloaded_on_return() {
    let dir = temp_dir("changed-away");
    let a = dir.join("a.md");
    std::fs::write(&a, "# A\n\nFirst version. [to b](b.md)\n").unwrap();
    std::fs::write(dir.join("b.md"), "# B\n\nOther page.\n").unwrap();
    let writer = a.clone();
    let term = FakeTerminal::new(50, 8)
        .code(KeyCode::Tab)
        .code(KeyCode::Enter)
        .mark("on b")
        .run(move || {
            std::fs::write(&writer, "# A\n\nSecond version, changed. [to b](b.md)\n").unwrap()
        })
        .code(KeyCode::Backspace)
        .wait(ms(800))
        .mark("back on a")
        .keys("q");
    let (term, _) = run(term, file_session(&a));
    assert!(screen_text(&term, 50, 8, "on b").contains("Other page."));
    let back = screen_text(&term, 50, 8, "back on a");
    assert!(back.contains("Second version, changed."), "{back}");
}

/// Whether a file with mode 000 cannot be read (false when running as
/// root).
#[cfg(unix)]
fn mode_000_blocks_reading(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let probe = dir.join("probe");
    std::fs::write(&probe, "x").unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000)).unwrap();
    let blocked = std::fs::File::open(&probe).is_err();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o644)).unwrap();
    blocked
}

#[cfg(unix)]
#[test]
fn a_failed_reload_is_tried_again() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = temp_dir("failed-reload");
    if !mode_000_blocks_reading(&dir) {
        return;
    }
    let path = dir.join("doc.md");
    std::fs::write(&path, "# Doc\n\nBefore.\n").unwrap();
    let locked = path.clone();
    let unlocked = path.clone();
    let term = FakeTerminal::new(50, 8)
        .run(move || {
            std::fs::write(&locked, "# Doc\n\nAfter, longer.\n").unwrap();
            let perms = std::fs::Permissions::from_mode(0o000);
            std::fs::set_permissions(&locked, perms).unwrap();
        })
        .wait(ms(800))
        .mark("unreadable")
        .run(move || {
            let perms = std::fs::Permissions::from_mode(0o644);
            std::fs::set_permissions(&unlocked, perms).unwrap();
        })
        .wait(ms(800))
        .mark("readable")
        .keys("q");
    let (term, _) = run(term, file_session(&path));
    let unreadable = screen_text(&term, 50, 8, "unreadable");
    assert!(unreadable.contains("reload failed"), "{unreadable}");
    assert!(unreadable.contains("Before."));
    let readable = screen_text(&term, 50, 8, "readable");
    assert!(readable.contains("After, longer."), "{readable}");
}

#[test]
fn manual_reload_and_watch_toggle() {
    let dir = temp_dir("manual-reload");
    let path = dir.join("doc.md");
    std::fs::write(&path, "# Doc\n\nOne.\n").unwrap();
    let writer = path.clone();
    let term = FakeTerminal::new(50, 8)
        .keys("R")
        .mark("unwatched")
        .run(move || std::fs::write(&writer, "# Doc\n\nTwo, changed.\n").unwrap())
        .wait(ms(1200))
        .mark("still old")
        .keys("r")
        .mark("reloaded")
        .keys("q");
    let (term, _) = run(term, file_session(&path));
    assert!(screen_text(&term, 50, 8, "unwatched").contains("not watching the file"));
    assert!(screen_text(&term, 50, 8, "still old").contains("One."));
    let reloaded = screen_text(&term, 50, 8, "reloaded");
    assert!(
        reloaded.contains("Two, changed.") && reloaded.contains("reloaded"),
        "{reloaded}"
    );
}

#[test]
fn mouse_wheel_and_clicks() {
    let md = format!(
        "# Mouse\n\n[jump down](#target)\n\n{filler}## Target\n\nHere.\n\n{filler}",
        filler = "filler\n\n".repeat(30)
    );
    let wheel = Mouse {
        kind: MouseKind::WheelDown,
        col: 10,
        row: 3,
    };
    let term = FakeTerminal::new(50, 10)
        .mouse(wheel)
        .mark("wheel")
        .mouse(Mouse {
            kind: MouseKind::WheelUp,
            ..wheel
        })
        .mark("up")
        .mouse(Mouse {
            kind: MouseKind::Press(Button::Left),
            col: 4,
            row: 2,
        })
        .mark("clicked")
        .keys("q");
    let (term, _) = run(term, session(&md));
    // The wheel moved three lines.
    assert!(row_text(&term, 50, 10, "wheel", 9).contains("L 4/"));
    assert_eq!(row_text(&term, 50, 10, "up", 2).trim(), "jump down");
    assert_eq!(row_text(&term, 50, 10, "clicked", 0).trim(), "Target");
}

#[test]
fn mouse_capture_can_be_turned_off() {
    let term = FakeTerminal::new(40, 10).keys("m").keys("m").keys("q");
    let (term, _) = run(term, session("text"));
    let writes = term.writes();
    let off = writes.iter().position(|w| *w == MOUSE_OFF).expect("off");
    let on = writes.iter().position(|w| *w == MOUSE_ON).expect("on");
    assert!(off < on);
    let mut session = session("text");
    session.pager = PagerOptions {
        mouse: false,
        ..PagerOptions::default()
    };
    let (term, _) = run(FakeTerminal::new(40, 10).keys("q"), session);
    assert_eq!(term.writes()[0], ENTER, "no mouse modes when it is off");
}

#[test]
fn copying_a_link_uses_osc52() {
    let term = FakeTerminal::new(60, 8)
        .code(KeyCode::Tab)
        .keys("y")
        .mark("copied")
        .keys("q");
    let (term, _) = run(term, session("[a link](https://example.com/x)"));
    let osc = b"\x1b]52;c;aHR0cHM6Ly9leGFtcGxlLmNvbS94\x1b\\";
    assert!(
        term.writes().iter().any(|w| *w == osc),
        "OSC 52 with the URL"
    );
    assert!(screen_text(&term, 60, 8, "copied").contains("copied https://example.com/x"));
}

#[test]
fn copying_inside_an_unreachable_tmux_falls_back_to_osc52() {
    let term = FakeTerminal::new(60, 8)
        .code(KeyCode::Tab)
        .keys("yy")
        .keys("q");
    let mut session = session("[a link](https://example.com/x)");
    // Inside tmux, without a query result: tmux is asked (once), and as its
    // server cannot be reached, OSC 52 it is.
    session.env = Env::from_pairs(&[("TMUX", "/nonexistent/emde-test-socket,1,0")]);
    let (term, _) = run(term, session);
    let osc = b"\x1b]52;c;aHR0cHM6Ly9leGFtcGxlLmNvbS94\x1b\\";
    assert_eq!(term.writes().iter().filter(|w| **w == osc).count(), 2);
}

#[test]
fn links_are_copied_when_opening_is_off() {
    let term = FakeTerminal::new(80, 8)
        .code(KeyCode::Tab)
        .code(KeyCode::Enter)
        .mark("opened")
        .keys("q");
    let mut session = session("[a link](https://example.com/y)");
    session.pager.open = emde::config::OpenCommand::Never;
    let (term, _) = run(term, session);
    let text = screen_text(&term, 80, 8, "opened");
    assert!(
        text.contains("copied https://example.com/y · opening links is off"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------
// Bytes
// ---------------------------------------------------------------------------

#[test]
fn setup_and_teardown_bytes() {
    let (term, exit) = run(FakeTerminal::new(40, 10).keys("q"), session("# Hi"));
    assert_eq!(exit, PagerExit::Quit);
    let writes = term.writes();
    let mut enter = ENTER.to_vec();
    enter.extend_from_slice(MOUSE_ON);
    assert_eq!(writes[0], enter.as_slice());
    assert_eq!(
        enter,
        b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[?2004h\x1b[?1000h\x1b[?1006h".to_vec()
    );
    assert_eq!(*writes.last().unwrap(), EXIT);
    assert_eq!(
        EXIT,
        b"\x1b[?2026l\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[r\x1b[?7h\x1b[0m\x1b[?25h\x1b[?1049l"
    );
    assert!(!term.is_raw());
    let p = screen_at(&term, 40, 10, None);
    assert!(!p.screen().alternate_screen(), "back on the main screen");
    assert!(!p.screen().hide_cursor());
    assert_eq!(
        p.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::None
    );
}

#[test]
fn sigterm_restores_the_terminal() {
    let signals = Signals::new();
    let mut term = FakeTerminal::new(40, 10)
        .with_signals(signals.clone())
        .keys("j")
        .step(Step::Signal(15))
        .keys("this is never read");
    let exit = run_on(&mut term, session(&long_doc(5)), &signals).unwrap();
    assert_eq!(exit, PagerExit::Signal(15));
    assert_eq!(exit.code(), 143);
    assert_eq!(*term.writes().last().unwrap(), EXIT);
    assert!(!term.is_raw());
    assert!(term.remaining() > 0, "it stopped at the signal");
}

#[test]
fn an_error_still_restores_the_terminal() {
    // A script that never quits: the fake gives up with an error, and the
    // drop guard still puts the terminal back.
    let mut term = FakeTerminal::new(40, 10).keys("j");
    let result = run_on(&mut term, session("text"), &Signals::new());
    assert!(result.is_err());
    assert_eq!(*term.writes().last().unwrap(), EXIT);
    assert!(!term.is_raw());
}

#[test]
fn suspend_and_resume() {
    let term = FakeTerminal::new(40, 10)
        .key(Key::ctrl('z'))
        .mark("resumed")
        .keys("q");
    let (term, _) = run(term, session(&long_doc(5)));
    let chunks = term.chunks();
    let suspend = chunks
        .iter()
        .position(|c| *c == Chunk::Suspend)
        .expect("suspended");
    assert_eq!(chunks[suspend - 1], Chunk::Write(EXIT.to_vec()));
    let mut enter = ENTER.to_vec();
    enter.extend_from_slice(MOUSE_ON);
    assert_eq!(chunks[suspend + 1], Chunk::Write(enter));
    // The whole screen is drawn again.
    let Chunk::Write(frame) = &chunks[suspend + 2] else {
        panic!("{:?}", chunks[suspend + 2]);
    };
    let text = String::from_utf8_lossy(frame);
    for row in 1..=10 {
        assert!(text.contains(&format!("\x1b[{row};1H")), "row {row}");
    }
    assert!(screen_text(&term, 40, 10, "resumed").contains("Long document"));
}

#[test]
fn a_stop_from_outside_is_repainted_at_once() {
    // Stopped by someone else (no Ctrl-Z) and continued: the terminal is set
    // up again and repainted before anything else happens.
    let signals = Signals::new();
    let mut term = FakeTerminal::new(40, 10)
        .with_signals(signals.clone())
        .wait(ms(50))
        .step(Step::Signal(signal_hook::consts::SIGCONT))
        .step(Step::Mark("continued".into()))
        .keys("q");
    run_on(&mut term, session(&long_doc(5)), &signals).unwrap();
    let chunks = term.chunks();
    let mark = chunks
        .iter()
        .position(|c| *c == Chunk::Mark("continued".into()))
        .unwrap();
    let mut enter = ENTER.to_vec();
    enter.extend_from_slice(MOUSE_ON);
    let entered = chunks
        .iter()
        .rposition(|c| *c == Chunk::Write(enter.clone()))
        .unwrap();
    assert!(entered > 0, "set up again");
    let Chunk::Write(frame) = &chunks[entered + 1] else {
        panic!("{:?}", chunks[entered + 1]);
    };
    assert!(frame.starts_with(SYNC_ON));
    let text = String::from_utf8_lossy(frame);
    assert!(
        text.contains("\x1b[1;1H") && text.contains("\x1b[10;1H"),
        "every row"
    );
    assert!(entered + 1 < mark, "repainted before the next event");
}

#[test]
fn every_frame_is_one_synchronized_write() {
    let term = typed(FakeTerminal::new(60, 15), "jjjj  /Section\nnnt\x1bhq").keys("q");
    let (term, _) = run(term, session(&long_doc(30)));
    let frames: Vec<&[u8]> = term
        .writes()
        .into_iter()
        .filter(|w| w.windows(SYNC_ON.len()).any(|x| x == SYNC_ON))
        .collect();
    assert!(frames.len() >= 8, "{}", frames.len());
    for f in frames {
        assert!(f.starts_with(SYNC_ON), "starts synchronized");
        assert!(f.ends_with(SYNC_OFF), "ends synchronized");
        let count = f.windows(SYNC_ON.len()).filter(|x| *x == SYNC_ON).count();
        assert_eq!(count, 1);
    }
    for t in term.poll_timeouts() {
        assert!(
            (Duration::from_millis(1)..=Duration::from_millis(250)).contains(t),
            "{t:?}"
        );
    }
}

/// A random script: keys (never `q`), mouse events, resizes (down to
/// nothing at all), quiet periods, SIGCONT and redraws.
fn random_script(mut term: FakeTerminal, next: &mut impl FnMut() -> u64) -> FakeTerminal {
    let chars: Vec<char> = "jkdufbgG%][}{tnN/?oashHLywxe15 rRim".chars().collect();
    let codes = [
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Backspace,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Down,
        KeyCode::PageUp,
        KeyCode::End,
        KeyCode::F(1),
    ];
    let mouse = [
        MouseKind::WheelDown,
        MouseKind::WheelUp,
        MouseKind::Press(Button::Left),
    ];
    for _ in 0..next() % 100 {
        let r = next();
        let pick = (r >> 8) as usize;
        let (a, b) = ((r >> 24) as u16 % 170, (r >> 40) as u16 % 70);
        term = match r % 10 {
            0..=4 => term.key(Key::char(chars[pick % chars.len()])),
            5 => term.code(codes[pick % codes.len()]),
            6 => term.mouse(Mouse {
                kind: mouse[pick % mouse.len()],
                col: a,
                row: b,
            }),
            7 => term.resize(a, b),
            8 => term.wait(ms(r % 400)),
            _ if r.is_multiple_of(3) => term.step(Step::Signal(signal_hook::consts::SIGCONT)),
            _ => term.key(Key::ctrl('l')),
        };
    }
    term
}

#[test]
fn random_scripts_always_end_with_the_terminal_restored() {
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..8 {
        for name in ["kitchen-sink", "links", "tables", "footnotes", "math"] {
            let mut s = session(&fixture(name));
            s.caps.hyperlinks = round % 2 == 0;
            s.pager.open = emde::config::OpenCommand::Never;
            s.open_toc = round == 3;
            let signals = Signals::new();
            let size = (next() % 160 + 1, next() % 60 + 1);
            let term =
                FakeTerminal::new(size.0 as u16, size.1 as u16).with_signals(signals.clone());
            // Out of any overlay or prompt, then quit.
            let mut term = random_script(term, &mut next).keys("\x1b\x1b\x1bq");
            let exit = run_on(&mut term, s, &signals);
            assert!(exit.is_ok(), "{name}, round {round}: {exit:?}");
            assert!(!term.is_raw());
            assert_eq!(
                *term.writes().last().unwrap(),
                EXIT,
                "{name}, round {round}"
            );
        }
    }
}

/// The frame written right after mark `name`.
fn frame_after<'a>(term: &'a FakeTerminal, name: &str) -> &'a [u8] {
    let chunks = term.chunks();
    let at = chunks
        .iter()
        .position(|c| *c == Chunk::Mark(name.to_owned()))
        .unwrap();
    chunks[at..]
        .iter()
        .find_map(|c| match c {
            Chunk::Write(b) if b.starts_with(SYNC_ON) => Some(b.as_slice()),
            _ => None,
        })
        .unwrap()
}

fn visible(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace('\u{1b}', "\\e")
}

#[test]
fn scrolling_uses_the_scroll_region() {
    let term = FakeTerminal::new(80, 24)
        .mark("start")
        .keys("j")
        .mark("down")
        .keys("k")
        .mark("up")
        .keys("3j")
        .mark("three")
        .keys("q");
    let (term, _) = run(term, session(&fixture("kitchen-sink")));
    let down = frame_after(&term, "start");
    let text = visible(down);
    assert!(
        text.starts_with("\\e[?2026h\\e[1;23r\\e[1S\\e[r\\e[23;1H"),
        "{text}"
    );
    // Only the exposed row and the status bar are drawn.
    let rows: Vec<&str> = text.matches(";1H").collect();
    assert_eq!(rows.len(), 2, "{text}");
    assert!(text.contains("\\e[24;1H"));
    assert!(down.len() <= 2048, "{} bytes", down.len());
    let up = visible(frame_after(&term, "down"));
    assert!(
        up.starts_with("\\e[?2026h\\e[1;23r\\e[1T\\e[r\\e[1;1H"),
        "{up}"
    );
    let three = visible(frame_after(&term, "up"));
    assert!(
        three.starts_with("\\e[?2026h\\e[1;23r\\e[3S\\e[r"),
        "{three}"
    );
    assert_eq!(three.matches(";1H").count(), 4, "three rows and the status");
    // The scrolled screen is what a full repaint shows.
    let scrolled = screen_text(&term, 80, 24, "three");
    let (fresh, _) = run(
        FakeTerminal::new(80, 24)
            .keys("3j")
            .key(Key::ctrl('l'))
            .mark("x")
            .keys("q"),
        session(&fixture("kitchen-sink")),
    );
    assert_eq!(scrolled, screen_text(&fresh, 80, 24, "x"));
}

#[test]
fn an_unchanged_screen_writes_nothing() {
    let term = FakeTerminal::new(40, 10)
        .keys("k")
        .mark("noop")
        .keys("x")
        .keys("q");
    let (term, _) = run(term, session(&long_doc(5)));
    let writes = term.writes();
    // Enter, the first frame, the exit: `k` at the top and an unbound key
    // changed nothing on screen.
    assert_eq!(writes.len(), 3, "{writes:?}");
}

#[test]
fn a_redraw_paints_every_row() {
    let term = FakeTerminal::new(40, 10)
        .mark("start")
        .key(Key::ctrl('l'))
        .mark("redrawn")
        .keys("q");
    let (term, _) = run(term, session(&long_doc(5)));
    let frame = visible(frame_after(&term, "start"));
    assert_eq!(frame.matches(";1H").count(), 10);
}

#[test]
fn rows_end_with_a_reset_and_erase_unless_full() {
    let term = FakeTerminal::new(40, 6).mark("first").keys("q");
    let (term, _) = run(term, session("# Bar\n\nplain\n\n---"));
    let first = term
        .writes()
        .into_iter()
        .find(|w| w.starts_with(SYNC_ON))
        .unwrap();
    let text = visible(first);
    // The h1 bar fills its measure but not the row (margins): erased;
    // a short plain line too.
    assert!(text.contains("plain\\e[0m\\e[K"), "{text}");
    let (term, _) = run(
        FakeTerminal::new(20, 6).keys("q"),
        session("# A bar as wide as the screen"),
    );
    let first = term
        .writes()
        .into_iter()
        .find(|w| w.starts_with(SYNC_ON))
        .unwrap();
    let text = visible(first);
    let row1 = text.split("\\e[2;1H").next().unwrap();
    assert!(
        !row1.ends_with("\\e[K"),
        "a full-width row is not erased: {row1}"
    );
}

// ---------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------

fn big_state(cols: u16, rows: u16) -> State {
    let doc = memory(&fixture("kitchen-sink").repeat(20));
    let layout = emde::layout::layout(
        &doc.doc,
        cols,
        &Theme::test(),
        &Caps::full(),
        &RenderOptions::default(),
        &FakeHighlighter,
        &emde::layout::NoImages,
    );
    let pager = PagerOptions::default();
    let settings = Settings::new(&pager, &RenderOptions::default());
    State::new(doc, None, layout, (cols, rows), &pager, settings)
}

#[test]
fn a_one_line_scroll_writes_little() {
    // 214×54, the screen of the plan's reference setup.
    let term = FakeTerminal::new(214, 54)
        .keys("]]")
        .mark("start")
        .keys("j")
        .mark("scrolled")
        .keys("q");
    let (term, _) = run(term, session(&fixture("kitchen-sink").repeat(5)));
    let frame = frame_after(&term, "start");
    assert!(frame.len() <= 2048, "{} bytes", frame.len());
}

/// Frame composition time at 214×54:
///
/// ```sh
/// cargo test --release --test pager -- --ignored --nocapture
/// ```
#[test]
#[ignore = "benchmark: cargo test --release --test pager -- --ignored --nocapture"]
fn frame_composition_benchmark() {
    let mut state = big_state(214, 54);
    let ctx = Ctx::new(&Theme::test(), &Caps::full());
    let cfg = RenderConfig::from_caps(&Caps::full());
    let mut screen = Screen::new();
    let doc = state.document().clone();
    let layout = state.layout().clone();
    let n = 2000;
    // Full frames.
    let start = Instant::now();
    for _ in 0..n {
        screen.invalidate();
        let frame = view(&state, &ctx);
        let bytes = screen.paint(&frame, &doc, &layout, &cfg);
        std::hint::black_box(bytes);
    }
    let full = start.elapsed() / n;
    // One-line scrolls.
    let mut total = 0usize;
    let start = Instant::now();
    for i in 0..n {
        let key = if i % 100 < 50 { 'j' } else { 'k' };
        emde::pager::update(&mut state, emde::pager::Action::Key(Key::char(key)));
        let frame = view(&state, &ctx);
        total += screen.paint(&frame, &doc, &layout, &cfg).len();
    }
    let scroll = start.elapsed() / n;
    // One-line scrolls with every `e` on screen highlighted.
    for c in "/e\n".chars() {
        let key = if c == '\n' {
            Key::plain(KeyCode::Enter)
        } else {
            Key::char(c)
        };
        emde::pager::update(&mut state, emde::pager::Action::Key(key));
    }
    let start = Instant::now();
    for i in 0..n {
        let key = if i % 100 < 50 { 'j' } else { 'k' };
        emde::pager::update(&mut state, emde::pager::Action::Key(Key::char(key)));
        let frame = view(&state, &ctx);
        std::hint::black_box(screen.paint(&frame, &doc, &layout, &cfg));
    }
    let searching = start.elapsed() / n;
    let report = format!(
        "214x54: full frame {full:?}, one-line scroll {scroll:?} ({} bytes), \
         with every `e` highlighted {searching:?}\n",
        total / n as usize
    );
    let _ = std::io::stdout().write_all(report.as_bytes());
    assert!(full < Duration::from_micros(500), "{full:?}");
    assert!(scroll < Duration::from_micros(500), "{scroll:?}");
    assert!(searching < Duration::from_micros(500), "{searching:?}");
}
