//! `update()` with the image view: what is in the gallery, the ways in,
//! stepping through it, and the way back.

use super::*;
use crate::layout::{LineKind, Placement};

/// Figures and links to images (and links that are not), with a linked
/// figure whose link leads to its own image.
const PICS: &str = concat!(
    "# Pictures\n\n",
    "![Loss curve](figures/loss.png \"Training loss\")\n\n",
    "See [the accuracy plot](figures/acc.jpg) and [the same loss](./figures/loss.png).\n\n",
    "A [remote chart](https://example.com/chart.png) and [a page](https://example.com/).\n\n",
    "[A screenshot](shots/screen) without an extension, and [notes](notes.txt).\n\n",
    "Inline: [dot](data:image/png;base64,iVBORw0KGgo=).\n\n",
    "## More\n\n",
    "[![Arch](figures/arch.gif)](figures/arch.gif)\n\n",
    "Last paragraph.\n",
);

fn pics() -> State {
    state_for(pdoc(PICS, Origin::File(file("pics.md"))), 80, 40)
}

fn names(s: &State) -> Vec<String> {
    gallery::build(s, None)
        .into_iter()
        .map(|p| p.name)
        .collect()
}

/// Tab to the link whose URL is `url`.
fn focus(s: &mut State, url: &str) {
    for _ in 0..20 {
        code(s, KeyCode::Tab);
        let link = s.focused_link().and_then(|l| s.document().link(l));
        if link.is_some_and(|l| &*l.url == url) {
            return;
        }
    }
    panic!("no link to {url}");
}

#[test]
fn the_gallery_is_in_document_order_once_each() {
    let s = pics();
    assert_eq!(
        names(&s),
        [
            "figures/loss.png",
            "figures/acc.jpg",
            "data:image/png",
            "figures/arch.gif"
        ]
    );
    let g = gallery::build(&s, None);
    assert_eq!(g[0].label, "Loss curve — Training loss");
    assert_eq!(g[1].label, "the accuracy plot");
    assert!(g[0].figure.is_some() && g[1].figure.is_none());
    assert_eq!(
        g[1].at,
        gallery::Location::File(PathBuf::from("/virtual/figures/acc.jpg"))
    );
    // With remote images on, links to remote images are in it too.
    let mut s = pics();
    s.settings.remote_images = true;
    assert_eq!(names(&s)[2], "https://example.com/chart.png");
    // A file a link leads to that turned out to be an image.
    let s = pics();
    let g = gallery::build(&s, Some(Path::new("/virtual/shots/screen")));
    let n: Vec<&str> = g.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(n[2], "shots/screen");
    assert_eq!(n.len(), 5);
}

#[test]
fn an_image_is_where_it_first_appears() {
    // A link before the figure of the same image: one entry, the link's.
    let s = state_for(
        pdoc(
            "[See the chart](a.png).\n\n![B](b.png)\n\n![A](./a.png)\n",
            Origin::File(file("x.md")),
        ),
        80,
        20,
    );
    let g = gallery::build(&s, None);
    let names: Vec<&str> = g.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["a.png", "b.png"]);
    assert_eq!(g[0].label, "See the chart");
}

#[test]
fn following_an_image_link_opens_the_view() {
    let mut s = pics();
    focus(&mut s, "figures/acc.jpg");
    let top = s.top();
    let generation = s.generation;
    assert_eq!(code(&mut s, KeyCode::Enter), [Effect::ShowImage]);
    assert_eq!(s.image_view(), Some((1, 4)));
    assert_eq!(s.mode_name(), "image");
    assert!(s.image.is_some(), "laid out");
    assert_ne!(s.generation, generation, "painted afresh");
    // Esc comes back to the same place, with the link still focused.
    let generation = s.generation;
    assert_eq!(code(&mut s, KeyCode::Esc), []);
    assert_eq!(s.image_view(), None);
    assert!(s.image.is_none());
    assert_ne!(s.generation, generation);
    assert_eq!(s.top(), top);
    assert!(s.focused_link().is_some());
    // `q` closes the view, it does not quit.
    code(&mut s, KeyCode::Enter);
    assert_eq!(keys(&mut s, "q"), []);
    assert_eq!(s.image_view(), None);
}

