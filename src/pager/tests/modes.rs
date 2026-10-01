//! `update()` in the modes: hints (`f`, `o`, `y`, `v`), Visual mode, marks,
//! `zz`/`zt`/`zb`, zoomed figures and the editor.

use super::*;
use crate::pager::hints::Act;
use crate::pager::state::HintKind;

/// Every kind of block, with display math the parser rewrites first.
const MIXED: &str = concat!(
    "# Guide\n\n",
    "Intro with [a link](https://example.com) and $x^2$ inline.\n\n",
    "```rust\nfn main() {}\nlet x = 1;\n```\n\n",
    "$$\nE = mc^2\n$$\n\n",
    "| a | b |\n|---|---|\n| 1 | 2 |\n\n",
    "- first item\n- second item\n\n  continued\n\n",
    "> A quote\n> over lines.\n\n",
    "![alt text](pic.png)\n\n",
    "## Next\n\n",
    "Last paragraph.\n",
);

fn mixed() -> State {
    state_for(pdoc(MIXED, Origin::File(file("guide.md"))), 70, 50)
}

/// The labels and targets of the open hints.
fn labels(s: &State) -> Vec<(String, Act)> {
    let Mode::Hints(h) = &s.mode else {
        panic!("no hints: {}", s.mode_name());
    };
    h.labels
        .iter()
        .map(|(l, t)| (l.clone(), t.act.clone()))
        .collect()
}

/// What each label of hints opened with `open` gives: the effects of
/// typing it (in a fresh state each time) and the state afterwards.
fn each_label(make: impl Fn() -> State, open: &str) -> Vec<(Vec<Effect>, State)> {
    let mut s = make();
    keys(&mut s, open);
    let names: Vec<String> = labels(&s).into_iter().map(|(l, _)| l).collect();
    names
        .iter()
        .map(|label| {
            let mut s = make();
            keys(&mut s, open);
            let effects = keys(&mut s, label);
            assert_eq!(s.mode_name(), "normal", "{label}");
            (effects, s)
        })
        .collect()
}

