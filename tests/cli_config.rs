//! The binary's configuration and diagnostics: the config file, `--set`
//! and flags in their layers, bad configuration, `--check-config`,
//! `--print-default-config`, `--doctor`, the `--list-*` commands and
//! `--credits`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The binary with a controlled environment: no inherited colour settings,
/// terminal hints or user configuration.
fn emde() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_emde"));
    for var in emde::term::env::VARS {
        cmd.env_remove(var);
    }
    for var in ["EMDE_CONFIG", "XDG_CONFIG_HOME", "EMDE_TRACE"] {
        cmd.env_remove(var);
    }
    cmd.env("HOME", "/nonexistent/emde-test-home");
    cmd.env("TERM", "xterm-256color");
    cmd
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("run emde")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Run with `md` on standard input.
fn with_input(cmd: &mut Command, md: &str) -> Output {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(md.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// A scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("emde-cli-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, text).unwrap();
        path
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The widest line of the output, in columns.
fn widest(o: &Output) -> usize {
    stdout(o)
        .lines()
        .map(|l| emde::text::str_width(l, false))
        .max()
        .unwrap_or(0)
}

const PROSE: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do \
    eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, \
    quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.\n";

#[test]
fn flags_beat_set_which_beats_the_file_which_beats_defaults() {
    let dir = Scratch::new("layers");
    let config = dir.file("config.toml", "[render]\nmax_width = 30\nmargin = 0\n");
    let widths = |extra: &[&str]| {
        let mut cmd = emde();
        cmd.args(["-w", "200", "-c"]).arg(&config).args(extra);
        widest(&with_input(&mut cmd, PROSE))
    };
    let from_file = widths(&[]);
    assert!(from_file <= 30 && from_file > 20, "{from_file}");
    let from_set = widths(&["--set", "render.max_width=40"]);
    assert!(from_set <= 40 && from_set > 30, "{from_set}");
    let from_flag = widths(&["--set", "render.max_width=40", "-m", "50"]);
    assert!(from_flag <= 50 && from_flag > 40, "{from_flag}");
    // Without the file, the default (100).
    let defaults = widest(&with_input(
        emde().args(["-w", "200", "--no-config"]),
        PROSE,
    ));
    assert!(defaults > 50 && defaults <= 100, "{defaults}");
}

#[test]
fn the_config_file_is_found_through_the_environment() {
    let dir = Scratch::new("env");
    dir.file("xdg/emde/config.toml", "[render]\nmax_width = 25\n");
    let xdg = dir.path().join("xdg");
    let o = with_input(
        emde().env("XDG_CONFIG_HOME", &xdg).args(["-w", "200"]),
        PROSE,
    );
    assert!(widest(&o) <= 25, "{}", widest(&o));
    // $EMDE_CONFIG names a file explicitly; --no-config skips any file.
    let explicit = dir.file("explicit.toml", "[render]\nmax_width = 35\n");
    let o = with_input(
        emde()
            .env("XDG_CONFIG_HOME", &xdg)
            .env("EMDE_CONFIG", &explicit)
            .args(["-w", "200"]),
        PROSE,
    );
    assert!(widest(&o) <= 35 && widest(&o) > 25, "{}", widest(&o));
    let o = with_input(
        emde()
            .env("EMDE_CONFIG", &explicit)
            .args(["-w", "200", "--no-config"]),
        PROSE,
    );
    assert!(widest(&o) > 35);
}

#[test]
fn a_bad_config_never_blocks_reading() {
    let dir = Scratch::new("bad");
    let config = dir.file(
        "config.toml",
        "[render]\nmargin = \"wide\"\n\n[pager]\nmouze = true\n\n[code]\ntab_width = 0\n",
    );
    let mut cmd = emde();
    cmd.arg("-c").arg(&config);
    let o = with_input(&mut cmd, "# Still shown\n");
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("Still shown"));
    let err = stderr(&o);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(
        lines.len(),
        4,
        "three problems, then where to find them: {err}"
    );
    // A type error drops its table ([render] keeps the defaults).
    assert!(lines[0].starts_with("emde: error: "), "{err}");
    assert!(lines[0].contains("config.toml:2: "), "{err}");
    assert!(
        lines[1].contains("config.toml:5: unknown key `pager.mouze`"),
        "{err}"
    );
    assert!(lines[1].contains("did you mean `pager.mouse`?"), "{err}");
    assert!(
        lines[2].contains("`code.tab_width` must be from 1 to 16"),
        "{err}"
    );
    assert!(lines[3].contains("--check-config"), "{err}");
    // -v shows each problem in full, with the TOML snippet.
    let mut cmd = emde();
    cmd.arg("-c").arg(&config).arg("-v");
    let err = stderr(&with_input(&mut cmd, "x\n"));
    assert!(err.contains('^'), "the caret snippet: {err}");
    assert!(!err.contains("--check-config"), "{err}");
    // A config file that is not there is an error, but still not fatal.
    let o = with_input(emde().args(["-c", "/nonexistent/emde.toml"]), "x\n");
    assert_eq!(o.status.code(), Some(0));
    assert!(stderr(&o).contains("config file ignored"), "{}", stderr(&o));
}

#[test]
fn set_takes_toml_or_bare_words() {
    let o = with_input(
        emde().args([
            "--color=truecolor",
            "--set",
            "theme.code=Nord",
            "-s",
            "palette.mauve=#ff0000",
        ]),
        "## Red heading\n\n```rust\nfn x() {}\n```\n",
    );
    assert!(o.status.success());
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("38;2;255;0;0"), "palette override: {out:?}");
    if cfg!(feature = "highlight") {
        assert!(out.contains("38;2;129;161;193mfn"), "Nord: {out:?}");
    }
    let o = with_input(emde().args(["--set", "render.max_width"]), "x\n");
    assert_eq!(o.status.code(), Some(0));
    assert!(stderr(&o).contains("expected KEY=VALUE"), "{}", stderr(&o));
}

