//! Colour depth, hyperlink and underline-style decisions from the
//! environment (`NO_COLOR`, `FORCE_COLOR`, `COLORTERM`, `TERM`, `$TMUX`, …).
//!
//! [`decide`] is pure: an [`Env`] snapshot, whether stdout is a terminal and
//! the `--color`/`--hyperlinks` settings in, the base [`Caps`] out (colour
//! depth, OSC 8, styled underlines, each with a [`Reason`]). The terminal
//! probe and the tmux query refine it later (`term::caps`).
//!
//! # Colour depth (first match wins)
//!
//! 1. `--color` (`always` forces colour and still detects the depth);
//! 2. `NO_COLOR` → attributes only (no escapes at all when piped);
//! 3. `FORCE_COLOR`: `0` off, `1`/`2`/`3` 16/256/24-bit colours, anything
//!    else forces colour;
//! 4. `CLICOLOR_FORCE` (not `0`) forces colour;
//! 5. `CLICOLOR=0` → attributes only (nothing when piped);
//! 6. not a terminal, or `TERM=dumb` → no escape sequences;
//! 7. `COLORTERM=truecolor|24bit`, or a `TERM` ending in `-direct` → 24-bit;
//! 8. inside tmux → 24-bit (tmux converts per client) with styled
//!    underlines; OSC 8 only from tmux 3.4 (`TERM_PROGRAM_VERSION`);
//! 9. a terminal known for 24-bit colour (iTerm2 — also via `LC_TERMINAL`,
//!    which SSH forwards — WezTerm, Ghostty, VS Code, kitty, Windows
//!    Terminal) → 24-bit;
//! 10. `TERM` naming 256 colours → 256;
//! 11. otherwise 16 colours.

use super::env::Env;
use super::{Caps, ColorDepth, Reason};
use crate::options::When;

/// `--color` / `render.color`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Detect from the environment and the terminal.
    #[default]
    Auto,
    /// Colour even when the output is not a terminal (depth still detected).
    Always,
    /// No colours and no escape sequences.
    Never,
    /// Force 24-bit colour.
    TrueColor,
    /// Force the xterm 256-colour palette.
    Ansi256,
    /// Force the 16 ANSI colours.
    Ansi16,
}

/// Reason topics (the same names `term::caps` and `--doctor` use).
pub mod topic {
    /// The colour depth.
    pub const COLOR: &str = "color";
    /// OSC 8 hyperlinks.
    pub const HYPERLINKS: &str = "hyperlinks";
    /// Styled underlines.
    pub const UNDERLINE: &str = "underline";
}

/// The base capabilities: colour depth, hyperlinks and styled underlines.
pub fn decide(env: &Env, is_tty: bool, color: ColorChoice, hyperlinks: When) -> Caps {
    let (depth, color_why) = color_depth(env, is_tty, color);
    let (links, links_why) = hyperlinks_for(env, depth, hyperlinks);
    let (underline, underline_why) = styled_underline(env, depth);
    Caps {
        is_tty,
        color: depth,
        hyperlinks: links,
        styled_underline: underline,
        sync: is_tty,
        in_tmux: in_tmux(env),
        over_ssh: env.is_set("SSH_CONNECTION") || env.is_set("SSH_TTY"),
        reasons: vec![
            Reason {
                topic: topic::COLOR,
                detail: color_why,
            },
            Reason {
                topic: topic::HYPERLINKS,
                detail: links_why,
            },
            Reason {
                topic: topic::UNDERLINE,
                detail: underline_why,
            },
        ],
        ..Caps::default()
    }
}

/// The colour depth and why (rules 1–11 in the module docs).
pub fn color_depth(env: &Env, is_tty: bool, choice: ColorChoice) -> (ColorDepth, String) {
    match choice {
        ColorChoice::Never => (ColorDepth::None, "--color=never".into()),
        ColorChoice::TrueColor => (ColorDepth::TrueColor, "--color=truecolor".into()),
        ColorChoice::Ansi256 => (ColorDepth::Ansi256, "--color=256".into()),
        ColorChoice::Ansi16 => (ColorDepth::Ansi16, "--color=16".into()),
        ColorChoice::Always => {
            let (depth, why) = detect(env);
            (depth, format!("--color=always, {why}"))
        }
        ColorChoice::Auto => auto_depth(env, is_tty),
    }
}

