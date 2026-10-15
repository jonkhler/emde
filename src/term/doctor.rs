//! `emde --doctor[=json]`: every terminal decision with its reason.
//!
//! [`report`] prints a few labelled lines, each a row of decisions with the
//! reason in parentheses or after a dash, followed by warnings (what keeps
//! emde from showing everything, and why: [`warnings`]) and tips. For the user's
//! iTerm2 → SSH → tmux 3.4 session it reads:
//!
//! ```text
//!  terminal  tmux 3.4 → iTerm2 3.6.9, 214×54, SSH   colour truecolor (in tmux)   background dark (tmux OSC 11)
//!  links     OSC 8 ✓   underline curly ✓   images blocks(half) — kitty/iterm ✗ passthrough off · sixel ✗ client cell 0x0
//!  probe     tmux answered in 2 ms   cell size unknown (client cell 0x0)
//!  warn      images are block characters: tmux's allow-passthrough is off
//!  tip       `set -g allow-passthrough on` → kitty Unicode placeholders via iTerm2: real pixels that scroll with the text
//! ```
//!
//! [`json`] gives the same decisions, the raw tmux and probe answers and the
//! environment snapshot as JSON (written by hand; `SSH_CONNECTION` is
//! redacted). Text reported by terminals was stripped of control characters
//! when it was parsed, environment text shown in the report is stripped
//! here, and JSON strings are escaped, so the output is safe to print.

use std::time::Duration;

use super::caps::{
    Emulator, Identity, IdentitySource, Version, clean, glyph_name, tmux_client_depth,
};
use super::env::Env;
use super::probe::{LATE_REPLY_GRACE, ProbeOutcome, ProbeReplies, ProbeStatus};
use super::tmux::TmuxInfo;
use super::{Caps, ColorDepth, Graphics, topic};
use crate::color::is_dark;
use crate::style::Rgb;

/// Width of the label column.
const LABEL_WIDTH: usize = 9;
/// Space between the decisions on one line.
const GAP: &str = "   ";

/// The human-readable report.
///
/// `tmux` and `probe` are the query and probe results that fed
/// [`decide`](super::caps::decide), when they ran.
pub fn report(
    env: &Env,
    caps: &Caps,
    tmux: Option<&TmuxInfo>,
    probe: Option<&ProbeOutcome>,
) -> String {
    let mut out = String::new();
    push_line(
        &mut out,
        "terminal",
        &[
            terminal_summary(env, caps, tmux),
            colour_summary(caps),
            background_summary(caps),
        ],
    );
    push_line(
        &mut out,
        "links",
        &[
            links_summary(caps),
            underline_summary(caps),
            images_summary(caps),
        ],
    );
    push_line(
        &mut out,
        "probe",
        &[probe_summary(env, caps, probe), cell_summary(caps)],
    );
    for warning in warnings(caps, tmux) {
        push_line(&mut out, "warn", &[warning]);
    }
    for (i, tip) in tips(env, caps, tmux).iter().enumerate() {
        let label = if i == 0 { "tip" } else { "" };
        push_line(&mut out, label, std::slice::from_ref(tip));
    }
    out
}

/// What keeps emde from showing everything, with the likely cause and the
/// fix. All come from tmux's report on its client (so only inside tmux,
/// after the query):
///
/// * a client without `RGB`: colours (and block images, dithered) are
///   rounded to 256 or 16;
/// * images drawn with blocks because the client has no way to show pixels
///   (no `RGB` for kitty placeholders, no `sixel` with a known cell size) or
///   because `allow-passthrough` is off (the tip then says what to set);
/// * no OSC 8 links through tmux.
///
/// mosh carries only text and gives tmux no `RGB`, no XTVERSION answer and
/// no cell size: a client with no `client_termtype` and no `RGB` is
/// *likely* mosh (a local terminal without `COLORTERM` looks the same, so
/// the warnings never say for sure).
pub fn warnings(caps: &Caps, tmux: Option<&TmuxInfo>) -> Vec<String> {
    let mut warnings = Vec::new();
    let Some(t) = tmux.filter(|t| caps.in_tmux && !t.features.is_empty()) else {
        return warnings;
    };
    let term = if t.client_termname.is_empty() {
        "xterm-256color"
    } else {
        t.client_termname.as_str()
    };
    let mosh = t.client_termtype.is_empty() && !t.has_feature("RGB");
    if let Some((depth, _)) = tmux_client_depth(t) {
        let likely = if mosh {
            "likely mosh or an older terminal".to_string()
        } else {
            format!("{} without 24-bit colour", t.client_termtype)
        };
        warnings.push(format!(
            "tmux's client reports no 24-bit colour: images and colours are rounded to {} \
             ({likely}); for full colour use SSH, or `set -as terminal-features \
             ',{term}:RGB'` if your terminal supports it",
            colour_count(depth)
        ));
    }
    let blocks_chosen =
        reason(caps, topic::GRAPHICS).is_some_and(|r| r.starts_with("images = blocks"));
    if caps.graphics == Graphics::Blocks && !blocks_chosen {
        let outer = Identity::from_xtversion(&t.client_termtype, IdentitySource::ClientTermtype);
        let placeholders =
            t.has_feature("RGB") && outer.as_ref().is_some_and(Identity::has_placeholders);
        let sixel = t.has_feature("sixel") && t.cell.is_some();
        if placeholders && !t.passthrough.enabled() {
            warnings.push("images are block characters: tmux's allow-passthrough is off".into());
        } else if !placeholders && !sixel {
            let mut missing = Vec::new();
            if !t.has_feature("RGB") {
                missing.push("no RGB for kitty placeholders".to_string());
            } else if let Some(id) = &outer {
                missing.push(format!("{} has no kitty placeholders", id.emulator.name()));
            } else {
                missing.push("the terminal is unknown".to_string());
            }
            if !t.has_feature("sixel") {
                missing.push("no sixel".into());
            } else {
                missing.push("client cell size 0x0".into());
            }
            let advice = if mosh {
                "; likely mosh, which carries text only: use SSH for pixels"
            } else {
                ""
            };
            warnings.push(format!(
                "images are block characters: tmux's client has no way to show pixels ({}){advice}",
                missing.join(", ")
            ));
        }
    }
    if !t.has_feature("hyperlinks") {
        let version = Version::parse(&t.version);
        warnings.push(if version.is_known() && !version.at_least(&[3, 4]) {
            format!(
                "links are not clickable: tmux {} drops OSC 8 hyperlinks (they need tmux ≥ 3.4)",
                t.version
            )
        } else {
            let via = if mosh { " (likely mosh, which drops them)" } else { "" };
            format!(
                "links are not clickable: tmux's client has no hyperlinks feature{via}; \
                 `set -as terminal-features ',{term}:hyperlinks'` if your terminal shows OSC 8 links"
            )
        });
    }
    warnings
}

