//! The man page (clap_mangen) and shell completions (clap_complete), both
//! from emde's command-line definition, `emde::cli::Cli`.
//!
//! The man page has clap_mangen's NAME, OPTIONS (one section per help
//! heading) and VERSION sections, around sections written here: SYNOPSIS,
//! DESCRIPTION, KEYS (from the pager's key table), FILES, ENVIRONMENT, EXIT
//! STATUS, EXAMPLES and SEE ALSO.

use std::fs;
use std::path::{Path, PathBuf};

use std::fmt::Write as _;

use clap::CommandFactory as _;
use clap::ValueHint;
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
    Ok(ascii_roff(&page))
}

/// `page` with every non-ASCII character as a roff escape (`↓` is
/// `\[u2193]`): groff reads its input as Latin-1 unless it is told
/// otherwise, and groff and mandoc both know the escapes. Everything the
/// roff writer adds is ASCII, so only text is changed.
fn ascii_roff(page: &str) -> String {
    let mut out = String::with_capacity(page.len());
    for c in page.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let _ = write!(out, "\\[u{:04X}]", u32::from(c));
        }
    }
    out
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

/// What some options take, for completion: numbers, `KEY=VALUE` and slugs
/// are no file names (no files are offered), and `--config` names a file.
/// Other options keep clap's default (any path), or their possible values.
const VALUE_HINTS: [(&str, ValueHint); 5] = [
    ("width", ValueHint::Other),
    ("max_width", ValueHint::Other),
    ("anchor", ValueHint::Other),
    ("set", ValueHint::Other),
    ("config", ValueHint::FilePath),
];

/// The command line as the completion script for `shell` offers it.
///
/// * Hidden options (`--dump`) are left out: clap_complete would offer them.
/// * Numbers, `KEY=VALUE` and slugs get no file name completion
///   ([`VALUE_HINTS`]).
/// * An option whose value is optional and must follow `=`
///   (`--doctor[=FORMAT]`) is a plain flag for bash and fish: their scripts
///   would offer the values as the next word, which emde takes for a file.
///   zsh completes `--doctor=json` as it is meant.
fn completion_command(shell: Shell) -> Result<clap::Command, String> {
    let cli = Cli::command();
    if let Some((id, _)) = VALUE_HINTS
        .iter()
        .find(|(id, _)| !cli.get_arguments().any(|a| a.get_id() == id))
    {
        return Err(format!("completions: emde has no option `{id}`"));
    }
    let visible: Vec<clap::Arg> = cli
        .get_arguments()
        .filter(|a| !a.is_hide_set())
        .map(|a| {
            let hint = VALUE_HINTS.iter().find(|(id, _)| a.get_id() == id);
            match hint {
                Some(&(_, hint)) => a.clone().value_hint(hint),
                None if shell != Shell::Zsh && optional_after_equals(a) => as_flag(a),
                None => a.clone(),
            }
        })
        .collect();
    // xtask has the workspace's version, which is emde's.
    Ok(clap::Command::new(BIN)
        .version(env!("CARGO_PKG_VERSION"))
        .args(visible))
}

/// Whether an option's value is optional and must follow `=`.
fn optional_after_equals(arg: &clap::Arg) -> bool {
    arg.is_require_equals_set() && arg.get_num_args().is_some_and(|n| n.min_values() == 0)
}

/// `arg` as a flag that takes no value.
fn as_flag(arg: &clap::Arg) -> clap::Arg {
    arg.clone()
        .num_args(0)
        .require_equals(false)
        .default_missing_value(None)
        .value_name(None)
        .value_parser(clap::value_parser!(bool))
        .action(clap::ArgAction::SetTrue)
}

/// The completion scripts in memory: `(file name, script)`.
pub(crate) fn completions() -> Result<Vec<(String, Vec<u8>)>, String> {
    SHELLS
        .iter()
        .map(|&shell| {
            let mut cmd = completion_command(shell)?;
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut cmd, BIN, &mut script);
            let name = shell.file_name(BIN);
            if script.is_empty() {
                return Err(format!("the {name} completion script is empty"));
            }
            Ok((name, script))
        })
        .collect()
}

