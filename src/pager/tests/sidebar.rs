//! `update()` with the outline sidebar: when it shows, `T` and the keys in
//! it, the scroll-spy, the mouse, resizes.

use super::*;
use crate::pager::sidebar;

/// A state on a `cols` × `rows` terminal laid out for the columns the
/// sidebar leaves (as the shell does).
fn wide(md: &str, cols: u16, rows: u16) -> State {
    let mut s = state(md, cols, rows);
    let l = lay(s.document(), s.doc_cols(), false);
    drive(&mut s, Action::Layout(l));
    s
}

fn click(s: &mut State, col: u16, row: u16) -> Vec<Effect> {
    drive(
        s,
        Action::Mouse(Mouse {
            kind: MouseKind::Press(Button::Left),
            col,
            row,
        }),
    )
}

fn wheel(s: &mut State, col: u16, down: bool) -> Vec<Effect> {
    let kind = if down {
        MouseKind::WheelDown
    } else {
        MouseKind::WheelUp
    };
    drive(s, Action::Mouse(Mouse { kind, col, row: 3 }))
}

/// The title of sidebar entry `i`.
fn entry(s: &State, i: usize) -> String {
    let (_, h) = s.derived.headings[i];
    s.document().heading(h).unwrap().title.to_string()
}

fn current(s: &State) -> Option<String> {
    sidebar::current(s).map(|i| entry(s, i))
}

#[test]
fn auto_shows_it_where_there_is_room() {
    let s = wide(&long_doc(3, 2), 160, 30);
    assert_eq!((s.sidebar_cols(), s.doc_cols(), s.left()), (32, 128, 32));
    assert_eq!(s.layout().width, 128, "laid out beside it");
    let s = wide(&long_doc(3, 2), 139, 30);
    assert_eq!((s.sidebar_cols(), s.doc_cols()), (0, 139));
    // No headings, no sidebar.
    let s = wide("Just a paragraph.\n", 200, 30);
    assert_eq!(s.sidebar_cols(), 0);
}

#[test]
fn t_shows_focuses_and_hides_it() {
    let mut s = wide(&long_doc(5, 2), 100, 30);
    assert_eq!(s.sidebar_cols(), 0);
    let before = top_text(&s);
    assert_eq!(keys(&mut s, "T"), [], "laid out again by the driver");
    assert_eq!(s.left(), 32);
    assert_eq!(s.layout().width, 68);
    assert_eq!(top_text(&s), before, "the same place");
    // A second T moves the keys into it, at the current section.
    let cur = sidebar::current(&s).unwrap();
    assert_eq!(entry(&s, cur), "Title", "the heading at the top line");
    keys(&mut s, "T");
    assert_eq!(s.mode, Mode::Sidebar(cur));
    keys(&mut s, "jjj");
    assert_eq!(s.mode, Mode::Sidebar(cur + 3));
    keys(&mut s, "k");
    assert_eq!(s.mode, Mode::Sidebar(cur + 2));
    keys(&mut s, "G");
    let last = s.derived.headings.len() - 1;
    assert_eq!(s.mode, Mode::Sidebar(last));
    keys(&mut s, "g");
    // Enter goes to the heading and gives the keys back.
    keys(&mut s, "jj\n");
    assert_eq!(s.mode, Mode::Normal);
    assert_eq!(top_text(&s).trim(), format!("▎ {}", entry(&s, 2)).trim());
    assert_eq!(s.back_len(), 1, "a jump in the history");
    // Esc gives the keys back too, and a third T hides it.
    keys(&mut s, "T");
    code(&mut s, KeyCode::Esc);
    assert_eq!(s.mode, Mode::Normal);
    keys(&mut s, "TT");
    assert_eq!(s.mode, Mode::Normal);
    assert_eq!(s.sidebar_cols(), 0);
    assert_eq!(s.layout().width, 100);
}

#[test]
fn t_explains_when_it_cannot_show() {
    let mut s = wide("Just a paragraph.\n", 200, 20);
    keys(&mut s, "T");
    assert_eq!(s.message(), Some("no headings"));
    let mut s = wide(&long_doc(2, 1), 60, 20);
    keys(&mut s, "T");
    assert_eq!(
        s.message(),
        Some("the outline sidebar needs 72 columns"),
        "32 + 40"
    );
    assert_eq!(s.sidebar_cols(), 0);
}

