//! The file picker (`O`) and `:e` end to end: scripted keys on a
//! [`FakeTerminal`], the screen replayed into `vt100` and snapshotted.
//!
//! In snapshots each row is shown between `│`s, followed (when it has any)
//! by a row of markers: `~` a matched character (the search match
//! colour), `=` reverse video (the selected result, the prompt cursor).

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::FakeHighlighter;
use emde::config::PickerOptions;
use emde::options::RenderOptions;
use emde::pager::term::{Chunk, EXIT, FakeTerminal, Key, KeyCode};
use emde::pager::{PagerDoc, PagerExit, PagerSession, PickerSetup, Signals, run_on};
use emde::parse::ParseOptions;
use emde::term::Caps;
use emde::term::env::Env;
use emde::theme::Theme;

const YELLOW: vt100::Color = vt100::Color::Rgb(0xf9, 0xe2, 0xaf);

/// A fresh directory under the target's temporary directory with these
/// files (a trailing `/` makes a directory; each file has a heading or two).
fn tree(name: &str, files: &[&str]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("picker-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in files {
        let p = dir.join(f);
        if f.ends_with('/') {
            std::fs::create_dir_all(&p).unwrap();
            continue;
        }
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
        let text = format!("# {stem}\n\nAbout {stem}.\n\n## Usage\n\n### Details\n\n## Notes\n");
        std::fs::write(&p, text).unwrap();
    }
    dir.canonicalize().unwrap()
}

/// A session for `dir/start`, with a picker that walks `dir` (no git,
/// results waited for), `~` being `dir`.
fn session(dir: &Path, start: &str) -> PagerSession {
    let doc = PagerDoc::load(&dir.join(start), &ParseOptions::default()).unwrap();
    let mut s = PagerSession::new(doc, Theme::test(), Caps::full(), RenderOptions::default());
    s.highlighter = Arc::new(FakeHighlighter);
    s.env = Env::default();
    s.picker = PickerSetup {
        options: PickerOptions::default(),
        home: Some(dir.to_path_buf()),
        cwd: Some(dir.to_path_buf()),
        recent: None,
        git: false,
        wait_when_idle: true,
    };
    s
}

fn run(mut term: FakeTerminal, session: PagerSession) -> (FakeTerminal, PagerExit) {
    let exit = run_on(&mut term, session, &Signals::new()).expect("the pager runs");
    (term, exit)
}

fn screen_at(term: &FakeTerminal, cols: u16, rows: u16, name: &str) -> vt100::Parser {
    let mut p = vt100::Parser::new(rows, cols, 0);
    for chunk in term.chunks() {
        match chunk {
            Chunk::Write(bytes) => p.process(bytes),
            Chunk::Resize(c, r) => p.screen_mut().set_size(*r, *c),
            Chunk::Mark(m) if m == name => break,
            Chunk::Mark(_) | Chunk::Suspend | Chunk::Run(_) => {}
        }
    }
    p
}

/// The screen as text; see the module docs.
fn screen(term: &FakeTerminal, cols: u16, rows: u16, name: &str) -> String {
    let p = screen_at(term, cols, rows, name);
    let screen = p.screen();
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
            let contents = cell.contents();
            text.push_str(if contents.is_empty() { " " } else { contents });
            let mark = if cell.bgcolor() == YELLOW {
                '~'
            } else if cell.inverse() {
                '='
            } else {
                ' '
            };
            for _ in 0..if cell.is_wide() { 2 } else { 1 } {
                marks.push(mark);
            }
        }
        out.push_str(&format!("{:>2}│{text}│\n", r + 1));
        if !marks.trim().is_empty() {
            out.push_str(&format!("  │{}│\n", marks.trim_end()));
        }
    }
    out
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Keys typed one by one, a pause after each.
fn typed(mut term: FakeTerminal, text: &str) -> FakeTerminal {
    let mut buf = [0u8; 4];
    for c in text.chars() {
        term = term.keys(c.encode_utf8(&mut buf)).wait(ms(5));
    }
    term
}

const NOTES: &[&str] = &[
    "README.md",
    "guide.md",
    "notes/getting-started.md",
    "notes/gardening.md",
    "notes/meeting-2024.md",
    "archive/old-guide.md",
    "src/main.rs",
    "node_modules/pkg/README.md",
];

#[test]
fn the_picker_finds_and_opens_files() {
    let dir = tree("fuzzy", NOTES);
    let term = typed(FakeTerminal::new(80, 20).keys("O").mark("open"), "gd")
        .mark("typed")
        .code(KeyCode::Down)
        .mark("down")
        .code(KeyCode::Enter)
        .mark("opened")
        .code(KeyCode::Backspace)
        .mark("back")
        .keys("q");
    let (term, exit) = run(term, session(&dir, "README.md"));
    assert_eq!(exit, PagerExit::Quit);
    insta::assert_snapshot!("picker-open", screen(&term, 80, 20, "open"));
    insta::assert_snapshot!("picker-typed", screen(&term, 80, 20, "typed"));
    let opened = screen(&term, 80, 20, "opened");
    assert!(opened.contains("gardening.md"), "the second: {opened}");
    assert!(screen(&term, 80, 20, "back").contains("README.md"));
}

