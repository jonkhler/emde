//! Long documents in the pager are laid out in steps: the first screens
//! before the first frame, the rest when idle or when an action needs it
//! (`G`, a search). What the reader sees is what the whole layout shows:
//! the first screen, the bottom, a resize, `<details>` opened near the top.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::FakeHighlighter;
use emde::config::PagerOptions;
use emde::layout::layout_with;
use emde::options::RenderOptions;
use emde::pager::term::{Chunk, FakeTerminal};
use emde::pager::{PagerDoc, PagerExit, PagerSession, Settings, Signals, run_on};
use emde::parse::ParseOptions;
use emde::source::{Origin, Source};
use emde::term::Caps;
use emde::term::env::Env;
use emde::theme::Theme;

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// About 400 KB: a collapsed `<details>` first, then numbered sections.
fn long_doc() -> String {
    let mut md = String::from(
        "# Long document\n\n<details>\n<summary>Notes</summary>\n\nHidden notes.\n\n</details>\n\n",
    );
    for s in 1..=3000 {
        md.push_str(&format!(
            "## Section {s}\n\nParagraph {s} has a few words of text in it, enough to \
             wrap on a narrow screen and to make the document long.\n\n"
        ));
    }
    md
}

fn doc(md: &str) -> PagerDoc {
    let source = Source::from_bytes(md.as_bytes().to_vec(), Origin::Memory);
    PagerDoc::parse(source, &ParseOptions::default())
}

fn session(md: &str) -> PagerSession {
    let mut s = PagerSession::new(
        doc(md),
        Theme::test(),
        Caps::full(),
        RenderOptions::default(),
    );
    s.highlighter = Arc::new(FakeHighlighter);
    s.env = Env::default();
    s
}

/// The rows of the screen at mark `name`.
fn rows_at(term: &FakeTerminal, name: &str) -> Vec<String> {
    let mut p = vt100::Parser::new(ROWS, COLS, 0);
    for chunk in term.chunks() {
        match chunk {
            Chunk::Write(bytes) => p.process(bytes),
            Chunk::Resize(c, r) => p.screen_mut().set_size(*r, *c),
            Chunk::Mark(m) if m == name => break,
            _ => {}
        }
    }
    let (_, cols) = p.screen().size();
    p.screen().rows(0, cols).collect()
}

/// The text of the lines of the whole pager layout, trimmed.
fn whole_layout(md: &str, cols: u16) -> Vec<String> {
    let d = doc(md);
    let settings = Settings::new(&PagerOptions::default(), &RenderOptions::default());
    let mut caps = Caps::full();
    caps.size = Some((cols, ROWS));
    let l = layout_with(
        &d.doc,
        cols,
        &Theme::test(),
        &caps,
        &RenderOptions::default(),
        &FakeHighlighter,
        &emde::layout::NoImages,
        settings.interactive(),
    );
    (0..l.len())
        .map(|i| l.line_text(i).trim().to_owned())
        .collect()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn the_first_screen_comes_before_the_rest() {
    let md = long_doc();
    assert!(md.len() > 256 * 1024, "laid out in steps");
    let term = FakeTerminal::new(COLS, ROWS)
        .mark("first")
        .keys("j")
        .mark("down")
        .keys("G")
        .mark("bottom")
        .keys("q");
    let (term, exit) = {
        let mut term = term;
        let exit = run_on(&mut term, session(&md), &Signals::new()).unwrap();
        (term, exit)
    };
    assert_eq!(exit, PagerExit::Quit);
    let whole = whole_layout(&md, COLS);
    let first = rows_at(&term, "first");
    // The screen is the start of the whole layout; the total is not known.
    for (row, line) in first.iter().zip(&whole).take(usize::from(ROWS) - 1) {
        assert_eq!(row.trim(), line, "{first:#?}");
    }
    let status = first.last().unwrap();
    assert!(status.contains("L 1/…"), "{status}");
    let down = rows_at(&term, "down");
    assert_eq!(down[0].trim(), whole[1]);
    // `G` lays out the rest: the bottom and the total are the whole
    // layout's.
    let bottom = rows_at(&term, "bottom");
    let total = whole.len();
    let status = bottom.last().unwrap();
    let top = total - (usize::from(ROWS) - 1) + 1;
    assert!(
        status.contains(&format!("L {top}/{total} 100%")),
        "{status}"
    );
    assert_eq!(bottom[usize::from(ROWS) - 2].trim(), whole[total - 1]);
}

#[test]
fn idle_time_finishes_the_layout() {
    let md = long_doc();
    let mut term = FakeTerminal::new(COLS, ROWS)
        .mark("first")
        .wait(ms(3000))
        .mark("idle")
        .keys("q");
    run_on(&mut term, session(&md), &Signals::new()).unwrap();
    assert!(rows_at(&term, "first").last().unwrap().contains("L 1/…"));
    let total = whole_layout(&md, COLS).len();
    let status = rows_at(&term, "idle").pop().unwrap();
    assert!(status.contains(&format!("L 1/{total} ")), "{status}");
}

#[test]
fn searching_lays_out_the_rest() {
    let md = long_doc();
    let mut term = FakeTerminal::new(COLS, ROWS)
        .keys("/Section 2999")
        .code(emde::pager::term::KeyCode::Enter)
        .mark("found")
        .keys("q");
    run_on(&mut term, session(&md), &Signals::new()).unwrap();
    let found = rows_at(&term, "found");
    assert!(
        found.iter().any(|r| r.contains("Section 2999")),
        "{found:#?}"
    );
    assert!(!found.last().unwrap().contains('…'), "{found:#?}");
}

#[test]
fn a_resize_keeps_the_place_and_details_open_near_the_top() {
    let md = long_doc();
    let mut term = FakeTerminal::new(COLS, ROWS)
        .keys("za")
        .wait(ms(5))
        .mark("opened")
        .keys("jjjjjjjjjj")
        .wait(ms(5))
        .mark("scrolled")
        .resize(60, ROWS)
        .wait(ms(50))
        .mark("resized")
        .keys("q");
    run_on(&mut term, session(&md), &Signals::new()).unwrap();
    let opened = rows_at(&term, "opened");
    assert!(
        opened.iter().any(|r| r.contains("Hidden notes.")),
        "{opened:#?}"
    );
    let scrolled = rows_at(&term, "scrolled");
    let resized = rows_at(&term, "resized");
    // The paragraph or heading at the top stays at the top.
    let top = scrolled[0].trim();
    let first_words: String = top.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
    assert!(
        resized[0].trim().starts_with(&first_words),
        "{top:?} became {:?}",
        resized[0]
    );
}