/// Rules 2–11.
fn auto_depth(env: &Env, is_tty: bool) -> (ColorDepth, String) {
    let attributes_only = |why: &str| {
        if is_tty {
            (ColorDepth::Mono, why.to_string())
        } else {
            (ColorDepth::None, format!("{why}, not a terminal"))
        }
    };
    if env.is_set("NO_COLOR") {
        return attributes_only("NO_COLOR");
    }
    if let Some(v) = env.get("FORCE_COLOR") {
        match v.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "no" | "off" => return (ColorDepth::None, "FORCE_COLOR=0".into()),
            "1" => return (ColorDepth::Ansi16, "FORCE_COLOR=1".into()),
            "2" => return (ColorDepth::Ansi256, "FORCE_COLOR=2".into()),
            "3" => return (ColorDepth::TrueColor, "FORCE_COLOR=3".into()),
            _ => {
                let (depth, why) = detect(env);
                return (depth, format!("FORCE_COLOR, {why}"));
            }
        }
    }
    if env
        .non_empty("CLICOLOR_FORCE")
        .is_some_and(|v| v.trim() != "0")
    {
        let (depth, why) = detect(env);
        return (depth, format!("CLICOLOR_FORCE, {why}"));
    }
    if env.get("CLICOLOR").is_some_and(|v| v.trim() == "0") {
        return attributes_only("CLICOLOR=0");
    }
    if !is_tty {
        return (ColorDepth::None, "not a terminal".into());
    }
    if env.get("TERM") == Some("dumb") {
        return (ColorDepth::None, "TERM=dumb".into());
    }
    detect(env)
}

/// Rules 7–11: the depth the terminal supports.
fn detect(env: &Env) -> (ColorDepth, String) {
    let term = env.get("TERM").unwrap_or("");
    if let Some(ct) = env.get("COLORTERM")
        && (ct.eq_ignore_ascii_case("truecolor") || ct.eq_ignore_ascii_case("24bit"))
    {
        return (ColorDepth::TrueColor, format!("COLORTERM={ct}"));
    }
    if term.ends_with("-direct") {
        return (ColorDepth::TrueColor, format!("TERM={term}"));
    }
    if in_tmux(env) {
        return (ColorDepth::TrueColor, "in tmux".into());
    }
    if let Some(name) = truecolor_terminal(env) {
        return (ColorDepth::TrueColor, name);
    }
    if term.contains("256color") {
        return (ColorDepth::Ansi256, format!("TERM={term}"));
    }
    let why = if term.is_empty() {
        "TERM unset".to_string()
    } else {
        format!("TERM={term}")
    };
    (ColorDepth::Ansi16, why)
}

/// Whether the output goes through tmux: `$TMUX`, or `TERM_PROGRAM=tmux`
/// (which tmux exports into its panes).
pub fn in_tmux(env: &Env) -> bool {
    env.is_set("TMUX") || term_program(env).is_some_and(|p| p.eq_ignore_ascii_case("tmux"))
}

fn term_program(env: &Env) -> Option<&str> {
    env.non_empty("TERM_PROGRAM")
}

/// A terminal emulator known for 24-bit colour, by name.
fn truecolor_terminal(env: &Env) -> Option<String> {
    let term = env.get("TERM").unwrap_or("");
    if let Some(p) = term_program(env)
        && ["iTerm.app", "WezTerm", "ghostty", "vscode"]
            .iter()
            .any(|n| p.eq_ignore_ascii_case(n))
    {
        return Some(format!("TERM_PROGRAM={p}"));
    }
    if env.get("LC_TERMINAL") == Some("iTerm2") {
        return Some("LC_TERMINAL=iTerm2".into());
    }
    if env.is_set("KITTY_WINDOW_ID") {
        return Some("kitty".into());
    }
    if env.is_set("WT_SESSION") {
        return Some("Windows Terminal".into());
    }
    if term == "xterm-kitty" || term == "xterm-ghostty" {
        return Some(format!("TERM={term}"));
    }
    None
}