/// The text of a copy effect.
fn copied(effects: &[Effect]) -> Option<(String, String)> {
    match effects {
        [Effect::Copy { text, what }] => Some((text.clone(), what.clone())),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Hints
// ---------------------------------------------------------------------------

#[test]
fn yank_hints_copy_the_source_of_everything() {
    let all: Vec<(String, String)> = each_label(mixed, "y")
        .into_iter()
        .map(|(e, _)| copied(&e).unwrap_or_else(|| panic!("{e:?}")))
        .collect();
    let texts: Vec<&str> = all.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        texts,
        [
            "guide.md#guide",
            "Intro with [a link](https://example.com) and $x^2$ inline.",
            "https://example.com",
            "x^2",
            "fn main() {}\nlet x = 1;",
            "E = mc^2",
            "| a | b |\n|---|---|\n| 1 | 2 |",
            "- first item",
            "- second item\n\n  continued",
            "> A quote\n> over lines.",
            "pic.png",
            "guide.md#next",
            "Last paragraph.",
        ]
    );
    let whats: Vec<&str> = all.iter().map(|(_, w)| w.as_str()).collect();
    assert_eq!(
        whats,
        [
            "copied guide.md#guide",
            "copied paragraph (1 line)",
            "copied https://example.com",
            "copied inline math (TeX) (1 line)",
            "copied code block (2 lines)",
            "copied math (TeX) (1 line)",
            "copied table (3 lines)",
            "copied list item (1 line)",
            "copied list item (3 lines)",
            "copied quote (2 lines)",
            "copied pic.png",
            "copied guide.md#next",
            "copied paragraph (1 line)",
        ]
    );
}

#[test]
fn follow_hints_go_to_headings_and_blocks() {
    let s = {
        let mut s = mixed();
        keys(&mut s, "f");
        s
    };
    assert_eq!(s.mode_name(), "hints");
    let acts = labels(&s);
    // Links, inline math, headings, code, math, the table and the figure;
    // no paragraphs, items or quotes.
    assert_eq!(acts.len(), 8, "{acts:?}");
    assert!(matches!(acts[0].1, Act::Jump(0)), "{acts:?}");
    assert!(acts.iter().any(|(_, a)| matches!(a, Act::Link(_))));
    // Jumps put the target at the top (as far as the end allows) and
    // record where the reader was.
    let long = format!("{MIXED}\n{}", "filler\n\n".repeat(40));
    let make = || state_for(pdoc(&long, Origin::File(file("guide.md"))), 70, 12);
    for (effects, s) in each_label(make, "f") {
        if !effects.is_empty() {
            assert_eq!(effects, [Effect::Open("https://example.com".into())]);
            continue;
        }
        let shown = top_text(&s);
        assert!(s.top() > 0 || shown.contains("Guide"), "{shown:?}");
    }
    // `f` on the heading below the screen's top.
    let mut s = make();
    keys(&mut s, "f");
    let next = labels(&s)
        .into_iter()
        .find(|(_, a)| matches!(a, Act::Jump(l) if *l == heading_line(&s, "Next")));
    assert!(next.is_none(), "Next is below the first screen");
    keys(&mut s, "\u{1b}");
}

#[test]
fn follow_hints_reach_footnotes_and_back() {
    let md = format!(
        "Text with a note[^n].\n\n{}[^n]: The note.",
        "filler\n\n".repeat(3)
    );
    let mut s = state(&md, 60, 30);
    keys(&mut s, "f");
    let acts = labels(&s);
    assert_eq!(acts.len(), 2, "the reference and the back-link: {acts:?}");
    keys(&mut s, "a");
    assert!(top_text(&s).contains("The note") || s.top() == s.max_top());
    let mut s = state(&md, 60, 30);
    keys(&mut s, "fs");
    assert_eq!(s.top(), 0, "the back-link leads to the reference");
}

#[test]
fn link_hints_stay_link_only() {
    let mut s = mixed();
    keys(&mut s, "o");
    let acts = labels(&s);
    assert_eq!(acts.len(), 1);
    assert!(matches!(acts[0].1, Act::Link(_)));
}

#[test]
fn y_copies_the_focused_link_else_opens_hints() {
    let mut s = mixed();
    code(&mut s, KeyCode::Tab);
    let effects = keys(&mut s, "y");
    assert_eq!(
        copied(&effects),
        Some((
            "https://example.com".into(),
            "copied https://example.com".into()
        ))
    );
    assert_eq!(s.mode_name(), "normal");
    code(&mut s, KeyCode::Esc);
    assert_eq!(s.focused_link(), None);
    keys(&mut s, "y");
    assert_eq!(s.mode_name(), "hints");
    let Mode::Hints(h) = &s.mode else {
        unreachable!()
    };
    assert_eq!(h.kind, HintKind::Yank);
}

#[test]
fn labels_narrow_as_they_are_typed() {
    let md: String = (1..=15)
        .map(|i| format!("[l{i}](https://e.x/{i}) "))
        .collect();
    let mut s = state(&md, 80, 20);
    keys(&mut s, "o");
    let names: Vec<String> = labels(&s).into_iter().map(|(l, _)| l).collect();
    assert_eq!(names.len(), 15);
    assert_eq!(names[8], "l");
    assert_eq!(names[9], ";a");
    keys(&mut s, ";");
    assert_eq!(s.mode_name(), "hints", "a prefix waits");
    code(&mut s, KeyCode::Backspace);
    keys(&mut s, ";");
    let effects = keys(&mut s, "s");
    assert_eq!(effects, [Effect::Open("https://e.x/11".into())]);
    // A key no label starts with ends the hints.
    keys(&mut s, "o;x");
    assert_eq!(s.mode_name(), "normal");
    assert_eq!(s.message(), Some("no label ;x"));
}

#[test]
fn more_targets_than_keys_get_longer_labels() {
    let md: String = (1..=150)
        .map(|i| format!("[{i}](https://e.x/{i}) "))
        .collect();
    let mut s = state(&md, 100, 60);
    keys(&mut s, "o");
    let names: Vec<String> = labels(&s).into_iter().map(|(l, _)| l).collect();
    assert_eq!(names.len(), 150);
    let last = names.last().cloned().unwrap();
    assert_eq!(last.len(), 3);
    let effects = keys(&mut s, &last);
    assert_eq!(effects, [Effect::Open("https://e.x/150".into())]);
}

#[test]
fn hints_end_with_a_new_layout_or_size() {
    let mut s = mixed();
    keys(&mut s, "y");
    drive(&mut s, Action::Resize { cols: 50, rows: 50 });
    assert_eq!(s.mode_name(), "normal");
    keys(&mut s, "f");
    drive(&mut s, Action::Resize { cols: 50, rows: 30 });
    assert_eq!(s.mode_name(), "normal", "rows only: still cancelled");
    let mut empty = state("", 40, 10);
    keys(&mut empty, "f");
    assert_eq!(empty.message(), Some("nothing to follow on screen"));
    keys(&mut empty, "y");
    assert_eq!(empty.message(), Some("nothing to copy on screen"));
    keys(&mut empty, "v");
    assert_eq!(empty.message(), Some("nothing to select on screen"));
}

#[test]
fn half_visible_elements_copy_whole() {
    let mut s = state(
        "```\nline 1\nline 2\nline 3\nline 4\nline 5\nline 6\n```\n\nafter",
        40,
        6,
    );
    keys(&mut s, "4j");
    keys(&mut s, "y");
    let acts = labels(&s);
    let code = acts
        .iter()
        .find_map(|(l, a)| match a {
            Act::Copy(c) if c.text.starts_with("line 1") => Some(l.clone()),
            _ => None,
        })
        .expect("the code block is labelled");
    let effects = keys(&mut s, &code);
    assert_eq!(
        copied(&effects).unwrap().0,
        "line 1\nline 2\nline 3\nline 4\nline 5\nline 6"
    );
}

#[test]
fn figures_zoom_only_when_shown_as_images() {
    // Without images the figure is an alt text box: `f` goes there.
    let mut s = mixed();
    keys(&mut s, "f");
    assert!(
        !labels(&s).iter().any(|(_, a)| matches!(a, Act::Zoom(_))),
        "no image shown"
    );
    // With a placement, `f` zooms and Esc comes back.
    let mut s = mixed();
    let mut l = s.layout.clone();
    l.images.push(crate::layout::Placement {
        image: crate::ir::ImageId(0),
        line: u32::try_from(heading_line(&s, "Next")).unwrap() - 2,
        col: 0,
        cols: 10,
        rows: 1,
    });
    drive(&mut s, Action::Layout(l));
    keys(&mut s, "f");
    let zoom = labels(&s)
        .into_iter()
        .find(|(_, a)| matches!(a, Act::Zoom(_)))
        .expect("a zoom label");
    let effects = update(
        &mut s,
        Action::Key(Key::char(zoom.0.chars().next().unwrap())),
    );
    let effects = if zoom.0.len() > 1 {
        update(
            &mut s,
            Action::Key(Key::char(zoom.0.chars().nth(1).unwrap())),
        )
    } else {
        effects
    };
    assert_eq!(effects, [Effect::Relayout]);
    assert_eq!(s.zoom(), Some(crate::ir::ImageId(0)));
    let wide = lay(s.document(), 70, true);
    drive(&mut s, Action::Layout(wide));
    assert_eq!(code(&mut s, KeyCode::Esc), []);
    assert_eq!(s.zoom(), None);
}

// ---------------------------------------------------------------------------
// Visual mode
// ---------------------------------------------------------------------------

#[test]
fn visual_line_mode_selects_and_moves() {
    let mut s = mixed();
    keys(&mut s, "V");
    assert_eq!(s.mode_name(), "visual");
    assert_eq!(s.selection(), Some((0, 0)), "the first block on screen");
    keys(&mut s, "j");
    assert_eq!(s.selection(), Some((0, 1)));
    keys(&mut s, "3j");
    assert_eq!(s.selection(), Some((0, 4)));
    keys(&mut s, "2k");
    assert_eq!(s.selection(), Some((0, 2)));
    keys(&mut s, "o");
    keys(&mut s, "j");
    assert_eq!(s.selection(), Some((1, 2)), "o moved the other end");
    keys(&mut s, "o");
    // `}`: the end of the block (the intro is one line: of the next one),
    // `{` the start.
    let block = |i: usize| s.layout().block_lines[i].clone();
    let (intro, code_block, math) = (block(1), block(2), block(3));
    keys(&mut s, "}");
    assert_eq!(s.selection(), Some((1, code_block.end as usize - 2)));
    keys(&mut s, "}");
    assert_eq!(s.selection(), Some((1, math.start as usize)));
    keys(&mut s, "{");
    assert_eq!(s.selection().unwrap().1, code_block.start as usize);
    keys(&mut s, "2{");
    assert_eq!(s.selection(), Some((0, 1)), "the cursor on the heading");
    keys(&mut s, "}");
    assert_eq!(s.selection().unwrap().1, intro.start as usize);
    keys(&mut s, "]");
    assert_eq!(s.selection().unwrap().1, heading_line(&s, "Next"));
    keys(&mut s, "G");
    assert_eq!(s.selection().unwrap().1, s.layout().len() - 1);
    keys(&mut s, "gg");
    assert_eq!(s.selection(), Some((0, 1)));
    keys(&mut s, "5G");
    assert_eq!(s.selection(), Some((1, 4)), "line 5");
    code(&mut s, KeyCode::Esc);
    // Esc, V and q leave.
    for leave in ["\u{1b}", "V", "q"] {
        keys(&mut s, "V");
        assert_eq!(s.mode_name(), "visual");
        match leave {
            "\u{1b}" => {
                code(&mut s, KeyCode::Esc);
            }
            k => {
                keys(&mut s, k);
            }
        }
        assert_eq!(s.mode_name(), "normal", "{leave:?}");
    }
}

#[test]
fn the_view_follows_the_cursor() {
    let mut s = state(&long_doc(10, 4), 60, 12);
    keys(&mut s, "V");
    keys(&mut s, "30j");
    let (_, cursor) = s.selection().unwrap();
    assert_eq!(cursor, 30);
    assert_eq!(s.top(), 30 - 10, "the cursor on the last row");
    key(&mut s, Key::ctrl('d'));
    assert_eq!(s.selection().unwrap().1, 35);
    assert!(s.top() <= 35 && 35 < s.top() + 11);
    keys(&mut s, "gg");
    assert_eq!(s.top(), 0);
    keys(&mut s, "zz");
    assert_eq!(s.top(), 0, "as far as the top allows");
    keys(&mut s, "20j");
    keys(&mut s, "zt");
    assert_eq!(s.top(), 20);
    keys(&mut s, "zb");
    assert_eq!(s.top(), 10);
    // `n` moves the cursor to matches.
    code(&mut s, KeyCode::Esc);
    keys(&mut s, "/Detail 3\n");
    keys(&mut s, "gg");
    assert_eq!(s.top(), 0);
    keys(&mut s, "V");
    keys(&mut s, "n");
    let line = s.selection().unwrap().1;
    assert!(s.layout().line_text(line).contains("Detail 3"));
}

#[test]
fn visual_y_copies_the_markdown_of_whole_blocks() {
    let mut s = mixed();
    keys(&mut s, "V");
    let intro = s.layout().block_lines[1].start;
    keys(&mut s, &format!("{intro}j"));
    let effects = keys(&mut s, "y");
    assert_eq!(s.mode_name(), "normal");
    assert_eq!(
        copied(&effects),
        Some((
            "# Guide\n\nIntro with [a link](https://example.com) and $x^2$ inline.".into(),
            "copied 2 blocks of Markdown (3 lines)".into()
        ))
    );
    // Inside a list: its items.
    let mut s = mixed();
    let second = (0..s.layout().len())
        .find(|&i| s.layout().line_text(i).contains("second item"))
        .unwrap();
    keys(&mut s, &format!("V{second}jo{second}j"));
    assert_eq!(s.selection(), Some((second, second)));
    let effects = keys(&mut s, "y");
    assert_eq!(
        copied(&effects),
        Some((
            "- second item\n\n  continued".into(),
            "copied 1 list item of Markdown (3 lines)".into()
        )),
        "{:?}",
        s.layout().line_text(second)
    );
}

#[test]
fn visual_y_gives_back_display_math_as_written() {
    let md = "Before.\n\n\\[ a =\nb \\]\n\nAfter $$\nx\n$$ end.\n";
    let mut s = state(md, 60, 20);
    keys(&mut s, "VG");
    let effects = keys(&mut s, "y");
    assert_eq!(copied(&effects).unwrap().0, md.trim_end());
    // The math block alone, from yank hints: its TeX.
    let mut s = state(md, 60, 20);
    keys(&mut s, "y");
    let tex: Vec<String> = labels(&s)
        .into_iter()
        .filter_map(|(_, a)| match a {
            Act::Copy(c) if c.what.contains("math") => Some(c.text),
            _ => None,
        })
        .collect();
    assert_eq!(tex, ["a =\nb", "x"], "x is display math in a paragraph");
    // Visual mode on the math block: its source.
    let mut s = state(md, 60, 20);
    keys(&mut s, "v");
    let math = labels(&s)
        .into_iter()
        .find(|(_, a)| matches!(a, Act::Select(r) if r.start > 0))
        .unwrap();
    keys(&mut s, &math.0);
    assert_eq!(s.mode_name(), "visual");
    let effects = keys(&mut s, "y");
    assert_eq!(copied(&effects).unwrap().0, "\\[ a =\nb \\]");
}

#[test]
fn display_math_with_a_line_of_equals_signs_copies_as_written() {
    // Without the rewrite the `=` would make `a + b` a setext heading.
    let md = "Intro\n\n\\[\na + b\n=\nc\n\\]\n\n# After\n\nText.\n";
    let math = "\\[\na + b\n=\nc\n\\]";
    let mut s = state_for(pdoc(md, Origin::File(file("eq.md"))), 50, 30);
    // Visual mode on the block: the file's text.
    keys(&mut s, "v");
    let select = labels(&s)
        .into_iter()
        .find(|(_, a)| matches!(a, Act::Select(r) if r.start > 0))
        .expect("the math block");
    keys(&mut s, &select.0);
    assert_eq!(copied(&keys(&mut s, "y")).unwrap().0, math);
    // Everything: the whole file.
    let all = keys(&mut s, "VGy");
    assert_eq!(copied(&all).unwrap().0, md.trim_end());
    // Yank hints: its TeX.
    let texts: Vec<String> = each_label(
        || state_for(pdoc(md, Origin::File(file("eq.md"))), 50, 30),
        "y",
    )
    .into_iter()
    .filter_map(|(e, _)| copied(&e).map(|(t, _)| t))
    .collect();
    assert_eq!(texts, ["Intro", "a + b\n=\nc", "eq.md#after", "Text."]);
    // The editor opens at the heading's line in the file (9), though the
    // rewrite moved it.
    let mut s = state_for(pdoc(md, Origin::File(file("eq.md"))), 50, 4);
    keys(&mut s, "]");
    assert!(top_text(&s).contains("After"), "{}", top_text(&s));
    assert_eq!(
        keys(&mut s, "e"),
        [Effect::Edit {
            path: file("eq.md"),
            line: 9
        }]
    );
}

#[test]
fn visual_shift_y_copies_the_text_of_the_lines() {
    let md = "> Quoted words\n> and more.\n\n\
              | Name | Size |\n|------|-----:|\n| one  | 1 |\n\n\
              ```\nindented\n    code\n```\n\nEnd.";
    let mut s = state(md, 60, 30);
    keys(&mut s, "VG");
    let effects = keys(&mut s, "Y");
    let (text, what) = copied(&effects).unwrap();
    assert_eq!(
        text,
        "Quoted words and more.\n\nName\tSize\none\t1\n\nindented\n    code\n\nEnd."
    );
    assert!(what.starts_with("copied the text of "), "{what}");
    assert!(text.lines().all(|l| l == l.trim_end()));
}

#[test]
fn shift_y_joins_wrapped_code_lines() {
    let long = "x".repeat(70);
    let md = format!("```\n{long}\nshort\n```");
    let mut s = state(&md, 40, 20);
    keys(&mut s, "VG");
    let (text, _) = copied(&keys(&mut s, "Y")).unwrap();
    assert_eq!(text, format!("{long}\nshort"));
}

#[test]
fn visual_hints_select_an_element() {
    let mut s = mixed();
    keys(&mut s, "v");
    let acts = labels(&s);
    let item = acts
        .iter()
        .find(|(_, a)| match a {
            Act::Select(r) => s.layout().line_text(r.start).contains("second item"),
            _ => false,
        })
        .unwrap()
        .0
        .clone();
    keys(&mut s, &item);
    let (lo, hi) = s.selection().unwrap();
    assert!(s.layout().line_text(lo).contains("second item"));
    assert!(s.layout().line_text(hi).contains("continued"));
    let (text, _) = copied(&keys(&mut s, "y")).unwrap();
    assert_eq!(text, "- second item\n\n  continued");
}

#[test]
fn a_selection_survives_a_resize() {
    let mut s = state(&long_doc(6, 3), 60, 12);
    keys(&mut s, "]]");
    keys(&mut s, "V2j");
    let text = |s: &State| {
        let (lo, hi) = s.selection().unwrap();
        (lo..=hi)
            .map(|i| s.layout().line_text(i))
            .collect::<Vec<_>>()
            .join("|")
    };
    let before = text(&s);
    drive(&mut s, Action::Resize { cols: 30, rows: 12 });
    assert_eq!(s.mode_name(), "visual");
    let after = text(&s);
    assert!(
        after.starts_with(before.split('|').next().unwrap()),
        "{before} / {after}"
    );
    // Reloading the file ends Visual mode.
    let doc = pdoc(&long_doc(6, 4), Origin::Memory);
    drive(&mut s, Action::Reloaded(doc));
    assert_eq!(s.mode_name(), "normal");
}

#[test]
fn visual_mode_on_an_empty_document() {
    let mut s = state("", 40, 10);
    keys(&mut s, "V");
    assert_eq!(s.mode_name(), "normal");
    assert_eq!(s.message(), Some("nothing to select"));
    let mut one = state("one line", 40, 10);
    keys(&mut one, "VjjGy");
    assert_eq!(one.mode_name(), "normal");
}

// ---------------------------------------------------------------------------
// Marks and zz
// ---------------------------------------------------------------------------

#[test]
fn marks_and_the_jump_back() {
    let mut s = state(&long_doc(10, 4), 60, 12);
    keys(&mut s, "20j");
    keys(&mut s, "ma");
    assert_eq!(s.message(), Some("mark a set"));
    keys(&mut s, "G");
    keys(&mut s, "'a");
    assert_eq!(s.top(), 20);
    keys(&mut s, "''");
    assert_eq!(s.top(), s.max_top(), "back to before the jump");
    keys(&mut s, "''");
    assert_eq!(s.top(), 20, "and back again");
    keys(&mut s, "'b");
    assert_eq!(s.message(), Some("no mark b"));
    keys(&mut s, "m1");
    assert_eq!(s.message(), Some("marks are a to z, not 1"));
    // Heading jumps, searches and hints record the place too.
    keys(&mut s, "gg]");
    let at = s.top();
    keys(&mut s, "''");
    assert_eq!(s.top(), 0);
    keys(&mut s, "''");
    assert_eq!(s.top(), at);
    keys(&mut s, "gg/Detail 5\n");
    keys(&mut s, "''");
    assert_eq!(s.top(), 0, "the search recorded where it started");
}

#[test]
fn marks_survive_a_resize_and_belong_to_their_document() {
    let mut s = state_for(pdoc(&long_doc(10, 4), Origin::File(file("a.md"))), 60, 12);
    keys(&mut s, "]]]");
    let shown = top_text(&s);
    keys(&mut s, "mq");
    keys(&mut s, "gg");
    drive(&mut s, Action::Resize { cols: 24, rows: 12 });
    keys(&mut s, "'q");
    assert_eq!(
        top_text(&s).split_whitespace().next(),
        shown.split_whitespace().next()
    );
    assert!(top_text(&s).contains("Section"), "{}", top_text(&s));
    // Another document has its own marks.
    opened(&mut s, "b.md", "# B\n\ntext", None);
    keys(&mut s, "'q");
    assert_eq!(s.message(), Some("no mark q"));
    code(&mut s, KeyCode::Backspace);
    keys(&mut s, "gg'q");
    assert!(top_text(&s).contains("Section"));
}

#[test]
fn zz_zt_zb_in_normal_mode() {
    let mut s = state(&long_doc(10, 4), 60, 12);
    keys(&mut s, "40j");
    keys(&mut s, "zt");
    assert_eq!(s.top(), 45, "the middle line to the top");
    keys(&mut s, "zb");
    assert_eq!(s.top(), 40, "and back to the bottom");
    keys(&mut s, "/Detail 7\n");
    let found = s.top();
    keys(&mut s, "zt");
    let line = s.layout().line_text(s.top());
    assert!(line.contains("Detail 7"), "{line} (was {found})");
    keys(&mut s, "zz");
    assert!(s.layout().line_text(s.top() + 5).contains("Detail 7"));
    // A pending z is dropped by a key that does not continue it.
    keys(&mut s, "zx");
    assert!(!s.keys_pending());
}

// ---------------------------------------------------------------------------
// The editor
// ---------------------------------------------------------------------------

#[test]
fn e_opens_the_editor_at_the_source_line() {
    // The math rewrite adds lines before the heading: the editor still
    // gets the line in the file.
    let md = "Intro\n\n\\[ a =\nb \\]\n\n## Target\n\ntext\n\n- one\n- two\n";
    let mut s = state_for(pdoc(md, Origin::File(file("doc.md"))), 40, 4);
    assert_eq!(
        keys(&mut s, "e"),
        [Effect::Edit {
            path: file("doc.md"),
            line: 1
        }]
    );
    keys(&mut s, "]");
    assert!(top_text(&s).contains("Target"));
    assert_eq!(
        keys(&mut s, "e"),
        [Effect::Edit {
            path: file("doc.md"),
            line: 6
        }]
    );
    // In Visual mode: the start of the selection, a list item there.
    keys(&mut s, "G");
    keys(&mut s, "Vjoj");
    let (lo, _) = s.selection().unwrap();
    assert!(
        s.layout().line_text(lo).contains("two"),
        "{}",
        s.layout().line_text(lo)
    );
    assert_eq!(
        keys(&mut s, "e"),
        [Effect::Edit {
            path: file("doc.md"),
            line: 11
        }]
    );
    assert_eq!(s.mode_name(), "normal");
}

#[test]
fn e_needs_a_file() {
    let mut s = state("from stdin", 40, 10);
    assert_eq!(keys(&mut s, "e"), []);
    assert_eq!(s.message(), Some("standard input cannot be edited"));
}

#[test]
fn source_lines() {
    use crate::pager::visual::source_line;
    assert_eq!(source_line("", 0), 1);
    assert_eq!(source_line("a\nb\nc", 0), 1);
    assert_eq!(source_line("a\nb\nc", 2), 2);
    assert_eq!(source_line("a\nb\nc", 4), 3);
    assert_eq!(source_line("a\r\nb", 3), 2);
    assert_eq!(source_line("a\nb", 99), 2, "clamped");
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

#[test]
fn remapped_keys_work_and_show_in_the_help() {
    let doc = pdoc(&long_doc(10, 4), Origin::Memory);
    let l = lay(&doc.doc, 60, false);
    let pager = PagerOptions {
        keys: vec![
            ("page-down".into(), vec!["f".into()]),
            ("top".into(), vec!["T".into()]),
            ("quit".into(), vec!["ZZ".into()]),
        ],
        ..PagerOptions::default()
    };
    let settings = Settings::new(&pager, &RenderOptions::default());
    let mut s = State::new(doc, None, l, (60, 12), &pager, settings);
    keys(&mut s, "f");
    assert_eq!(s.top(), 11);
    keys(&mut s, "T");
    assert_eq!(s.top(), 0);
    keys(&mut s, "gg");
    assert_eq!(s.top(), 0);
    assert_eq!(keys(&mut s, "q"), []);
    assert_eq!(keys(&mut s, "Z"), []);
    assert!(s.keys_pending());
    assert_eq!(keys(&mut s, "Z"), [Effect::Quit]);
}
