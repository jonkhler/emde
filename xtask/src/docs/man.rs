//! The man page (clap_mangen) and shell completions (clap_complete), both
//! from emde's command-line definition, `emde::cli::Cli`.
//!
//! The man page has clap_mangen's NAME, OPTIONS (one section per help
//! heading) and VERSION sections, around sections written here: SYNOPSIS,
//! DESCRIPTION, KEYS (from the pager's key table), FILES, ENVIRONMENT, EXIT
//! STATUS, EXAMPLES and SEE ALSO.

use std::fs;
use std::path::{Path, PathBuf};

use clap::CommandFactory as _;
use clap_complete::{Generator as _, Shell};
use clap_mangen::Man;
use clap_mangen::roff::{Roff, bold, italic, roman};
use emde::cli::Cli;
use emde::pager::keymap::{self, BINDINGS, Section};

/// The shells completions are generated for.
const SHELLS: [Shell; 3] = [Shell::Bash, Shell::Zsh, Shell::Fish];

/// The name the program is installed as.
const BIN: &str = "emde";

/// The man page, as roff.
pub(crate) fn man_page() -> Result<String, String> {
    let cmd = Cli::command();
    let version = cmd.get_version().unwrap_or_default().to_owned();
    let man = Man::new(cmd);
    // Every rendered piece starts with the same preamble; it is needed once.
    let preamble = Roff::new().render();
    let mut page = preamble.clone();
    // The title by hand: roff drops empty arguments (the date) instead of
    // quoting them, which would shift the others.
    page.push_str(&format!(
        ".TH EMDE 1 \"\" \"{BIN} {version}\" \"User Commands\"\n"
    ));
    for text in [
        piece(|w| man.render_name_section(w))?,
        piece(|w| synopsis_and_description().to_writer(w))?,
        piece(|w| man.render_options_section(w))?,
        piece(|w| keys().to_writer(w))?,
        piece(|w| reference().to_writer(w))?,
        piece(|w| man.render_version_section(w))?,
    ] {
        page.push_str(text.strip_prefix(preamble.as_str()).unwrap_or(&text));
    }
    Ok(page)
}

/// What `render` writes, as text.
fn piece(render: impl FnOnce(&mut Vec<u8>) -> std::io::Result<()>) -> Result<String, String> {
    let mut out = Vec::new();
    render(&mut out).map_err(|e| e.to_string())?;
    String::from_utf8(out).map_err(|e| e.to_string())
}

/// Write the man page into `dir`; returns its path.
pub(crate) fn write_man_page(dir: &Path) -> Result<PathBuf, String> {
    let page = man_page()?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{BIN}.1"));
    fs::write(&path, page).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// The command line as completions offer it: clap_complete would also
/// offer hidden options (`--dump`), so they are left out.
fn completion_command() -> clap::Command {
    let cli = Cli::command();
    let visible: Vec<clap::Arg> = cli
        .get_arguments()
        .filter(|a| !a.is_hide_set())
        .cloned()
        .collect();
    // xtask has the workspace's version, which is emde's.
    clap::Command::new(BIN)
        .version(env!("CARGO_PKG_VERSION"))
        .args(visible)
}

/// The completion scripts in memory: `(file name, script)`.
pub(crate) fn completions() -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut cmd = completion_command();
    let scripts = SHELLS
        .iter()
        .map(|&shell| {
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut cmd, BIN, &mut script);
            (shell.file_name(BIN), script)
        })
        .collect::<Vec<_>>();
    match scripts.iter().find(|(_, s)| s.is_empty()) {
        Some((name, _)) => Err(format!("the {name} completion script is empty")),
        None => Ok(scripts),
    }
}

/// Write the completion scripts into `dir`; returns their paths.
pub(crate) fn write_completions(dir: &Path) -> Result<Vec<PathBuf>, String> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut cmd = completion_command();
    SHELLS
        .iter()
        .map(|&shell| {
            clap_complete::generate_to(shell, &mut cmd, BIN, dir)
                .map_err(|e| format!("{}: {shell} completions: {e}", dir.display()))
        })
        .collect()
}

/// A paragraph of plain text.
fn para(roff: &mut Roff, text: &str) {
    roff.control("PP", []).text([roman(text)]);
}

