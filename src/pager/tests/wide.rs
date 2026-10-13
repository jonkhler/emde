//! `update()` with wide blocks shifted sideways and with collapsible
//! `<details>`.

use super::*;
use crate::ir::HeadingId;
use crate::pager::hints::Act;
use crate::pager::update::toggle;

/// A table of `n` columns (cell 10 a link, cell 11 the word `needle`)
/// after `before` paragraphs.
fn table_doc(n: usize, before: usize) -> String {
    let head: Vec<String> = (1..=n).map(|i| format!("Head {i}")).collect();
    let row: Vec<String> = (1..=n)
        .map(|i| match i {
            10 => "[link](https://example.com/6)".to_owned(),
            11 => "needle".to_owned(),
            _ => format!("cell {i}"),
        })
        .collect();
    let mut md = "Text before.\n\n".repeat(before);
    md.push_str(&format!(
        "| {} |\n|{}\n| {} |\n\nText after.\n",
        head.join(" | "),
        "---|".repeat(n),
        row.join(" | ")
    ));
    md
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

fn mouse(s: &mut State, kind: MouseKind) -> Vec<Effect> {
    drive(
        s,
        Action::Mouse(Mouse {
            kind,
            col: 5,
            row: 2,
        }),
    )
}

/// The most the wide block of the document can be shifted.
fn max_shift(s: &State) -> u16 {
    let l = s.layout();
    l.wide[0].max_shift(l.measure)
}

// ---------------------------------------------------------------------------
// Wide blocks
// ---------------------------------------------------------------------------

#[test]
fn keys_shift_wide_blocks() {
    let mut s = state(&table_doc(12, 0), 60, 12);
    assert_eq!(s.layout().wide.len(), 1, "{}", s.layout().dump());
    let max = max_shift(&s);
    assert!(max > 40, "{max}");
    keys(&mut s, "zl");
    assert_eq!(s.hscroll(), 8);
    keys(&mut s, "2zl");
    assert_eq!(s.hscroll(), 24);
    keys(&mut s, "zh");
    assert_eq!(s.hscroll(), 16);
    code(&mut s, KeyCode::Right);
    assert_eq!(s.hscroll(), 24);
    code(&mut s, KeyCode::Left);
    assert_eq!(s.hscroll(), 16);
    let half = s.layout().measure / 2;
    keys(&mut s, "zL");
    assert_eq!(s.hscroll(), 16 + half);
    keys(&mut s, "zH");
    assert_eq!(s.hscroll(), 16);
    keys(&mut s, "$");
    assert_eq!(s.hscroll(), max);
    keys(&mut s, "zl");
    assert_eq!(s.hscroll(), max, "not past the end");
    keys(&mut s, "^");
    assert_eq!(s.hscroll(), 0);
    keys(&mut s, "zh");
    assert_eq!(s.hscroll(), 0, "not before the start");
    // Normal text is never shifted.
    let f = view(&s, &Ctx::new(&Theme::test(), &Caps::full()));
    keys(&mut s, "$");
    let g = view(&s, &Ctx::new(&Theme::test(), &Caps::full()));
    let shift_of = |f: &Frame, row: usize| match &f.lines[row].body {
        Body::Line { shift, .. } => *shift,
        _ => 0,
    };
    assert_eq!(shift_of(&f, 0), 0);
    assert_eq!(shift_of(&g, 0), max, "the table starts at line 0");
    assert_eq!(shift_of(&g, 2), max);
    let after = (0..s.layout().len())
        .find(|&i| s.layout().line_text(i).contains("Text after"))
        .unwrap();
    assert_eq!(shift_of(&g, after), 0);
    assert_eq!(f.lines[after].hash, g.lines[after].hash, "not repainted");
    assert_ne!(f.lines[2].hash, g.lines[2].hash, "repainted");
}

#[test]
fn shifting_needs_a_wide_block_on_screen() {
    let mut s = state(
        &format!("{}{}", table_doc(12, 0), "More.\n\n".repeat(40)),
        60,
        10,
    );
    keys(&mut s, "zl");
    assert_eq!(s.hscroll(), 8);
    keys(&mut s, "G");
    keys(&mut s, "zl");
    assert_eq!(s.hscroll(), 8, "the table is off screen");
    assert_eq!(s.message(), Some("no wide code or table on screen"));
    let mut plain = state("Just text.", 60, 10);
    keys(&mut plain, "$");
    assert_eq!(plain.hscroll(), 0);
}

#[test]
fn the_wheel_shifts_sideways() {
    let mut s = state(&table_doc(12, 0), 60, 12);
    mouse(&mut s, MouseKind::WheelRight);
    mouse(&mut s, MouseKind::WheelRight);
    assert_eq!(s.hscroll(), 16);
    mouse(&mut s, MouseKind::WheelLeft);
    assert_eq!(s.hscroll(), 8);
    assert_eq!(s.top(), 0, "no vertical scroll");
}

#[test]
fn the_shift_is_kept_per_document_through_resizes_and_reloads() {
    let path = file("wide.md");
    let mut s = state_for(pdoc(&table_doc(12, 0), Origin::File(path.clone())), 60, 12);
    keys(&mut s, "3zl");
    assert_eq!(s.hscroll(), 24);
    drive(&mut s, Action::Resize { cols: 70, rows: 12 });
    assert_eq!(s.hscroll(), 24, "a resize keeps it");
    drive(&mut s, Action::Resize { cols: 60, rows: 12 });
    drive(
        &mut s,
        Action::Reloaded(pdoc(&table_doc(13, 0), Origin::File(path))),
    );
    assert_eq!(s.hscroll(), 24, "a reload keeps it");
    // Another document has its own.
    opened(&mut s, "other.md", &table_doc(12, 0), None);
    assert_eq!(s.hscroll(), 0);
    keys(&mut s, "zl");
    code(&mut s, KeyCode::Backspace);
    assert_eq!(s.hscroll(), 24, "back: the first document's shift");
    // A narrower terminal shows the same columns, as far as the table
    // goes; a wider one with room for it all shows it whole.
    drive(
        &mut s,
        Action::Resize {
            cols: 300,
            rows: 12,
        },
    );
    assert!(s.layout().wide.is_empty() || s.layout().shift_at(2, s.hscroll()) <= max_shift(&s));
}

#[test]
fn clicks_on_links_in_shifted_tables() {
    let mut s = state(&table_doc(12, 0), 60, 12);
    let hit = s.layout().link_hits[0].clone();
    let row = u16::try_from(hit.line).unwrap();
    let indent = s.layout().indent;
    assert!(hit.cols.start > s.layout().measure, "beyond the right edge");
    // Not shifted: where the link would be is no link.
    let effects = click(&mut s, indent + hit.cols.start - 40, row);
    assert!(effects.is_empty(), "{effects:?}");
    keys(&mut s, "4zl");
    let shift = s.hscroll();
    let effects = click(&mut s, indent + hit.cols.start - shift, row);
    assert_eq!(effects, [Effect::Open("https://example.com/6".into())]);
}

#[test]
fn hints_on_shifted_tables_go_where_their_text_is() {
    let mut s = state(&table_doc(12, 0), 60, 12);
    let hit = s.layout().link_hits[0].clone();
    let link_hint = |s: &mut State| {
        keys(s, "f");
        let found = match &s.mode {
            Mode::Hints(h) => h
                .labels
                .iter()
                .find(|(_, t)| matches!(t.act, Act::Link(_)))
                .map(|(_, t)| t.col),
            _ => None,
        };
        code(s, KeyCode::Esc);
        found
    };
    assert_eq!(link_hint(&mut s), None, "the link is out of view");
    keys(&mut s, "4zl");
    let shift = s.hscroll();
    assert_eq!(link_hint(&mut s), Some(hit.cols.start - shift));
    // The table's own chip stays at its left edge.
    keys(&mut s, "f");
    let Mode::Hints(h) = &s.mode else {
        panic!("{}", s.mode_name());
    };
    assert!(h.labels.iter().any(|(_, t)| t.line == 0 && t.col == 0));
}

#[test]
fn a_search_shifts_the_match_into_view() {
    let mut s = state(&table_doc(14, 0), 60, 12);
    keys(&mut s, "/cell 13\n");
    let (line, cols) = {
        let l = s.layout();
        let line = (0..l.len())
            .find(|&i| l.line_text(i).contains("cell 13"))
            .unwrap();
        let text = l.line_text(line);
        let byte = text.find("cell 13").unwrap();
        let col = crate::text::str_width(&text[..byte], false);
        (line, u16::try_from(col).unwrap())
    };
    let shift = s.hscroll();
    assert!(shift > 0);
    let shown = s
        .layout()
        .shown_col(line, cols, s.layout().shift_at(line, shift));
    assert!(
        shown.is_some_and(|c| c + 7 < s.layout().measure),
        "{shown:?} at {shift}"
    );
    // Back to a match at the start: shifted back.
    keys(&mut s, "/cell 1 \n");
    keys(&mut s, "/Head 1\n");
    assert_eq!(s.hscroll(), 0);
}

#[test]
fn tab_shifts_a_focused_link_into_view() {
    let mut s = state(&table_doc(12, 0), 60, 12);
    code(&mut s, KeyCode::Tab);
    assert!(s.focused_link().is_some());
    let hit = s.layout().link_hits[0].clone();
    let line = hit.line as usize;
    let shift = s.layout().shift_at(line, s.hscroll());
    assert!(s.layout().shown_col(line, hit.cols.start, shift).is_some());
}

#[test]
fn remapped_keys_shift_too() {
    // `Left` given to `back`: `zh` still shifts.
    let pager = PagerOptions {
        keys: vec![("back".into(), vec!["Left".into()])],
        ..PagerOptions::default()
    };
    let doc = pdoc(&table_doc(12, 0), Origin::Memory);
    let settings = Settings::new(&pager, &RenderOptions::default());
    let l = lay(&doc.doc, 60, false);
    let mut s = State::new(doc, None, l, (60, 12), &pager, settings);
    keys(&mut s, "zlzl");
    code(&mut s, KeyCode::Left);
    assert_eq!(s.hscroll(), 16, "Left goes back now");
    assert_eq!(s.message(), Some("nothing to go back to"));
    keys(&mut s, "zh");
    assert_eq!(s.hscroll(), 8);
    // And a new key for an action.
    let pager = PagerOptions {
        keys: vec![("scroll-end".into(), vec!["E".into()])],
        ..PagerOptions::default()
    };
    let doc = pdoc(&table_doc(12, 0), Origin::Memory);
    let settings = Settings::new(&pager, &RenderOptions::default());
    let l = lay(&doc.doc, 60, false);
    let mut s = State::new(doc, None, l, (60, 12), &pager, settings);
    keys(&mut s, "$");
    assert_eq!(s.hscroll(), 0, "$ is not bound now");
    keys(&mut s, "E");
    assert_eq!(s.hscroll(), max_shift(&s));
}

// ---------------------------------------------------------------------------
// Collapsible details
// ---------------------------------------------------------------------------

/// Paragraphs, a collapsed block with a heading and a match inside, more
/// paragraphs, a second collapsed block with a match, and filler.
fn details_doc() -> String {
    let filler = "Filler paragraph.\n\n".repeat(12);
    format!(
        "# Top\n\n{filler}<details>\n<summary>First</summary>\n\n## Hidden heading\n\n\
         The first needle.\n\nMore inside.\n\n</details>\n\n{filler}<details>\n\
         <summary>Second</summary>\n\nThe second needle.\n\n</details>\n\n{filler}"
    )
}

/// Open or close details `id` as the shell would (laying out again).
fn toggle_and_lay(s: &mut State, id: u32) {
    assert_eq!(toggle(s, id), [Effect::Relayout]);
    let l = lay_with(s.document(), s.doc_cols(), s.wide(), s.interactive());
    drive(s, Action::Layout(l));
}

fn summary_line(s: &State, id: usize) -> usize {
    s.layout().details[id].line as usize
}

#[test]
fn details_open_and_close_keeping_the_place() {
    let mut s = state(&details_doc(), 60, 12);
    let first = summary_line(&s, 0);
    assert!(s.layout().line_text(first).contains("▸ First"));
    // Scroll past the first block, then open it: the text at the top stays.
    keys(&mut s, &format!("{}j", first + 3));
    let before = top_text(&s);
    toggle_and_lay(&mut s, 0);
    assert!(s.layout().details[0].open);
    assert_eq!(top_text(&s), before, "the top stays");
    // `za` at the top (the second summary is near it now?) closes the
    // block the top line is in.
    keys(&mut s, "gg");
    keys(&mut s, &format!("{}j", first + 2));
    assert!(s.layout().details[0].holds(s.layout().lines[s.top()].pos));
    keys(&mut s, "za");
    assert!(!s.layout().details[0].open, "closed");
    assert_eq!(
        s.top(),
        s.max_top().min(summary_line(&s, 0)),
        "on its summary"
    );
}

#[test]
fn za_toggles_the_summary_near_the_top() {
    let mut s = state(&details_doc(), 60, 12);
    let first = summary_line(&s, 0);
    keys(&mut s, &format!("{}j", first - 1));
    keys(&mut s, "za");
    assert!(s.layout().details[0].open);
    keys(&mut s, "za");
    assert!(!s.layout().details[0].open);
    keys(&mut s, "gg");
    keys(&mut s, "za");
    assert_eq!(s.message(), Some("no details block at the top"));
}

#[test]
fn tab_enter_hints_and_clicks_toggle() {
    let mut s = state(&details_doc(), 60, 40);
    // Tab stops at the summaries (no links here).
    code(&mut s, KeyCode::Tab);
    assert_eq!(s.summary_focus, Some(0));
    code(&mut s, KeyCode::Enter);
    assert!(s.layout().details[0].open);
    assert_eq!(s.summary_focus, Some(0), "still focused");
    code(&mut s, KeyCode::Tab);
    assert_eq!(s.summary_focus, Some(1));
    code(&mut s, KeyCode::BackTab);
    assert_eq!(s.summary_focus, Some(0));
    code(&mut s, KeyCode::Esc);
    assert_eq!(s.summary_focus, None);
    // `f` labels summaries.
    keys(&mut s, "f");
    let Mode::Hints(h) = &s.mode else {
        panic!("{}", s.mode_name());
    };
    let label = h
        .labels
        .iter()
        .find(|(_, t)| t.act == Act::Toggle(0))
        .map(|(l, _)| l.clone())
        .unwrap();
    keys(&mut s, &label);
    assert!(!s.layout().details[0].open);
    // A click on a summary line.
    let row = u16::try_from(summary_line(&s, 0) - s.top()).unwrap();
    click(&mut s, 10, row);
    assert!(s.layout().details[0].open);
    click(&mut s, 10, row + 1);
    assert!(s.layout().details[0].open, "not on the summary");
}

#[test]
fn search_opens_the_block_of_the_match() {
    let mut s = state(&details_doc(), 60, 12);
    keys(&mut s, "/needle\n");
    assert!(s.layout().details[0].open, "the first match's block");
    assert!(!s.layout().details[1].open);
    let shown = (s.top()..s.top() + 11).any(|i| s.layout().line_text(i).contains("first needle"));
    assert!(shown, "{}", top_text(&s));
    assert_eq!(s.current_match(), Some(0));
    keys(&mut s, "n");
    assert!(s.layout().details[1].open, "n opens the next one");
    assert_eq!(s.current_match(), Some(1));
    let shown = (s.top()..s.top() + 11).any(|i| s.layout().line_text(i).contains("second needle"));
    assert!(shown);
    keys(&mut s, "N");
    assert_eq!(s.current_match(), Some(0));
}

#[test]
fn hidden_headings_are_dimmed_entries_that_open_their_block() {
    let mut s = state(&details_doc(), 60, 12);
    let d = &s.derived;
    assert_eq!(d.headings.len(), 2, "{:?}", d.headings);
    assert!(!d.is_folded(0) && d.is_folded(1));
    assert_eq!(d.headings[1].0 as usize, summary_line(&s, 0));
    // `]` skips it; the section of a line below is still `Top`.
    keys(&mut s, "]");
    assert_eq!(s.message(), Some("no heading below"));
    assert_eq!(
        s.derived.section_at(summary_line(&s, 0) + 2),
        Some(HeadingId(0))
    );
    // The outline goes to it, opening its block.
    keys(&mut s, "t");
    code(&mut s, KeyCode::Down);
    code(&mut s, KeyCode::Enter);
    assert!(s.layout().details[0].open);
    assert_eq!(top_text(&s).trim(), "Hidden heading");
    assert!(!s.derived.is_folded(1));
    code(&mut s, KeyCode::Backspace);
    assert_eq!(s.top(), 0, "back where it was");
}

#[test]
fn toggling_with_the_sidebar_open() {
    let mut s = state(&details_doc(), 150, 12);
    let l = lay_with(s.document(), s.doc_cols(), false, s.interactive());
    drive(&mut s, Action::Layout(l));
    assert!(s.sidebar_cols() > 0);
    keys(&mut s, "T");
    assert_eq!(s.mode_name(), "sidebar");
    keys(&mut s, "j\n");
    assert!(
        s.layout().details[0].open,
        "the sidebar goes to the hidden heading"
    );
    assert_eq!(top_text(&s).trim(), "Hidden heading");
    assert_eq!(
        s.layout().width,
        s.doc_cols(),
        "laid out beside the sidebar"
    );
    // Clicks in the text are offset by the sidebar.
    keys(&mut s, "k");
    let row = u16::try_from(summary_line(&s, 0) - s.top()).unwrap();
    let left = s.sidebar_cols();
    click(&mut s, left + 10, row);
    assert!(!s.layout().details[0].open);
}

#[test]
fn a_reload_keeps_open_blocks_open() {
    let path = file("details.md");
    let mut s = state_for(pdoc(&details_doc(), Origin::File(path.clone())), 60, 12);
    toggle_and_lay(&mut s, 1);
    assert!(s.layout().details[1].open);
    let md = format!("{}\n\nAppended.\n", details_doc());
    drive(&mut s, Action::Reloaded(pdoc(&md, Origin::File(path))));
    assert!(s.layout().details[1].open);
    assert!(!s.layout().details[0].open);
}

#[test]
fn visual_yank_of_a_collapsed_block_copies_its_source() {
    let mut s = state(&details_doc(), 60, 12);
    let first = summary_line(&s, 0);
    keys(&mut s, &format!("{first}j"));
    keys(&mut s, "V");
    let effects = keys(&mut s, "y");
    let [Effect::Copy { text, .. }] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert!(text.starts_with("<details>"), "{text}");
    assert!(text.contains("The first needle."), "{text}");
    assert!(text.trim_end().ends_with("</details>"), "{text}");
}

#[test]
fn details_expanded_by_the_settings() {
    let pager = PagerOptions {
        details: crate::options::DetailsMode::Expanded,
        ..PagerOptions::default()
    };
    let doc = pdoc(&details_doc(), Origin::Memory);
    let settings = Settings::new(&pager, &RenderOptions::default());
    let l = lay_with(&doc.doc, 60, false, settings.interactive());
    let mut s = State::new(doc, None, l, (60, 12), &pager, settings);
    assert!(s.layout().details.iter().all(|d| d.open));
    toggle_and_lay(&mut s, 0);
    assert!(!s.layout().details[0].open);
}

// ---------------------------------------------------------------------------
// Second pass: corners
// ---------------------------------------------------------------------------

/// Filler, then a collapsed block holding a collapsed block with a heading
/// and a match, then a link to the heading, then filler.
fn nested_doc() -> String {
    let filler = "Filler paragraph.\n\n".repeat(10);
    format!(
        "# Top\n\n[to the deep heading](#deep-heading)\n\n{filler}<details>\n<summary>Outer</summary>\n\n\
         Outer text.\n\n<details>\n<summary>Inner</summary>\n\n## Deep heading\n\nThe hidden needle.\n\n\
         </details>\n\n</details>\n\n{filler}"
    )
}

#[test]
fn search_opens_nested_blocks() {
    let mut s = state(&nested_doc(), 60, 12);
    assert!(s.layout().details.iter().all(|d| !d.open));
    keys(&mut s, "/hidden needle\n");
    assert!(s.layout().details.iter().all(|d| d.open), "both opened");
    let shown = (s.top()..s.top() + 11).any(|i| s.layout().line_text(i).contains("hidden needle"));
    assert!(shown);
}

#[test]
fn links_and_the_outline_open_nested_blocks() {
    let mut s = state(&nested_doc(), 60, 12);
    // A link to the hidden heading.
    code(&mut s, KeyCode::Tab);
    code(&mut s, KeyCode::Enter);
    assert!(s.layout().details.iter().all(|d| d.open));
    assert_eq!(top_text(&s).trim(), "Deep heading");
    // Closed again, the outline goes there too.
    let mut s = state(&nested_doc(), 60, 12);
    keys(&mut s, "t");
    code(&mut s, KeyCode::Down);
    code(&mut s, KeyCode::Enter);
    assert!(s.layout().details.iter().all(|d| d.open));
    assert_eq!(top_text(&s).trim(), "Deep heading");
}

#[test]
fn anchors_of_other_documents_open_their_blocks() {
    let mut s = state(&long_doc(2, 2), 60, 12);
    opened(&mut s, "nested.md", &nested_doc(), Some("deep-heading"));
    assert!(
        s.layout().details.iter().all(|d| d.open),
        "{:?}",
        s.message()
    );
    assert_eq!(top_text(&s).trim(), "Deep heading");
    assert_eq!(s.message(), None);
}

#[test]
fn clicks_on_a_link_cut_by_the_left_edge() {
    let mut s = state(&table_doc(20, 0), 60, 12);
    let hit = s.layout().link_hits[0].clone();
    let line = hit.line as usize;
    let x0 = s.layout().wide_at(line).unwrap().x0;
    // Shift so the link starts two columns left of the edge: its first
    // columns are hidden (one under the `‹`), the rest shown.
    let shift = hit.cols.start - x0 + 2;
    assert!(shift <= max_shift(&s));
    s.doc_view_mut().hscroll = shift;
    assert_eq!(s.layout().shown_col(line, hit.cols.start, shift), None);
    let shown = s
        .layout()
        .shown_col(line, hit.cols.start + 3, shift)
        .unwrap();
    let indent = s.layout().indent;
    let row = u16::try_from(line).unwrap();
    // The `‹` covers a column of the link: a click on it does nothing.
    assert!(click(&mut s, indent + x0, row).is_empty());
    let effects = click(&mut s, indent + shown, row);
    assert_eq!(effects, [Effect::Open("https://example.com/6".into())]);
}

#[test]
fn a_narrower_terminal_clamps_the_shift_it_shows() {
    let mut s = state(&table_doc(12, 0), 100, 12);
    keys(&mut s, "$");
    let wide_max = s.hscroll();
    drive(&mut s, Action::Resize { cols: 60, rows: 12 });
    let max = max_shift(&s);
    assert!(max > wide_max, "more to shift in fewer columns");
    let f = view(&s, &Ctx::new(&Theme::test(), &Caps::full()));
    let Body::Line { shift, .. } = f.lines[2].body else {
        panic!();
    };
    assert_eq!(shift, wide_max, "the same columns on the left");
    drive(
        &mut s,
        Action::Resize {
            cols: 200,
            rows: 12,
        },
    );
    assert!(s.layout().wide.is_empty(), "it fits now");
    let f = view(&s, &Ctx::new(&Theme::test(), &Caps::full()));
    assert!(matches!(f.lines[2].body, Body::Line { shift: 0, .. }));
    drive(&mut s, Action::Resize { cols: 60, rows: 12 });
    assert_eq!(s.hscroll(), wide_max, "kept for when it is wide again");
}

#[test]
fn a_link_cut_by_the_left_edge_gets_its_hint_where_it_shows() {
    let mut s = state(&table_doc(20, 0), 60, 12);
    let hit = s.layout().link_hits[0].clone();
    let line = hit.line as usize;
    let x0 = s.layout().wide_at(line).unwrap().x0;
    s.doc_view_mut().hscroll = hit.cols.start - x0 + 2;
    keys(&mut s, "o");
    let Mode::Hints(h) = &s.mode else {
        panic!("{:?}", s.message());
    };
    let [(_, t)] = h.labels.as_slice() else {
        panic!("{:?}", h.labels);
    };
    // Right after the `‹`.
    assert_eq!((t.line, t.col), (line, x0 + 1));
}

#[test]
fn the_selection_fills_what_shifted_lines_leave() {
    // Gutter code: short lines in a wide block end before the edge.
    let long = "x".repeat(90);
    let md = format!("```\n{long}\nab\n```\n");
    let mut opts = RenderOptions::default();
    opts.code.wrap = false;
    opts.code.style = crate::options::CodeStyle::Gutter;
    let doc = pdoc(&md, Origin::Memory);
    let pager = PagerOptions::default();
    let settings = Settings::new(&pager, &opts);
    let l = crate::layout::layout_with(
        &doc.doc,
        60,
        &Theme::test(),
        &Caps::full(),
        &opts,
        &PlainHighlighter,
        &NoImages,
        settings.interactive(),
    );
    let mut s = State::new(doc, None, l, (60, 12), &pager, settings);
    let short = 1;
    assert_eq!(s.layout().line_text(short).trim(), "▎ ab");
    keys(&mut s, "zl");
    keys(&mut s, "Vj");
    let f = view(&s, &Ctx::new(&Theme::test(), &Caps::full()));
    let x0 = s.layout().wide_at(short).unwrap().x0;
    let indent = s.layout().indent;
    // Shifted past its end: the fill starts right after the `‹`.
    let fill = &f.lines[short].overlays;
    assert_eq!(fill.len(), 1, "{fill:?}");
    assert_eq!(fill[0].col, indent + x0 + 1);
    // The long line fills the text column itself.
    assert!(f.lines[0].overlays.is_empty());
}

/// A footnote back-link whose reference is inside a collapsed block opens
/// it and goes to the reference (it said there was no reference).
#[test]
fn footnote_back_links_open_the_block_of_their_reference() {
    let filler = "Filler paragraph.\n\n".repeat(20);
    let md = format!(
        "# Top\n\n<details>\n<summary>More</summary>\n\nA hidden note[^n].\n\n</details>\n\n\
         {filler}[^n]: The note.\n"
    );
    let mut s = state(&md, 60, 12);
    assert!(!s.layout().details[0].open);
    keys(&mut s, "G");
    code(&mut s, KeyCode::Tab);
    code(&mut s, KeyCode::Enter);
    assert_eq!(s.message(), None);
    assert!(s.layout().details[0].open);
    assert!(top_text(&s).contains("A hidden note"), "{:?}", top_text(&s));
    // Back where the note is.
    code(&mut s, KeyCode::Backspace);
    assert_eq!(s.top(), s.max_top());
}
