//! The key bindings: one table ([`BINDINGS`]) drives input handling, the
//! help overlay and the documentation ([`markdown`]).
//!
//! Every binding belongs to a [`Section`], which names its group in the
//! help and the input [`Context`] it applies in. In the text-input contexts
//! (the search prompt, the outline filter, link hints) printable keys that
//! are not bound type text.

use std::fmt::Write as _;

use super::term::{Key, KeyCode, Mods};

/// Where input goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Context {
    /// Reading: the document has the keys.
    Normal,
    /// The search prompt.
    Prompt,
    /// The outline overlay.
    Outline,
    /// Link hints.
    Hints,
    /// The help overlay.
    Help,
    /// The `:` prompt.
    Command,
}

/// A group of bindings in the help.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Scroll,
    Jump,
    Search,
    Links,
    Other,
    Prompt,
    Outline,
    Hints,
    Help,
    Command,
}

impl Section {
    /// Every section, in help order.
    pub const ALL: [Section; 10] = [
        Section::Scroll,
        Section::Jump,
        Section::Search,
        Section::Links,
        Section::Other,
        Section::Prompt,
        Section::Command,
        Section::Outline,
        Section::Hints,
        Section::Help,
    ];

    /// The input context its bindings apply in.
    pub const fn context(self) -> Context {
        match self {
            Section::Prompt => Context::Prompt,
            Section::Outline => Context::Outline,
            Section::Hints => Context::Hints,
            Section::Help => Context::Help,
            Section::Command => Context::Command,
            Section::Scroll | Section::Jump | Section::Search | Section::Links | Section::Other => {
                Context::Normal
            }
        }
    }

    /// The heading in the help.
    pub const fn title(self) -> &'static str {
        match self {
            Section::Scroll => "Scrolling",
            Section::Jump => "Jumping",
            Section::Search => "Searching",
            Section::Links => "Links",
            Section::Other => "Other",
            Section::Prompt => "In the search prompt",
            Section::Outline => "In the outline",
            Section::Hints => "With link hints",
            Section::Help => "In this help",
            Section::Command => "At the : prompt",
        }
    }
}

/// What a key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    /// A digit of the count for the next command.
    Count,
    LineDown,
    LineUp,
    PageDown,
    PageUp,
    HalfDown,
    HalfUp,
    Top,
    Bottom,
    Percent,
    NextHeading,
    PrevHeading,
    NextSection,
    PrevSection,
    Outline,
    SearchForward,
    SearchBackward,
    NextMatch,
    PrevMatch,
    ClearSearch,
    FocusNext,
    FocusPrev,
    Follow,
    Hints,
    Back,
    Forward,
    CopyUrl,
    Reload,
    ToggleWatch,
    CycleImages,
    ToggleWidth,
    ToggleMouse,
    /// Open the `:` prompt.
    CommandLine,
    Help,
    Redraw,
    Suspend,
    Quit,
    PromptAccept,
    PromptCancel,
    PromptErase,
    PromptClear,
    OutlineDown,
    OutlineUp,
    OutlinePageDown,
    OutlinePageUp,
    OutlineJump,
    OutlineClose,
    OutlineErase,
    HintsCancel,
    HintsErase,
    HelpDown,
    HelpUp,
    HelpPageDown,
    HelpPageUp,
    HelpClose,
    CommandRun,
    CommandCancel,
    CommandErase,
    CommandClear,
}

/// One row of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub section: Section,
    pub keys: &'static [Key],
    pub command: Command,
    /// What the help says.
    pub help: &'static str,
}

const fn c(ch: char) -> Key {
    Key::char(ch)
}

const fn k(code: KeyCode) -> Key {
    Key::plain(code)
}

const fn ctrl(ch: char) -> Key {
    Key::ctrl(ch)
}

const DIGITS: [Key; 10] = [
    c('0'),
    c('1'),
    c('2'),
    c('3'),
    c('4'),
    c('5'),
    c('6'),
    c('7'),
    c('8'),
    c('9'),
];

