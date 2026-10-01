//! Terminal replies: `term::probe::ReplyParser` on arbitrary bytes, the
//! tmux query parser, and the decisions and `--doctor` report made from
//! what they found.
//!
//! Input: two option bytes, then the terminal's replies, optionally followed
//! by a NUL byte and the output of the tmux query.
//!
//! | byte | meaning |
//! |---|---|
//! | 0 | bit 0 the batch was sent inside tmux, 1 with wrapped queries, 2–4 the environment |
//! | 1 | where the replies are split in two |
//!
//! Checked:
//! * the parser gives the same answers however the bytes are split (in two,
//!   or byte by byte);
//! * the answers make sense: sizes are positive, the terminal's name is
//!   printable and at most 128 characters, and each kitty answer is
//!   recorded only for the batch that asked;
//! * the `--doctor` report and its JSON hold no control characters other
//!   than newlines, whatever the terminal and tmux said.

#![no_main]

use std::time::Duration;

use emde::config::ColorChoice;
use emde::options::{ImageOptions, When};
use emde::term::env::Env;
use emde::term::probe::{ProbeOutcome, ProbeReplies, ProbeStatus, ReplyParser, parse_x11_color};
use emde::term::{caps, color, doctor, tmux};
use emde_fuzz::header;
use libfuzzer_sys::fuzz_target;

fuzz_target!(init: emde_fuzz::init(), |data: &[u8]| {
    let ([flags, split], body) = header::<2>(data);
    let in_tmux = flags & 1 != 0;
    let wrapped = flags & 2 != 0;
    let (replies, tmux_text) = match body.iter().position(|&b| b == 0) {
        Some(i) => (&body[..i], &body[i + 1..]),
        None => (body, &[][..]),
    };

    let whole = parse(&[replies], in_tmux, wrapped);
    let at = usize::from(split) * replies.len() / 255;
    let halves = parse(&[&replies[..at], &replies[at..]], in_tmux, wrapped);
    assert_eq!(halves.replies(), whole.replies(), "split at {at}");
    assert_eq!(halves.is_done(), whole.is_done(), "split at {at}");
    let bytes: Vec<&[u8]> = replies.chunks(1).collect();
    let bytewise = parse(&bytes, in_tmux, wrapped);
    assert_eq!(bytewise.replies(), whole.replies(), "byte by byte");
    check_replies(whole.replies(), in_tmux, wrapped);
    let _ = parse_x11_color(replies);

    let tmux = tmux::parse(&String::from_utf8_lossy(tmux_text));
    if let Some(t) = &tmux {
        for text in [&t.version, &t.client_termtype, &t.client_termname, &t.set_clipboard]
            .into_iter()
            .chain(&t.features)
        {
            assert!(!text.chars().any(char::is_control), "tmux text {text:?}");
        }
    }

    let env = environment(flags >> 2);
    let base = color::decide(&env, true, ColorChoice::Auto, When::Auto);
    let caps = caps::decide(&env, base, tmux.as_ref(), Some(whole.replies()), &ImageOptions::default());
    let outcome = ProbeOutcome {
        replies: whole.replies().clone(),
        status: if whole.is_done() { ProbeStatus::Complete } else { ProbeStatus::TimedOut },
        elapsed: Duration::from_millis(u64::from(split)),
        wrapped,
    };
    for out in [
        doctor::report(&env, &caps, tmux.as_ref(), Some(&outcome)),
        doctor::json(&env, &caps, tmux.as_ref(), Some(&outcome)),
    ] {
        assert!(
            !out.chars().any(|c| c.is_control() && c != '\n'),
            "a control character in --doctor output: {out:?}"
        );
    }
});

/// Feed `chunks` to a new parser.
fn parse(chunks: &[&[u8]], in_tmux: bool, wrapped: bool) -> ReplyParser {
    let mut p = ReplyParser::new(in_tmux, wrapped);
    for chunk in chunks {
        p.feed(chunk);
    }
    p
}

fn check_replies(r: &ProbeReplies, in_tmux: bool, wrapped: bool) {
    for (w, h) in [r.cell_px, r.text_area_px, r.size_cells, r.cell_size()]
        .into_iter()
        .flatten()
    {
        assert!(w > 0 && h > 0, "an empty size in {r:?}");
    }
    if let Some(name) = &r.xtversion {
        assert!(!name.is_empty() && name.trim() == name, "{name:?}");
        assert!(!name.chars().any(char::is_control), "{name:?}");
        assert!(name.chars().count() <= 128, "{name:?}");
    }
    assert!(
        !r.kitty_ok || (!in_tmux && !wrapped),
        "an unasked kitty answer: {r:?}"
    );
    assert!(
        !r.outer_kitty_ok || wrapped,
        "an unasked outer kitty answer: {r:?}"
    );
    assert!(
        !r.outer_dsr_seen || wrapped,
        "an unasked status answer: {r:?}"
    );
    assert!(!r.tmux_own_da1_sixel || (in_tmux && r.da1_has(4)), "{r:?}");
}

/// One of eight environments.
fn environment(n: u8) -> Env {
    let pairs: &[(&str, &str)] = match n % 8 {
        0 => &[],
        1 => &[
            ("TMUX", "/tmp/tmux-1000/default,1,0"),
            ("TERM", "tmux-256color"),
            ("TERM_PROGRAM", "tmux"),
            ("TERM_PROGRAM_VERSION", "3.4"),
            ("LC_TERMINAL", "iTerm2"),
            ("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22"),
        ],
        2 => &[("TERM_PROGRAM", "vscode"), ("TERM", "xterm-256color")],
        3 => &[("KITTY_WINDOW_ID", "1"), ("TERM", "xterm-kitty")],
        4 => &[("TERM", "xterm-256color"), ("COLORFGBG", "15;0")],
        5 => &[("TERM_PROGRAM", "iTerm.app"), ("LC_TERMINAL", "iTerm2")],
        6 => &[("TERM", "xterm-ghostty"), ("COLORTERM", "truecolor")],
        _ => &[("TERM", "dumb"), ("NO_COLOR", "1")],
    };
    Env::from_pairs(pairs)
}
