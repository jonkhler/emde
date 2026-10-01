//! Unit tests of the pure parts: `update()` for every group of actions,
//! with a small driver standing in for the shell (it lays documents out
//! when asked to).

use std::path::{Path, PathBuf};

use super::keymap::{self, Command};
use super::state::{DocKey, MAX_PAGES, Mode};
use super::term::{Button, Key, KeyCode, Mouse, MouseKind};
use super::*;
use crate::highlight::PlainHighlighter;
use crate::layout::{NoImages, layout};
use crate::options::RenderOptions;
use crate::parse::ParseOptions;
use crate::source::{Origin, Source};
use crate::term::Caps;
use crate::theme::Theme;

/// A document of `sections` sections of `paras` paragraphs each.
fn long_doc(sections: usize, paras: usize) -> String {
    let mut md = String::from("# Title\n\nIntro paragraph.\n\n");
    for s in 1..=sections {
        md.push_str(&format!("## Section {s}\n\n"));
        for p in 1..=paras {
            md.push_str(&format!("Paragraph {s}.{p} with some words.\n\n"));
        }
        md.push_str(&format!("### Detail {s}\n\nDetail text {s}.\n\n"));
    }
    md
}

fn pdoc(md: &str, origin: Origin) -> PagerDoc {
    let source = Source::from_bytes(md.as_bytes().to_vec(), origin);
    PagerDoc::parse(source, &ParseOptions::default())
}

fn lay(doc: &Document, cols: u16, wide: bool) -> Layout {
    let mut opts = RenderOptions::default();
    if wide {
        opts.max_width = 0;
    }
    layout(
        doc,
        cols,
        &Theme::test(),
        &Caps::full(),
        &opts,
        &PlainHighlighter,
        &NoImages,
    )
}

fn state_for(doc: PagerDoc, cols: u16, rows: u16) -> State {
    let l = lay(&doc.doc, cols, false);
    let pager = PagerOptions::default();
    let settings = Settings::new(&pager, &RenderOptions::default());
    let key = match &doc.source.origin {
        Origin::File(p) => Some(DocKey::Path(p.clone())),
        _ => None,
    };
    State::new(doc, key, l, (cols, rows), &pager, settings)
}

fn state(md: &str, cols: u16, rows: u16) -> State {
    state_for(pdoc(md, Origin::Memory), cols, rows)
}

/// Update, doing what the shell does for [`Effect::Relayout`]; the other
/// effects are returned.
fn drive(s: &mut State, action: Action) -> Vec<Effect> {
    let mut out = Vec::new();
    let mut queue = update(s, action);
    while !queue.is_empty() {
        let effect = queue.remove(0);
        if effect == Effect::Relayout {
            let l = lay(s.document(), s.size().0, s.wide());
            queue.extend(update(s, Action::Layout(l)));
        } else {
            out.push(effect);
        }
    }
    out
}

fn key(s: &mut State, k: Key) -> Vec<Effect> {
    drive(s, Action::Key(k))
}

fn keys(s: &mut State, text: &str) -> Vec<Effect> {
    let mut out = Vec::new();
    for c in text.chars() {
        let k = match c {
            '\n' => Key::plain(KeyCode::Enter),
            c => Key::char(c),
        };
        out.extend(key(s, k));
    }
    out
}

fn code(s: &mut State, c: KeyCode) -> Vec<Effect> {
    key(s, Key::plain(c))
}

fn top_text(s: &State) -> String {
    s.layout().line_text(s.top())
}

/// The line showing a heading.
fn heading_line(s: &State, title: &str) -> usize {
    let doc = s.document();
    let h = doc
        .headings
        .iter()
        .position(|h| &*h.title == title)
        .unwrap();
    s.layout().heading_line[h] as usize
}

// ---------------------------------------------------------------------------
// Scrolling
// ---------------------------------------------------------------------------

#[test]
fn line_and_page_scrolling() {
    let mut s = state(&long_doc(10, 4), 60, 12);
    let rows = 11;
    assert_eq!(s.top(), 0);
    keys(&mut s, "j");
    assert_eq!(s.top(), 1);
    keys(&mut s, "5j");
    assert_eq!(s.top(), 6);
    key(&mut s, Key::ctrl('y'));
    assert_eq!(s.top(), 5);
    code(&mut s, KeyCode::Down);
    key(&mut s, Key::ctrl('e'));
    assert_eq!(s.top(), 7);
    code(&mut s, KeyCode::Up);
    keys(&mut s, "k");
    assert_eq!(s.top(), 5);
    keys(&mut s, " ");
    assert_eq!(s.top(), 5 + rows);
    keys(&mut s, "b");
    assert_eq!(s.top(), 5);
    keys(&mut s, "d");
    assert_eq!(s.top(), 5 + rows / 2);
    keys(&mut s, "u");
    assert_eq!(s.top(), 5);
    code(&mut s, KeyCode::PageDown);
    assert_eq!(s.top(), 5 + rows);
    code(&mut s, KeyCode::PageUp);
    key(&mut s, Key::ctrl('f'));
    assert_eq!(s.top(), 5 + rows);
    keys(&mut s, "2b");
    assert_eq!(s.top(), 0, "clamped at the top");
}