#[test]
fn check_config_reports_and_sets_the_exit_status() {
    let dir = Scratch::new("check");
    let good = dir.file("good.toml", "[render]\nmax_width = 80\n");
    let o = run(emde().arg("--check-config").arg("-c").arg(&good));
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert_eq!(stdout(&o), format!("{}: OK\n", good.display()));
    let o = run(emde().arg("--check-config").arg("--no-config"));
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(stdout(&o), "no config file (built-in defaults): OK\n");
    let bad = dir.file(
        "bad.toml",
        "[render]\nmax_widht = 80\n\n[code.aliases]\njsx = \"javascrpit\"\n",
    );
    let o = run(emde().arg("--check-config").arg("-c").arg(&bad));
    assert_eq!(o.status.code(), Some(1));
    let out = stdout(&o);
    assert!(out.contains("unknown key `render.max_widht`"), "{out}");
    if cfg!(feature = "highlight") {
        // Only the thorough check looks languages up.
        assert!(out.contains("javascrpit"), "{out}");
    }
    assert!(out.trim_end().ends_with("0 errors, 2 warnings") || out.contains("warning"));
}

#[test]
fn the_default_config_is_printed_as_embedded() {
    let o = run(emde().arg("--print-default-config"));
    assert!(o.status.success());
    let text = stdout(&o);
    let embedded =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/default.toml"))
            .unwrap();
    assert_eq!(text, embedded);
    // Printed, it is a valid config that changes nothing.
    let dir = Scratch::new("default");
    let copy = dir.file("config.toml", &text);
    let o = run(emde().arg("--check-config").arg("-c").arg(&copy));
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
}

#[test]
fn doctor_json_without_a_terminal() {
    let o = run(emde().arg("--doctor=json").env("TERM", "xterm-kitty"));
    assert!(o.status.success(), "{o:?}");
    let json = stdout(&o);
    assert!(json.starts_with("{\n  \"terminal\": {\n"), "{json}");
    for field in [
        "\"tty\": false",
        "\"color\": \"none\"",
        "\"graphics\": \"none\"",
        "\"probe\": null",
        "\"tmux\": null",
        "\"TERM\": \"xterm-kitty\"",
    ] {
        assert!(json.contains(field), "{field}: {json}");
    }
    // Inside tmux, but piped: neither the probe nor the tmux query runs.
    let o = run(emde()
        .arg("--doctor=json")
        .args(["--color=always", "--images", "auto"])
        .env("TMUX", "/nonexistent/emde-test/socket,1,0")
        .env("TERM", "tmux-256color")
        .env("SSH_CONNECTION", "192.0.2.1 1 192.0.2.2 22"));
    let json = stdout(&o);
    assert!(json.contains("\"tmux\": true"), "{json}");
    assert!(json.contains("\"probe\": null") && json.contains("\"tmux\": null"));
    assert!(json.contains("\"color\": \"truecolor\""), "{json}");
    if cfg!(feature = "images") {
        assert!(json.contains("\"graphics\": \"blocks\""), "{json}");
        assert!(json.contains("pixels ✗ output is not a terminal"), "{json}");
    } else {
        assert!(json.contains("built without image support"), "{json}");
    }
    assert!(json.contains("\"SSH_CONNECTION\": \"(set)\""), "redacted");
    assert!(!json.contains("192.0.2.1"));
}

#[test]
fn doctor_report_without_a_terminal() {
    let o = run(emde().arg("--doctor").arg("README.md"));
    assert!(o.status.success());
    let text = stdout(&o);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with(" terminal  "), "{text}");
    assert!(lines[1].starts_with(" links     "), "{text}");
    assert_eq!(
        lines[2],
        " probe     skipped (stdout is not a terminal)   cell size unknown (not probed)"
    );
}

#[test]
fn lists_and_credits() {
    let o = run(emde().arg("--list-themes"));
    let themes = stdout(&o);
    for name in [
        "emde",
        "ansi",
        "mono",
        "dracula",
        "gruvbox",
        "nord",
        "solarized",
        "tokyonight",
    ] {
        assert!(
            themes
                .lines()
                .any(|l| l.starts_with(name) && l.ends_with("built-in")),
            "{themes}"
        );
    }
    let o = run(emde().arg("--list-code-themes"));
    let languages = run(emde().arg("--list-languages"));
    let credits = run(emde().arg("--credits"));
    assert!(o.status.success() && languages.status.success() && credits.status.success());
    if cfg!(feature = "highlight") {
        assert!(
            stdout(&o).lines().any(|l| l == "OneHalfDark"),
            "{}",
            stdout(&o)
        );
        assert!(
            stdout(&languages)
                .lines()
                .any(|l| l.starts_with("Rust: rs")),
            "{}",
            stdout(&languages)
        );
        assert!(stdout(&credits).starts_with("emde bundles syntax definitions"));
    }
}

#[test]
fn info_commands_survive_a_closed_pipe() {
    let mut child = emde()
        .arg("--credits")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stderr.is_empty(), "{}", stderr(&o));
}