#[test]
fn the_current_section_follows_the_top_line() {
    let mut s = wide(&long_doc(30, 3), 150, 21);
    assert_eq!(current(&s).as_deref(), Some("Title"));
    let line = heading_line(&s, "Section 4");
    // Section 4 is on screen but not yet at the top: still in Detail 3.
    s.top = line - 1;
    drive(&mut s, Action::Message(String::new()));
    assert_eq!(current(&s).as_deref(), Some("Detail 3"));
    s.top = line;
    drive(&mut s, Action::Message(String::new()));
    assert_eq!(current(&s).as_deref(), Some("Section 4"));
    // At the bottom, the last sections count from the top third, and a
    // heading jumped to there (it cannot reach the top line) is current.
    keys(&mut s, "G");
    assert_eq!(current(&s).as_deref(), Some("Detail 29"));
    let last = s.derived.headings.len() - 1;
    let row = u16::try_from(last - s.sidebar.scroll).unwrap();
    click(&mut s, 5, row);
    assert_eq!(current(&s).as_deref(), Some("Detail 30"));
    // The sidebar keeps the current entry in view.
    let cur = sidebar::current(&s).unwrap();
    let rows = s.view_rows();
    assert!(
        (s.sidebar.scroll..s.sidebar.scroll + rows).contains(&cur),
        "{} {cur}",
        s.sidebar.scroll
    );
    keys(&mut s, "gg");
    assert_eq!(
        s.sidebar.scroll, 0,
        "with the entries around the current one"
    );
}

#[test]
fn hundreds_of_headings_scroll_within_bounds() {
    let mut md = String::new();
    for i in 1..=500 {
        md.push_str(&format!("# Heading {i}\n\nText {i}.\n\n"));
    }
    let mut s = wide(&md, 150, 12);
    let n = s.derived.headings.len();
    assert_eq!(n, 500);
    for _ in 0..40 {
        keys(&mut s, " ");
        let cur = sidebar::current(&s).unwrap();
        assert!((s.sidebar.scroll..s.sidebar.scroll + 11).contains(&cur));
    }
    keys(&mut s, "G");
    let cur = sidebar::current(&s).unwrap();
    assert!(cur > n - 11 && cur < n, "{cur}");
    assert!((s.sidebar.scroll..s.sidebar.scroll + 11).contains(&cur));
    assert!(s.sidebar.scroll <= n - 11);
    keys(&mut s, "T");
    keys(&mut s, "g");
    assert_eq!(s.sidebar.scroll, 0, "the selection is kept in view");
    keys(&mut s, "G");
    assert_eq!(s.sidebar.scroll, n - 11);
}

#[test]
fn clicks_in_the_sidebar_jump_and_in_the_text_are_offset() {
    let md = "# Top\n\nSee [the example](https://example.com).\n\n## Second\n\nMore.\n";
    let mut s = wide(md, 150, 20);
    assert_eq!(s.left(), 32);
    // Row 1 of the sidebar is the second heading.
    click(&mut s, 5, 1);
    assert_eq!(s.message(), None);
    assert_eq!(s.back_len(), 1, "a jump");
    keys(&mut s, "gg");
    // The link, where it is on screen: after the sidebar and the indent.
    let hit = s.layout().link_hits[0].clone();
    let col = 32 + s.layout().indent + hit.cols.start;
    let row = u16::try_from(hit.line).unwrap();
    assert_eq!(
        click(&mut s, col, row),
        [Effect::Open("https://example.com".into())]
    );
    // Without the sidebar's offset the click misses.
    assert_eq!(click(&mut s, col - 32, row), []);
    // Clicking the text gives the keys back from the sidebar.
    keys(&mut s, "T");
    assert!(matches!(s.mode, Mode::Sidebar(_)));
    click(&mut s, 100, 10);
    assert_eq!(s.mode, Mode::Normal);
    // Rows past the entries do nothing.
    let top = s.top();
    click(&mut s, 3, 15);
    assert_eq!(s.top(), top);
}

#[test]
fn the_wheel_over_the_sidebar_scrolls_it() {
    let mut s = wide(&long_doc(30, 1), 150, 12);
    let top = s.top();
    wheel(&mut s, 3, true);
    assert_eq!(s.top(), top, "the text stays");
    assert_eq!(s.sidebar.scroll, 3);
    wheel(&mut s, 3, false);
    wheel(&mut s, 3, false);
    assert_eq!(s.sidebar.scroll, 0);
    wheel(&mut s, 3, true);
    wheel(&mut s, 3, true);
    assert_eq!(s.sidebar.scroll, 6, "left where the reader put it");
    // Once the current section changes, it follows again.
    keys(&mut s, "]]");
    let cur = sidebar::current(&s).unwrap();
    assert!((s.sidebar.scroll..s.sidebar.scroll + 11).contains(&cur));
    wheel(&mut s, 100, true);
    assert!(s.top() > 0, "the wheel over the text scrolls it");
}