#[test]
fn jumps_to_ends_lines_and_percentages() {
    let mut s = state(&long_doc(10, 4), 60, 12);
    let total = s.layout().len();
    let max = total - 11;
    keys(&mut s, "G");
    assert_eq!(s.top(), max, "the last line at the bottom");
    keys(&mut s, "j");
    assert_eq!(s.top(), max, "no scrolling past the end");
    keys(&mut s, "gg");
    assert_eq!(s.top(), 0);
    keys(&mut s, "G");
    keys(&mut s, "g");
    assert_eq!(s.top(), max, "g waits for a second key");
    drive(&mut s, Action::KeyTimeout);
    assert_eq!(s.top(), 0, "…or for the time to run out");
    keys(&mut s, "Ggj");
    assert_eq!(s.top(), 1, "…or for a key that does not continue it");
    code(&mut s, KeyCode::End);
    assert_eq!(s.top(), max);
    code(&mut s, KeyCode::Home);
    assert_eq!(s.top(), 0);
    keys(&mut s, "50%");
    assert_eq!(s.top(), total / 2);
    keys(&mut s, "%");
    assert_eq!(s.top(), 0, "no count: 0%");
    keys(&mut s, "20gg");
    assert_eq!(s.top(), 19, "line 20");
    keys(&mut s, "3G");
    assert_eq!(s.top(), 2);
    keys(&mut s, "999999999gg");
    assert_eq!(s.top(), max, "huge counts are clamped");
}

#[test]
fn a_stray_key_drops_the_count() {
    let mut s = state(&long_doc(5, 3), 60, 12);
    keys(&mut s, "5xj");
    assert_eq!(s.top(), 1, "x dropped the 5");
}

#[test]
fn short_documents_do_not_scroll() {
    let mut s = state("# One\n\nTwo lines.", 60, 20);
    keys(&mut s, "Gjj ");
    assert_eq!(s.top(), 0);
}

// ---------------------------------------------------------------------------
// Headings
// ---------------------------------------------------------------------------

#[test]
fn heading_jumps() {
    let mut s = state(&long_doc(6, 3), 60, 12);
    keys(&mut s, "]");
    assert_eq!(top_text(&s).trim(), "Section 1");
    keys(&mut s, "]");
    assert_eq!(top_text(&s).trim(), "▎ Detail 1");
    keys(&mut s, "2]");
    assert_eq!(top_text(&s).trim(), "▎ Detail 2");
    keys(&mut s, "[");
    assert_eq!(top_text(&s).trim(), "Section 2");
    // `}` stays at the level of the current section: h2 → h2.
    keys(&mut s, "}");
    assert_eq!(top_text(&s).trim(), "Section 3");
    keys(&mut s, "{");
    assert_eq!(top_text(&s).trim(), "Section 2");
    keys(&mut s, "g[");
    assert_eq!(s.top(), 0);
    assert_eq!(s.message(), Some("no heading above"));
    // At the end, the next heading is on screen already.
    keys(&mut s, "G");
    let shown = (s.top()..s.layout().len()).any(|i| s.layout().line_text(i).contains("Detail 6"));
    assert!(shown);
    keys(&mut s, "]");
    assert_eq!(s.message(), Some("end of the document"));
}

#[test]
fn same_or_higher_level_from_a_subsection() {
    let mut s = state(&long_doc(3, 2), 60, 10);
    let detail = heading_line(&s, "Detail 1");
    s.top = detail;
    keys(&mut s, "}");
    // From an h3, the next h3-or-higher is the next h2.
    assert_eq!(top_text(&s).trim(), "Section 2");
}

// ---------------------------------------------------------------------------
// Outline
// ---------------------------------------------------------------------------

fn outline(s: &State) -> Option<(String, Vec<String>, usize)> {
    match &s.mode {
        Mode::Outline(o) => Some((
            o.filter.clone(),
            o.items
                .iter()
                .map(|h| s.document().heading(*h).unwrap().title.to_string())
                .collect(),
            o.selected,
        )),
        _ => None,
    }
}

#[test]
fn outline_filters_and_jumps() {
    let mut s = state(&long_doc(4, 3), 60, 16);
    s.top = heading_line(&s, "Section 2") + 1;
    keys(&mut s, "t");
    let (filter, items, selected) = outline(&s).unwrap();
    assert!(filter.is_empty());
    assert_eq!(items.len(), 9);
    assert_eq!(
        items[selected], "Section 2",
        "starts at the current section"
    );
    keys(&mut s, "det");
    let (_, items, selected) = outline(&s).unwrap();
    assert_eq!(items, ["Detail 1", "Detail 2", "Detail 3", "Detail 4"]);
    assert_eq!(selected, 0);
    code(&mut s, KeyCode::Down);
    code(&mut s, KeyCode::Down);
    code(&mut s, KeyCode::Up);
    code(&mut s, KeyCode::Backspace);
    let (filter, items, _) = outline(&s).unwrap();
    assert_eq!(filter, "de");
    assert_eq!(items.len(), 4);
    code(&mut s, KeyCode::Enter);
    assert!(outline(&s).is_none());
    assert_eq!(top_text(&s).trim(), "▎ Detail 2");
    assert_eq!(s.back_len(), 1, "an outline jump can be undone");
    code(&mut s, KeyCode::Backspace);
    assert_eq!(s.top(), heading_line(&s, "Section 2") + 1);
}

#[test]
fn outline_closes_and_ignores_a_document_without_headings() {
    let mut s = state(&long_doc(2, 2), 60, 16);
    keys(&mut s, "t");
    assert!(outline(&s).is_some());
    code(&mut s, KeyCode::Esc);
    assert!(outline(&s).is_none());
    keys(&mut s, "tq");
    assert_eq!(outline(&s).unwrap().0, "q", "q types into the filter");
    let effects = key(&mut s, Key::ctrl('c'));
    assert!(effects.is_empty());
    assert!(outline(&s).is_none());
    let mut plain = state("no headings here", 60, 10);
    keys(&mut plain, "t");
    assert!(outline(&plain).is_none());
    assert_eq!(plain.message(), Some("no headings"));
}

