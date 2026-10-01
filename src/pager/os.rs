//! What the pager asks of the system: opening URLs, the clipboard and the
//! editor.
//!
//! Opening: `open` (macOS) or `xdg-open`, or the configured command, run
//! directly (never through a shell) with the URL as the last argument,
//! detached in its own process group with no standard streams. Only when
//! running locally: over SSH (or without a display) the URL is copied
//! instead, and the terminal's own link handling (⌘-click on the OSC 8
//! link) opens it on the machine the reader sits at.
//!
//! The clipboard: OSC 52, which the terminal handles. Inside tmux with
//! `set-clipboard` other than `on` (tmux's default is `external`), tmux
//! ignores OSC 52 from programs, so the text goes to `tmux load-buffer -w
//! -` instead, which fills a tmux buffer and forwards it to the outer
//! terminal's clipboard.
//!
//! The editor: `$VISUAL`, else `$EDITOR`, else `vi`, split on whitespace
//! and run directly (no shell) with `+LINE FILE`, which vim, neovim, nano,
//! emacs, micro, helix and kakoune all take; the pager waits for it with
//! the terminal put back.

use std::ffi::OsString;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::OpenCommand;
use crate::gfx::b64;
use crate::term::env::Env;
use crate::term::tmux::TmuxInfo;

/// Longest text put on the clipboard.
const MAX_CLIPBOARD: usize = 64 * 1024;
/// How long `tmux load-buffer` may take.
const TMUX_TIMEOUT: Duration = Duration::from_millis(500);

/// How to open a URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenPlan {
    /// Run this program with these arguments (the URL last).
    Spawn(Vec<String>),
    /// Copy the URL instead, for this reason.
    Copy(&'static str),
}

/// Whether emde runs over SSH.
pub(crate) fn over_ssh(env: &Env) -> bool {
    env.is_set("SSH_CONNECTION") || env.is_set("SSH_TTY")
}

/// Schemes handed to the opener; other links (custom protocol handlers)
/// are only copied.
const OPENABLE: [&str; 5] = ["http", "https", "mailto", "ftp", "ftps"];

/// `url` as the opener gets it: a web or mail link (`//host` becomes
/// `https://host`), or `None` for any other scheme.
pub(crate) fn openable(url: &str) -> Option<String> {
    if let Some(rest) = url.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    let (scheme, _) = url.split_once(':')?;
    OPENABLE
        .iter()
        .any(|s| s.eq_ignore_ascii_case(scheme))
        .then(|| url.to_owned())
}

/// How to open `url` (already made safe: printable ASCII, no scheme that
/// runs code).
pub(crate) fn open_plan(open: &OpenCommand, env: &Env, remote: bool, url: &str) -> OpenPlan {
    let Some(url) = openable(url) else {
        return OpenPlan::Copy("only web and mail links are opened");
    };
    let url = url.as_str();
    match open {
        OpenCommand::Never => OpenPlan::Copy("opening links is off (pager.open)"),
        OpenCommand::Command(cmd) => {
            let mut argv: Vec<String> = cmd.split_whitespace().map(str::to_owned).collect();
            if argv.is_empty() {
                return OpenPlan::Copy("pager.open is empty");
            }
            argv.push(url.to_owned());
            OpenPlan::Spawn(argv)
        }
        OpenCommand::Auto if remote || over_ssh(env) => {
            OpenPlan::Copy("over SSH: ⌘-click the link to open it")
        }
        OpenCommand::Auto if cfg!(target_os = "macos") => {
            OpenPlan::Spawn(vec!["open".to_owned(), url.to_owned()])
        }
        OpenCommand::Auto if env.is_set("DISPLAY") || env.is_set("WAYLAND_DISPLAY") => {
            OpenPlan::Spawn(vec!["xdg-open".to_owned(), url.to_owned()])
        }
        OpenCommand::Auto => OpenPlan::Copy("no display to open it on: ⌘-click the link"),
    }
}