#[test]
fn resizes_re_evaluate_auto() {
    let mut s = wide(&long_doc(8, 2), 150, 20);
    keys(&mut s, "]]]]");
    let place = top_text(&s);
    keys(&mut s, "T");
    assert!(matches!(s.mode, Mode::Sidebar(_)));
    drive(
        &mut s,
        Action::Resize {
            cols: 100,
            rows: 20,
        },
    );
    assert_eq!(s.sidebar_cols(), 0);
    assert_eq!(s.layout().width, 100);
    assert_eq!(s.mode, Mode::Normal, "the keys come back");
    assert_eq!(top_text(&s), place, "the same place");
    drive(
        &mut s,
        Action::Resize {
            cols: 160,
            rows: 20,
        },
    );
    assert_eq!((s.left(), s.layout().width), (32, 128));
    assert_eq!(top_text(&s), place);
    // A tiny terminal: no sidebar, nothing breaks.
    for (cols, rows) in [(1, 1), (3, 2), (72, 1), (200, 2)] {
        drive(&mut s, Action::Resize { cols, rows });
        keys(&mut s, "TjT\x1b");
        let _ = view(&s, &Ctx::new(&Theme::test(), &Caps::full()));
    }
}

#[test]
fn hiding_it_is_remembered_across_documents_and_resizes() {
    let mut s = wide(&long_doc(3, 1), 150, 20);
    keys(&mut s, "TT");
    assert_eq!(s.sidebar_cols(), 0);
    drive(
        &mut s,
        Action::Resize {
            cols: 180,
            rows: 20,
        },
    );
    assert_eq!(s.sidebar_cols(), 0, "T overrides auto");
}

#[test]
fn a_reload_with_fewer_headings_keeps_the_selection_in_range() {
    let mut s = state_for(pdoc(&long_doc(8, 1), Origin::File(file("r.md"))), 150, 20);
    let l = lay(s.document(), s.doc_cols(), false);
    drive(&mut s, Action::Layout(l));
    keys(&mut s, "T");
    keys(&mut s, "G");
    assert!(matches!(s.mode, Mode::Sidebar(i) if i > 10));
    drive(
        &mut s,
        Action::Reloaded(pdoc(
            "# One\n\n## Two\n\ntext\n",
            Origin::File(file("r.md")),
        )),
    );
    assert_eq!(s.mode, Mode::Sidebar(1));
    assert_eq!(s.sidebar.scroll, 0);
    keys(&mut s, "\n");
    assert_eq!(s.mode, Mode::Normal);
    assert_eq!(s.back_len(), 1, "it went to Two (all on screen)");
}

#[test]
fn the_sidebar_rows_fill_its_width_exactly() {
    let mut s = wide(&long_doc(3, 1), 150, 12);
    let ctx = Ctx::new(&Theme::test(), &Caps::full());
    let side = |s: &State| -> Vec<String> {
        view(s, &ctx)
            .lines
            .iter()
            .take(s.view_rows())
            .map(|r| plain(&String::from_utf8_lossy(&r.side)))
            .collect()
    };
    for row in side(&s) {
        assert_eq!(crate::text::str_width(&row, false), 32, "{row:?}");
        assert!(row.ends_with('│'), "{row:?}");
    }
    // Where ambiguous characters are wide, the border and the marker are
    // ASCII (box glyphs would take two columns).
    s.settings.ambiguous_wide = true;
    let rows = side(&s);
    assert!(rows.iter().all(|r| r.ends_with('|')), "{rows:?}");
    assert!(rows.iter().any(|r| r.contains("> ")), "{rows:?}");
}

/// `s` without its SGR sequences.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Reported by the user: clicking a short section's entry marked the entry
/// below it, because the next heading also sat in the top third.
#[test]
fn a_jump_to_a_short_section_marks_that_section() {
    let mut md = String::from("# Long\n\n");
    for i in 0..40 {
        md.push_str(&format!("Filler paragraph {i}.\n\n"));
    }
    md.push_str("## Short\n\nOne line.\n\n## Next\n\n");
    for i in 0..40 {
        md.push_str(&format!("More text {i}.\n\n"));
    }
    let mut s = wide(&md, 150, 30);
    let short = (0..s.derived.headings.len())
        .find(|&i| entry(&s, i) == "Short")
        .unwrap();
    let row = u16::try_from(short - s.sidebar.scroll).unwrap();
    click(&mut s, 5, row);
    assert_eq!(
        s.top(),
        heading_line(&s, "Short"),
        "the jump puts it on top"
    );
    assert_eq!(current(&s).as_deref(), Some("Short"));
    // Scrolling a line keeps it marked until the next heading reaches the
    // top.
    keys(&mut s, "j");
    assert_eq!(current(&s).as_deref(), Some("Short"));
    s.top = heading_line(&s, "Next");
    drive(&mut s, Action::Message(String::new()));
    assert_eq!(current(&s).as_deref(), Some("Next"));
}
