//! Layout snapshots of every fixture in `tests/fixtures/md/` at widths 24,
//! 40, 80 and 120, with the frozen test theme:
//!
//! * `<name>.plain` — what a pipe gets (no escapes, numbered link
//!   references, frames instead of panels);
//! * `<name>.styled` — a truecolor terminal with OSC 8 and styled
//!   underlines, as style tags ([`emde::render::debug_text`]).
//!
//! `kitchen-sink` is also rendered for 256 and 16 colours. Each width
//! starts with a ruler exactly that wide.

mod common;

use common::{FIXTURES, WIDTHS, fixture, lay_out, parse};
use emde::options::RenderOptions;
use emde::render::{debug_text, plain_text};
use emde::term::{Caps, ColorDepth};

fn ruler(width: u16) -> String {
    let label = format!("━━ width {width} ");
    let rest = usize::from(width).saturating_sub(label.chars().count());
    format!("{label}{}\n", "━".repeat(rest))
}

fn plain(name: &str) -> String {
    let doc = parse(&fixture(name));
    let opts = RenderOptions::default();
    let mut out = String::new();
    for width in WIDTHS {
        out.push_str(&ruler(width));
        let layout = lay_out(&doc, width, &Caps::plain(), &opts);
        out.push_str(&plain_text(&doc, &layout));
    }
    out
}

fn styled(name: &str, caps: &Caps, widths: &[u16]) -> String {
    let doc = parse(&fixture(name));
    let opts = RenderOptions::default();
    let mut out = String::new();
    for &width in widths {
        out.push_str(&ruler(width));
        let layout = lay_out(&doc, width, caps, &opts);
        out.push_str(&debug_text(&layout, caps.color, caps.styled_underline));
    }
    out
}

#[test]
fn fixtures_are_listed() {
    let dir = format!("{}/tests/fixtures/md", env!("CARGO_MANIFEST_DIR"));
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            name.strip_suffix(".md").map(str::to_string)
        })
        .collect();
    on_disk.sort();
    let mut listed: Vec<String> = FIXTURES.iter().map(|s| s.to_string()).collect();
    listed.sort();
    assert_eq!(on_disk, listed, "every fixture is snapshotted");
}

macro_rules! snapshots {
    ($($test:ident => $name:literal),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                insta::assert_snapshot!(concat!($name, ".plain"), plain($name));
                insta::assert_snapshot!(
                    concat!($name, ".styled"),
                    styled($name, &Caps::full(), &WIDTHS)
                );
            }
        )*
    };
}

snapshots! {
    kitchen_sink => "kitchen-sink",
    headings => "headings",
    lists => "lists",
    quotes_alerts => "quotes-alerts",
    code => "code",
    tables => "tables",
    footnotes => "footnotes",
    html => "html",
    math => "math",
    links => "links",
    cjk_emoji => "cjk-emoji",
    regressions => "regressions",
    llm_style => "llm-style",
    llm_math => "llm-math",
    readme_html => "readme-html",
    edge_cases => "edge-cases",
}

#[test]
fn kitchen_sink_at_lower_depths() {
    for (depth, label) in [(ColorDepth::Ansi256, "256"), (ColorDepth::Ansi16, "16")] {
        let caps = Caps {
            color: depth,
            ..Caps::full()
        };
        insta::assert_snapshot!(
            format!("kitchen-sink.styled-{label}"),
            styled("kitchen-sink", &caps, &[80])
        );
    }
}