#[test]
fn the_picker_lists_directories() {
    let dir = tree("path", NOTES);
    let term = typed(FakeTerminal::new(80, 16).keys("O").wait(ms(5)), "./")
        .mark("listed")
        .keys("not")
        .wait(ms(5))
        .code(KeyCode::Tab)
        .mark("completed")
        .keys("ga")
        .wait(ms(5))
        .code(KeyCode::Enter)
        .mark("opened")
        .keys("q");
    let (term, _) = run(term, session(&dir, "README.md"));
    insta::assert_snapshot!("picker-path", screen(&term, 80, 16, "listed"));
    insta::assert_snapshot!("picker-path-completed", screen(&term, 80, 16, "completed"));
    let opened = screen(&term, 80, 16, "opened");
    assert!(opened.contains("gardening"), "{opened}");
}

#[test]
fn the_picker_previews_headings_on_wide_screens() {
    let dir = tree("preview", NOTES);
    let term = typed(FakeTerminal::new(120, 16).keys("O"), "start")
        .mark("preview")
        .code(KeyCode::Esc)
        .keys("q");
    let (term, _) = run(term, session(&dir, "README.md"));
    insta::assert_snapshot!("picker-preview", screen(&term, 120, 16, "preview"));
}

#[test]
fn colon_e_completes_and_opens() {
    let dir = tree("colon-e", NOTES);
    let term = typed(FakeTerminal::new(60, 8), ":e notes/me")
        .code(KeyCode::Tab)
        .mark("completed")
        .code(KeyCode::Enter)
        .mark("opened")
        .keys(":e ~/nowhere.md\n")
        .mark("missing")
        .keys(":e g")
        .code(KeyCode::Tab)
        .mark("several")
        .code(KeyCode::Esc)
        .keys(":e ~/notes\n")
        .mark("directory")
        .code(KeyCode::Esc)
        .keys("q");
    let (term, _) = run(term, session(&dir, "README.md"));
    insta::assert_snapshot!("colon-e-completed", screen(&term, 60, 8, "completed"));
    let opened = screen(&term, 60, 8, "opened");
    assert!(opened.contains("meeting-2024"), "{opened}");
    let missing = screen(&term, 60, 8, "missing");
    assert!(missing.contains("no such file: ~/nowhere.md"), "{missing}");
    let several = screen(&term, 60, 8, "several");
    assert!(
        several.contains("gardening.md  getting-started.md"),
        "{several}"
    );
    // A directory without a README: listed in the picker.
    let listed = screen(&term, 60, 8, "directory");
    assert!(listed.contains("~/notes/"), "{listed}");
}

#[test]
fn the_picker_on_a_tiny_screen() {
    let dir = tree("tiny", NOTES);
    let term = typed(FakeTerminal::new(20, 4).keys("O"), "gui")
        .mark("tiny")
        .code(KeyCode::Enter)
        .mark("opened")
        .keys("q");
    let (term, _) = run(term, session(&dir, "README.md"));
    let tiny = screen(&term, 20, 4, "tiny");
    assert!(tiny.contains("›gui"), "{tiny}");
    assert!(screen(&term, 20, 4, "opened").contains("guide"));
    // Nothing at all fits: no panic either.
    let term = FakeTerminal::new(3, 1).keys("O").wait(ms(5)).keys("x\x1bq");
    run(term, session(&dir, "README.md"));
}

#[test]
fn quitting_while_the_search_runs() {
    // Many files, a search not waited for: quitting does not wait for it.
    let names: Vec<String> = (0..3000).map(|i| format!("d{}/f{i}.md", i % 50)).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let dir = tree("busy", &refs);
    let mut s = session(&dir, "d0/f0.md");
    s.picker.wait_when_idle = false;
    s.picker.options.roots = vec![dir.clone(), dir.clone()];
    let term = FakeTerminal::new(80, 20)
        .keys("O")
        .key(Key::ctrl('t'))
        .keys("f1")
        .key(Key::ctrl('c'))
        .keys("q");
    let (term, exit) = run(term, s);
    assert_eq!(exit, PagerExit::Quit);
    assert!(term.output().ends_with(EXIT), "the terminal is put back");
}

#[test]
fn opened_files_are_recorded() {
    let dir = tree("recent", NOTES);
    let store = dir.join("state/emde/recent");
    let mut s = session(&dir, "README.md");
    s.picker.recent = Some(store.clone());
    let term = typed(FakeTerminal::new(80, 20), ":e guide.md\n")
        .mark("opened")
        .keys("O")
        .mark("picker")
        .code(KeyCode::Esc)
        .keys("q");
    let (term, _) = run(term, s);
    let text = std::fs::read_to_string(&store).unwrap();
    assert!(
        text.contains("README.md") && text.contains("guide.md"),
        "{text}"
    );
    // The recent file comes first in the picker (the current one is left
    // out).
    let picker = screen(&term, 80, 20, "picker");
    let first = picker
        .lines()
        .find(|l| l.contains(".md"))
        .unwrap_or_default();
    assert!(first.contains("README.md"), "{picker}");
}
