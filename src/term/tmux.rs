//! The single `tmux display -p` query and its parser.
//!
//! Inside tmux, the terminal emde writes to is tmux itself: queries sent to
//! the pane are answered by tmux, not by the terminal the user looks at.
//! What emde needs to know about that *outer* terminal comes from one
//! `tmux display-message -p` call (about 3 ms):
//!
//! * the client's terminal type (its XTVERSION answer, e.g. `iTerm2 3.6.9`)
//!   and `$TERM`;
//! * the tmux features tmux enabled for the client (`RGB`, `sixel`,
//!   `hyperlinks`, `usstyle`, …);
//! * the client's cell size in pixels (`0x0` when unknown, in which case tmux
//!   replaces sixel images with a text box);
//! * whether `allow-passthrough` lets wrapped escape sequences reach the
//!   outer terminal, and the `set-clipboard` mode.
//!
//! The query only reads; emde never changes a tmux option.

use std::process::Command;
use std::time::Duration;

use super::env::Env;
use super::process::output_with_deadline;

/// The `tmux display-message -p` format: seven `|`-separated fields.
pub const FORMAT: &str = "#{version}|#{client_termtype}|#{client_termname}|\
                          #{client_termfeatures}|#{client_cell_width}x#{client_cell_height}|\
                          #{allow-passthrough}|#{set-clipboard}";

/// How long the query may take before the `tmux` process is killed.
pub const TIMEOUT: Duration = Duration::from_millis(200);

/// Upper bound on the query output (one short line in practice).
const MAX_OUTPUT: usize = 16 * 1024;

/// tmux's `allow-passthrough` pane option.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Passthrough {
    /// `off` (tmux's default): `ESC Ptmux;` sequences are dropped.
    #[default]
    Off,
    /// `on`: wrapped sequences from visible panes reach the outer terminal.
    On,
    /// `all`: wrapped sequences from every pane reach the outer terminal.
    All,
}

impl Passthrough {
    /// Parse the option value. Anything but `on` or `all` counts as off,
    /// including the empty string that tmux versions without the option print.
    pub fn parse(value: &str) -> Passthrough {
        let value = value.trim();
        if value.eq_ignore_ascii_case("on") {
            Passthrough::On
        } else if value.eq_ignore_ascii_case("all") {
            Passthrough::All
        } else {
            Passthrough::Off
        }
    }

    /// Whether wrapped sequences reach the outer terminal (`on` or `all`).
    pub fn enabled(self) -> bool {
        !matches!(self, Passthrough::Off)
    }

    /// The option value as tmux prints it.
    pub fn as_str(self) -> &'static str {
        match self {
            Passthrough::Off => "off",
            Passthrough::On => "on",
            Passthrough::All => "all",
        }
    }
}

/// What tmux knows about its attached client, plus the relevant options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxInfo {
    /// tmux version (`#{version}`), e.g. `3.4` or `3.3a`.
    pub version: String,
    /// The client terminal's XTVERSION answer (`#{client_termtype}`), e.g.
    /// `iTerm2 3.6.9`; empty when the terminal did not answer.
    pub client_termtype: String,
    /// The client's `$TERM` (`#{client_termname}`), e.g. `xterm-256color`.
    pub client_termname: String,
    /// tmux terminal features of the client (`#{client_termfeatures}`).
    pub features: Vec<String>,
    /// Client cell size in pixels as `(width, height)`; `None` when tmux
    /// reports `0x0` (unknown).
    pub cell: Option<(u16, u16)>,
    /// The `allow-passthrough` option.
    pub passthrough: Passthrough,
    /// The `set-clipboard` option (`on`, `external` or `off`).
    pub set_clipboard: String,
}

impl TmuxInfo {
    /// Whether the client has a tmux terminal feature. Names compare
    /// case-insensitively, as tmux itself does (`RGB`, `sixel`, …).
    pub fn has_feature(&self, name: &str) -> bool {
        self.features.iter().any(|f| f.eq_ignore_ascii_case(name))
    }
}

/// Parse the output of `tmux display-message -p` with [`FORMAT`].
///
/// Returns `None` when there is no usable client: empty output, a line
/// without the seven fields, or empty client fields (tmux prints
/// `3.4||||x|off|external` when no client is attached).
///
/// The client terminal type is free text reported by the terminal and may
/// itself contain `|`, so the fields after it are taken from the right.
pub fn parse(output: &str) -> Option<TmuxInfo> {
    let line = output.lines().map(str::trim).find(|l| !l.is_empty())?;
    let fields: Vec<&str> = line.split('|').collect();
    let n = fields.len();
    if n < 7 {
        return None;
    }
    let [termname, features, cell, passthrough, set_clipboard] = fields.get(n - 5..)? else {
        return None;
    };
    let info = TmuxInfo {
        version: clean(fields.first()?),
        client_termtype: clean(&fields.get(1..n - 5)?.join("|")),
        client_termname: clean(termname),
        features: features
            .split(',')
            .map(clean)
            .filter(|f| !f.is_empty())
            .collect(),
        cell: parse_cell(cell),
        passthrough: Passthrough::parse(passthrough),
        set_clipboard: clean(set_clipboard),
    };
    let no_client = info.client_termname.is_empty()
        && info.client_termtype.is_empty()
        && info.features.is_empty();
    (!no_client).then_some(info)
}