/// The status bar note for block images drawn at reduced depth because of
/// the tmux client (shown once by the pager, the first time one is on
/// screen), e.g. `images: 256 colours (tmux client without RGB) — see emde
/// --doctor`.
pub fn image_colour_note(caps: &Caps, tmux: Option<&TmuxInfo>) -> Option<String> {
    let t = tmux.filter(|_| caps.in_tmux)?;
    let (depth, _) = tmux_client_depth(t)?;
    Some(format!(
        "images: {} colours (tmux client without RGB) — see emde --doctor",
        colour_count(depth)
    ))
}

fn colour_count(depth: ColorDepth) -> &'static str {
    match depth {
        ColorDepth::Ansi16 => "16",
        _ => "256",
    }
}

/// Advice for getting more out of this terminal.
///
/// * Inside tmux with `allow-passthrough` off: `set -g allow-passthrough on`
///   (kitty placeholders are the best image path through tmux), unless the
///   client has no `RGB`, which placeholders need.
/// * VS Code showing block images: `terminal.integrated.enableImages`.
pub fn tips(env: &Env, caps: &Caps, tmux: Option<&TmuxInfo>) -> Vec<String> {
    let mut tips = Vec::new();
    // Placeholders carry the image id in a 24-bit colour: without `RGB`
    // (mosh, say) passthrough would not help.
    let rgb = |t: &TmuxInfo| t.features.is_empty() || t.has_feature("RGB");
    if caps.in_tmux && tmux.is_some_and(|t| !t.passthrough.enabled() && rgb(t)) {
        let outer = caps
            .terminal
            .as_deref()
            .and_then(|t| Identity::from_xtversion(t, IdentitySource::ClientTermtype));
        tips.push(match outer {
            Some(id) if id.has_placeholders() => format!(
                "`set -g allow-passthrough on` → kitty Unicode placeholders via {}: \
                 real pixels that scroll with the text",
                id.emulator.name()
            ),
            _ => "`set -g allow-passthrough on` → kitty Unicode placeholders if the outer \
                  terminal has them (kitty ≥ 0.28, Ghostty, iTerm2 ≥ 3.6)"
                .to_string(),
        });
    }
    let vscode = env
        .non_empty("TERM_PROGRAM")
        .is_some_and(|p| p.eq_ignore_ascii_case("vscode"))
        || env.is_set("VSCODE_INJECTION");
    if vscode && !caps.in_tmux && caps.graphics == Graphics::Blocks {
        tips.push(
            "VS Code: enable `terminal.integrated.enableImages` for real pixels \
             (kitty graphics), then run `emde --doctor --reprobe`"
                .to_string(),
        );
    }
    tips
}

/// The same decisions as a JSON document.
pub fn json(
    env: &Env,
    caps: &Caps,
    tmux: Option<&TmuxInfo>,
    probe: Option<&ProbeOutcome>,
) -> String {
    let reasons = caps
        .reasons
        .iter()
        .map(|r| {
            Json::obj([
                ("topic", Json::str(r.topic)),
                ("detail", Json::str(&r.detail)),
            ])
        })
        .collect();
    let env_vars = env
        .iter()
        .map(|(k, v)| {
            let v = if k == "SSH_CONNECTION" { "(set)" } else { v };
            (k.to_string(), Json::str(v))
        })
        .collect();
    let doc = Json::obj([
        (
            "terminal",
            Json::obj([
                ("identity", Json::opt_str(caps.terminal.as_deref())),
                ("tty", Json::Bool(caps.is_tty)),
                ("tmux", Json::Bool(caps.in_tmux)),
                ("ssh", Json::Bool(caps.over_ssh)),
                ("size", Json::opt_pair(caps.size)),
                ("cell_px", Json::opt_pair(caps.cell_px)),
            ]),
        ),
        ("color", Json::str(color_id(caps.color))),
        ("hyperlinks", Json::Bool(caps.hyperlinks)),
        ("styled_underline", Json::Bool(caps.styled_underline)),
        ("sync", Json::Bool(caps.sync)),
        (
            "background",
            caps.background.map_or(Json::Null, |bg| {
                let variant = if is_dark(bg) { "dark" } else { "light" };
                Json::obj([("rgb", Json::Str(hex(bg))), ("variant", Json::str(variant))])
            }),
        ),
        ("graphics", Json::str(graphics_id(caps.graphics))),
        ("block_glyphs", Json::str(glyph_name(caps.block_glyphs))),
        ("tmux", tmux.map_or(Json::Null, tmux_json)),
        ("probe", probe.map_or(Json::Null, probe_json)),
        ("reasons", Json::Arr(reasons)),
        (
            "warnings",
            Json::Arr(warnings(caps, tmux).into_iter().map(Json::Str).collect()),
        ),
        (
            "tips",
            Json::Arr(tips(env, caps, tmux).into_iter().map(Json::Str).collect()),
        ),
        ("env", Json::Obj(env_vars)),
    ]);
    let mut out = String::new();
    doc.write(&mut out, 0);
    out.push('\n');
    out
}