#[test]
fn outline_mouse() {
    let mut s = state(&long_doc(4, 3), 80, 24);
    keys(&mut s, "t");
    let g = toc::outline_box(80, 24, 9);
    // Third entry: rows below the border and the filter row.
    drive(
        &mut s,
        Action::Mouse(Mouse {
            kind: MouseKind::Press(Button::Left),
            col: g.x + 3,
            row: g.y + 2 + 2,
        }),
    );
    assert!(outline(&s).is_none());
    assert_eq!(top_text(&s).trim(), "▎ Detail 1");
    keys(&mut s, "t");
    drive(
        &mut s,
        Action::Mouse(Mouse {
            kind: MouseKind::Press(Button::Left),
            col: 0,
            row: 0,
        }),
    );
    assert!(outline(&s).is_none(), "a click outside closes it");
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[test]
fn incremental_search_and_matches() {
    let mut s = state(&long_doc(8, 3), 60, 10);
    keys(&mut s, "/");
    assert_eq!(s.mode_name(), "prompt");
    keys(&mut s, "Paragraph 5");
    assert_eq!(s.match_count(), Some(3));
    let shown = (s.top()..s.top() + 9)
        .map(|i| s.layout().line_text(i))
        .any(|t| t.contains("Paragraph 5.1"));
    assert!(shown, "the first match is on screen");
    code(&mut s, KeyCode::Enter);
    assert_eq!(s.mode_name(), "normal");
    assert_eq!(s.current_match(), Some(0));
    keys(&mut s, "n");
    assert_eq!(s.current_match(), Some(1));
    keys(&mut s, "nn");
    assert_eq!(s.current_match(), Some(0), "wrapped");
    assert_eq!(s.message(), Some("search wrapped to the top"));
    keys(&mut s, "N");
    assert_eq!(s.current_match(), Some(2));
    code(&mut s, KeyCode::Esc);
    assert_eq!(s.match_count(), None, "Esc clears the search");
    keys(&mut s, "n");
    assert_eq!(
        s.match_count(),
        Some(3),
        "n searches the last pattern again"
    );
}

#[test]
fn a_resize_while_searching_keeps_the_origin() {
    let mut s = state(&long_doc(10, 4), 80, 12);
    s.top = heading_line(&s, "Section 3");
    keys(&mut s, "/Detail 9");
    drive(&mut s, Action::Resize { cols: 40, rows: 12 });
    code(&mut s, KeyCode::Esc);
    assert_eq!(
        top_text(&s).trim(),
        "Section 3",
        "back where the search began"
    );
}

#[test]
fn n_keeps_the_direction_after_the_search_is_cleared() {
    let mut s = state(&long_doc(8, 3), 60, 10);
    keys(&mut s, "G?Section\n");
    let first = s.current_match().unwrap();
    code(&mut s, KeyCode::Esc);
    assert_eq!(s.match_count(), None);
    keys(&mut s, "n");
    let next = s.current_match().unwrap();
    assert!(next < first, "still going up: {next} after {first}");
}

#[test]
fn smart_case() {
    let mut s = state("Rust and rust and RUST.", 60, 10);
    keys(&mut s, "/rust\n");
    assert_eq!(s.match_count(), Some(3));
    keys(&mut s, "/Rust\n");
    assert_eq!(s.match_count(), Some(1));
}

#[test]
fn backward_search_and_cancel() {
    let mut s = state(&long_doc(8, 3), 60, 10);
    keys(&mut s, "G");
    let bottom = s.top();
    keys(&mut s, "?Section");
    let during = s.top();
    code(&mut s, KeyCode::Esc);
    assert_eq!(s.top(), bottom, "cancelling goes back");
    assert_eq!(s.match_count(), None);
    keys(&mut s, "?Section\n");
    assert_eq!(s.top(), during);
    let cur = s.current_match().unwrap();
    assert_eq!(cur, 7, "the last Section above the screen");
    keys(&mut s, "n");
    assert_eq!(s.current_match(), Some(6), "n goes up after ?");
    keys(&mut s, "N");
    assert_eq!(s.current_match(), Some(7));
}

#[test]
fn search_editing_and_misses() {
    let mut s = state("alpha beta gamma", 60, 10);
    keys(&mut s, "/betx");
    assert_eq!(s.match_count(), Some(0));
    code(&mut s, KeyCode::Backspace);
    assert_eq!(s.match_count(), Some(1));
    key(&mut s, Key::ctrl('u'));
    assert_eq!(s.match_count(), None);
    code(&mut s, KeyCode::Backspace);
    assert_eq!(
        s.mode_name(),
        "normal",
        "Backspace on an empty prompt cancels"
    );
    keys(&mut s, "/zeta\n");
    assert_eq!(s.message(), Some("not found: zeta"));
    assert_eq!(s.match_count(), None);
    drive(&mut s, Action::Key(Key::char('/')));
    drive(&mut s, Action::Paste("gam\u{1b}ma".into()));
    assert_eq!(s.match_count(), Some(1), "pasted text, controls dropped");
    code(&mut s, KeyCode::Enter);
    keys(&mut s, "/\n");
    assert_eq!(
        s.match_count(),
        Some(1),
        "an empty pattern repeats the last"
    );
}

#[test]
fn matches_across_wrapped_lines() {
    let mut s = state("aaaa bbbb cccc dddd eeee ffff", 12, 10);
    keys(&mut s, "/bbbb cccc\n");
    assert_eq!(s.match_count(), Some(1));
    let l = s.layout();
    let lines: Vec<String> = (0..l.len()).map(|i| l.line_text(i)).collect();
    assert!(
        !lines.iter().any(|t| t.contains("bbbb cccc")),
        "the phrase is wrapped: {lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

const LINKS: &str = "# Top\n\nSee [install](#install), [web](https://example.com), \
[guide](guide.md#usage), [image](pic.png) and a note[^n].\n\n## Install\n\nSteps.\n\n\
[back up](#top)\n\n[^n]: The note.";

#[test]
fn tab_focus_and_following_anchors() {
    let mut s = state(&(LINKS.to_owned() + &"\n\nfiller\n".repeat(40)), 60, 10);
    assert_eq!(s.focused_link(), None);
    code(&mut s, KeyCode::Enter);
    assert_eq!(s.message(), Some("no link focused: Tab or o picks one"));
    code(&mut s, KeyCode::Tab);
    let first = s.focused_link().unwrap();
    assert_eq!(&*s.document().link(first).unwrap().url, "#install");
    code(&mut s, KeyCode::Enter);
    assert_eq!(top_text(&s).trim(), "Install");
    assert_eq!(s.back_len(), 1);
    // The focused link is off screen now: Enter does not follow it again.
    code(&mut s, KeyCode::Enter);
    assert_eq!(s.back_len(), 1);
    assert_eq!(s.message(), Some("no link focused: Tab or o picks one"));
    code(&mut s, KeyCode::Backspace);
    assert_eq!(s.top(), 0);
    assert_eq!(s.forward_len(), 1);
    keys(&mut s, "L");
    assert_eq!(top_text(&s).trim(), "Install");
    keys(&mut s, "H");
    assert_eq!(s.top(), 0);
    code(&mut s, KeyCode::BackTab);
    code(&mut s, KeyCode::BackTab);
    let before = s.focused_link().unwrap();
    code(&mut s, KeyCode::Tab);
    assert_ne!(s.focused_link(), Some(before));
    code(&mut s, KeyCode::Esc);
    assert_eq!(
        s.focused_link(),
        None,
        "Esc without a search clears the focus"
    );
}

#[test]
fn following_other_kinds_of_links() {
    let long = format!("{LINKS}\n\n{}", "filler\n\n".repeat(30));
    let mut s = state(&long, 80, 20);
    code(&mut s, KeyCode::Tab);
    code(&mut s, KeyCode::Tab);
    assert_eq!(
        code(&mut s, KeyCode::Enter),
        [Effect::Open("https://example.com".into())]
    );
    assert_eq!(
        keys(&mut s, "y"),
        [Effect::Copy {
            text: "https://example.com".into(),
            what: "copied https://example.com".into()
        }]
    );
    code(&mut s, KeyCode::Tab);
    let effects = code(&mut s, KeyCode::Enter);
    assert_eq!(
        effects,
        [Effect::Load(LoadRequest {
            path: PathBuf::from("./guide.md"),
            anchor: Some("usage".into()),
            restore: None,
            nav: Nav::Push,
        })]
    );
    code(&mut s, KeyCode::Tab);
    assert_eq!(
        code(&mut s, KeyCode::Enter),
        [Effect::ShowFile(PathBuf::from("./pic.png"))]
    );
    // The footnote reference leads to the note (at the end: on screen).
    code(&mut s, KeyCode::Tab);
    code(&mut s, KeyCode::Enter);
    assert_eq!(s.top(), s.max_top());
    let shown: Vec<String> = (s.top()..s.layout().len())
        .map(|i| s.layout().line_text(i))
        .collect();
    assert!(shown.iter().any(|t| t.contains("The note.")), "{shown:?}");
}

#[test]
fn link_hints() {
    let mut s = state(LINKS, 80, 20);
    keys(&mut s, "o");
    assert_eq!(s.mode_name(), "hints");
    let Mode::Hints(h) = &s.mode else {
        unreachable!()
    };
    let labels: Vec<&str> = h.labels.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, ["a", "s", "d", "f", "g", "h", "j"]);
    let effects = keys(&mut s, "s");
    assert_eq!(effects, [Effect::Open("https://example.com".into())]);
    assert_eq!(s.mode_name(), "normal");
    keys(&mut s, "ox");
    assert_eq!(s.mode_name(), "normal");
    assert_eq!(s.message(), Some("no label x"));
    keys(&mut s, "o");
    code(&mut s, KeyCode::Esc);
    assert_eq!(s.mode_name(), "normal");
    // A pasted label follows the link too.
    keys(&mut s, "o");
    let effects = drive(&mut s, Action::Paste("s".into()));
    assert_eq!(effects, [Effect::Open("https://example.com".into())]);
    let mut none = state("no links", 40, 10);
    keys(&mut none, "o");
    assert_eq!(none.message(), Some("no links on screen"));
}

#[test]
fn clicking_a_link_follows_it() {
    let mut s = state(LINKS, 80, 20);
    let hit = s
        .layout()
        .link_hits
        .iter()
        .find(|h| &*s.document().link(h.link).unwrap().url == "https://example.com")
        .unwrap()
        .clone();
    let col = s.layout().indent + hit.cols.start;
    let effects = drive(
        &mut s,
        Action::Mouse(Mouse {
            kind: MouseKind::Press(Button::Left),
            col,
            row: u16::try_from(hit.line).unwrap(),
        }),
    );
    assert_eq!(effects, [Effect::Open("https://example.com".into())]);
    let effects = drive(
        &mut s,
        Action::Mouse(Mouse {
            kind: MouseKind::Press(Button::Left),
            col: 0,
            row: 19,
        }),
    );
    assert!(effects.is_empty(), "the status bar is not a link");
}

// ---------------------------------------------------------------------------
// Documents and history
// ---------------------------------------------------------------------------

fn file(name: &str) -> PathBuf {
    PathBuf::from(format!("/virtual/{name}"))
}

fn opened(s: &mut State, name: &str, md: &str, anchor: Option<&str>) -> Vec<Effect> {
    let path = file(name);
    let doc = pdoc(md, Origin::File(path.clone()));
    drive(
        s,
        Action::Opened {
            doc,
            key: Some(DocKey::Path(path.clone())),
            request: LoadRequest {
                path,
                anchor: anchor.map(str::to_owned),
                restore: None,
                nav: Nav::Push,
            },
        },
    )
}

#[test]
fn opening_documents_and_going_back() {
    let mut s = state(&long_doc(4, 3), 60, 10);
    keys(&mut s, "10j");
    opened(&mut s, "b.md", &long_doc(3, 2), Some("section-2"));
    assert_eq!(s.key(), &DocKey::Path(file("b.md")));
    assert_eq!(top_text(&s).trim(), "Section 2");
    code(&mut s, KeyCode::Backspace);
    assert_eq!(s.key(), &DocKey::Unnamed(0), "back in memory: no load");
    assert_eq!(s.top(), 10);
    let effects = keys(&mut s, "L");
    assert!(effects.is_empty());
    assert_eq!(top_text(&s).trim(), "Section 2");
    let effects = opened(&mut s, "c.md", "# C\n\nMissing anchor.", Some("nope"));
    assert!(effects.is_empty());
    assert_eq!(s.message(), Some("no anchor #nope in c.md"));
    assert_eq!(s.back_len(), 2);
    assert_eq!(s.forward_len(), 0, "a new document clears forward");
}

#[test]
fn documents_beyond_the_lru_are_loaded_again() {
    let mut s = state("# Start", 60, 10);
    for i in 0..MAX_PAGES + 2 {
        opened(&mut s, &format!("{i}.md"), &format!("# Doc {i}"), None);
    }
    assert_eq!(
        s.pages_kept(),
        MAX_PAGES,
        "at most eight documents in memory"
    );
    assert!(s.has_page(&DocKey::Unnamed(0)), "standard input is kept");
    assert!(!s.has_page(&DocKey::Path(file("0.md"))));
    // Back through the kept documents without loading…
    for _ in 0..MAX_PAGES - 2 {
        assert!(code(&mut s, KeyCode::Backspace).is_empty());
    }
    // …until one that was dropped has to be read again.
    let effects = code(&mut s, KeyCode::Backspace);
    let [Effect::Load(req)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(req.nav, Nav::Back);
    assert_eq!(req.path, file("2.md"), "start and 3.md to 9.md are kept");
    assert!(req.restore.is_some());
}

#[test]
fn a_back_entry_that_cannot_be_read_is_dropped() {
    let mut s = state("# Start", 60, 10);
    for i in 0..MAX_PAGES + 2 {
        opened(&mut s, &format!("{i}.md"), &format!("# Doc {i}"), None);
    }
    for _ in 0..MAX_PAGES - 2 {
        code(&mut s, KeyCode::Backspace);
    }
    let effects = code(&mut s, KeyCode::Backspace);
    let [Effect::Load(req)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    let back = s.back_len();
    drive(
        &mut s,
        Action::LoadFailed {
            request: req.clone(),
            error: "2.md: gone".into(),
        },
    );
    assert_eq!(s.message(), Some("2.md: gone"));
    assert_eq!(s.back_len(), back - 1, "the next Back goes further");
    let effects = code(&mut s, KeyCode::Backspace);
    let [Effect::Load(req)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(req.path, file("1.md"));
}

#[test]
fn switching_to_a_document_in_memory() {
    let mut s = state("# Start\n\n[self](#start)", 60, 10);
    opened(&mut s, "b.md", "# B", None);
    drive(
        &mut s,
        Action::Switch {
            key: DocKey::Unnamed(0),
            request: LoadRequest {
                path: PathBuf::new(),
                anchor: Some("start".into()),
                restore: None,
                nav: Nav::Push,
            },
        },
    );
    assert_eq!(s.key(), &DocKey::Unnamed(0));
    assert_eq!(s.back_len(), 2);
    drive(
        &mut s,
        Action::Switch {
            key: DocKey::Path(file("gone.md")),
            request: LoadRequest {
                path: file("gone.md"),
                anchor: None,
                restore: None,
                nav: Nav::Push,
            },
        },
    );
    assert_eq!(s.message(), Some("that document is no longer in memory"));
}

// ---------------------------------------------------------------------------
// The files named on the command line, and the `:` prompt
// ---------------------------------------------------------------------------

/// A state for files `a.md`, `b.md` and `c.md`, showing `a.md`.
fn three_files() -> State {
    let mut s = state_for(pdoc("# A", Origin::File(file("a.md"))), 60, 10);
    let more = ["b.md", "c.md"]
        .into_iter()
        .map(|name| {
            let doc = pdoc(&format!("# {name}"), Origin::File(file(name)));
            (doc, Some(DocKey::Path(file(name))))
        })
        .collect();
    s.add_files(more);
    s
}

/// Type a command at the `:` prompt.
fn colon(s: &mut State, command: &str) -> Vec<Effect> {
    keys(s, &format!(":{command}\n"))
}

#[test]
fn colon_n_and_colon_p_walk_the_files() {
    let mut s = three_files();
    assert_eq!(s.file_position(), Some((0, 3)));
    assert_eq!(
        s.key(),
        &DocKey::Path(file("a.md")),
        "the first stays current"
    );
    assert_eq!(s.pages_kept(), 3, "all in memory");
    assert!(colon(&mut s, "n").is_empty(), "in memory: no load");
    assert_eq!(s.key(), &DocKey::Path(file("b.md")));
    assert_eq!(s.file_position(), Some((1, 3)));
    colon(&mut s, "next");
    assert_eq!(s.file_position(), Some((2, 3)));
    colon(&mut s, "n");
    assert_eq!(s.message(), Some("this is the last file"));
    assert_eq!(s.file_position(), Some((2, 3)));
    colon(&mut s, "p");
    assert_eq!(s.file_position(), Some((1, 3)));
    colon(&mut s, "x");
    assert_eq!(s.file_position(), Some((0, 3)));
    colon(&mut s, "p");
    assert_eq!(s.message(), Some("this is the first file"));
    // Back goes where `:n` came from.
    colon(&mut s, "n");
    code(&mut s, KeyCode::Backspace);
    assert_eq!(s.file_position(), Some((0, 3)));
    assert_eq!(colon(&mut s, "q"), [Effect::Quit]);
}

#[test]
fn the_colon_prompt_edits_and_cancels() {
    let mut s = three_files();
    keys(&mut s, ":");
    assert_eq!(s.mode_name(), "command");
    keys(&mut s, "nq");
    code(&mut s, KeyCode::Backspace);
    assert!(matches!(&s.mode, Mode::Command(input) if input == "n"));
    key(&mut s, Key::ctrl('u'));
    assert!(matches!(&s.mode, Mode::Command(input) if input.is_empty()));
    code(&mut s, KeyCode::Backspace);
    assert_eq!(s.mode_name(), "normal", "erasing nothing closes it");
    keys(&mut s, ":abc");
    code(&mut s, KeyCode::Esc);
    assert_eq!(s.mode_name(), "normal");
    assert_eq!(s.key(), &DocKey::Path(file("a.md")));
    drive(&mut s, Action::Paste("n".into()));
    assert_eq!(
        s.mode_name(),
        "normal",
        "pasting outside a prompt does nothing"
    );
    keys(&mut s, ":");
    drive(&mut s, Action::Paste("n".into()));
    code(&mut s, KeyCode::Enter);
    assert_eq!(s.file_position(), Some((1, 3)));
    colon(&mut s, "frobnicate");
    assert_eq!(
        s.message(),
        Some("unknown command :frobnicate (:n, :p, :x, :q)")
    );
    assert!(colon(&mut s, "").is_empty());
}

#[test]
fn files_the_history_dropped_are_read_again() {
    let mut s = three_files();
    for i in 0..MAX_PAGES {
        opened(&mut s, &format!("{i}.md"), &format!("# Doc {i}"), None);
    }
    assert!(!s.has_page(&DocKey::Path(file("b.md"))));
    assert_eq!(s.file_position(), None, "not one of the files");
    let effects = colon(&mut s, "n");
    let [Effect::Load(req)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(req.path, file("b.md"));
    let request = req.clone();
    drive(
        &mut s,
        Action::Opened {
            doc: pdoc("# b again", Origin::File(file("b.md"))),
            key: Some(DocKey::Path(file("b.md"))),
            request: request.clone(),
        },
    );
    assert_eq!(s.file_position(), Some((1, 3)));
    // A file that cannot be read: nothing changes.
    let effects = colon(&mut s, "n");
    let [Effect::Load(req)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    drive(
        &mut s,
        Action::LoadFailed {
            request: req.clone(),
            error: "c.md: gone".into(),
        },
    );
    assert_eq!(s.file_position(), Some((1, 3)));
    assert_eq!(s.message(), Some("c.md: gone"));
}

#[test]
fn a_single_file_has_no_neighbours() {
    let mut s = state("# Only", 60, 10);
    assert_eq!(s.file_position(), Some((0, 1)));
    colon(&mut s, "n");
    assert_eq!(s.message(), Some("there is only one file"));
}

#[test]
fn following_a_link_to_one_of_the_files_moves_the_position() {
    let mut s = three_files();
    opened(&mut s, "c.md", "# c", None);
    assert_eq!(s.file_position(), Some((2, 3)));
    colon(&mut s, "p");
    assert_eq!(s.key(), &DocKey::Path(file("b.md")));
}

#[test]
fn image_modes_lay_out_again_only_when_sizes_change() {
    let mut s = state(&long_doc(3, 2), 60, 10);
    keys(&mut s, "5j");
    let place = s.top_place();
    let effects = update(
        &mut s,
        Action::ImageMode {
            name: "blocks".into(),
            relayout: false,
        },
    );
    assert!(effects.is_empty());
    assert_eq!(s.message(), Some("images: blocks"));
    assert!(s.pending.is_none(), "nothing waits for a layout");
    let effects = update(
        &mut s,
        Action::ImageMode {
            name: "off (alt text)".into(),
            relayout: true,
        },
    );
    assert_eq!(effects, [Effect::Relayout]);
    assert_eq!(s.pending, Some(state::Goto::Place(place)));
}

#[test]
fn reloading_keeps_the_place_and_the_search() {
    let path = file("r.md");
    let mut s = state_for(pdoc(&long_doc(6, 3), Origin::File(path.clone())), 60, 10);
    s.top = heading_line(&s, "Section 4");
    keys(&mut s, "/Detail\n");
    let before = s.match_count();
    let top_before = top_text(&s);
    // A new first block: the view stays on the same text.
    let changed = long_doc(6, 3).replace("Intro paragraph.", "Intro paragraph, longer now.");
    let effects = drive(&mut s, Action::Reloaded(pdoc(&changed, Origin::File(path))));
    assert!(effects.is_empty());
    assert_eq!(s.message(), Some("reloaded"));
    assert_eq!(s.match_count(), before);
    assert_eq!(top_text(&s), top_before);
    // Standard input cannot be reloaded or watched.
    let mut stdin = state("text", 40, 10);
    assert!(keys(&mut stdin, "r").is_empty());
    assert_eq!(stdin.message(), Some("standard input cannot be reloaded"));
    keys(&mut stdin, "R");
    assert_eq!(stdin.message(), Some("standard input cannot be watched"));
}

#[test]
fn files_opened_from_standard_input_are_watched() {
    let mut s = state("# From stdin\n\n[x](x.md)", 40, 10);
    assert!(!s.watching(), "standard input is never watched");
    opened(&mut s, "x.md", "# X", None);
    assert!(s.watching(), "but the files it leads to are");
}

#[test]
fn a_reload_gets_fresh_link_ids() {
    let path = file("l.md");
    let mut s = state_for(pdoc("[a](#a)", Origin::File(path.clone())), 40, 10);
    opened(&mut s, "other.md", "[b](#b) [c](#c)", None);
    code(&mut s, KeyCode::Backspace);
    let before = s.page.link_base;
    let other = s
        .history
        .page(&DocKey::Path(file("other.md")))
        .unwrap()
        .link_base;
    let more = "[a](#a) [b](#b) [c](#c) [d](#d)";
    drive(&mut s, Action::Reloaded(pdoc(more, Origin::File(path))));
    let after = s.page.link_base;
    assert_ne!(after, before);
    assert!(
        after >= other + 2,
        "no overlap with the other document's ids"
    );
}

#[test]
fn reload_and_watch_keys() {
    let mut s = state_for(pdoc("# F", Origin::File(file("f.md"))), 40, 10);
    assert!(s.watching());
    assert_eq!(keys(&mut s, "r"), [Effect::Reload]);
    keys(&mut s, "R");
    assert!(!s.watching());
    keys(&mut s, "R");
    assert!(s.watching());
}

// ---------------------------------------------------------------------------
// Resizing
// ---------------------------------------------------------------------------

#[test]
fn a_new_width_re_anchors_on_the_same_text() {
    let mut s = state(&long_doc(10, 4), 80, 12);
    s.top = heading_line(&s, "Section 6");
    let effects = drive(&mut s, Action::Resize { cols: 30, rows: 12 });
    assert!(effects.is_empty());
    assert_eq!(s.size(), (30, 12));
    assert_eq!(s.layout().width, 30);
    assert_eq!(top_text(&s).trim(), "Section 6");
    drive(
        &mut s,
        Action::Resize {
            cols: 100,
            rows: 12,
        },
    );
    assert_eq!(top_text(&s).trim(), "Section 6");
}

#[test]
fn only_new_rows_need_no_layout() {
    let mut s = state(&long_doc(10, 4), 80, 12);
    let generation = s.generation;
    keys(&mut s, "G");
    let effects = update(&mut s, Action::Resize { cols: 80, rows: 30 });
    assert!(effects.is_empty(), "no relayout");
    assert_eq!(s.generation, generation);
    assert_eq!(
        s.top(),
        s.layout().len() - 29,
        "clamped to the taller screen"
    );
    assert!(update(&mut s, Action::Resize { cols: 80, rows: 30 }).is_empty());
}

// ---------------------------------------------------------------------------
// Toggles, help, quitting
// ---------------------------------------------------------------------------

#[test]
fn toggles_and_simple_effects() {
    let mut s = state(&long_doc(3, 2), 140, 10);
    let narrow = s.layout().measure;
    let effects = keys(&mut s, "w");
    assert!(effects.is_empty());
    assert!(s.wide());
    assert!(s.layout().measure > narrow, "max_width is off");
    assert_eq!(s.message(), Some("full width"));
    keys(&mut s, "w");
    assert_eq!(s.layout().measure, narrow);
    assert_eq!(keys(&mut s, "M"), [Effect::SetMouse(false)]);
    assert!(!s.mouse());
    assert_eq!(keys(&mut s, "M"), [Effect::SetMouse(true)]);
    assert_eq!(keys(&mut s, "i"), [Effect::CycleImages]);
    assert_eq!(keys(&mut s, "q"), [Effect::Quit]);
    assert_eq!(key(&mut s, Key::ctrl('c')), [Effect::Quit]);
    assert_eq!(key(&mut s, Key::ctrl('l')), [Effect::Redraw]);
    assert_eq!(key(&mut s, Key::ctrl('z')), [Effect::Suspend]);
    assert_eq!(
        drive(&mut s, Action::Command(Command::Quit)),
        [Effect::Quit]
    );
}

#[test]
fn help_overlay() {
    let mut s = state("text", 80, 12);
    keys(&mut s, "h");
    assert_eq!(s.mode_name(), "help");
    keys(&mut s, "jjj");
    assert!(matches!(s.mode, Mode::Help { scroll: 3 }));
    keys(&mut s, "kkkkk");
    assert!(matches!(s.mode, Mode::Help { scroll: 0 }));
    code(&mut s, KeyCode::PageDown);
    assert!(matches!(s.mode, Mode::Help { scroll } if scroll > 3));
    assert!(keys(&mut s, "x").is_empty(), "other keys do nothing");
    keys(&mut s, "q");
    assert_eq!(s.mode_name(), "normal");
    code(&mut s, KeyCode::F(1));
    assert_eq!(s.mode_name(), "help");
    code(&mut s, KeyCode::Esc);
    assert_eq!(s.mode_name(), "normal");
    // Scrolled to the end, then the screen grows: still in range.
    keys(&mut s, "h");
    for _ in 0..200 {
        keys(&mut s, "j");
    }
    drive(&mut s, Action::Resize { cols: 80, rows: 60 });
    keys(&mut s, "k");
    let Mode::Help { scroll } = s.mode else {
        panic!("help")
    };
    let lines = keymap::Keymap::default().help_lines().len();
    let shown = toc::help_box(80, 60, lines).inner_rows();
    assert_eq!(scroll, lines.saturating_sub(shown).saturating_sub(1));
}

#[test]
fn wheel_scrolls_by_the_configured_step() {
    let mut s = state(&long_doc(10, 4), 60, 12);
    let wheel = |kind| {
        Action::Mouse(Mouse {
            kind,
            col: 5,
            row: 5,
        })
    };
    drive(&mut s, wheel(MouseKind::WheelDown));
    assert_eq!(s.top(), 3);
    drive(&mut s, wheel(MouseKind::WheelDown));
    drive(&mut s, wheel(MouseKind::WheelUp));
    assert_eq!(s.top(), 3);
    drive(&mut s, wheel(MouseKind::WheelLeft));
    assert_eq!(s.top(), 3);
    keys(&mut s, "t");
    drive(&mut s, wheel(MouseKind::WheelDown));
    let Mode::Outline(o) = &s.mode else {
        panic!("outline")
    };
    assert_eq!(o.selected, 1, "the wheel moves the outline selection");
}

#[test]
fn messages_last_until_the_next_key() {
    let mut s = state("text", 40, 10);
    drive(&mut s, Action::Message("hello".into()));
    assert_eq!(s.message(), Some("hello"));
    keys(&mut s, "j");
    assert_eq!(s.message(), None);
    drive(&mut s, Action::Error("bad".into()));
    assert_eq!(s.message(), Some("bad"));
}

#[test]
fn initial_anchor() {
    let mut s = state(&long_doc(5, 3), 60, 10);
    drive(&mut s, Action::Anchor("section-3".into()));
    assert_eq!(top_text(&s).trim(), "Section 3");
    assert_eq!(s.back_len(), 0, "not a navigation");
}

#[test]
fn fits_on_screen_leaves_a_line_for_the_prompt() {
    let s = state("one\n\ntwo", 40, 10);
    let lines = s.layout().len();
    assert_eq!(lines, 3);
    assert!(fits_on_screen(s.layout(), 4));
    assert!(!fits_on_screen(s.layout(), 3));
    assert!(!fits_on_screen(s.layout(), 0));
}

#[test]
fn empty_documents_and_tiny_screens_are_fine() {
    let ctx = Ctx::new(&Theme::test(), &Caps::full());
    for (md, cols, rows) in [
        ("", 80, 24),
        ("", 1, 1),
        ("# T\n\ntext [l](#t)", 1, 1),
        ("x", 3, 2),
    ] {
        let mut s = state(md, cols, rows);
        for k in "jkGg /x\nnNt\x1bo\x1bh\x1b]\x1b[}{w".chars() {
            let k = match k {
                '\n' => Key::plain(KeyCode::Enter),
                '\x1b' => Key::plain(KeyCode::Esc),
                c => Key::char(c),
            };
            drive(&mut s, Action::Key(k));
            let frame = view(&s, &ctx);
            assert_eq!(frame.lines.len(), usize::from(rows));
        }
        code(&mut s, KeyCode::Tab);
        code(&mut s, KeyCode::Enter);
        drive(&mut s, Action::Resize { cols: 2, rows: 1 });
        let frame = view(&s, &ctx);
        let mut screen = Screen::new();
        let bytes = screen.paint(
            &frame,
            s.document(),
            s.layout(),
            &crate::render::RenderConfig::from_caps(&Caps::full()),
        );
        assert!(!bytes.is_empty());
    }
}

#[test]
fn pager_exit_codes() {
    assert_eq!(PagerExit::Quit.code(), 0);
    assert_eq!(PagerExit::Signal(15).code(), 143);
    assert_eq!(PagerExit::Signal(1).code(), 129);
}

#[test]
fn file_loader_reads_readmes() {
    let dir = std::env::temp_dir().join(format!("emde-pager-loader-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("README.md"), "# Read me").unwrap();
    let loader = FileLoader::default();
    let doc = loader.load(&dir).unwrap();
    assert_eq!(doc.doc.headings.len(), 1);
    assert!(matches!(&doc.source.origin, Origin::File(p) if p.ends_with("README.md")));
    assert!(loader.load(Path::new("/nonexistent/emde.md")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Random input
// ---------------------------------------------------------------------------

/// One random input for the property test.
fn input(n: u32, col: u16, row: u16) -> Action {
    const CHARS: &[char] = &[
        'j', 'k', 'd', 'u', 'f', 'b', 'g', 'G', '%', ']', '[', '}', '{', 't', 'n', 'N', '/', '?',
        'o', 'a', 's', 'h', 'H', 'L', 'y', 'w', 'q', 'x', 'e', '1', '5', ' ',
    ];
    const CODES: &[KeyCode] = &[
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Backspace,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
    ];
    let pick = n as usize;
    match n % 7 {
        0..=3 => Action::Key(Key::char(CHARS[pick / 7 % CHARS.len()])),
        4 => Action::Key(Key::plain(CODES[pick / 7 % CODES.len()])),
        5 => Action::Mouse(Mouse {
            kind: [
                MouseKind::WheelDown,
                MouseKind::WheelUp,
                MouseKind::Press(Button::Left),
            ][pick / 7 % 3],
            col,
            row,
        }),
        _ => Action::Resize {
            cols: col.max(1),
            rows: row.max(1),
        },
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(128))]

    #[test]
    fn random_input_keeps_the_invariants(
        doc in 0usize..3,
        cols in 1u16..140,
        rows in 1u16..50,
        steps in proptest::collection::vec((0u32..10_000, 0u16..150, 0u16..60), 1..80),
    ) {
        let md = match doc {
            0 => include_str!("../../tests/fixtures/md/kitchen-sink.md").to_owned(),
            1 => include_str!("../../tests/fixtures/md/links.md").to_owned(),
            _ => long_doc(6, 3),
        };
        let mut s = state(&md, cols, rows);
        let ctx = Ctx::new(&Theme::test(), &Caps::full());
        let cfg = crate::render::RenderConfig::from_caps(&Caps::full());
        let mut screen = Screen::new();
        for (n, col, row) in steps {
            drive(&mut s, input(n, col, row));
            proptest::prop_assert!(s.top() <= s.max_top(), "{} > {}", s.top(), s.max_top());
            let frame = view(&s, &ctx);
            proptest::prop_assert_eq!(frame.lines.len(), usize::from(s.size().1));
            let _ = screen.paint(&frame, s.document(), s.layout(), &cfg);
        }
    }
}

// Hints, Visual mode, marks and the editor.
mod modes;
