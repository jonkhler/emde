//! `update()` in the file picker (`O`) and for `:e`: what it asks the
//! shell for, and what it does with the answers.

use super::*;
use crate::pager::picker;
use crate::pager::picker::paths::Entry;
use crate::pager::picker::scan::{Display, Request};
use crate::pager::picker::{Event, Job, Picker, PickerEvent, PickerJob, Preview};

fn doc() -> State {
    let mut s = state_for(pdoc("# Notes\n\ntext\n", Origin::File(file("notes.md"))), 80, 24);
    s.settings.home = Some(PathBuf::from("/home/u"));
    s.settings.cwd = PathBuf::from("/work");
    s
}

fn picker(s: &State) -> &Picker {
    match &s.mode {
        Mode::Picker(p) => p,
        _ => panic!("no picker: {}", s.mode_name()),
    }
}

fn jobs(effects: &[Effect]) -> Vec<Job> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Picker(PickerJob(j)) => Some(j.clone()),
            _ => None,
        })
        .collect()
}

fn scan_of(effects: &[Effect]) -> Request {
    jobs(effects)
        .into_iter()
        .find_map(|j| match j {
            Job::Scan(r) => Some(r),
            _ => None,
        })
        .expect("a search")
}

fn event(s: &mut State, e: Event) -> Vec<Effect> {
    drive(s, Action::Picker(PickerEvent(e)))
}

/// Candidates for `paths` (under `/repo`, the document in `/repo/docs`),
/// `frecent` ones with frecency.
fn found(paths: &[&str], frecent: &[&str]) -> Vec<picker::Candidate> {
    let display = Display {
        base: PathBuf::from("/repo"),
        repo: Some(PathBuf::from("/repo")),
        dir: PathBuf::from("/repo/docs"),
        home: Some(PathBuf::from("/home/u")),
    };
    paths
        .iter()
        .map(|p| {
            let f = u32::from(frecent.contains(p)) * 10;
            display.candidate(PathBuf::from(p), f)
        })
        .collect()
}

fn shown(s: &State) -> Vec<String> {
    let p = picker(s);
    p.hits
        .iter()
        .map(|h| p.text(h).unwrap().0.to_owned())
        .collect()
}

fn entry(dir: &str, name: &str, is_dir: bool) -> Entry {
    Entry {
        name: name.into(),
        path: Path::new(dir).join(name),
        dir: is_dir,
        hidden: name.starts_with('.'),
    }
}

#[test]
fn o_opens_the_picker_and_starts_a_search() {
    let mut s = doc();
    let out = keys(&mut s, "O");
    assert_eq!(s.mode_name(), "picker");
    let req = scan_of(&out);
    assert_eq!(req.dir, PathBuf::from("/virtual"));
    assert_eq!(req.current, Some(file("notes.md")));
    assert!(!req.all);
    assert!(picker(&s).scanning);
    // Esc closes and cancels the search.
    let out = code(&mut s, KeyCode::Esc);
    assert_eq!(s.mode_name(), "normal");
    assert_eq!(jobs(&out), [Job::Stop]);
}

#[test]
fn results_stream_in_and_are_ranked() {
    let mut s = doc();
    let req = scan_of(&keys(&mut s, "O"));
    let files = [
        "/repo/additive_bias_results.md",
        "/repo/ala2_md_recovery_results.md",
        "/repo/ala2_all_atom_md_protocol.md",
        "/repo/docs/guide.md",
        "/repo/ala2_all_atom_md_results.md",
    ];
    // The empty query: recent files first, then the nearest.
    event(
        &mut s,
        Event::Found {
            scan: req.scan,
            files: found(&files[..3], &["/repo/ala2_md_recovery_results.md"]),
            done: false,
        },
    );
    assert_eq!(
        shown(&s),
        [
            "ala2_md_recovery_results.md",
            "additive_bias_results.md",
            "ala2_all_atom_md_protocol.md"
        ]
    );
    event(
        &mut s,
        Event::Found {
            scan: req.scan,
            files: found(&files[3..], &[]),
            done: true,
        },
    );
    assert!(!picker(&s).scanning);
    assert_eq!(shown(&s)[1], "docs/guide.md", "in the document's directory");
    // Typing filters and ranks.
    keys(&mut s, "aamr");
    assert_eq!(shown(&s)[0], "ala2_all_atom_md_results.md");
    let p = picker(&s);
    assert_eq!(p.hits[0].positions, [5, 9, 14, 17]);
    assert_eq!(p.matched, shown(&s).len());
    // A stale search's files are ignored.
    event(
        &mut s,
        Event::Found {
            scan: req.scan + 7,
            files: found(&["/repo/aamr.md"], &[]),
            done: true,
        },
    );
    assert!(!shown(&s).contains(&"aamr.md".to_owned()));
    // Enter opens the selection.
    let out = code(&mut s, KeyCode::Enter);
    assert_eq!(s.mode_name(), "normal");
    assert_eq!(
        out,
        [Effect::OpenPath(PathBuf::from("/repo/ala2_all_atom_md_results.md"))]
    );
}