// --- text report -------------------------------------------------------------

fn push_line(out: &mut String, label: &str, groups: &[String]) {
    out.push(' ');
    out.push_str(label);
    let pad = LABEL_WIDTH.saturating_sub(label.chars().count()) + 1;
    out.extend(std::iter::repeat_n(' ', pad));
    let groups: Vec<&str> = groups
        .iter()
        .map(String::as_str)
        .filter(|g| !g.is_empty())
        .collect();
    out.push_str(&groups.join(GAP));
    out.push('\n');
}

/// The latest reason recorded for `topic`.
fn reason<'c>(caps: &'c Caps, topic: &str) -> Option<&'c str> {
    caps.reasons
        .iter()
        .rev()
        .find(|r| r.topic == topic)
        .map(|r| r.detail.as_str())
        .filter(|d| !d.is_empty())
}

fn with_reason(text: String, why: Option<&str>) -> String {
    match why {
        Some(why) => format!("{text} ({why})"),
        None => text,
    }
}

fn mark(ok: bool) -> &'static str {
    if ok { "✓" } else { "✗" }
}

/// `tmux 3.4 → iTerm2 3.6.9, 214×54, SSH`
fn terminal_summary(env: &Env, caps: &Caps, tmux: Option<&TmuxInfo>) -> String {
    let mut text = if caps.in_tmux {
        let version = tmux.map(|t| t.version.clone()).or_else(|| {
            env.get("TERM_PROGRAM")
                .filter(|p| p.eq_ignore_ascii_case("tmux"))
                .and(env.non_empty("TERM_PROGRAM_VERSION"))
                .map(clean)
        });
        let outer = caps
            .terminal
            .as_deref()
            .or_else(|| {
                tmux.map(|t| t.client_termname.as_str())
                    .filter(|n| !n.is_empty())
            })
            .unwrap_or("unknown terminal");
        match version.filter(|v| !v.is_empty()) {
            Some(v) => format!("tmux {v} → {outer}"),
            None => format!("tmux → {outer}"),
        }
    } else {
        match caps.terminal.as_deref() {
            Some(reported) => with_host(env, reported),
            None => "unknown terminal".to_string(),
        }
    };
    if let Some((cols, rows)) = caps.size {
        text.push_str(&format!(", {cols}×{rows}"));
    }
    if caps.over_ssh {
        text.push_str(", SSH");
    }
    text
}

/// xterm.js is always embedded in some application: name it when
/// `TERM_PROGRAM` does (`xterm.js(6.0.0) in VS Code 1.105.0`).
fn with_host(env: &Env, reported: &str) -> String {
    let embedded = Identity::from_xtversion(reported, IdentitySource::Xtversion)
        .is_some_and(|id| id.emulator == Emulator::XtermJs);
    let host =
        Identity::from_env(env).filter(|id| id.source == IdentitySource::Env("TERM_PROGRAM"));
    match host {
        Some(host) if embedded => format!("{reported} in {}", host.text),
        _ => reported.to_string(),
    }
}

fn colour_summary(caps: &Caps) -> String {
    let name = match caps.color {
        ColorDepth::TrueColor => "truecolor",
        ColorDepth::Ansi256 => "256 colours",
        ColorDepth::Ansi16 => "16 colours",
        ColorDepth::Mono => "mono",
        ColorDepth::None => "off",
    };
    with_reason(format!("colour {name}"), reason(caps, topic::COLOR))
}

fn background_summary(caps: &Caps) -> String {
    let name = match caps.background {
        Some(bg) if is_dark(bg) => "dark",
        Some(_) => "light",
        None => "unknown",
    };
    with_reason(
        format!("background {name}"),
        reason(caps, topic::BACKGROUND),
    )
}

fn links_summary(caps: &Caps) -> String {
    let text = format!("OSC 8 {}", mark(caps.hyperlinks));
    if caps.hyperlinks {
        text
    } else {
        with_reason(text, reason(caps, topic::HYPERLINKS))
    }
}

fn underline_summary(caps: &Caps) -> String {
    let text = format!("underline curly {}", mark(caps.styled_underline));
    if caps.styled_underline {
        text
    } else {
        with_reason(text, reason(caps, topic::UNDERLINE))
    }
}

fn images_summary(caps: &Caps) -> String {
    let label = match caps.graphics {
        Graphics::None => "none".to_string(),
        Graphics::Blocks => format!("blocks({})", glyph_name(caps.block_glyphs)),
        Graphics::KittyPlaceholders => "kitty placeholders".to_string(),
        Graphics::KittyClassic => "kitty".to_string(),
        Graphics::Iterm => "iTerm2 (OSC 1337)".to_string(),
        Graphics::Sixel => "sixel".to_string(),
    };
    match reason(caps, topic::GRAPHICS) {
        Some(why) => format!("images {label} — {why}"),
        None => format!("images {label}"),
    }
}