#[test]
fn other_ways_in() {
    // `gi`: the first image.
    let mut s = pics();
    assert_eq!(keys(&mut s, "gi"), [Effect::ShowImage]);
    assert_eq!(s.image_view(), Some((0, 4)));
    let mut plain = state("# No pictures\n\nJust text.\n", 60, 10);
    assert_eq!(keys(&mut plain, "gi"), []);
    assert_eq!(plain.message(), Some("no images in this document"));
    assert_eq!(plain.image_view(), None);
    // Link hints.
    let mut s = pics();
    keys(&mut s, "o");
    let Mode::Hints(h) = &s.mode else {
        panic!("{}", s.mode_name());
    };
    let label = h
        .labels
        .iter()
        .find(|(_, t)| {
            matches!(t.act, hints::Act::Link(i)
                if s.document().link(s.derived.links[i].link).is_some_and(|l| &*l.url == "figures/acc.jpg"))
        })
        .map(|(l, _)| l.clone())
        .unwrap();
    assert_eq!(keys(&mut s, &label), [Effect::ShowImage]);
    assert_eq!(s.image_view(), Some((1, 4)));
    // A click on the link.
    let mut s = pics();
    let o = s
        .derived
        .links
        .iter()
        .find(|o| {
            s.document()
                .link(o.link)
                .is_some_and(|l| &*l.url == "data:image/png;base64,iVBORw0KGgo=")
        })
        .copied()
        .unwrap();
    let hit = s.layout().link_hits[o.first as usize].clone();
    let col = s.layout().indent + hit.cols.start;
    let effects = drive(
        &mut s,
        Action::Mouse(Mouse {
            kind: MouseKind::Press(Button::Left),
            col,
            row: u16::try_from(hit.line).unwrap(),
        }),
    );
    assert_eq!(effects, [Effect::ShowImage]);
    assert_eq!(s.image_view(), Some((2, 4)));
}

#[test]
fn clicking_a_figure_shown_as_an_image_views_it() {
    let mut s = pics();
    // Make the last figure a placed image, as the shell would with images.
    let mut l = s.layout().clone();
    let line = heading_line(&s, "More") + 2;
    l.lines[line].kind = LineKind::Image {
        placement: 0,
        row: 0,
    };
    l.images.push(Placement {
        image: crate::ir::ImageId(1),
        line: u32::try_from(line).unwrap(),
        col: 0,
        cols: 10,
        rows: 1,
    });
    // (Without the link on its label, which a click would follow.)
    l.link_hits.retain(|h| h.line as usize != line);
    drive(&mut s, Action::Layout(l));
    let click = |s: &mut State, col: u16| {
        drive(
            s,
            Action::Mouse(Mouse {
                kind: MouseKind::Press(Button::Left),
                col: s.layout().indent + col,
                row: u16::try_from(line).unwrap(),
            }),
        )
    };
    assert_eq!(click(&mut s, 40), [], "beside the image");
    assert_eq!(click(&mut s, 3), [Effect::ShowImage]);
    assert_eq!(s.image_view(), Some((3, 4)), "the arch figure");
}

#[test]
fn stepping_through_the_gallery() {
    let mut s = pics();
    keys(&mut s, "gi");
    let at = |s: &State| s.image_view().map(|(i, _)| i);
    assert_eq!(keys(&mut s, "n"), [Effect::ShowImage]);
    assert_eq!(at(&s), Some(1));
    keys(&mut s, "j ");
    assert_eq!(at(&s), Some(3));
    // At the end it stays, and says so.
    assert_eq!(keys(&mut s, "n"), []);
    assert_eq!(s.message(), Some("the last image"));
    keys(&mut s, "p");
    assert_eq!(at(&s), Some(2));
    keys(&mut s, "k");
    code(&mut s, KeyCode::Right);
    assert_eq!(at(&s), Some(2));
    code(&mut s, KeyCode::Left);
    code(&mut s, KeyCode::Backspace);
    assert_eq!(at(&s), Some(0));
    assert_eq!(code(&mut s, KeyCode::Backspace), []);
    assert_eq!(s.message(), Some("the first image"));
    keys(&mut s, "G");
    assert_eq!(at(&s), Some(3));
    code(&mut s, KeyCode::Home);
    assert_eq!(at(&s), Some(0));
    // The keys of the text do nothing here.
    assert_eq!(keys(&mut s, "/xd"), []);
    assert_eq!(s.mode_name(), "image");
}

#[test]
fn copying_and_opening_the_image() {
    let mut s = pics();
    keys(&mut s, "gin");
    let effects = keys(&mut s, "y");
    assert_eq!(
        effects,
        [Effect::Copy {
            text: "/virtual/figures/acc.jpg".into(),
            what: "copied /virtual/figures/acc.jpg".into()
        }]
    );
    assert_eq!(
        keys(&mut s, "o"),
        [Effect::OpenFile(PathBuf::from("/virtual/figures/acc.jpg"))]
    );
    keys(&mut s, "n");
    assert_eq!(keys(&mut s, "o"), [], "a data: URI");
    assert!(s.message().unwrap().contains("data: URI"));
    let mut s = pics();
    s.settings.remote_images = true;
    keys(&mut s, "ginn");
    assert_eq!(
        keys(&mut s, "o"),
        [Effect::Open("https://example.com/chart.png".into())]
    );
    assert_eq!(s.mode_name(), "image", "still in the view");
}