#[test]
fn the_selection_stays_while_results_come_in() {
    let mut s = doc();
    let req = scan_of(&keys(&mut s, "O"));
    let more = |name: &str| Event::Found {
        scan: req.scan,
        files: found(&[name], &[]),
        done: false,
    };
    event(&mut s, more("/repo/b.md"));
    event(&mut s, more("/repo/c.md"));
    code(&mut s, KeyCode::Down);
    assert_eq!(picker(&s).selected, 1);
    let chosen = shown(&s)[1].clone();
    event(&mut s, more("/repo/a.md"));
    let p = picker(&s);
    assert_eq!(shown(&s)[p.selected], chosen);
    // Moving stays in range.
    for _ in 0..10 {
        key(&mut s, Key::ctrl('n'));
    }
    assert_eq!(picker(&s).selected, 2);
    key(&mut s, Key::ctrl('k'));
    code(&mut s, KeyCode::PageUp);
    assert_eq!(picker(&s).selected, 0);
}

#[test]
fn ctrl_t_searches_every_file() {
    let mut s = doc();
    let first = scan_of(&keys(&mut s, "O"));
    let out = key(&mut s, Key::ctrl('t'));
    let again = scan_of(&out);
    assert_eq!(jobs(&out)[0], Job::Stop);
    assert!(again.all);
    assert!(again.scan > first.scan);
    assert!(picker(&s).all);
}

/// Answer the directory listings among `effects` from `tree` (`(dir,
/// entries)`), as the shell does.
fn answer(s: &mut State, effects: Vec<Effect>, tree: &[(&str, Vec<Entry>)]) -> Vec<Effect> {
    let mut out = Vec::new();
    for e in effects {
        match e {
            Effect::Picker(PickerJob(Job::List(dir))) => {
                let entries = tree
                    .iter()
                    .find(|(d, _)| Path::new(d) == dir.as_path())
                    .map(|(_, e)| e.clone())
                    .ok_or_else(|| "not found".to_owned());
                let more = event(s, Event::Listed { dir, entries });
                out.extend(answer(s, more, tree));
            }
            other => out.push(other),
        }
    }
    out
}

#[test]
fn paths_list_directories() {
    let tree = [
        (
            "/home/u",
            vec![
                entry("/home/u", "dev", true),
                entry("/home/u", "docs", true),
                entry("/home/u", ".config", true),
                entry("/home/u", "diary.md", false),
                entry("/home/u", "zz.txt", false),
            ],
        ),
        ("/home/u/dev/", vec![entry("/home/u/dev", "emde", true)]),
        ("/home/u/docs/", Vec::new()),
    ];
    let mut s = doc();
    keys(&mut s, "O");
    let out = keys(&mut s, "~/");
    assert_eq!(jobs(&out), [Job::Stop, Job::List(PathBuf::from("/home/u"))]);
    assert!(!picker(&s).scanning, "the search stopped");
    answer(&mut s, out, &tree);
    assert_eq!(shown(&s), ["dev", "docs", "diary.md", "zz.txt"]);
    let typing = |s: &mut State, k: Key| {
        let out = key(s, k);
        answer(s, out, &tree)
    };
    typing(&mut s, Key::char('d'));
    assert_eq!(shown(&s), ["dev", "docs", "diary.md"], "directories first");
    // Tab: no longer common prefix, so the selected entry.
    typing(&mut s, Key::plain(KeyCode::Tab));
    assert_eq!(picker(&s).query, "~/dev/");
    assert_eq!(shown(&s), ["emde"]);
    // C-w takes the last component back.
    typing(&mut s, Key::ctrl('w'));
    assert_eq!(picker(&s).query, "~/");
    typing(&mut s, Key::char('d'));
    typing(&mut s, Key::char('o'));
    typing(&mut s, Key::plain(KeyCode::Tab));
    assert_eq!(picker(&s).query, "~/docs/", "the only match");
    assert_eq!(picker(&s).hits.len(), 0);
    // Enter on a directory goes in, on a file opens it.
    typing(&mut s, Key::ctrl('w'));
    typing(&mut s, Key::char('d'));
    typing(&mut s, Key::char('e'));
    typing(&mut s, Key::plain(KeyCode::Enter));
    assert_eq!(picker(&s).query, "~/dev/");
    typing(&mut s, Key::ctrl('w'));
    typing(&mut s, Key::char('z'));
    let out = typing(&mut s, Key::plain(KeyCode::Enter));
    assert_eq!(out, [Effect::OpenPath(PathBuf::from("/home/u/zz.txt"))]);
    // A path typed in full opens though nothing is listed.
    keys(&mut s, "O./new.md");
    let out = code(&mut s, KeyCode::Enter);
    assert_eq!(out, [Effect::OpenPath(PathBuf::from("/virtual/./new.md"))]);
}