fn probe_summary(env: &Env, caps: &Caps, probe: Option<&ProbeOutcome>) -> String {
    let Some(outcome) = probe else {
        let why = if !caps.is_tty {
            "skipped (stdout is not a terminal)"
        } else if env.get("TERM") == Some("dumb") {
            "skipped (TERM=dumb)"
        } else {
            "not run"
        };
        return why.to_string();
    };
    match &outcome.status {
        ProbeStatus::Complete => {
            let who = match (caps.in_tmux, outcome.wrapped) {
                (true, true) => {
                    let outer = caps.terminal.as_deref().unwrap_or("the outer terminal");
                    format!("tmux and {outer} answered")
                }
                (true, false) => "tmux answered".to_string(),
                (false, _) => "answered".to_string(),
            };
            format!("{who} in {}", millis(outcome.elapsed))
        }
        ProbeStatus::TimedOut => format!(
            "timed out after {} (late replies are dropped for {} s)",
            millis(outcome.elapsed),
            LATE_REPLY_GRACE.as_secs()
        ),
        ProbeStatus::Cached { age } => {
            format!("cached {} ago (--reprobe refreshes it)", age_text(*age))
        }
        ProbeStatus::Failed(error) => format!("failed: {error}"),
    }
}

fn cell_summary(caps: &Caps) -> String {
    let text = match caps.cell_px {
        Some((w, h)) => format!("cell {w}×{h} px"),
        None => "cell size unknown".to_string(),
    };
    with_reason(text, reason(caps, topic::CELL))
}

fn millis(d: Duration) -> String {
    match d.as_millis() {
        0 => "<1 ms".to_string(),
        ms => format!("{ms} ms"),
    }
}

fn age_text(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        _ => format!("{} h", secs / 3600),
    }
}

// --- JSON ----------------------------------------------------------------------

fn color_id(depth: ColorDepth) -> &'static str {
    match depth {
        ColorDepth::None => "none",
        ColorDepth::Mono => "mono",
        ColorDepth::Ansi16 => "16",
        ColorDepth::Ansi256 => "256",
        ColorDepth::TrueColor => "truecolor",
    }
}

fn graphics_id(graphics: Graphics) -> &'static str {
    match graphics {
        Graphics::None => "none",
        Graphics::Blocks => "blocks",
        Graphics::KittyPlaceholders => "kitty-placeholders",
        Graphics::KittyClassic => "kitty",
        Graphics::Iterm => "iterm",
        Graphics::Sixel => "sixel",
    }
}

fn hex(Rgb(r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn tmux_json(t: &TmuxInfo) -> Json {
    Json::obj([
        ("version", Json::str(&t.version)),
        ("client_termtype", Json::str(&t.client_termtype)),
        ("client_termname", Json::str(&t.client_termname)),
        (
            "features",
            Json::Arr(t.features.iter().map(Json::str).collect()),
        ),
        ("cell", Json::opt_pair(t.cell)),
        ("passthrough", Json::str(t.passthrough.as_str())),
        ("set_clipboard", Json::str(&t.set_clipboard)),
    ])
}

fn probe_json(outcome: &ProbeOutcome) -> Json {
    let mut fields = vec![
        (
            "status".to_string(),
            Json::str(match outcome.status {
                ProbeStatus::Complete => "complete",
                ProbeStatus::TimedOut => "timed-out",
                ProbeStatus::Cached { .. } => "cached",
                ProbeStatus::Failed(_) => "failed",
            }),
        ),
        (
            "elapsed_us".to_string(),
            Json::Num(u64::try_from(outcome.elapsed.as_micros()).unwrap_or(u64::MAX)),
        ),
        ("wrapped".to_string(), Json::Bool(outcome.wrapped)),
    ];
    match &outcome.status {
        ProbeStatus::Cached { age } => fields.push(("age_s".into(), Json::Num(age.as_secs()))),
        ProbeStatus::Failed(error) => fields.push(("error".into(), Json::str(error))),
        ProbeStatus::Complete | ProbeStatus::TimedOut => {}
    }
    fields.push(("replies".into(), replies_json(&outcome.replies)));
    Json::Obj(fields)
}

fn replies_json(r: &ProbeReplies) -> Json {
    Json::obj([
        ("kitty_ok", Json::Bool(r.kitty_ok)),
        ("outer_kitty_ok", Json::Bool(r.outer_kitty_ok)),
        ("xtversion", Json::opt_str(r.xtversion.as_deref())),
        ("cell_px", Json::opt_pair(r.cell_px)),
        ("text_area_px", Json::opt_pair(r.text_area_px)),
        ("size_cells", Json::opt_pair(r.size_cells)),
        (
            "background",
            r.background.map_or(Json::Null, |bg| Json::Str(hex(bg))),
        ),
        (
            "da1",
            r.da1.as_ref().map_or(Json::Null, |attrs| {
                Json::Arr(attrs.iter().map(|&a| Json::Num(u64::from(a))).collect())
            }),
        ),
        ("outer_dsr_seen", Json::Bool(r.outer_dsr_seen)),
        ("tmux_own_da1_sixel", Json::Bool(r.tmux_own_da1_sixel)),
    ])
}

/// Just enough JSON for `--doctor=json`, pretty-printed with two-space
/// indentation; arrays of scalars stay on one line.
#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(u64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn str(s: impl AsRef<str>) -> Json {
        Json::Str(s.as_ref().to_string())
    }

    fn opt_str(s: Option<&str>) -> Json {
        s.map_or(Json::Null, Json::str)
    }

    fn opt_pair(pair: Option<(u16, u16)>) -> Json {
        pair.map_or(Json::Null, |(a, b)| {
            Json::Arr(vec![Json::Num(u64::from(a)), Json::Num(u64::from(b))])
        })
    }

    fn obj<const N: usize>(fields: [(&str, Json); N]) -> Json {
        Json::Obj(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    fn is_scalar(&self) -> bool {
        !matches!(self, Json::Arr(_) | Json::Obj(_))
    }

    fn write(&self, out: &mut String, indent: usize) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => out.push_str(&n.to_string()),
            Json::Str(s) => write_string(out, s),
            Json::Arr(items) if items.is_empty() => out.push_str("[]"),
            Json::Arr(items) if items.iter().all(Json::is_scalar) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write(out, indent);
                }
                out.push(']');
            }
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    out.push_str(if i > 0 { ",\n" } else { "\n" });
                    push_indent(out, indent + 1);
                    item.write(out, indent + 1);
                }
                out.push('\n');
                push_indent(out, indent);
                out.push(']');
            }
            Json::Obj(fields) if fields.is_empty() => out.push_str("{}"),
            Json::Obj(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    out.push_str(if i > 0 { ",\n" } else { "\n" });
                    push_indent(out, indent + 1);
                    write_string(out, key);
                    out.push_str(": ");
                    value.write(out, indent + 1);
                }
                out.push('\n');
                push_indent(out, indent);
                out.push('}');
            }
        }
    }
}