#[test]
fn remote_image_links_open_as_links_without_remote_images() {
    let mut s = pics();
    focus(&mut s, "https://example.com/chart.png");
    assert_eq!(
        code(&mut s, KeyCode::Enter),
        [Effect::Open("https://example.com/chart.png".into())]
    );
    let mut s = pics();
    s.settings.remote_images = true;
    focus(&mut s, "https://example.com/chart.png");
    assert_eq!(code(&mut s, KeyCode::Enter), [Effect::ShowImage]);
}

#[test]
fn files_that_turn_out_to_be_images() {
    let mut s = pics();
    focus(&mut s, "shots/screen");
    let path = PathBuf::from("/virtual/shots/screen");
    assert_eq!(
        code(&mut s, KeyCode::Enter),
        [Effect::ShowFile(path.clone())]
    );
    // The shell sniffed it.
    assert_eq!(drive(&mut s, Action::ViewFile(path)), [Effect::ShowImage]);
    assert_eq!(s.image_view(), Some((2, 5)));
    // Written with `./`, it is the same file.
    let mut s = state_for(
        pdoc("[shot](./shots/screen)\n", Origin::File(file("x.md"))),
        60,
        10,
    );
    let effects = drive(
        &mut s,
        Action::ViewFile(PathBuf::from("/virtual/./shots/screen")),
    );
    assert_eq!(effects, [Effect::ShowImage]);
    assert_eq!(s.image_view(), Some((0, 1)));
    // A file no link leads to is not in the gallery.
    let mut s = pics();
    drive(&mut s, Action::ViewFile(PathBuf::from("/elsewhere/x")));
    assert_eq!(s.image_view(), None);
    assert_eq!(s.message(), Some("that image is not in the gallery"));
}

#[test]
fn the_image_mode_and_resizes_lay_the_view_out_again() {
    let mut s = state_for(
        pdoc(
            &format!("{}{PICS}", long_doc(4, 3)),
            Origin::File(file("p.md")),
        ),
        80,
        20,
    );
    keys(&mut s, "30j");
    let place = top_text(&s);
    keys(&mut s, "gi");
    assert_eq!(keys(&mut s, "i"), [Effect::CycleImages]);
    let effects = drive(
        &mut s,
        Action::ImageMode {
            name: "off".into(),
            relayout: true,
        },
    );
    assert_eq!(effects, [Effect::ShowImage], "after the relayout");
    let effects = drive(&mut s, Action::Resize { cols: 50, rows: 12 });
    assert_eq!(effects, [Effect::ShowImage]);
    assert_eq!(s.layout().width, 50, "the text too");
    drive(&mut s, Action::Resize { cols: 50, rows: 30 });
    code(&mut s, KeyCode::Esc);
    assert_eq!(top_text(&s), place, "the same place after the resizes");
    // A reload keeps the view open.
    keys(&mut s, "gin");
    drive(
        &mut s,
        Action::Reloaded(pdoc(
            "# Changed\n\n![x](x.png)\n",
            Origin::File(file("p.md")),
        )),
    );
    assert_eq!(s.image_view(), Some((1, 4)));
}

#[test]
fn the_view_shows_why_an_image_cannot_be_shown() {
    let mut s = pics();
    keys(&mut s, "gi");
    let frame = view(&s, &Ctx::new(&Theme::test(), &Caps::full()));
    let text: String = frame
        .lines
        .iter()
        .flat_map(|r| r.overlays.iter())
        .map(|seg| String::from_utf8_lossy(&seg.bytes).into_owned())
        .collect();
    assert!(text.contains("Loss curve — Training loss"), "{text:?}");
    assert!(text.contains("figures/loss.png"), "{text:?}");
    assert!(text.contains("images are off"), "{text:?}");
    let status = match &frame.lines.last().unwrap().body {
        Body::Text(b) => String::from_utf8_lossy(b).into_owned(),
        b => panic!("{b:?}"),
    };
    assert!(
        status.contains("image 1/4 · figures/loss.png"),
        "{status:?}"
    );
    assert_eq!(frame.left, 0);
    // Tiny screens draw what fits.
    for (cols, rows) in [(1, 1), (5, 2), (30, 3), (2, 40)] {
        drive(&mut s, Action::Resize { cols, rows });
        let f = view(&s, &Ctx::new(&Theme::test(), &Caps::full()));
        assert_eq!(f.lines.len(), usize::from(rows));
    }
}