#[test]
fn listing_problems_show() {
    let mut s = doc();
    s.settings.home = None;
    keys(&mut s, "O~/");
    assert_eq!(picker(&s).problem.as_deref(), Some("~ needs $HOME"));
    key(&mut s, Key::ctrl('u'));
    keys(&mut s, "/root/");
    event(
        &mut s,
        Event::Listed {
            dir: PathBuf::from("/root/"),
            entries: Err("/root: permission denied".into()),
        },
    );
    assert_eq!(picker(&s).problem.as_deref(), Some("/root: permission denied"));
    // Back to fuzzy mode: a new search.
    let out = key(&mut s, Key::ctrl('u'));
    assert_eq!(scan_of(&out).dir, PathBuf::from("/virtual"));
    assert!(jobs(&keys(&mut s, "x")).is_empty(), "the same search");
}

#[test]
fn previews_on_wide_screens() {
    let mut s = doc();
    drive(&mut s, Action::Resize { cols: 120, rows: 30 });
    let req = scan_of(&keys(&mut s, "O"));
    let out = event(
        &mut s,
        Event::Found {
            scan: req.scan,
            files: found(&["/repo/a.md", "/repo/b.md"], &[]),
            done: true,
        },
    );
    assert_eq!(jobs(&out), [Job::Preview(PathBuf::from("/repo/a.md"))]);
    event(
        &mut s,
        Event::Previewed(Preview {
            path: PathBuf::from("/repo/a.md"),
            lines: vec![(1, "A".into())],
            note: None,
        }),
    );
    assert!(picker(&s).preview.is_some());
    let out = code(&mut s, KeyCode::Down);
    assert_eq!(jobs(&out), [Job::Preview(PathBuf::from("/repo/b.md"))]);
    // A late preview of another file is dropped.
    event(
        &mut s,
        Event::Previewed(Preview {
            path: PathBuf::from("/repo/a.md"),
            lines: Vec::new(),
            note: None,
        }),
    );
    assert!(picker(&s).preview.is_none());
    // Narrow: no preview asked for.
    drive(&mut s, Action::Resize { cols: 80, rows: 30 });
    assert!(jobs(&code(&mut s, KeyCode::Up)).is_empty());
}

#[test]
fn the_mouse_picks() {
    let mut s = doc();
    let req = scan_of(&keys(&mut s, "O"));
    event(
        &mut s,
        Event::Found {
            scan: req.scan,
            files: found(&["/repo/a.md", "/repo/b.md"], &[]),
            done: true,
        },
    );
    let g = picker::picker_box(80, 24).list;
    let wheel = |kind| Mouse {
        kind,
        col: g.x + 2,
        row: g.y + 2,
    };
    drive(&mut s, Action::Mouse(wheel(MouseKind::WheelDown)));
    assert_eq!(picker(&s).selected, 1);
    // The second result's row.
    let click = Mouse {
        kind: MouseKind::Press(Button::Left),
        col: g.x + 4,
        row: g.y + 3,
    };
    let out = drive(&mut s, Action::Mouse(click));
    assert_eq!(out, [Effect::OpenPath(PathBuf::from("/repo/b.md"))]);
    // Outside the box: closes.
    keys(&mut s, "O");
    let outside = Mouse {
        col: 0,
        row: 0,
        ..click
    };
    drive(&mut s, Action::Mouse(outside));
    assert_eq!(s.mode_name(), "normal");
}