/// A tagged paragraph: `tag` in bold, then `text`.
fn item(roff: &mut Roff, tag: &str, text: &str) {
    roff.control("TP", []).text([bold(tag)]).text([roman(text)]);
}

/// SYNOPSIS and DESCRIPTION.
fn synopsis_and_description() -> Roff {
    let mut roff = Roff::new();
    roff.control("SH", ["SYNOPSIS"]).text([
        bold(BIN),
        roman(" ["),
        italic("OPTIONS"),
        roman("] ["),
        italic("FILE"),
        roman("["),
        bold("#"),
        italic("ANCHOR"),
        roman("] | "),
        italic("DIR"),
        roman(" | "),
        bold("-"),
        roman("]..."),
    ]);
    roff.control("SH", ["DESCRIPTION"]);
    roff.text([
        bold(BIN),
        roman(
            " shows Markdown documents in the terminal: headings, lists, tables, quotes and \
             alerts, footnotes, syntax-highlighted code, LaTeX math (inline formulas as one line \
             of Unicode, display formulas laid out in two dimensions) and images (kitty, iTerm2 \
             or sixel graphics where the terminal has them, coloured block characters \
             elsewhere). Links are clickable where the terminal supports OSC 8 hyperlinks.",
        ),
    ]);
    para(
        &mut roff,
        "On a terminal, a document taller than the screen opens in the built-in pager (see \
         KEYS). Otherwise, and with --plain, emde writes the rendered document to standard \
         output; when that is not a terminal, without escape sequences unless --color says \
         otherwise. With -, or without FILE when standard input is not a terminal, emde reads \
         standard input. A directory shows its README. FILE#ANCHOR opens the pager at the \
         heading whose slug is ANCHOR, as in GitHub links (lower case, with spaces as \
         hyphens).",
    );
    para(
        &mut roff,
        "Every setting has a default in the built-in configuration (emde \
         --print-default-config prints it with comments), which the configuration file (see \
         FILES), --set and the options below override, in that order. emde --check-config \
         reports mistakes; emde --doctor shows what emde found out about the terminal, and why.",
    );
    roff
}

/// KEYS: the pager's bindings, section by section.
fn keys() -> Roff {
    let mut roff = Roff::new();
    roff.control("SH", ["KEYS"]);
    para(
        &mut roff,
        "In the pager (^X is Control-X, S-Tab is Shift-Tab). A count typed before a command \
         repeats it or gives it a line or a percentage: 5j, 120g, 50%.",
    );
    for section in Section::ALL {
        roff.control("SS", [section.title()]);
        for binding in BINDINGS.iter().filter(|b| b.section == section) {
            item(&mut roff, &keymap::keys_label(binding), binding.help);
        }
    }
    roff
}