/// The tmux version from `TERM_PROGRAM_VERSION` (`3.4`, `3.3a`,
/// `next-3.5`), as `(major, minor)`.
fn tmux_version(env: &Env) -> Option<(u32, u32)> {
    if !term_program(env).is_some_and(|p| p.eq_ignore_ascii_case("tmux")) {
        return None;
    }
    let v = env.non_empty("TERM_PROGRAM_VERSION")?;
    let v = v.trim_start_matches(|c: char| !c.is_ascii_digit());
    let mut parts = v.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// A number from an environment variable (`VTE_VERSION`, …).
fn version_number(env: &Env, key: &str) -> Option<u64> {
    env.non_empty(key)?.trim().parse().ok()
}

/// Terminals that show OSC 8 hyperlinks, by name.
fn hyperlink_terminal(env: &Env) -> Option<String> {
    let term = env.get("TERM").unwrap_or("");
    if let Some(p) = term_program(env)
        && ["iTerm.app", "WezTerm", "ghostty", "vscode", "rio", "Tabby"]
            .iter()
            .any(|n| p.eq_ignore_ascii_case(n))
    {
        return Some(format!("TERM_PROGRAM={p}"));
    }
    if env.get("LC_TERMINAL") == Some("iTerm2") {
        return Some("LC_TERMINAL=iTerm2".into());
    }
    if env.is_set("KITTY_WINDOW_ID") || term == "xterm-kitty" || term == "xterm-ghostty" {
        return Some(format!("TERM={term}"));
    }
    if env.is_set("WT_SESSION") {
        return Some("Windows Terminal".into());
    }
    if version_number(env, "VTE_VERSION").is_some_and(|v| v >= 5000) {
        return Some("VTE ≥ 0.50".into());
    }
    if version_number(env, "KONSOLE_VERSION").is_some_and(|v| v >= 201_200) {
        return Some("Konsole ≥ 20.12".into());
    }
    if term.starts_with("foot") || term == "alacritty" {
        return Some(format!("TERM={term}"));
    }
    None
}

/// Whether to emit OSC 8 hyperlinks, and why.
fn hyperlinks_for(env: &Env, depth: ColorDepth, when: When) -> (bool, String) {
    if depth == ColorDepth::None {
        return (false, "no escape sequences".into());
    }
    match when {
        When::Always => return (true, "--hyperlinks=always".into()),
        When::Never => return (false, "--hyperlinks=never".into()),
        When::Auto => {}
    }
    if in_tmux(env) {
        return match tmux_version(env) {
            Some(v) if v >= (3, 4) => (true, format!("tmux {}.{} ≥ 3.4", v.0, v.1)),
            Some(v) => (false, format!("tmux {}.{} < 3.4", v.0, v.1)),
            None => (false, "tmux version unknown".into()),
        };
    }
    match hyperlink_terminal(env) {
        Some(name) => (true, name),
        None => (false, "terminal not known to show OSC 8 links".into()),
    }
}

/// Whether to use styled underlines (`4:3`) and underline colours, and why.
fn styled_underline(env: &Env, depth: ColorDepth) -> (bool, String) {
    if depth == ColorDepth::None {
        return (false, "no escape sequences".into());
    }
    if in_tmux(env) {
        return (true, "in tmux (it adapts them per client)".into());
    }
    let term = env.get("TERM").unwrap_or("");
    if let Some(p) = term_program(env)
        && ["iTerm.app", "WezTerm", "ghostty", "vscode"]
            .iter()
            .any(|n| p.eq_ignore_ascii_case(n))
    {
        return (true, format!("TERM_PROGRAM={p}"));
    }
    if env.get("LC_TERMINAL") == Some("iTerm2") {
        return (true, "LC_TERMINAL=iTerm2".into());
    }
    if env.is_set("KITTY_WINDOW_ID")
        || matches!(term, "xterm-kitty" | "xterm-ghostty" | "alacritty")
        || term.starts_with("foot")
    {
        return (true, format!("TERM={term}"));
    }
    if version_number(env, "VTE_VERSION").is_some_and(|v| v >= 5200) {
        return (true, "VTE ≥ 0.52".into());
    }
    (false, "terminal not known to draw styled underlines".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depth(pairs: &[(&str, &str)], tty: bool) -> ColorDepth {
        color_depth(&Env::from_pairs(pairs), tty, ColorChoice::Auto).0
    }

    /// The environment recorded in the user's session: iTerm2 3.6.9 → SSH
    /// → tmux 3.4, with `COLORTERM` unset.
    const USER_SESSION: &[(&str, &str)] = &[
        ("TERM", "tmux-256color"),
        ("TERM_PROGRAM", "tmux"),
        ("TERM_PROGRAM_VERSION", "3.4"),
        ("LC_TERMINAL", "iTerm2"),
        ("LC_TERMINAL_VERSION", "3.6.9"),
        ("SSH_CONNECTION", "192.0.2.10 50000 192.0.2.20 22"),
    ];

    #[test]
    fn users_session_gets_everything() {
        for with_tmux in [false, true] {
            let mut pairs = USER_SESSION.to_vec();
            if with_tmux {
                pairs.push(("TMUX", "/tmp/tmux-1001/default,443340,2"));
            }
            let env = Env::from_pairs(&pairs);
            let caps = decide(&env, true, ColorChoice::Auto, When::Auto);
            assert_eq!(caps.color, ColorDepth::TrueColor, "{:?}", caps.reasons);
            assert!(caps.hyperlinks, "{:?}", caps.reasons);
            assert!(caps.styled_underline, "{:?}", caps.reasons);
            assert!(caps.in_tmux);
            assert!(caps.over_ssh);
            assert!(caps.is_tty);
            let topics: Vec<&str> = caps.reasons.iter().map(|r| r.topic).collect();
            assert_eq!(topics, ["color", "hyperlinks", "underline"]);
            assert_eq!(caps.reasons[1].detail, "tmux 3.4 ≥ 3.4");
        }
    }

    #[test]
    fn users_session_piped_has_no_escapes() {
        let env = Env::from_pairs(USER_SESSION);
        let caps = decide(&env, false, ColorChoice::Auto, When::Auto);
        assert_eq!(caps.color, ColorDepth::None);
        assert!(!caps.hyperlinks && !caps.styled_underline);
        // Forced colour keeps the detected depth.
        let caps = decide(&env, false, ColorChoice::Always, When::Auto);
        assert_eq!(caps.color, ColorDepth::TrueColor);
        assert!(caps.hyperlinks);
    }

    #[test]
    fn flag_wins() {
        let env = Env::from_pairs(&[("NO_COLOR", "1"), ("TERM", "xterm-256color")]);
        assert_eq!(
            color_depth(&env, false, ColorChoice::TrueColor).0,
            ColorDepth::TrueColor
        );
        assert_eq!(
            color_depth(&env, true, ColorChoice::Ansi16).0,
            ColorDepth::Ansi16
        );
        assert_eq!(
            color_depth(&env, true, ColorChoice::Ansi256).0,
            ColorDepth::Ansi256
        );
        assert_eq!(
            color_depth(&env, true, ColorChoice::Never).0,
            ColorDepth::None
        );
        assert_eq!(
            color_depth(&env, false, ColorChoice::Always).0,
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn no_color_means_attributes_only() {
        assert_eq!(
            depth(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")], true),
            ColorDepth::Mono
        );
        assert_eq!(depth(&[("NO_COLOR", "1")], false), ColorDepth::None);
        // An empty NO_COLOR is ignored.
        assert_eq!(
            depth(&[("NO_COLOR", ""), ("COLORTERM", "truecolor")], true),
            ColorDepth::TrueColor
        );
        // NO_COLOR beats FORCE_COLOR and CLICOLOR_FORCE.
        assert_eq!(
            depth(&[("NO_COLOR", "1"), ("FORCE_COLOR", "3")], true),
            ColorDepth::Mono
        );
        assert_eq!(
            depth(&[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")], true),
            ColorDepth::Mono
        );
    }

    #[test]
    fn force_color_levels() {
        assert_eq!(
            depth(&[("FORCE_COLOR", "0"), ("COLORTERM", "truecolor")], true),
            ColorDepth::None
        );
        assert_eq!(depth(&[("FORCE_COLOR", "1")], false), ColorDepth::Ansi16);
        assert_eq!(depth(&[("FORCE_COLOR", "2")], false), ColorDepth::Ansi256);
        assert_eq!(depth(&[("FORCE_COLOR", "3")], false), ColorDepth::TrueColor);
        assert_eq!(depth(&[("FORCE_COLOR", "false")], true), ColorDepth::None);
        // Any other value forces colour at the detected depth.
        assert_eq!(
            depth(&[("FORCE_COLOR", "yes"), ("TERM", "xterm-256color")], false),
            ColorDepth::Ansi256
        );
        assert_eq!(depth(&[("FORCE_COLOR", "")], false), ColorDepth::Ansi16);
    }

    #[test]
    fn clicolor() {
        assert_eq!(
            depth(
                &[("CLICOLOR_FORCE", "1"), ("TERM", "xterm-256color")],
                false
            ),
            ColorDepth::Ansi256
        );
        assert_eq!(depth(&[("CLICOLOR_FORCE", "0")], false), ColorDepth::None);
        assert_eq!(
            depth(&[("CLICOLOR", "0"), ("COLORTERM", "truecolor")], true),
            ColorDepth::Mono
        );
        assert_eq!(depth(&[("CLICOLOR", "0")], false), ColorDepth::None);
        assert_eq!(
            depth(&[("CLICOLOR", "1"), ("TERM", "xterm")], true),
            ColorDepth::Ansi16
        );
    }

    #[test]
    fn terminals_and_dumb() {
        assert_eq!(
            depth(&[("TERM", "dumb"), ("COLORTERM", "truecolor")], true),
            ColorDepth::None
        );
        assert_eq!(
            depth(&[("COLORTERM", "24bit")], true),
            ColorDepth::TrueColor
        );
        assert_eq!(
            depth(&[("TERM", "xterm-direct")], true),
            ColorDepth::TrueColor
        );
        assert_eq!(
            depth(&[("TMUX", "/tmp/t,1,0"), ("TERM", "screen")], true),
            ColorDepth::TrueColor
        );
        for pairs in [
            &[("TERM_PROGRAM", "iTerm.app")][..],
            &[("TERM_PROGRAM", "WezTerm")],
            &[("TERM_PROGRAM", "ghostty")],
            &[("TERM_PROGRAM", "vscode")],
            &[("LC_TERMINAL", "iTerm2")],
            &[("KITTY_WINDOW_ID", "1")],
            &[("WT_SESSION", "abc")],
            &[("TERM", "xterm-kitty")],
            &[("TERM", "xterm-ghostty")],
        ] {
            assert_eq!(depth(pairs, true), ColorDepth::TrueColor, "{pairs:?}");
        }
        assert_eq!(
            depth(&[("TERM", "xterm-256color")], true),
            ColorDepth::Ansi256
        );
        assert_eq!(depth(&[("TERM", "xterm")], true), ColorDepth::Ansi16);
        assert_eq!(depth(&[], true), ColorDepth::Ansi16);
        assert_eq!(
            depth(
                &[
                    ("TERM_PROGRAM", "Apple_Terminal"),
                    ("TERM", "xterm-256color")
                ],
                true
            ),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn hyperlinks_decisions() {
        let links = |pairs: &[(&str, &str)], when: When| {
            decide(&Env::from_pairs(pairs), true, ColorChoice::Auto, when).hyperlinks
        };
        assert!(links(
            &[("TERM_PROGRAM", "tmux"), ("TERM_PROGRAM_VERSION", "3.4")],
            When::Auto
        ));
        assert!(links(
            &[("TERM_PROGRAM", "tmux"), ("TERM_PROGRAM_VERSION", "3.5a")],
            When::Auto
        ));
        assert!(links(
            &[
                ("TERM_PROGRAM", "tmux"),
                ("TERM_PROGRAM_VERSION", "next-3.6")
            ],
            When::Auto
        ));
        assert!(!links(
            &[("TERM_PROGRAM", "tmux"), ("TERM_PROGRAM_VERSION", "3.3a")],
            When::Auto
        ));
        // In tmux the outer terminal does not matter: tmux decides.
        assert!(!links(
            &[("TMUX", "x"), ("LC_TERMINAL", "iTerm2")],
            When::Auto
        ));
        assert!(links(&[("LC_TERMINAL", "iTerm2")], When::Auto));
        assert!(links(&[("VTE_VERSION", "7600")], When::Auto));
        assert!(!links(&[("VTE_VERSION", "4600")], When::Auto));
        assert!(links(&[("KONSOLE_VERSION", "221201")], When::Auto));
        assert!(!links(&[("TERM", "xterm-256color")], When::Auto));
        assert!(links(&[("TERM", "xterm-256color")], When::Always));
        assert!(!links(&[("LC_TERMINAL", "iTerm2")], When::Never));
        // No escapes, no links, whatever was asked.
        let env = Env::from_pairs(&[("LC_TERMINAL", "iTerm2")]);
        assert!(!decide(&env, false, ColorChoice::Auto, When::Always).hyperlinks);
        assert!(!decide(&env, true, ColorChoice::Never, When::Always).hyperlinks);
    }

    #[test]
    fn styled_underline_decisions() {
        let ul = |pairs: &[(&str, &str)]| {
            decide(&Env::from_pairs(pairs), true, ColorChoice::Auto, When::Auto).styled_underline
        };
        assert!(ul(&[("TMUX", "x")]));
        assert!(ul(&[("TERM_PROGRAM", "WezTerm")]));
        assert!(ul(&[("TERM", "xterm-kitty")]));
        assert!(ul(&[("TERM", "foot-extra")]));
        assert!(ul(&[("VTE_VERSION", "6003")]));
        assert!(!ul(&[("VTE_VERSION", "5000")]));
        assert!(!ul(&[("TERM", "xterm-256color")]));
        // Attributes-only output keeps curly underlines.
        assert!(ul(&[("NO_COLOR", "1"), ("TERM_PROGRAM", "ghostty")]));
    }
}