macro_rules! bind {
    ($section:ident, [$($key:expr),* $(,)?], $command:ident, $help:literal) => {
        Binding {
            section: Section::$section,
            keys: &[$($key),*],
            command: Command::$command,
            help: $help,
        }
    };
}

/// Every key binding.
#[rustfmt::skip]
pub const BINDINGS: &[Binding] = &[
    Binding {
        section: Section::Scroll,
        keys: &DIGITS,
        command: Command::Count,
        help: "count for the next command (5j, 50%, 120g)",
    },
    bind!(Scroll, [c('j'), k(KeyCode::Down), ctrl('e'), ctrl('n')], LineDown, "down a line"),
    bind!(Scroll, [c('k'), k(KeyCode::Up), ctrl('y'), ctrl('p')], LineUp, "up a line"),
    bind!(Scroll, [c(' '), c('f'), k(KeyCode::PageDown), ctrl('f')], PageDown, "down a page"),
    bind!(Scroll, [c('b'), k(KeyCode::PageUp), ctrl('b')], PageUp, "up a page"),
    bind!(Scroll, [c('d'), ctrl('d')], HalfDown, "down half a page"),
    bind!(Scroll, [c('u'), ctrl('u')], HalfUp, "up half a page"),
    bind!(Scroll, [c('g'), k(KeyCode::Home), c('<')], Top, "top (line N with a count)"),
    bind!(Scroll, [c('G'), k(KeyCode::End), c('>')], Bottom, "bottom (line N with a count)"),
    bind!(Scroll, [c('%')], Percent, "N percent into the document"),
    bind!(Jump, [c(']')], NextHeading, "next heading"),
    bind!(Jump, [c('[')], PrevHeading, "previous heading"),
    bind!(Jump, [c('}')], NextSection, "next heading of the same or a higher level"),
    bind!(Jump, [c('{')], PrevSection, "previous heading of the same or a higher level"),
    bind!(Jump, [c('t')], Outline, "outline (type to filter)"),
    bind!(Search, [c('/')], SearchForward, "search forward"),
    bind!(Search, [c('?')], SearchBackward, "search backward"),
    bind!(Search, [c('n')], NextMatch, "next match"),
    bind!(Search, [c('N')], PrevMatch, "previous match"),
    bind!(Search, [k(KeyCode::Esc)], ClearSearch, "clear the search (then the link focus)"),
    bind!(Links, [k(KeyCode::Tab)], FocusNext, "focus the next link"),
    bind!(Links, [k(KeyCode::BackTab)], FocusPrev, "focus the previous link"),
    bind!(Links, [k(KeyCode::Enter)], Follow, "follow the focused link"),
    bind!(Links, [c('o')], Hints, "link hints: type a label to follow"),
    bind!(Links, [k(KeyCode::Backspace), c('H')], Back, "back"),
    bind!(Links, [c('L')], Forward, "forward"),
    bind!(Links, [c('y')], CopyUrl, "copy the focused link's URL"),
    bind!(Other, [c('r')], Reload, "reload the file"),
    bind!(Other, [c('R')], ToggleWatch, "watch the file for changes, on or off"),
    bind!(Other, [c('i')], CycleImages, "cycle the image mode"),
    bind!(Other, [c('w')], ToggleWidth, "full width, on or off"),
    bind!(Other, [c('m')], ToggleMouse, "mouse, on or off (off: select text)"),
    bind!(Other, [c(':')], CommandLine, "command: :n next file, :p previous file, :q quit"),
    bind!(Other, [c('h'), k(KeyCode::F(1))], Help, "this help"),
    bind!(Other, [ctrl('l')], Redraw, "redraw the screen (anywhere)"),
    bind!(Other, [ctrl('z')], Suspend, "suspend (anywhere)"),
    bind!(Other, [c('q'), ctrl('c')], Quit, "quit"),
    bind!(Prompt, [k(KeyCode::Enter)], PromptAccept, "search"),
    bind!(Prompt, [k(KeyCode::Esc), ctrl('c')], PromptCancel, "cancel"),
    bind!(Prompt, [k(KeyCode::Backspace)], PromptErase, "delete a character"),
    bind!(Prompt, [ctrl('u')], PromptClear, "clear the pattern"),
    bind!(Command, [k(KeyCode::Enter)], CommandRun, "run the command"),
    bind!(Command, [k(KeyCode::Esc), ctrl('c')], CommandCancel, "cancel"),
    bind!(Command, [k(KeyCode::Backspace)], CommandErase, "delete a character"),
    bind!(Command, [ctrl('u')], CommandClear, "clear the command"),
    bind!(Outline, [k(KeyCode::Down), ctrl('n'), ctrl('j')], OutlineDown, "next entry"),
    bind!(Outline, [k(KeyCode::Up), ctrl('p'), ctrl('k')], OutlineUp, "previous entry"),
    bind!(Outline, [k(KeyCode::PageDown)], OutlinePageDown, "down a page"),
    bind!(Outline, [k(KeyCode::PageUp)], OutlinePageUp, "up a page"),
    bind!(Outline, [k(KeyCode::Enter)], OutlineJump, "go to the heading"),
    bind!(Outline, [k(KeyCode::Esc), ctrl('c')], OutlineClose, "close"),
    bind!(Outline, [k(KeyCode::Backspace)], OutlineErase, "delete a filter character"),
    bind!(Hints, [k(KeyCode::Esc), ctrl('c')], HintsCancel, "cancel"),
    bind!(Hints, [k(KeyCode::Backspace)], HintsErase, "delete a label character"),
    bind!(Help, [c('j'), k(KeyCode::Down)], HelpDown, "down a line"),
    bind!(Help, [c('k'), k(KeyCode::Up)], HelpUp, "up a line"),
    bind!(Help, [c(' '), c('f'), k(KeyCode::PageDown)], HelpPageDown, "down a page"),
    bind!(Help, [c('b'), k(KeyCode::PageUp)], HelpPageUp, "up a page"),
    bind!(
        Help,
        [c('q'), c('h'), k(KeyCode::Esc), k(KeyCode::F(1)), ctrl('c')],
        HelpClose,
        "close"
    ),
];