/// FILES, ENVIRONMENT, EXIT STATUS, EXAMPLES and SEE ALSO.
fn reference() -> Roff {
    let mut roff = Roff::new();
    roff.control("SH", ["FILES"]);
    item(
        &mut roff,
        "~/.config/emde/config.toml",
        "The configuration file, or $XDG_CONFIG_HOME/emde/config.toml when XDG_CONFIG_HOME is \
         set (an absolute path). --config or $EMDE_CONFIG name another file; --no-config reads \
         none. The same paths are used on Linux and macOS.",
    );
    item(
        &mut roff,
        "~/.config/emde/themes/NAME.toml",
        "Theme files, chosen with --theme NAME or theme.name in the configuration file.",
    );
    item(
        &mut roff,
        "$XDG_RUNTIME_DIR/emde/caps-*",
        "What the terminal answered when it was last asked (for slow connections; kept for 12 \
         hours). --reprobe asks again.",
    );

    roff.control("SH", ["ENVIRONMENT"]);
    for (name, text) in [
        ("EMDE_CONFIG", "The configuration file to read."),
        (
            "XDG_CONFIG_HOME, HOME",
            "Where the configuration directory is.",
        ),
        (
            "NO_COLOR",
            "When set and not empty: no colours, only bold, italic, underline and reverse text \
             (--color overrides it).",
        ),
        (
            "FORCE_COLOR, CLICOLOR_FORCE, CLICOLOR",
            "Colours on or off whatever the output is: FORCE_COLOR=0 turns them off, 1, 2 and 3 \
             choose 16, 256 and 24-bit colours; CLICOLOR=0 turns them off.",
        ),
        (
            "COLORTERM, TERM, TERM_PROGRAM, LC_TERMINAL, TMUX",
            "How many colours the terminal shows, and which terminal it is. Inside tmux, \
             COLORTERM is usually unset; emde uses 24-bit colours there, which tmux converts \
             for each client.",
        ),
        (
            "COLUMNS",
            "The width when the output is not a terminal (else 80).",
        ),
        (
            "SSH_CONNECTION, SSH_TTY",
            "Over SSH emde waits longer for the terminal's answers, and the pager copies links \
             instead of opening them.",
        ),
        (
            "XDG_RUNTIME_DIR",
            "Where the terminal's answers are cached.",
        ),
        (
            "EMDE_TRACE",
            "When 1, print how long each stage took, on standard error.",
        ),
    ] {
        item(&mut roff, name, text);
    }

    roff.control("SH", ["EXIT STATUS"]);
    for (code, text) in [
        (
            "0",
            "Success, also when the program reading the output stops early.",
        ),
        (
            "1",
            "An input could not be read (the others are still shown), or --check-config found \
             problems.",
        ),
        ("2", "A usage error: an unknown option or a bad value."),
    ] {
        item(&mut roff, code, text);
    }

    roff.control("SH", ["EXAMPLES"]);
    for (command, text) in [
        (
            "emde README.md",
            "Read a document (in the pager when it is long).",
        ),
        (
            "emde docs/guide.md#installation",
            "Open the pager at the Installation heading.",
        ),
        (
            "curl -sL https://example.org/README.md | emde",
            "Read standard input.",
        ),
        (
            "emde -p --color=always notes.md | less -R",
            "Page with less instead, keeping colours and styles.",
        ),
        (
            "emde --set render.max_width=80 --set theme.code=Nord notes.md",
            "Change settings for one run.",
        ),
        (
            "emde --doctor",
            "Show what emde detected about the terminal: colours, links, images.",
        ),
    ] {
        item(&mut roff, command, text);
    }

    roff.control("SH", ["SEE ALSO"]);
    roff.text([
        bold("less"),
        roman("(1), "),
        bold("tmux"),
        roman("(1). CONFIG.md, which comes with emde, describes every setting."),
    ]);
    roff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn man_page_sections() {
        let page = man_page().unwrap();
        assert!(
            page.contains("\n.TH EMDE 1 \"\" \"emde 0.1.0\" \"User Commands\"\n"),
            "{page}"
        );
        assert_eq!(page.matches(".ds Aq").count(), 2, "one preamble");
        for section in [
            "NAME",
            "SYNOPSIS",
            "DESCRIPTION",
            "OPTIONS",
            "OUTPUT",
            "CONFIGURATION",
            "KEYS",
            "FILES",
            "ENVIRONMENT",
            "\"EXIT STATUS\"",
            "EXAMPLES",
            "\"SEE ALSO\"",
            "VERSION",
        ] {
            assert!(page.contains(&format!("\n.SH {section}\n")), "{section}");
        }
        assert!(page.contains("\\-\\-print\\-default\\-config"));
        // Hidden options stay hidden.
        assert!(!page.contains("dump"));
        // Every key section is there.
        for section in Section::ALL {
            assert!(page.contains(section.title()), "{}", section.title());
        }
    }

    #[test]
    fn completion_scripts() {
        let scripts = completions().unwrap();
        let names: Vec<&str> = scripts.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["emde.bash", "_emde", "emde.fish"]);
        for (name, script) in &scripts {
            let text = String::from_utf8_lossy(script);
            assert!(text.contains("print-default-config"), "{name}");
            assert!(text.contains("truecolor"), "{name}: possible values");
            assert!(!text.contains("dump"), "{name}: a hidden option");
        }
        assert_eq!(
            completion_command().get_version(),
            Cli::command().get_version()
        );
        // Hidden options are not offered.
        let bash = String::from_utf8_lossy(&scripts[0].1);
        let offered = bash
            .lines()
            .find(|l| l.trim_start().starts_with("opts=\"-"))
            .unwrap();
        assert!(
            offered.contains("--plain") && !offered.contains("--dump"),
            "{offered}"
        );
    }
}