/// Write the completion scripts into `dir`; returns their paths.
pub(crate) fn write_completions(dir: &Path) -> Result<Vec<PathBuf>, String> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    SHELLS
        .iter()
        .map(|&shell| {
            let mut cmd = completion_command(shell)?;
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
        "On a terminal, a document opens in the built-in pager (see KEYS; with pager.enabled \
         = \"auto\", only one taller than the screen). Otherwise, and with --plain, emde \
         writes the rendered document to standard output; when that is not a terminal, \
         without escape sequences unless --color says otherwise. With -, or without FILE when standard input is not a terminal, emde reads \
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
        "The configuration file. $XDG_CONFIG_HOME/emde/config.toml comes first when \
         XDG_CONFIG_HOME is set (to an absolute path) and the file exists. --config or \
         $EMDE_CONFIG name another file; --no-config reads none. The same paths are used on \
         Linux and macOS.",
    );
    item(
        &mut roff,
        "~/.config/emde/themes/NAME.toml",
        "Theme files, chosen with --theme NAME or theme.name in the configuration file; \
         $XDG_CONFIG_HOME/emde/themes/ is searched first.",
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
            "COLORFGBG",
            "The terminal's colours (fg;bg), for the dark or light variant of the theme when \
             the terminal does not answer emde's question about its background.",
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
        (
            "2",
            "A usage error: an unknown option, a bad value, or no FILE while standard input is \
             a terminal.",
        ),
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
        let title = format!(
            "\n.TH EMDE 1 \"\" \"emde {}\" \"User Commands\"\n",
            env!("CARGO_PKG_VERSION")
        );
        assert!(page.contains(&title), "{page}");
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
        // The arrow keys, as escapes: the page is ASCII.
        assert!(page.is_ascii());
        assert!(page.contains("\\fBj \\[u2193] ^E ^N\\fR"), "{page}");
    }

    #[test]
    fn roff_escapes() {
        assert_eq!(ascii_roff("a ↓ b"), "a \\[u2193] b");
        assert_eq!(ascii_roff("é😀"), "\\[u00E9]\\[u1F600]");
        assert_eq!(ascii_roff(".TH X\n"), ".TH X\n");
    }

    /// The completion script for `shell`, as text.
    fn script(shell: Shell) -> String {
        let scripts = completions().unwrap();
        let name = shell.file_name(BIN);
        let (_, script) = scripts.iter().find(|(n, _)| *n == name).unwrap();
        String::from_utf8(script.clone()).unwrap()
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
        for shell in SHELLS {
            assert_eq!(
                completion_command(shell).unwrap().get_version(),
                Cli::command().get_version()
            );
        }
        // Hidden options are not offered.
        let bash = script(Shell::Bash);
        let offered = bash
            .lines()
            .find(|l| l.trim_start().starts_with("opts=\"-"))
            .unwrap();
        assert!(
            offered.contains("--plain") && !offered.contains("--dump"),
            "{offered}"
        );
    }

    /// The body of the `case` branch of the bash script for `option`.
    fn bash_case(bash: &str, option: &str) -> String {
        let after = bash.split(&format!("\n                {option})\n")).nth(1);
        let body = after.and_then(|a| a.split(";;").next());
        body.unwrap_or_else(|| panic!("no case for {option}"))
            .to_owned()
    }

    /// `--doctor json` would be `--doctor` and a file named `json`: bash and
    /// fish offer no value after `--doctor`, zsh completes `--doctor=`.
    #[test]
    fn values_after_equals_only() {
        let bash = script(Shell::Bash);
        assert!(bash.contains(" --doctor "), "still offered");
        assert!(!bash.contains("--doctor)"), "{bash}");
        let fish = script(Shell::Fish);
        let doctor = fish.lines().find(|l| l.contains("-l doctor")).unwrap();
        assert!(
            !doctor.contains("json") && !doctor.contains(" -r"),
            "{doctor}"
        );
        let zsh = script(Shell::Zsh);
        let doctor = zsh.split("'--doctor=[").nth(1).unwrap();
        let doctor = doctor.split("' \\\n").next().unwrap();
        assert!(
            doctor.contains("::FORMAT:") && doctor.contains("json"),
            "{doctor}"
        );
    }

    /// Numbers and `KEY=VALUE` are not completed as file names, `--config`
    /// is (names with spaces too).
    #[test]
    fn file_names_only_for_files() {
        let bash = script(Shell::Bash);
        for option in ["--width", "-m", "--set", "--anchor"] {
            let case = bash_case(&bash, option);
            assert!(!case.contains("compgen -f"), "{option}: {case}");
        }
        let config = bash_case(&bash, "--config");
        assert!(config.contains("compgen -f") && config.contains("-o filenames"));
        let zsh = script(Shell::Zsh);
        let width = zsh.lines().find(|l| l.starts_with("'--width=")).unwrap();
        assert!(width.ends_with(":N:' \\"), "{width}");
        assert!(zsh.contains(":PATH:_files' \\"));
    }
}