/// Run the query when `$TMUX` is set: spawns `tmux display-message -p`,
/// killing it if it has not finished after [`TIMEOUT`].
///
/// The child gets the snapshot's `$TMUX`, so it asks the server the snapshot
/// describes.
pub fn query(env: &Env) -> Option<TmuxInfo> {
    let socket = env.non_empty("TMUX")?;
    let mut cmd = Command::new("tmux");
    cmd.args(["display-message", "-p", FORMAT])
        .env("TMUX", socket);
    let output = output_with_deadline(cmd, TIMEOUT, MAX_OUTPUT)?;
    parse(&String::from_utf8_lossy(&output))
}

/// `WxH` in pixels; `0x0`, `x` (tmux without the formats) and junk are `None`.
fn parse_cell(s: &str) -> Option<(u16, u16)> {
    let (w, h) = s.trim().split_once(['x', 'X'])?;
    let w: u16 = w.trim().parse().ok()?;
    let h: u16 = h.trim().parse().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

/// Trim a field and drop control characters: terminal-reported text must
/// never inject escape sequences into `--doctor` output.
fn clean(s: &str) -> String {
    s.trim().chars().filter(|c| !c.is_control()).collect()
}

/// `tmux display-message -p` output for the tests of the `term` modules.
#[cfg(test)]
pub(crate) mod fixtures {
    /// Recorded in the user's session: iTerm2 3.6.9 → SSH → tmux 3.4.
    pub(crate) const USER_SESSION: &str = "3.4|iTerm2 3.6.9|xterm-256color|256,bpaste,ccolour,\
        clipboard,hyperlinks,cstyle,extkeys,focus,margins,mouse,osc7,rectfill,RGB,sixel,\
        strikethrough,sync,title,usstyle|0x0|off|external\n";

    /// The user's session after `set -g allow-passthrough on`.
    pub(crate) const USER_SESSION_PASSTHROUGH: &str = "3.4|iTerm2 3.6.9|xterm-256color|256,bpaste,\
        ccolour,clipboard,hyperlinks,cstyle,extkeys,focus,margins,mouse,osc7,rectfill,RGB,sixel,\
        strikethrough,sync,title,usstyle|0x0|on|external\n";

    /// Recorded from a tmux 3.4 server with no attached client.
    pub(crate) const NO_CLIENT: &str = "3.4||||x|off|external\n";

    /// tmux in VS Code (xterm.js answers XTVERSION), passthrough on.
    pub(crate) const VSCODE_CLIENT: &str = "3.4|xterm.js(6.0.0)|xterm-256color|256,RGB,clipboard,hyperlinks,mouse,sixel,sync|0x0|on|external\n";

    /// tmux in foot, which reports its cell size: tmux can draw sixel.
    pub(crate) const FOOT_CLIENT: &str = "3.4|foot(1.20.2)|foot|256,RGB,clipboard,hyperlinks,mouse,sixel,sync,usstyle|10x20|off|external\n";
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn format_has_seven_fields() {
        assert_eq!(FORMAT.split('|').count(), 7);
        assert!(!FORMAT.contains(char::is_whitespace));
        assert!(FORMAT.starts_with("#{version}|#{client_termtype}|"));
    }

    #[test]
    fn fixtures_parse() {
        let parsed = |line: &str| parse(line).unwrap();
        let on = parsed(USER_SESSION_PASSTHROUGH);
        assert_eq!(on.passthrough, Passthrough::On);
        assert_eq!(on.features, parsed(USER_SESSION).features);
        let vscode = parsed(VSCODE_CLIENT);
        assert_eq!(vscode.client_termtype, "xterm.js(6.0.0)");
        assert_eq!(vscode.cell, None);
        let foot = parsed(FOOT_CLIENT);
        assert_eq!(foot.cell, Some((10, 20)));
        assert!(foot.has_feature("sixel") && foot.has_feature("RGB"));
        assert_eq!(foot.passthrough, Passthrough::Off);
    }

    #[test]
    fn user_session() {
        let t = parse(USER_SESSION).unwrap();
        assert_eq!(t.version, "3.4");
        assert_eq!(t.client_termtype, "iTerm2 3.6.9");
        assert_eq!(t.client_termname, "xterm-256color");
        assert_eq!(t.features.len(), 18);
        assert_eq!(t.features.first().map(String::as_str), Some("256"));
        assert!(t.has_feature("RGB"));
        assert!(t.has_feature("rgb"));
        assert!(t.has_feature("sixel"));
        assert!(t.has_feature("usstyle"));
        assert!(!t.has_feature("sixe"));
        assert_eq!(t.cell, None);
        assert_eq!(t.passthrough, Passthrough::Off);
        assert!(!t.passthrough.enabled());
        assert_eq!(t.set_clipboard, "external");
    }

    #[test]
    fn no_client() {
        assert_eq!(parse(NO_CLIENT), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("\n"), None);
        assert_eq!(parse("  \r\n  \n"), None);
    }

    #[test]
    fn passthrough_on_and_all() {
        let on = parse("3.4|kitty(0.49.1)|xterm-kitty|RGB,sixel|10x21|on|on").unwrap();
        assert_eq!(on.passthrough, Passthrough::On);
        assert!(on.passthrough.enabled());
        assert_eq!(on.cell, Some((10, 21)));
        let all = parse("3.5a|ghostty 1.3.1|xterm-ghostty|RGB|16x32|all|off").unwrap();
        assert_eq!(all.passthrough, Passthrough::All);
        assert!(all.passthrough.enabled());
        assert_eq!(all.version, "3.5a");
        assert_eq!(all.set_clipboard, "off");
        for value in ["off", "", "yes", "1"] {
            assert_eq!(Passthrough::parse(value), Passthrough::Off, "{value:?}");
        }
        assert_eq!(Passthrough::parse(" ON "), Passthrough::On);
        assert_eq!(Passthrough::All.as_str(), "all");
        assert_eq!(Passthrough::default(), Passthrough::Off);
    }

    #[test]
    fn odd_whitespace() {
        let t = parse(
            "\r\n  3.4 | iTerm2 3.6.9 |xterm-256color | 256, RGB ,,sixel , | 16 x 32 | on | on\r\n",
        )
        .unwrap();
        assert_eq!(t.version, "3.4");
        assert_eq!(t.client_termtype, "iTerm2 3.6.9");
        assert_eq!(t.client_termname, "xterm-256color");
        assert_eq!(t.features, ["256", "RGB", "sixel"]);
        assert_eq!(t.cell, Some((16, 32)));
        assert_eq!(t.passthrough, Passthrough::On);
        assert_eq!(t.set_clipboard, "on");
        let tabs = parse("\t3.4\t|\tkitty(0.49.1)\t|xterm-kitty|RGB|0x0|all|on\t").unwrap();
        assert_eq!(tabs.client_termtype, "kitty(0.49.1)");
        assert_eq!(tabs.passthrough, Passthrough::All);
        // Only six fields: not an answer to this query.
        assert_eq!(parse("3.4 | a | b | c | 1x1 | on\n"), None);
    }

    #[test]
    fn missing_termtype_is_kept_empty() {
        // A client whose terminal did not answer XTVERSION (e.g. VS Code).
        let t = parse("3.4||xterm-256color|256,RGB,hyperlinks|0x0|on|external").unwrap();
        assert_eq!(t.client_termtype, "");
        assert_eq!(t.client_termname, "xterm-256color");
        assert!(t.passthrough.enabled());
    }

    #[test]
    fn termtype_containing_separator() {
        let t = parse("3.4|Weird|Term 1.0|xterm-256color|RGB|8x16|on|external").unwrap();
        assert_eq!(t.client_termtype, "Weird|Term 1.0");
        assert_eq!(t.client_termname, "xterm-256color");
        assert_eq!(t.features, ["RGB"]);
        assert_eq!(t.cell, Some((8, 16)));
    }

    #[test]
    fn control_characters_are_dropped() {
        let t = parse("3.4|evil\x1b]0;x\x07term|xterm|RGB|0x0|off|on").unwrap();
        assert_eq!(t.client_termtype, "evil]0;xterm");
    }

    #[test]
    fn only_the_first_line_counts() {
        let t = parse("3.4|a 1|xterm|RGB|1x2|off|on\n3.5|b 2|xterm|RGB|3x4|on|on\n").unwrap();
        assert_eq!(t.client_termtype, "a 1");
    }

    #[test]
    fn cell_sizes() {
        assert_eq!(parse_cell("16x32"), Some((16, 32)));
        assert_eq!(parse_cell("0x0"), None);
        assert_eq!(parse_cell("16x0"), None);
        assert_eq!(parse_cell("x"), None);
        assert_eq!(parse_cell(""), None);
        assert_eq!(parse_cell("70000x2"), None);
        assert_eq!(parse_cell("-1x2"), None);
    }

    #[test]
    fn query_needs_tmux() {
        assert_eq!(query(&Env::from_pairs(&[("TERM", "xterm")])), None);
        assert_eq!(query(&Env::from_pairs(&[("TMUX", "")])), None);
    }

    #[test]
    fn query_with_unreachable_server_is_none() {
        // A socket path that cannot exist: tmux (if installed) fails fast.
        let env = Env::from_pairs(&[("TMUX", "/nonexistent/emde-test/socket,1,0")]);
        let start = std::time::Instant::now();
        assert_eq!(query(&env), None);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn captures_output_of_fast_commands() {
        let mut cmd = Command::new("printf");
        cmd.arg("3.4|kitty(0.49.1)|xterm-kitty|RGB|10x21|on|off\\n");
        let out = output_with_deadline(cmd, Duration::from_secs(2), MAX_OUTPUT).unwrap();
        let t = parse(&String::from_utf8_lossy(&out)).unwrap();
        assert_eq!(t.client_termtype, "kitty(0.49.1)");
    }
}