/// Start `argv` detached: no standard streams, its own process group (so
/// signals from the terminal do not reach it); a thread reaps it.
pub(crate) fn spawn_detached(argv: &[String]) -> io::Result<()> {
    let Some((program, args)) = argv.split_first() else {
        return Err(io::Error::other("no command"));
    };
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;
    std::thread::Builder::new()
        .name("emde-open".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

/// The command line that opens `path` at `line` in the reader's editor.
pub(crate) fn editor_command(env: &Env, path: &Path, line: usize) -> Vec<OsString> {
    let set = |name| env.get(name).filter(|v| !v.trim().is_empty());
    let editor = set("VISUAL").or_else(|| set("EDITOR")).unwrap_or("vi");
    let mut argv: Vec<OsString> = editor.split_whitespace().map(OsString::from).collect();
    if argv.is_empty() {
        argv.push("vi".into());
    }
    argv.push(format!("+{}", line.max(1)).into());
    argv.push(path.as_os_str().to_owned());
    argv
}

/// Run `argv` in the foreground and wait for it: it shares the terminal
/// (standard input from `/dev/tty` when standard input is not one).
/// Returns whether it succeeded.
pub(crate) fn run_foreground(argv: &[OsString]) -> io::Result<bool> {
    let Some((program, args)) = argv.split_first() else {
        return Err(io::Error::other("no command"));
    };
    let mut cmd = Command::new(program);
    cmd.args(args);
    if !io::stdin().is_terminal()
        && let Ok(tty) = std::fs::File::open("/dev/tty")
    {
        cmd.stdin(tty);
    }
    Ok(cmd.status()?.success())
}

/// Where copied text goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipboardPlan {
    /// OSC 52 to the terminal.
    Osc52,
    /// `tmux load-buffer -w -`.
    TmuxBuffer,
}

/// Where to copy to: inside tmux, OSC 52 only works with `set-clipboard
/// on`; without the tmux query result, OSC 52 is all there is.
pub(crate) fn clipboard_plan(env: &Env, tmux: Option<&TmuxInfo>) -> ClipboardPlan {
    match tmux {
        Some(info) if env.is_set("TMUX") && !info.set_clipboard.eq_ignore_ascii_case("on") => {
            ClipboardPlan::TmuxBuffer
        }
        _ => ClipboardPlan::Osc52,
    }
}

/// `ESC ] 52 ; c ; <base64> ESC \`.
pub(crate) fn osc52(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let bytes = bytes.get(..MAX_CLIPBOARD).unwrap_or(bytes);
    let mut out = b"\x1b]52;c;".to_vec();
    b64::encode_into(bytes, &mut out);
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Put `text` in a tmux buffer (and, with `-w`, the outer clipboard);
/// tmux before 3.2 has no `-w`, so a failure is retried without it.
pub(crate) fn tmux_load_buffer(env: &Env, text: &str) -> io::Result<()> {
    let text = text.as_bytes();
    let text = text.get(..MAX_CLIPBOARD).unwrap_or(text);
    match tmux(env, &["load-buffer", "-w", "-"], text) {
        Ok(()) => Ok(()),
        Err(_) => tmux(env, &["load-buffer", "-"], text),
    }
}

/// Run `tmux args` with `input` on its standard input.
fn tmux(env: &Env, args: &[&str], input: &[u8]) -> io::Result<()> {
    let mut cmd = Command::new("tmux");
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(socket) = env.non_empty("TMUX") {
        cmd.env("TMUX", socket);
    }
    let mut child = cmd.spawn()?;
    // Written from a thread: a tmux that does not read must not block the
    // pager past the deadline (killing it ends the write).
    let writer = child.stdin.take().map(|mut stdin| {
        let input = input.to_vec();
        std::thread::spawn(move || stdin.write_all(&input))
    });
    let deadline = Instant::now() + TMUX_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let written = writer.map_or(Ok(()), |w| {
        w.join()
            .unwrap_or_else(|_| Err(io::Error::other("the writer thread panicked")))
    });
    match status {
        None => Err(io::Error::other("tmux did not answer")),
        Some(s) if !s.success() => Err(io::Error::other(format!("tmux exited with {s}"))),
        Some(_) => written,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmux_info(set_clipboard: &str) -> TmuxInfo {
        crate::term::tmux::parse(&format!(
            "3.4|iTerm2 3.6.9|xterm-256color|RGB,sixel|0x0|off|{set_clipboard}"
        ))
        .unwrap()
    }

    #[test]
    fn opening_locally_over_ssh_and_by_command() {
        let url = "https://example.com";
        let local = Env::from_pairs(&[("DISPLAY", ":0")]);
        let want = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        assert_eq!(
            open_plan(&OpenCommand::Auto, &local, false, url),
            OpenPlan::Spawn(vec![want.into(), url.into()])
        );
        let ssh = Env::from_pairs(&[("DISPLAY", ":0"), ("SSH_CONNECTION", "1 2 3 4")]);
        assert_eq!(
            open_plan(&OpenCommand::Auto, &ssh, false, url),
            OpenPlan::Copy("over SSH: ⌘-click the link to open it")
        );
        assert_eq!(
            open_plan(&OpenCommand::Auto, &local, true, url),
            OpenPlan::Copy("over SSH: ⌘-click the link to open it")
        );
        assert_eq!(
            open_plan(&OpenCommand::Never, &local, false, url),
            OpenPlan::Copy("opening links is off (pager.open)")
        );
        let cmd = OpenCommand::Command("firefox --new-tab".into());
        assert_eq!(
            open_plan(&cmd, &ssh, false, url),
            OpenPlan::Spawn(vec!["firefox".into(), "--new-tab".into(), url.into()]),
            "an explicit command runs even over SSH"
        );
        if !cfg!(target_os = "macos") {
            assert_eq!(
                open_plan(&OpenCommand::Auto, &Env::default(), false, url),
                OpenPlan::Copy("no display to open it on: ⌘-click the link")
            );
        }
    }

    #[test]
    fn only_web_and_mail_links_are_opened() {
        let local = Env::from_pairs(&[("DISPLAY", ":0")]);
        let cmd = OpenCommand::Command("opener".into());
        for (url, want) in [
            ("https://example.com", Some("https://example.com")),
            ("HTTP://example.com", Some("HTTP://example.com")),
            ("mailto:me@example.org", Some("mailto:me@example.org")),
            ("//example.com/x", Some("https://example.com/x")),
            ("vscode://file/etc/passwd", None),
            ("ssh://host", None),
            ("no-scheme", None),
        ] {
            assert_eq!(openable(url).as_deref(), want, "{url}");
            let plan = open_plan(&cmd, &local, false, url);
            match want {
                Some(u) => assert_eq!(plan, OpenPlan::Spawn(vec!["opener".into(), u.into()])),
                None => assert_eq!(plan, OpenPlan::Copy("only web and mail links are opened")),
            }
        }
    }

    #[test]
    fn clipboard_choice() {
        let tmux = Env::from_pairs(&[("TMUX", "/tmp/tmux-1/default,1,0")]);
        assert_eq!(
            clipboard_plan(&tmux, Some(&tmux_info("external"))),
            ClipboardPlan::TmuxBuffer
        );
        assert_eq!(
            clipboard_plan(&tmux, Some(&tmux_info("on"))),
            ClipboardPlan::Osc52
        );
        assert_eq!(clipboard_plan(&tmux, None), ClipboardPlan::Osc52);
        assert_eq!(
            clipboard_plan(&Env::default(), Some(&tmux_info("external"))),
            ClipboardPlan::Osc52,
            "not inside tmux"
        );
    }

    #[test]
    fn osc52_bytes() {
        assert_eq!(osc52("hi"), b"\x1b]52;c;aGk=\x1b\\");
        let big = "x".repeat(MAX_CLIPBOARD + 10);
        assert!(osc52(&big).len() < MAX_CLIPBOARD * 2);
    }

    #[test]
    fn editor_commands() {
        let path = Path::new("docs/guide.md");
        let words = |env: &[(&str, &str)], line| -> Vec<String> {
            editor_command(&Env::from_pairs(env), path, line)
                .iter()
                .map(|w| w.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(words(&[], 12), ["vi", "+12", "docs/guide.md"]);
        assert_eq!(
            words(&[("EDITOR", "nano")], 3),
            ["nano", "+3", "docs/guide.md"]
        );
        assert_eq!(
            words(&[("EDITOR", "nano"), ("VISUAL", "code --wait")], 3),
            ["code", "--wait", "+3", "docs/guide.md"],
            "VISUAL first, split on whitespace"
        );
        assert_eq!(
            words(&[("VISUAL", "  "), ("EDITOR", "hx")], 0),
            ["hx", "+1", "docs/guide.md"],
            "blank VISUAL is unset; lines start at 1"
        );
    }

    #[test]
    fn running_programs() {
        assert!(run_foreground(&[]).is_err());
        assert!(run_foreground(&["/nonexistent/emde-test-editor".into()]).is_err());
        assert_eq!(run_foreground(&["true".into()]).ok(), Some(true));
        assert_eq!(run_foreground(&["false".into()]).ok(), Some(false));
    }

    #[test]
    fn spawning_nothing_fails() {
        assert!(spawn_detached(&[]).is_err());
        assert!(spawn_detached(&["/nonexistent/emde-test-program".into()]).is_err());
    }
}