#[test]
fn colon_e_opens_paths() {
    let mut s = doc();
    assert_eq!(colon(&mut s, "e other.md"), [Effect::OpenPath(file("other.md"))]);
    assert_eq!(
        colon(&mut s, "edit ~/notes/x.md"),
        [Effect::OpenPath(PathBuf::from("/home/u/notes/x.md"))]
    );
    assert_eq!(
        colon(&mut s, "open /abs/dir"),
        [Effect::OpenPath(PathBuf::from("/abs/dir"))]
    );
    assert_eq!(
        colon(&mut s, "e ../up/a file.md"),
        [Effect::OpenPath(PathBuf::from("/virtual/../up/a file.md"))]
    );
    assert!(colon(&mut s, "e").is_empty());
    assert!(s.message().unwrap().contains(":e needs a file name"));
    assert!(colon(&mut s, "e ~bob/x").is_empty());
    assert_eq!(s.message(), Some("~user is not supported"));
    // Standard input: relative to the working directory.
    let mut s = state("# stdin\n", 80, 24);
    s.settings.cwd = PathBuf::from("/work");
    assert_eq!(
        colon(&mut s, "e a.md"),
        [Effect::OpenPath(PathBuf::from("/work/a.md"))]
    );
}

#[test]
fn colon_e_completes_with_tab() {
    let mut s = doc();
    keys(&mut s, ":e su");
    let out = code(&mut s, KeyCode::Tab);
    assert_eq!(jobs(&out), [Job::List(PathBuf::from("/virtual"))]);
    let listed = |s: &mut State, entries| {
        event(
            s,
            Event::Listed {
                dir: PathBuf::from("/virtual"),
                entries: Ok(entries),
            },
        )
    };
    listed(
        &mut s,
        vec![
            entry("/virtual", "sub", true),
            entry("/virtual", "summary.md", false),
            entry("/virtual", "suburb.md", false),
        ],
    );
    assert!(matches!(&s.mode, Mode::Command(i) if i == "e su"), "no common prefix");
    assert_eq!(
        s.completions,
        Some(vec!["sub".into(), "summary.md".into(), "suburb.md".into()])
    );
    keys(&mut s, "m");
    assert_eq!(s.completions, None, "a key clears them");
    code(&mut s, KeyCode::Tab);
    listed(
        &mut s,
        vec![entry("/virtual", "sub", true), entry("/virtual", "summary.md", false)],
    );
    assert!(matches!(&s.mode, Mode::Command(i) if i == "e summary.md"));
    // A directory gets its slash; a stale listing changes nothing.
    key(&mut s, Key::ctrl('u'));
    keys(&mut s, "e sub");
    code(&mut s, KeyCode::Tab);
    listed(&mut s, vec![entry("/virtual", "sub", true)]);
    assert!(matches!(&s.mode, Mode::Command(i) if i == "e sub/"));
    listed(&mut s, vec![entry("/virtual", "sub", true)]);
    assert!(matches!(&s.mode, Mode::Command(i) if i == "e sub/"));
    // Not an :e command: Tab does nothing.
    key(&mut s, Key::ctrl('u'));
    keys(&mut s, "n");
    assert!(code(&mut s, KeyCode::Tab).is_empty());
}

#[test]
fn a_directory_without_a_document_is_browsed() {
    let mut s = doc();
    let out = event(&mut s, Event::Browse(PathBuf::from("/home/u/dev")));
    assert_eq!(picker(&s).query, "~/dev/");
    assert_eq!(jobs(&out), [Job::List(PathBuf::from("/home/u/dev/"))]);
    let out = event(&mut s, Event::Browse(PathBuf::from("/srv/x")));
    assert_eq!(picker(&s).query, "/srv/x/");
    assert_eq!(jobs(&out), [Job::List(PathBuf::from("/srv/x/"))]);
}

#[test]
fn remapped_picker_keys() {
    let pager = PagerOptions {
        keys: vec![
            ("open-file".into(), vec!["C-o".into()]),
            ("picker-down".into(), vec!["C-j".into()]),
        ],
        ..PagerOptions::default()
    };
    let mut s = doc();
    s.settings.keymap = keymap::Keymap::new(&pager.keys).0;
    keys(&mut s, "O");
    assert_eq!(s.mode_name(), "normal", "O no longer opens it");
    key(&mut s, Key::ctrl('o'));
    assert_eq!(s.mode_name(), "picker");
    // Down no longer moves: nothing to check but that it is no text.
    code(&mut s, KeyCode::Down);
    assert_eq!(picker(&s).query, "");
    keys(&mut s, "jk");
    assert_eq!(picker(&s).query, "jk", "letters type");
}