/// Commands whose keys work in every context.
pub const GLOBAL: [Command; 2] = [Command::Redraw, Command::Suspend];

/// The command bound to `key` in `context` (or everywhere).
pub fn lookup(context: Context, key: Key) -> Option<Command> {
    BINDINGS
        .iter()
        .find(|b| {
            (b.section.context() == context || GLOBAL.contains(&b.command)) && b.keys.contains(&key)
        })
        .map(|b| b.command)
}

/// How a key is written in the help: `j`, `^E`, `Space`, `↓`, `PgDn`.
pub fn label(key: Key) -> String {
    let mut s = String::new();
    if key.mods.contains(Mods::ALT) {
        s.push_str("M-");
    }
    if key.mods.contains(Mods::SHIFT) {
        s.push_str("S-");
    }
    let name = match key.code {
        KeyCode::Char(' ') => "Space",
        KeyCode::Char(ch) => {
            if key.mods.contains(Mods::CTRL) {
                s.push('^');
                s.push(ch.to_ascii_uppercase());
            } else {
                s.push(ch);
            }
            return s;
        }
        KeyCode::Enter => "Enter",
        KeyCode::Esc => "Esc",
        KeyCode::Backspace => "Backspace",
        KeyCode::Tab => "Tab",
        KeyCode::BackTab => "S-Tab",
        KeyCode::Up => "↑",
        KeyCode::Down => "↓",
        KeyCode::Left => "←",
        KeyCode::Right => "→",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PgUp",
        KeyCode::PageDown => "PgDn",
        KeyCode::Insert => "Ins",
        KeyCode::Delete => "Del",
        KeyCode::F(n) => {
            let _ = write!(s, "F{n}");
            return s;
        }
    };
    if key.mods.contains(Mods::CTRL) {
        s.push('^');
    }
    s.push_str(name);
    s
}