fn push_indent(out: &mut String, level: usize) {
    out.extend(std::iter::repeat_n(' ', 2 * level));
}

/// A JSON string literal. Every control character is escaped, so the JSON
/// is safe to print to a terminal as well as valid.
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::options::ImageOptions;
    use crate::term::Reason;
    use crate::term::caps::decide;
    use crate::term::probe::fixtures::{self, ITERM2_VIA_TMUX, KITTY, USER_SESSION};
    use crate::term::tmux::{self, fixtures as tmux_lines};

    const USER_ENV: &[(&str, &str)] = &[
        ("TERM", "tmux-256color"),
        ("TERM_PROGRAM", "tmux"),
        ("TERM_PROGRAM_VERSION", "3.4"),
        ("TMUX", "/tmp/tmux-1001/default,443340,2"),
        ("LC_TERMINAL", "iTerm2"),
        ("LC_TERMINAL_VERSION", "3.6.9"),
        ("SSH_CONNECTION", "192.0.2.10 50000 192.0.2.20 22"),
        ("COLUMNS", "214"),
    ];

    /// What `term::color` decides for the user's session (plan §6: `$TMUX`
    /// gives truecolor and styled underlines, OSC 8 with tmux ≥ 3.4).
    fn tmux_base() -> Caps {
        Caps {
            reasons: vec![
                Reason {
                    topic: topic::COLOR,
                    detail: "in tmux".into(),
                },
                Reason {
                    topic: topic::HYPERLINKS,
                    detail: "tmux ≥ 3.4".into(),
                },
            ],
            ..Caps::full()
        }
    }

    /// Everything `--doctor` would compute for one scenario.
    struct Scenario {
        env: Env,
        caps: Caps,
        tmux: Option<TmuxInfo>,
        probe: Option<ProbeOutcome>,
    }

    impl Scenario {
        fn new(
            env: &[(&str, &str)],
            base: Caps,
            tmux_line: Option<&str>,
            replies: Option<(&[u8], bool)>,
            elapsed_ms: u64,
        ) -> Scenario {
            let env = Env::from_pairs(env);
            let tmux = tmux_line.and_then(tmux::parse);
            let in_tmux = env.is_set("TMUX");
            let probe = replies.map(|(bytes, wrapped)| ProbeOutcome {
                replies: fixtures::replies(bytes, in_tmux, wrapped),
                status: ProbeStatus::Complete,
                elapsed: Duration::from_millis(elapsed_ms),
                wrapped,
            });
            let caps = decide(
                &env,
                base,
                tmux.as_ref(),
                probe.as_ref().and_then(ProbeOutcome::answers),
                &ImageOptions::default(),
            );
            Scenario {
                env,
                caps,
                tmux,
                probe,
            }
        }

        fn users_session() -> Scenario {
            Scenario::new(
                USER_ENV,
                tmux_base(),
                Some(tmux_lines::USER_SESSION),
                Some((USER_SESSION, false)),
                2,
            )
        }

        fn report(&self) -> String {
            report(
                &self.env,
                &self.caps,
                self.tmux.as_ref(),
                self.probe.as_ref(),
            )
        }

        fn tips(&self) -> Vec<String> {
            tips(&self.env, &self.caps, self.tmux.as_ref())
        }
    }

    /// The user's tmux session attached through mosh: no `LC_TERMINAL`
    /// (mosh does not forward it), and what `term::color` decides there.
    const MOSH_ENV: &[(&str, &str)] = &[
        ("TERM", "tmux-256color"),
        ("TERM_PROGRAM", "tmux"),
        ("TERM_PROGRAM_VERSION", "3.4"),
        ("TMUX", "/tmp/tmux-1001/default,443340,2"),
        ("SSH_CONNECTION", "192.0.2.10 50000 192.0.2.20 22"),
        ("COLUMNS", "214"),
    ];

    impl Scenario {
        /// `env` with `term::color`'s own decision as the base.
        fn detected(env: &[(&str, &str)], tmux_line: &str) -> Scenario {
            let base = crate::term::color::decide(
                &Env::from_pairs(env),
                true,
                crate::term::color::ColorChoice::Auto,
                crate::options::When::Auto,
            );
            let base = Caps {
                size: Some((214, 54)),
                ..base
            };
            Scenario::new(env, base, Some(tmux_line), Some((USER_SESSION, false)), 2)
        }

        fn mosh() -> Scenario {
            Scenario::detected(MOSH_ENV, tmux_lines::MOSH_CLIENT)
        }

        fn warnings(&self) -> Vec<String> {
            warnings(&self.caps, self.tmux.as_ref())
        }
    }

    #[test]
    fn mosh_warnings() {
        let s = Scenario::mosh();
        assert_eq!(s.caps.color, ColorDepth::Ansi256);
        let w = s.warnings();
        assert_eq!(
            w.len(),
            if cfg!(feature = "images") { 3 } else { 2 },
            "{w:?}"
        );
        assert_eq!(
            w[0],
            "tmux's client reports no 24-bit colour: images and colours are rounded to 256 \
             (likely mosh or an older terminal); for full colour use SSH, or `set -as \
             terminal-features ',xterm-256color:RGB'` if your terminal supports it"
        );
        if cfg!(feature = "images") {
            let blocks = "images are block characters: tmux's client has no way to show pixels \
                          (no RGB for kitty placeholders, no sixel); likely mosh, which carries \
                          text only: use SSH for pixels";
            assert_eq!(w[1], blocks);
        }
        assert_eq!(
            w.last().map(String::as_str),
            Some(
                "links are not clickable: tmux's client has no hyperlinks feature (likely \
                 mosh, which drops them); `set -as terminal-features ',xterm-256color:hyperlinks'` \
                 if your terminal shows OSC 8 links"
            )
        );
        assert_eq!(
            image_colour_note(&s.caps, s.tmux.as_ref()).as_deref(),
            Some("images: 256 colours (tmux client without RGB) — see emde --doctor")
        );
    }

    #[test]
    fn warnings_only_for_what_the_client_lacks() {
        // iTerm2 with passthrough on: kitty placeholders, everything shown.
        let on = Scenario::new(
            USER_ENV,
            tmux_base(),
            Some(tmux_lines::USER_SESSION_PASSTHROUGH),
            Some((ITERM2_VIA_TMUX, true)),
            41,
        );
        assert!(on.warnings().is_empty(), "{:?}", on.warnings());
        assert_eq!(image_colour_note(&on.caps, on.tmux.as_ref()), None);
        // Outside tmux nothing is known about a client.
        let direct = Scenario::new(
            &[("TERM", "xterm-256color")],
            Caps::full(),
            Some(tmux_lines::MOSH_CLIENT),
            None,
            0,
        );
        assert!(direct.warnings().is_empty());
        assert_eq!(image_colour_note(&direct.caps, direct.tmux.as_ref()), None);
        // tmux before 3.2 reports no features: no guesses.
        let old = Scenario::detected(MOSH_ENV, "3.1c||xterm-256color||0x0||external");
        assert!(old.warnings().is_empty());
        // A 16-colour client that named itself; tmux 3.3 drops links.
        let s = Scenario::detected(MOSH_ENV, "3.3a|SomeTerm 1.0|xterm|title|0x0|off|external");
        let w = s.warnings();
        assert!(
            w[0].contains("rounded to 16 (SomeTerm 1.0 without 24-bit colour)")
                && w[0].contains("',xterm:RGB'"),
            "{w:?}"
        );
        assert_eq!(
            w.last().map(String::as_str),
            Some(
                "links are not clickable: tmux 3.3a drops OSC 8 hyperlinks (they need tmux ≥ 3.4)"
            )
        );
        assert_eq!(
            image_colour_note(&s.caps, s.tmux.as_ref()).as_deref(),
            Some("images: 16 colours (tmux client without RGB) — see emde --doctor")
        );
    }

    #[cfg(feature = "images")]
    #[test]
    fn block_image_warnings() {
        // VS Code's xterm.js: RGB, but no placeholders and no cell size.
        let s = Scenario::detected(MOSH_ENV, tmux_lines::VSCODE_CLIENT);
        let w = s.warnings();
        assert_eq!(
            w,
            [
                "images are block characters: tmux's client has no way to show pixels (xterm.js \
              has no kitty placeholders, client cell size 0x0)"
            ],
        );
        // Blocks the user asked for are no warning.
        let base = crate::term::color::decide(
            &Env::from_pairs(MOSH_ENV),
            true,
            crate::term::color::ColorChoice::Auto,
            crate::options::When::Auto,
        );
        let env = Env::from_pairs(MOSH_ENV);
        let tmux = tmux::parse(tmux_lines::MOSH_CLIENT);
        let opts = ImageOptions {
            mode: crate::options::ImageMode::Blocks,
            ..ImageOptions::default()
        };
        let caps = decide(&env, base, tmux.as_ref(), None, &opts);
        assert_eq!(caps.graphics, Graphics::Blocks);
        let w = warnings(&caps, tmux.as_ref());
        assert!(w.iter().all(|w| !w.starts_with("images are")), "{w:?}");
        assert_eq!(w.len(), 2);
    }

    #[test]
    fn passthrough_tip_only_in_tmux_with_passthrough_off() {
        assert_eq!(Scenario::users_session().tips().len(), 1);
        let on = Scenario::new(
            USER_ENV,
            tmux_base(),
            Some(tmux_lines::USER_SESSION_PASSTHROUGH),
            Some((ITERM2_VIA_TMUX, true)),
            41,
        );
        assert!(on.tips().is_empty());
        let direct = Scenario::new(
            &[("TERM", "xterm-kitty")],
            Caps::full(),
            None,
            Some((KITTY, false)),
            3,
        );
        assert!(direct.tips().is_empty());
        // An outer terminal without placeholders gets the careful wording.
        let vscode = Scenario::new(
            &[("TMUX", "/tmp/tmux-1001/default,1,0")],
            tmux_base(),
            Some("3.4|xterm.js(6.0.0)|xterm-256color|256,RGB|0x0|off|external"),
            Some((USER_SESSION, false)),
            2,
        );
        let tips = vscode.tips();
        assert_eq!(tips.len(), 1);
        assert!(
            tips[0].contains("if the outer terminal has them"),
            "{tips:?}"
        );
    }

    #[test]
    fn probe_lines() {
        let mut s = Scenario::users_session();
        let line = |s: &Scenario| s.report().lines().nth(2).unwrap_or_default().to_string();
        let outcome = s.probe.clone().unwrap();
        s.probe = Some(ProbeOutcome {
            status: ProbeStatus::TimedOut,
            elapsed: Duration::from_millis(150),
            ..outcome.clone()
        });
        assert!(line(&s).contains("timed out after 150 ms (late replies are dropped for 2 s)"));
        s.probe = Some(ProbeOutcome {
            status: ProbeStatus::Cached {
                age: Duration::from_secs(7300),
            },
            ..outcome.clone()
        });
        assert!(line(&s).contains("cached 2 h ago (--reprobe refreshes it)"));
        s.probe = Some(ProbeOutcome {
            status: ProbeStatus::Failed("No such device or address".into()),
            ..outcome.clone()
        });
        assert!(line(&s).contains("probe     failed: No such device or address"));
        s.probe = Some(ProbeOutcome {
            elapsed: Duration::from_micros(300),
            ..outcome
        });
        assert!(line(&s).contains("tmux answered in <1 ms"));
        s.probe = None;
        assert!(line(&s).contains("probe     not run"));
        s.env = Env::from_pairs(&[("TERM", "dumb")]);
        assert!(line(&s).contains("probe     skipped (TERM=dumb)"));
        s.caps.is_tty = false;
        assert!(line(&s).contains("probe     skipped (stdout is not a terminal)"));
    }

    #[test]
    fn environment_text_is_made_printable() {
        let env = [
            ("TMUX", "/tmp/tmux-1001/default,1,0"),
            ("TERM_PROGRAM", "tmux"),
            ("TERM_PROGRAM_VERSION", "3.4\u{1b}]0;pwned\u{7}"),
        ];
        let text = Scenario::new(&env, tmux_base(), None, None, 0).report();
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{7}'),
            "{text:?}"
        );
        assert!(text.starts_with(" terminal  tmux 3.4]0;pwned → unknown terminal"));
    }

    #[test]
    fn ages() {
        assert_eq!(age_text(Duration::from_secs(5)), "5 s");
        assert_eq!(age_text(Duration::from_secs(125)), "2 min");
        assert_eq!(age_text(Duration::from_secs(43_199)), "11 h");
    }

    #[test]
    fn unfavourable_decisions_show_their_reasons() {
        let base = Caps {
            color: ColorDepth::Ansi256,
            hyperlinks: false,
            styled_underline: false,
            reasons: vec![
                Reason {
                    topic: topic::HYPERLINKS,
                    detail: "tmux < 3.4".into(),
                },
                Reason {
                    topic: topic::UNDERLINE,
                    detail: "no usstyle".into(),
                },
            ],
            ..Caps::full()
        };
        let s = Scenario::new(&[("TERM", "xterm-256color")], base, None, None, 0);
        let text = s.report();
        assert!(text.contains("colour 256 colours"), "{text}");
        assert!(text.contains("OSC 8 ✗ (tmux < 3.4)"), "{text}");
        assert!(text.contains("underline curly ✗ (no usstyle)"), "{text}");
        assert!(
            text.contains("background unknown (not probed, no COLORFGBG)"),
            "{text}"
        );
    }

    #[test]
    fn json_strings_are_escaped() {
        let mut out = String::new();
        write_string(&mut out, "a\"b\\c\nd\te\u{1b}[31m\u{7f}\u{9b}é→");
        assert_eq!(out, "\"a\\\"b\\\\c\\nd\\te\\u001b[31m\\u007f\\u009bé→\"");
        let doc = Json::obj([
            ("empty_list", Json::Arr(vec![])),
            ("empty_map", Json::Obj(vec![])),
            ("nested", Json::Arr(vec![Json::obj([("x", Json::Null)])])),
            ("flat", Json::Arr(vec![Json::Num(1), Json::Bool(false)])),
        ]);
        let mut out = String::new();
        doc.write(&mut out, 0);
        assert_eq!(
            out,
            "{\n  \"empty_list\": [],\n  \"empty_map\": {},\n  \"nested\": [\n    {\n      \
             \"x\": null\n    }\n  ],\n  \"flat\": [1, false]\n}"
        );
    }

    #[test]
    fn terminal_line_without_tmux_info() {
        let env = [
            ("TMUX", "/tmp/tmux-1001/default,1,0"),
            ("TERM_PROGRAM", "tmux"),
            ("TERM_PROGRAM_VERSION", "3.3a"),
        ];
        let s = Scenario::new(&env, tmux_base(), None, None, 0);
        assert!(
            s.report()
                .starts_with(" terminal  tmux 3.3a → unknown terminal   ")
        );
        let vscode_client = "3.4||xterm-256color|256,RGB|0x0|on|external";
        let s = Scenario::new(&env, tmux_base(), Some(vscode_client), None, 0);
        assert!(
            s.report()
                .starts_with(" terminal  tmux 3.4 → xterm-256color   ")
        );
    }

    /// Reports whose text depends on the image and sixel features.
    #[cfg(all(feature = "images", feature = "sixel"))]
    mod full {
        use super::*;
        use crate::term::probe::fixtures::VSCODE_PLAIN;

        impl Scenario {
            fn json(&self) -> String {
                json(
                    &self.env,
                    &self.caps,
                    self.tmux.as_ref(),
                    self.probe.as_ref(),
                )
            }
        }

        #[test]
        fn users_session_report() {
            insta::assert_snapshot!(Scenario::users_session().report());
        }

        #[test]
        fn users_session_json() {
            insta::assert_snapshot!(Scenario::users_session().json());
        }

        /// The same session attached through mosh (`mosh-server -c 256`).
        #[test]
        fn mosh_report() {
            insta::assert_snapshot!(Scenario::mosh().report());
        }

        #[test]
        fn mosh_json() {
            insta::assert_snapshot!(Scenario::mosh().json());
        }

        #[test]
        fn passthrough_on_report() {
            let s = Scenario::new(
                USER_ENV,
                tmux_base(),
                Some(tmux_lines::USER_SESSION_PASSTHROUGH),
                Some((ITERM2_VIA_TMUX, true)),
                41,
            );
            insta::assert_snapshot!(s.report());
        }

        #[test]
        fn vscode_without_images_report() {
            let env = [
                ("TERM", "xterm-256color"),
                ("TERM_PROGRAM", "vscode"),
                ("TERM_PROGRAM_VERSION", "1.105.0"),
            ];
            let base = Caps {
                size: Some((150, 50)),
                ..Caps::full()
            };
            let s = Scenario::new(&env, base, None, Some((VSCODE_PLAIN, false)), 27);
            insta::assert_snapshot!(s.report());
        }

        #[test]
        fn piped_report() {
            let s = Scenario::new(&[("TERM", "xterm-kitty")], Caps::plain(), None, None, 0);
            insta::assert_snapshot!(s.report());
        }

        #[test]
        fn report_follows_the_plan_example() {
            let text = Scenario::users_session().report();
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 5, "{text}");
            assert!(lines[0].starts_with(" terminal  tmux 3.4 → iTerm2 3.6.9, 214×54, SSH   "));
            assert!(lines[0].contains("colour truecolor (in tmux)"));
            assert!(lines[0].ends_with("background dark (tmux OSC 11)"));
            assert!(
                lines[1]
                    .starts_with(" links     OSC 8 ✓   underline curly ✓   images blocks(half) — ")
            );
            assert!(lines[1].contains("kitty/iterm ✗ passthrough off"));
            assert!(lines[2].starts_with(" probe     tmux answered in 2 ms"));
            assert_eq!(
                lines[3],
                " warn      images are block characters: tmux's allow-passthrough is off"
            );
            assert_eq!(
                lines[4],
                " tip       `set -g allow-passthrough on` → kitty Unicode placeholders via iTerm2: \
                 real pixels that scroll with the text"
            );
        }

        #[test]
        fn vscode_tip() {
            let env = [("TERM_PROGRAM", "vscode"), ("TERM", "xterm-256color")];
            let plain = Scenario::new(&env, Caps::full(), None, Some((VSCODE_PLAIN, false)), 27);
            assert_eq!(plain.caps.graphics, Graphics::Blocks);
            let tips = plain.tips();
            assert_eq!(tips.len(), 1);
            assert!(tips[0].contains("terminal.integrated.enableImages"));
            let with_images = Scenario::new(
                &env,
                Caps::full(),
                None,
                Some((fixtures::VSCODE_IMAGES, false)),
                27,
            );
            assert_eq!(with_images.caps.graphics, Graphics::KittyClassic);
            assert!(with_images.tips().is_empty());
        }

        #[test]
        fn json_is_well_formed() {
            let text = Scenario::users_session().json();
            assert!(text.starts_with("{\n  \"terminal\": {\n"));
            assert!(text.ends_with("}\n"));
            assert!(text.contains("\"graphics\": \"blocks\""));
            assert!(text.contains("\"SSH_CONNECTION\": \"(set)\""));
            assert!(!text.contains("192.0.2.10"), "SSH_CONNECTION is redacted");
            assert!(text.contains("\"cell\": null"));
            assert!(text.contains("\"da1\": [1, 2, 4]"));
            // Balanced brackets outside strings.
            let mut depth = 0i32;
            let mut in_string = false;
            let mut escaped = false;
            for c in text.chars() {
                match (in_string, escaped, c) {
                    (true, true, _) => escaped = false,
                    (true, false, '\\') => escaped = true,
                    (true, false, '"') => in_string = false,
                    (false, _, '"') => in_string = true,
                    (false, _, '{' | '[') => depth += 1,
                    (false, _, '}' | ']') => depth -= 1,
                    _ => {}
                }
                assert!(depth >= 0);
            }
            assert_eq!(depth, 0);
            assert!(!in_string);
        }
    }
}