/// The keys of a binding as the help shows them: `0`–`9` as one range.
pub fn keys_label(b: &Binding) -> String {
    if b.command == Command::Count {
        return "0-9".to_owned();
    }
    let labels: Vec<String> = b.keys.iter().map(|&key| label(key)).collect();
    labels.join(" ")
}

/// One line of the help overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelpLine {
    /// A section heading.
    Title(&'static str),
    /// Keys and what they do.
    Entry { keys: String, help: &'static str },
    /// Space between sections.
    Blank,
}

/// The help, section by section.
pub fn help_lines() -> Vec<HelpLine> {
    let mut out = Vec::new();
    for section in Section::ALL {
        if !out.is_empty() {
            out.push(HelpLine::Blank);
        }
        out.push(HelpLine::Title(section.title()));
        for b in BINDINGS.iter().filter(|b| b.section == section) {
            out.push(HelpLine::Entry {
                keys: keys_label(b),
                help: b.help,
            });
        }
        let note = match section {
            Section::Prompt => Some("other keys type the pattern (smart case)"),
            Section::Command => Some("other keys type the command: n, p, x (first file) or q"),
            Section::Outline => Some("other keys type a filter"),
            Section::Hints => Some("other keys type the label of a link"),
            _ => None,
        };
        if let Some(help) = note {
            out.push(HelpLine::Entry {
                keys: String::new(),
                help,
            });
        }
    }
    out
}

/// The bindings as Markdown tables, one per section, for the README.
pub fn markdown() -> String {
    let mut out = String::new();
    for section in Section::ALL {
        let _ = writeln!(
            out,
            "**{}**\n\n| Keys | Action |\n|---|---|",
            section.title()
        );
        for b in BINDINGS.iter().filter(|b| b.section == section) {
            let keys: Vec<String> = if b.command == Command::Count {
                vec!["`0`–`9`".to_owned()]
            } else {
                b.keys
                    .iter()
                    .map(|&key| match label(key).as_str() {
                        "`" => "`` ` ``".to_owned(),
                        l => format!("`{l}`"),
                    })
                    .collect()
            };
            let _ = writeln!(
                out,
                "| {} | {} |",
                keys.join(" "),
                b.help.replace('|', "\\|")
            );
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn keys_are_unique_per_context() {
        let mut seen = HashSet::new();
        for b in BINDINGS {
            for &key in b.keys {
                assert!(
                    seen.insert((b.section.context(), key)),
                    "{key:?} is bound twice in {:?}",
                    b.section.context()
                );
            }
        }
        // Keys that work everywhere are bound nowhere else.
        let global: Vec<Key> = BINDINGS
            .iter()
            .filter(|b| GLOBAL.contains(&b.command))
            .flat_map(|b| b.keys.iter().copied())
            .collect();
        for b in BINDINGS.iter().filter(|b| !GLOBAL.contains(&b.command)) {
            assert!(
                !b.keys.iter().any(|k| global.contains(k)),
                "{:?} takes a global key",
                b.command
            );
        }
        for context in [
            Context::Prompt,
            Context::Outline,
            Context::Hints,
            Context::Help,
            Context::Command,
        ] {
            assert_eq!(lookup(context, ctrl('z')), Some(Command::Suspend));
            assert_eq!(lookup(context, ctrl('l')), Some(Command::Redraw));
        }
    }

    #[test]
    fn every_binding_has_keys_and_help() {
        for b in BINDINGS {
            assert!(!b.keys.is_empty(), "{:?}", b.command);
            assert!(!b.help.is_empty(), "{:?}", b.command);
        }
        let commands: HashSet<Command> = BINDINGS.iter().map(|b| b.command).collect();
        assert_eq!(commands.len(), BINDINGS.len(), "one row per command");
    }

    #[test]
    fn plan_keys_are_bound() {
        use Command as C;
        for (key, want) in [
            (c('j'), C::LineDown),
            (k(KeyCode::Down), C::LineDown),
            (ctrl('e'), C::LineDown),
            (c('k'), C::LineUp),
            (ctrl('y'), C::LineUp),
            (c(' '), C::PageDown),
            (c('f'), C::PageDown),
            (k(KeyCode::PageDown), C::PageDown),
            (c('b'), C::PageUp),
            (c('d'), C::HalfDown),
            (c('u'), C::HalfUp),
            (c('g'), C::Top),
            (k(KeyCode::Home), C::Top),
            (c('G'), C::Bottom),
            (k(KeyCode::End), C::Bottom),
            (c('%'), C::Percent),
            (c(']'), C::NextHeading),
            (c('['), C::PrevHeading),
            (c('}'), C::NextSection),
            (c('{'), C::PrevSection),
            (c('t'), C::Outline),
            (c('/'), C::SearchForward),
            (c('?'), C::SearchBackward),
            (c('n'), C::NextMatch),
            (c('N'), C::PrevMatch),
            (k(KeyCode::Tab), C::FocusNext),
            (k(KeyCode::BackTab), C::FocusPrev),
            (k(KeyCode::Enter), C::Follow),
            (c('o'), C::Hints),
            (k(KeyCode::Backspace), C::Back),
            (c('H'), C::Back),
            (c('L'), C::Forward),
            (c('y'), C::CopyUrl),
            (c('r'), C::Reload),
            (c('R'), C::ToggleWatch),
            (c('i'), C::CycleImages),
            (c('w'), C::ToggleWidth),
            (c('m'), C::ToggleMouse),
            (c(':'), C::CommandLine),
            (c('h'), C::Help),
            (k(KeyCode::F(1)), C::Help),
            (c('q'), C::Quit),
            (ctrl('c'), C::Quit),
            (ctrl('l'), C::Redraw),
            (ctrl('z'), C::Suspend),
            (c('7'), C::Count),
        ] {
            assert_eq!(lookup(Context::Normal, key), Some(want), "{key:?}");
        }
        assert_eq!(lookup(Context::Normal, c('x')), None);
        assert_eq!(
            lookup(Context::Prompt, c('j')),
            None,
            "j types into the prompt"
        );
        assert_eq!(
            lookup(Context::Outline, k(KeyCode::Enter)),
            Some(C::OutlineJump)
        );
        assert_eq!(lookup(Context::Help, c('q')), Some(C::HelpClose));
        assert_eq!(
            lookup(Context::Command, k(KeyCode::Enter)),
            Some(C::CommandRun)
        );
        assert_eq!(lookup(Context::Command, c('n')), None, "n types");
    }

    #[test]
    fn labels() {
        assert_eq!(label(c('j')), "j");
        assert_eq!(label(ctrl('e')), "^E");
        assert_eq!(label(c(' ')), "Space");
        assert_eq!(label(k(KeyCode::Down)), "↓");
        assert_eq!(label(k(KeyCode::PageDown)), "PgDn");
        assert_eq!(label(k(KeyCode::F(1))), "F1");
        assert_eq!(label(k(KeyCode::BackTab)), "S-Tab");
        let b = BINDINGS
            .iter()
            .find(|b| b.command == Command::Count)
            .unwrap();
        assert_eq!(keys_label(b), "0-9");
    }

    #[test]
    fn help_covers_every_binding() {
        let lines = help_lines();
        let entries = lines
            .iter()
            .filter(|l| matches!(l, HelpLine::Entry { keys, .. } if !keys.is_empty()))
            .count();
        assert_eq!(entries, BINDINGS.len());
        assert_eq!(lines.first(), Some(&HelpLine::Title("Scrolling")));
        let md = markdown();
        assert!(md.contains("| `j` `↓` `^E` `^N` | down a line |"), "{md}");
        assert!(md.contains("| `0`–`9` |"));
        assert_eq!(md.matches("| Keys | Action |").count(), Section::ALL.len());
    }
}
