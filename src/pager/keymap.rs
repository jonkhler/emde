//! The key bindings: one table ([`BINDINGS`]) drives input handling, the
//! help overlay and the documentation ([`markdown`]).
//!
//! Every binding belongs to a [`Section`], which names its group in the
//! help and the input [`Context`] it applies in, and has a stable action
//! name (`page-down`, `yank-hints`) that `[pager.keys]` in the config file
//! uses to give it other keys ([`Keymap::new`]). In the text-input
//! contexts (the search prompt, the outline filter, hints) printable keys
//! that are not bound type text.
//!
//! # Keys
//!
//! Keys are written as in the config file ([`parse_keys`]): a character
//! (`j`, `G`, `?`), a named key (`Enter`, `Esc`, `Space`, `Tab`, `S-Tab`,
//! `Backspace`, `Up`, `Down`, `Left`, `Right`, `Home`, `End`, `PgUp`,
//! `PgDn`, `Ins`, `Del`, `F1`…`F24`), a key with a modifier (`C-x` for
//! Ctrl, `M-x` for Alt, `S-` for Shift), or several of those in a row: `gg`
//! (characters typed one after the other) or `g Home` (keys separated by
//! spaces). A sequence that is bound and also starts a longer one (`g` and
//! `gg`) runs when the next key does not continue it, or after
//! [`SEQUENCE_TIMEOUT`].

use std::fmt::Write as _;
use std::time::Duration;

use super::term::{Key, KeyCode, Mods};
use crate::config::suggest::did_you_mean;

/// How long a bound sequence that also starts a longer one waits for the
/// next key.
pub const SEQUENCE_TIMEOUT: Duration = Duration::from_millis(600);

/// Where input goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Context {
    /// Reading: the document has the keys.
    Normal,
    /// Selecting lines.
    Visual,
    /// The search prompt.
    Prompt,
    /// The outline overlay.
    Outline,
    /// Hint labels.
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
    Pick,
    Other,
    Visual,
    Prompt,
    Command,
    Outline,
    Hints,
    Help,
}

impl Section {
    /// Every section, in help order.
    pub const ALL: [Section; 12] = [
        Section::Scroll,
        Section::Jump,
        Section::Search,
        Section::Links,
        Section::Pick,
        Section::Other,
        Section::Visual,
        Section::Prompt,
        Section::Command,
        Section::Outline,
        Section::Hints,
        Section::Help,
    ];

    /// The input context its bindings apply in.
    pub const fn context(self) -> Context {
        match self {
            Section::Visual => Context::Visual,
            Section::Prompt => Context::Prompt,
            Section::Outline => Context::Outline,
            Section::Hints => Context::Hints,
            Section::Help => Context::Help,
            Section::Command => Context::Command,
            Section::Scroll
            | Section::Jump
            | Section::Search
            | Section::Links
            | Section::Pick
            | Section::Other => Context::Normal,
        }
    }

    /// The heading in the help.
    pub const fn title(self) -> &'static str {
        match self {
            Section::Scroll => "Scrolling",
            Section::Jump => "Jumping",
            Section::Search => "Searching",
            Section::Links => "Links",
            Section::Pick => "Hints, copying and editing",
            Section::Other => "Other",
            Section::Visual => "In Visual mode",
            Section::Prompt => "In the search prompt",
            Section::Outline => "In the outline",
            Section::Hints => "With hints",
            Section::Help => "In this help",
            Section::Command => "At the : prompt",
        }
    }
}

/// What a key does. In Visual mode the motions (`LineDown`, `HalfDown`,
/// `Top`, `NextHeading`, `NextMatch`, `ScrollCenter`, …) move the end of
/// the selection, and the view follows it.
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
    /// `zz`: the current line to the middle of the screen.
    ScrollCenter,
    /// `zt`: the current line to the top.
    ScrollTop,
    /// `zb`: the current line to the bottom.
    ScrollBottom,
    NextHeading,
    PrevHeading,
    NextSection,
    PrevSection,
    /// The end of the block, or of the next one (Visual mode).
    NextBlock,
    /// The start of the block, or of the previous one (Visual mode).
    PrevBlock,
    Outline,
    /// `m` and a letter.
    SetMark,
    /// `'` and a letter (or `'`).
    JumpMark,
    SearchForward,
    SearchBackward,
    NextMatch,
    PrevMatch,
    ClearSearch,
    FocusNext,
    FocusPrev,
    Follow,
    /// Link hints: the label follows the link.
    LinkHints,
    /// Hints on everything worth acting on: the label follows a link, goes
    /// to a heading or a block, zooms an image.
    FollowHints,
    /// The focused link's URL, or hints: the label copies what it is on.
    YankHints,
    /// Hints: the label selects what it is on (Visual mode).
    VisualHints,
    /// Visual mode on the first block on screen.
    VisualLine,
    /// Open the file in the editor.
    Edit,
    Back,
    Forward,
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
    /// The other end of the selection.
    VisualSwap,
    /// Copy the Markdown of the selected blocks.
    VisualYank,
    /// Copy the text of the selected lines.
    VisualYankText,
    VisualExit,
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
    /// The action name `[pager.keys]` uses (kebab-case, stable).
    pub name: &'static str,
    /// The default keys, written as [`parse_keys`] reads them, separated
    /// by commas.
    pub keys: &'static str,
    pub command: Command,
    /// What the help says.
    pub help: &'static str,
}

impl Binding {
    /// The default key sequences.
    pub fn default_keys(&self) -> impl Iterator<Item = Vec<Key>> {
        self.keys.split(',').filter_map(|k| parse_keys(k).ok())
    }
}

macro_rules! bind {
    ($section:ident, $name:literal, $keys:literal, $command:ident, $help:literal) => {
        Binding {
            section: Section::$section,
            name: $name,
            keys: $keys,
            command: Command::$command,
            help: $help,
        }
    };
}

/// The digits of a count.
const DIGITS: &str = "0,1,2,3,4,5,6,7,8,9";

/// The action name of the count digits (they cannot be changed).
const COUNT: &str = "count";

/// Every key binding.
#[rustfmt::skip]
pub const BINDINGS: &[Binding] = &[
    Binding {
        section: Section::Scroll,
        name: COUNT,
        keys: DIGITS,
        command: Command::Count,
        help: "count for the next command (5j, 50%, 120gg)",
    },
    bind!(Scroll, "line-down", "j,Down,C-e,C-n", LineDown, "down a line"),
    bind!(Scroll, "line-up", "k,Up,C-y,C-p", LineUp, "up a line"),
    bind!(Scroll, "page-down", "Space,PgDn,C-f", PageDown, "down a page"),
    bind!(Scroll, "page-up", "b,PgUp,C-b", PageUp, "up a page"),
    bind!(Scroll, "half-page-down", "d,C-d", HalfDown, "down half a page"),
    bind!(Scroll, "half-page-up", "u,C-u", HalfUp, "up half a page"),
    bind!(Scroll, "top", "gg,g,Home,<", Top, "top (line N with a count)"),
    bind!(Scroll, "bottom", "G,End,>", Bottom, "bottom (line N with a count)"),
    bind!(Scroll, "percent", "%", Percent, "N percent into the document"),
    bind!(Scroll, "scroll-center", "zz", ScrollCenter, "centre the current match, link or middle line"),
    bind!(Scroll, "scroll-top", "zt", ScrollTop, "… to the top"),
    bind!(Scroll, "scroll-bottom", "zb", ScrollBottom, "… to the bottom"),
    bind!(Jump, "next-heading", "]", NextHeading, "next heading"),
    bind!(Jump, "prev-heading", "[", PrevHeading, "previous heading"),
    bind!(Jump, "next-section", "}", NextSection, "next heading of the same or a higher level"),
    bind!(Jump, "prev-section", "{", PrevSection, "previous heading of the same or a higher level"),
    bind!(Jump, "outline", "t", Outline, "outline (type to filter)"),
    bind!(Jump, "set-mark", "m", SetMark, "mark the place: m and a letter (ma)"),
    bind!(Jump, "jump-mark", "'", JumpMark, "go to a mark ('a); '' back to before the last jump"),
    bind!(Search, "search-forward", "/", SearchForward, "search forward"),
    bind!(Search, "search-backward", "?", SearchBackward, "search backward"),
    bind!(Search, "next-match", "n", NextMatch, "next match"),
    bind!(Search, "prev-match", "N", PrevMatch, "previous match"),
    bind!(Search, "clear-search", "Esc", ClearSearch, "clear the zoomed image, the search, the link focus"),
    bind!(Links, "focus-next", "Tab", FocusNext, "focus the next link"),
    bind!(Links, "focus-prev", "S-Tab", FocusPrev, "focus the previous link"),
    bind!(Links, "follow", "Enter", Follow, "follow the focused link"),
    bind!(Links, "link-hints", "o", LinkHints, "link hints: type a label to follow"),
    bind!(Links, "back", "Backspace,H", Back, "back"),
    bind!(Links, "forward", "L", Forward, "forward"),
    bind!(Pick, "hints-follow", "f", FollowHints, "hints: follow a link, go to a heading or block, zoom an image"),
    bind!(Pick, "yank-hints", "y", YankHints, "copy the focused link's URL, else hints: copy code, math, a table…"),
    bind!(Pick, "visual-hints", "v", VisualHints, "hints: select a block (Visual mode)"),
    bind!(Pick, "visual-line", "V", VisualLine, "Visual mode: select lines"),
    bind!(Pick, "edit", "e", Edit, "edit the file ($VISUAL, $EDITOR) at the top block"),
    bind!(Other, "reload", "r", Reload, "reload the file"),
    bind!(Other, "toggle-watch", "R", ToggleWatch, "watch the file for changes, on or off"),
    bind!(Other, "cycle-images", "i", CycleImages, "cycle the image mode"),
    bind!(Other, "toggle-width", "w", ToggleWidth, "full width, on or off"),
    bind!(Other, "toggle-mouse", "M", ToggleMouse, "mouse, on or off (off: select text)"),
    bind!(Other, "command-line", ":", CommandLine, "command: :n next file, :p previous file, :q quit"),
    bind!(Other, "help", "h,F1", Help, "this help"),
    bind!(Other, "redraw", "C-l", Redraw, "redraw the screen (anywhere)"),
    bind!(Other, "suspend", "C-z", Suspend, "suspend (anywhere)"),
    bind!(Other, "quit", "q,C-c", Quit, "quit"),
    Binding {
        section: Section::Visual,
        name: COUNT,
        keys: DIGITS,
        command: Command::Count,
        help: "count for the next motion",
    },
    bind!(Visual, "visual-down", "j,Down,C-n", LineDown, "extend down a line"),
    bind!(Visual, "visual-up", "k,Up,C-p", LineUp, "extend up a line"),
    bind!(Visual, "visual-half-down", "C-d,PgDn", HalfDown, "down half a page"),
    bind!(Visual, "visual-half-up", "C-u,PgUp", HalfUp, "up half a page"),
    bind!(Visual, "visual-next-block", "}", NextBlock, "to the end of the block, or of the next"),
    bind!(Visual, "visual-prev-block", "{", PrevBlock, "to the start of the block, or of the previous"),
    bind!(Visual, "visual-next-heading", "]", NextHeading, "to the next heading"),
    bind!(Visual, "visual-prev-heading", "[", PrevHeading, "to the previous heading"),
    bind!(Visual, "visual-top", "gg,g,Home", Top, "to the top"),
    bind!(Visual, "visual-bottom", "G,End", Bottom, "to the bottom"),
    bind!(Visual, "visual-next-match", "n", NextMatch, "to the next match"),
    bind!(Visual, "visual-prev-match", "N", PrevMatch, "to the previous match"),
    bind!(Visual, "visual-scroll-center", "zz", ScrollCenter, "the cursor line to the middle"),
    bind!(Visual, "visual-scroll-top", "zt", ScrollTop, "… to the top"),
    bind!(Visual, "visual-scroll-bottom", "zb", ScrollBottom, "… to the bottom"),
    bind!(Visual, "visual-swap", "o", VisualSwap, "go to the other end"),
    bind!(Visual, "visual-yank", "y", VisualYank, "copy the Markdown of the selected blocks"),
    bind!(Visual, "visual-yank-text", "Y", VisualYankText, "copy the text of the selected lines"),
    bind!(Visual, "visual-edit", "e", Edit, "edit the file at the selection"),
    bind!(Visual, "visual-exit", "Esc,V,q,C-c", VisualExit, "leave Visual mode"),
    bind!(Prompt, "prompt-accept", "Enter", PromptAccept, "search"),
    bind!(Prompt, "prompt-cancel", "Esc,C-c", PromptCancel, "cancel"),
    bind!(Prompt, "prompt-erase", "Backspace", PromptErase, "delete a character"),
    bind!(Prompt, "prompt-clear", "C-u", PromptClear, "clear the pattern"),
    bind!(Command, "command-run", "Enter", CommandRun, "run the command"),
    bind!(Command, "command-cancel", "Esc,C-c", CommandCancel, "cancel"),
    bind!(Command, "command-erase", "Backspace", CommandErase, "delete a character"),
    bind!(Command, "command-clear", "C-u", CommandClear, "clear the command"),
    bind!(Outline, "outline-down", "Down,C-n,C-j", OutlineDown, "next entry"),
    bind!(Outline, "outline-up", "Up,C-p,C-k", OutlineUp, "previous entry"),
    bind!(Outline, "outline-page-down", "PgDn", OutlinePageDown, "down a page"),
    bind!(Outline, "outline-page-up", "PgUp", OutlinePageUp, "up a page"),
    bind!(Outline, "outline-jump", "Enter", OutlineJump, "go to the heading"),
    bind!(Outline, "outline-close", "Esc,C-c", OutlineClose, "close"),
    bind!(Outline, "outline-erase", "Backspace", OutlineErase, "delete a filter character"),
    bind!(Hints, "hints-cancel", "Esc,C-c", HintsCancel, "cancel"),
    bind!(Hints, "hints-erase", "Backspace", HintsErase, "delete a label character"),
    bind!(Help, "help-down", "j,Down", HelpDown, "down a line"),
    bind!(Help, "help-up", "k,Up", HelpUp, "up a line"),
    bind!(Help, "help-page-down", "Space,f,PgDn", HelpPageDown, "down a page"),
    bind!(Help, "help-page-up", "b,PgUp", HelpPageUp, "up a page"),
    bind!(Help, "help-close", "q,h,Esc,F1,C-c", HelpClose, "close"),
];

/// Commands whose keys work in every context.
pub const GLOBAL: [Command; 2] = [Command::Redraw, Command::Suspend];

/// The key names [`parse_keys`] knows, as the help writes them.
const NAMED: &[(&str, KeyCode)] = &[
    ("Enter", KeyCode::Enter),
    ("Esc", KeyCode::Esc),
    ("Space", KeyCode::Char(' ')),
    ("Tab", KeyCode::Tab),
    ("Backspace", KeyCode::Backspace),
    ("Up", KeyCode::Up),
    ("Down", KeyCode::Down),
    ("Left", KeyCode::Left),
    ("Right", KeyCode::Right),
    ("Home", KeyCode::Home),
    ("End", KeyCode::End),
    ("PgUp", KeyCode::PageUp),
    ("PgDn", KeyCode::PageDown),
    ("Ins", KeyCode::Insert),
    ("Del", KeyCode::Delete),
];

/// Other spellings of key names.
const ALIASES: &[(&str, KeyCode)] = &[
    ("Return", KeyCode::Enter),
    ("Escape", KeyCode::Esc),
    ("BS", KeyCode::Backspace),
    ("BackTab", KeyCode::BackTab),
    ("PageUp", KeyCode::PageUp),
    ("PageDown", KeyCode::PageDown),
    ("Insert", KeyCode::Insert),
    ("Delete", KeyCode::Delete),
];

/// A named key, ignoring case (`Enter`, `pgdn`, `F5`).
fn named(name: &str) -> Option<KeyCode> {
    if let Some(n) = name
        .strip_prefix(['F', 'f'])
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=24).contains(n))
    {
        return Some(KeyCode::F(n));
    }
    NAMED
        .iter()
        .chain(ALIASES)
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|&(_, code)| code)
}

#[inline(never)]
fn unknown_key(token: &str) -> String {
    let names = NAMED.iter().map(|(n, _)| *n).chain(["S-Tab"]);
    match did_you_mean(token, names) {
        Some(s) => format!("unknown key `{token}` (did you mean `{s}`?)"),
        None => format!("unknown key `{token}`"),
    }
}

/// One key: a character, a named key, or either with a modifier.
#[inline(never)]
fn parse_key(token: &str) -> Result<Key, String> {
    let mut chars = token.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(Key::char(c));
    }
    if let Some(code) = named(token) {
        return Ok(Key::plain(code));
    }
    let Some((m, rest)) = token
        .split_once('-')
        .filter(|(m, rest)| m.len() == 1 && !rest.is_empty())
    else {
        return Err(unknown_key(token));
    };
    let mods = match m {
        "C" | "c" => Mods::CTRL,
        "M" | "m" | "A" | "a" => Mods::ALT,
        "S" | "s" => Mods::SHIFT,
        _ => return Err(unknown_key(token)),
    };
    let mut key = parse_key(rest).map_err(|_| unknown_key(token))?;
    if !key.mods.is_empty() {
        return Err(unknown_key(token));
    }
    match (key.code, mods) {
        (KeyCode::Tab, Mods::SHIFT) => key.code = KeyCode::BackTab,
        // Shifted letters arrive as capitals.
        (KeyCode::Char(c), Mods::SHIFT) => key.code = KeyCode::Char(c.to_ascii_uppercase()),
        // Control characters arrive as the lowercase letter.
        (KeyCode::Char(c), Mods::CTRL) => {
            key.code = KeyCode::Char(c.to_ascii_lowercase());
            key.mods = Mods::CTRL;
        }
        _ => key.mods = mods,
    }
    Ok(key)
}

/// A key sequence written as in the config file (see the module docs):
/// `j`, `C-f`, `PgDn`, `gg`, `g Home`.
#[inline(never)]
pub fn parse_keys(s: &str) -> Result<Vec<Key>, String> {
    let mut out = Vec::new();
    for token in s.split_whitespace() {
        match parse_key(token) {
            Ok(key) => out.push(key),
            // A word that is no key name is typed character by character
            // (`gg`, `zt`), unless it looks like a misspelt name or a
            // modifier.
            Err(e) if !token.contains('-') && !e.contains("did you mean") => {
                out.extend(token.chars().map(Key::char));
            }
            Err(e) => return Err(e),
        }
    }
    if out.is_empty() {
        return Err(format!("no key in `{s}` (a space is `Space`)"));
    }
    Ok(out)
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

/// How a key sequence is written in the help: `gg`, `^W j`.
#[inline(never)]
pub fn seq_label(keys: &[Key]) -> String {
    let chars = keys.iter().all(|k| k.text().is_some_and(|c| c != ' '));
    let labels: Vec<String> = keys.iter().map(|&k| label(k)).collect();
    labels.join(if chars { "" } else { " " })
}

/// The default keys of a binding as the help shows them: `0`–`9` as one
/// range.
pub fn keys_label(b: &Binding) -> String {
    if b.command == Command::Count {
        return "0-9".to_owned();
    }
    let labels: Vec<String> = b.default_keys().map(|k| seq_label(&k)).collect();
    labels.join(" ")
}

/// What a key sequence means in a context.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lookup {
    /// The command bound to exactly this sequence.
    pub exact: Option<Command>,
    /// Whether a longer sequence starts with it.
    pub longer: bool,
}

/// A problem with `[pager.keys]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyIssue {
    /// The action name it is about (as written).
    pub action: String,
    pub message: String,
}

/// The keys in effect: [`BINDINGS`] with the changes of `[pager.keys]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    /// Every key sequence and the binding it runs: in the order of
    /// [`BINDINGS`], and in the order given within a binding.
    keys: Vec<(&'static Binding, Vec<Key>)>,
}

impl Default for Keymap {
    fn default() -> Self {
        let mut keys = Vec::new();
        for b in BINDINGS {
            keys.extend(b.default_keys().map(|seq| (b, seq)));
        }
        Keymap { keys }
    }
}

/// Whether the keys of `a` and `b` can clash: the same context, or one of
/// them works everywhere.
fn overlap(a: &Binding, b: &Binding) -> bool {
    a.section.context() == b.section.context()
        || GLOBAL.contains(&a.command)
        || GLOBAL.contains(&b.command)
}

/// Whether `b` is one of `list`.
fn among(b: &Binding, list: &[&'static Binding]) -> bool {
    list.iter().any(|o| std::ptr::eq(*o, b))
}

/// `action` names no binding.
#[inline(never)]
fn unknown_action(action: &str) -> KeyIssue {
    let message = if action == COUNT {
        "the count digits cannot be changed".to_owned()
    } else {
        let names = BINDINGS.iter().map(|b| b.name).filter(|&n| n != COUNT);
        match did_you_mean(action, names) {
            Some(s) => format!("unknown action `{action}` (did you mean `{s}`?)"),
            None => format!("unknown action `{action}`"),
        }
    };
    KeyIssue {
        action: action.to_owned(),
        message,
    }
}

/// `keeps` takes `seq` from `loses` (`set`: whether the config set
/// `loses` too).
#[inline(never)]
fn taken(seq: &[Key], loses: &Binding, keeps: &Binding, set: bool) -> KeyIssue {
    let (key, loses, keeps) = (seq_label(seq), loses.name, keeps.name);
    let message = if set {
        format!(
            "`{key}` is bound twice, to `{loses}` and `{keeps}`: the later one, `{keeps}`, gets it"
        )
    } else {
        format!("`{key}` is a default key of `{loses}`, which loses it to `{keeps}`")
    };
    KeyIssue {
        action: keeps.to_owned(),
        message,
    }
}

impl Keymap {
    /// The default keys with `overrides` (`[pager.keys]`, in order): each
    /// gives an action its keys instead of the default ones (an empty
    /// list leaves it without keys). Unknown actions and keys are left
    /// out; a key bound to two actions stays with the one bound later, an
    /// action of the config winning over a default one. Every problem is
    /// returned.
    pub fn new(overrides: &[(String, Vec<String>)]) -> (Keymap, Vec<KeyIssue>) {
        let mut keys = Keymap::default().keys;
        let mut issues = Vec::new();
        // The bindings the config sets, in the order they were last set.
        let mut set: Vec<&'static Binding> = Vec::new();
        for (action, seqs) in overrides {
            let Some(b) = BINDINGS
                .iter()
                .find(|b| b.name == action && b.command != Command::Count)
            else {
                issues.push(unknown_action(action));
                continue;
            };
            keys.retain(|(o, _)| !std::ptr::eq(*o, b));
            set.retain(|o| !std::ptr::eq(*o, b));
            set.push(b);
            for k in seqs {
                match parse_keys(k) {
                    Ok(seq) => {
                        if !keys.iter().any(|(o, s)| std::ptr::eq(*o, b) && *s == seq) {
                            keys.push((b, seq));
                        }
                    }
                    Err(message) => issues.push(KeyIssue {
                        action: action.clone(),
                        message,
                    }),
                }
            }
        }
        // Keys bound twice: the binding set later keeps them.
        for (i, &b) in set.iter().enumerate() {
            let later = set.get(i + 1..).unwrap_or_default();
            let mine: Vec<Vec<Key>> = keys
                .iter()
                .filter(|(o, _)| std::ptr::eq(*o, b))
                .map(|(_, seq)| seq.clone())
                .collect();
            keys.retain(|(o, seq)| {
                let loses =
                    !std::ptr::eq(*o, b) && !among(o, later) && overlap(b, o) && mine.contains(seq);
                if loses {
                    issues.push(taken(seq, o, b, among(o, &set)));
                }
                !loses
            });
        }
        (Keymap { keys }, issues)
    }

    /// What `seq` means in `context`.
    pub fn lookup(&self, context: Context, seq: &[Key]) -> Lookup {
        let mut out = Lookup::default();
        for (b, s) in &self.keys {
            if b.section.context() != context && !GLOBAL.contains(&b.command) {
                continue;
            }
            if s.as_slice() == seq {
                out.exact.get_or_insert(b.command);
            } else if s.starts_with(seq) {
                out.longer = true;
            }
        }
        out
    }

    /// The command bound to the single key `key` in `context` (or
    /// everywhere).
    pub fn command(&self, context: Context, key: Key) -> Option<Command> {
        self.lookup(context, &[key]).exact
    }

    /// The help, section by section, with the keys in effect.
    pub fn help_lines(&self) -> Vec<HelpLine> {
        let mut out = Vec::new();
        for section in Section::ALL {
            if !out.is_empty() {
                out.push(HelpLine::Blank);
            }
            out.push(HelpLine::Title(section.title()));
            for b in BINDINGS.iter().filter(|b| b.section == section) {
                let keys = if b.command == Command::Count {
                    "0-9".to_owned()
                } else {
                    let labels: Vec<String> = self
                        .keys
                        .iter()
                        .filter(|(o, _)| std::ptr::eq(*o, b))
                        .map(|(_, seq)| seq_label(seq))
                        .collect();
                    labels.join(" ")
                };
                out.push(HelpLine::Entry { keys, help: b.help });
            }
            let note = match section {
                Section::Prompt => Some("other keys type the pattern (smart case)"),
                Section::Command => Some("other keys type the command: n, p, x (first file) or q"),
                Section::Outline => Some("other keys type a filter"),
                Section::Hints => Some("other keys type a label"),
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

/// The default bindings as Markdown tables, one per section: for the
/// README, and with `names` the action names for `[pager.keys]` too.
pub fn markdown(names: bool) -> String {
    let mut out = String::new();
    for section in Section::ALL {
        let _ = writeln!(out, "**{}**\n", section.title());
        out.push_str(if names {
            "| Keys | Action | Name |\n|---|---|---|\n"
        } else {
            "| Keys | Action |\n|---|---|\n"
        });
        for b in BINDINGS.iter().filter(|b| b.section == section) {
            let keys: Vec<String> = if b.command == Command::Count {
                vec!["`0`–`9`".to_owned()]
            } else {
                b.default_keys()
                    .map(|k| match seq_label(&k) {
                        l if l.contains('`') => format!("`` {l} ``"),
                        l => format!("`{l}`"),
                    })
                    .collect()
            };
            let _ = write!(
                out,
                "| {} | {} |",
                keys.join(" "),
                b.help.replace('|', "\\|")
            );
            if names && b.command != Command::Count {
                let _ = write!(out, " `{}` |", b.name);
            } else if names {
                out.push_str(" |");
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn c(ch: char) -> Key {
        Key::char(ch)
    }

    fn k(code: KeyCode) -> Key {
        Key::plain(code)
    }

    fn ctrl(ch: char) -> Key {
        Key::ctrl(ch)
    }

    fn lookup(context: Context, key: Key) -> Option<Command> {
        Keymap::default().command(context, key)
    }

    fn overrides(pairs: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        pairs
            .iter()
            .map(|(a, ks)| (a.to_string(), ks.iter().map(|k| k.to_string()).collect()))
            .collect()
    }

    #[test]
    fn every_default_key_parses() {
        for b in BINDINGS {
            for key in b.keys.split(',') {
                assert!(parse_keys(key).is_ok(), "{} {key:?}", b.name);
            }
        }
    }

    #[test]
    fn keys_are_unique_per_context() {
        let map = Keymap::default();
        let mut seen = HashSet::new();
        for (b, seq) in &map.keys {
            assert!(
                seen.insert((b.section.context(), seq.clone())),
                "{seq:?} is bound twice in {:?}",
                b.section.context()
            );
        }
        // Keys that work everywhere are bound nowhere else.
        let global: Vec<&Vec<Key>> = map
            .keys
            .iter()
            .filter(|(b, _)| GLOBAL.contains(&b.command))
            .map(|(_, seq)| seq)
            .collect();
        for (b, seq) in &map.keys {
            assert!(
                GLOBAL.contains(&b.command) || !global.contains(&seq),
                "{} takes a global key",
                b.name
            );
        }
        for context in [
            Context::Visual,
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
    fn every_binding_has_keys_help_and_a_unique_name() {
        let mut names = HashSet::new();
        for b in BINDINGS {
            assert!(!b.keys.is_empty(), "{:?}", b.command);
            assert!(!b.help.is_empty(), "{:?}", b.command);
            assert!(
                b.name.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{} is kebab-case",
                b.name
            );
            if b.command != Command::Count {
                assert!(names.insert(b.name), "{} twice", b.name);
            }
        }
        for name in ["page-down", "hints-follow", "visual-line", "yank-hints"] {
            assert!(names.contains(name), "{name}");
        }
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
            (k(KeyCode::PageDown), C::PageDown),
            (ctrl('f'), C::PageDown),
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
            (c('m'), C::SetMark),
            (c('\''), C::JumpMark),
            (c('/'), C::SearchForward),
            (c('?'), C::SearchBackward),
            (c('n'), C::NextMatch),
            (c('N'), C::PrevMatch),
            (k(KeyCode::Tab), C::FocusNext),
            (k(KeyCode::BackTab), C::FocusPrev),
            (k(KeyCode::Enter), C::Follow),
            (c('o'), C::LinkHints),
            (c('f'), C::FollowHints),
            (c('y'), C::YankHints),
            (c('v'), C::VisualHints),
            (c('V'), C::VisualLine),
            (c('e'), C::Edit),
            (k(KeyCode::Backspace), C::Back),
            (c('H'), C::Back),
            (c('L'), C::Forward),
            (c('r'), C::Reload),
            (c('R'), C::ToggleWatch),
            (c('i'), C::CycleImages),
            (c('w'), C::ToggleWidth),
            (c('M'), C::ToggleMouse),
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
        assert_eq!(lookup(Context::Normal, c('z')), None, "only a prefix");
        let map = Keymap::default();
        assert_eq!(
            map.lookup(Context::Normal, &[c('g')]),
            Lookup {
                exact: Some(C::Top),
                longer: true
            }
        );
        assert_eq!(
            map.lookup(Context::Normal, &[c('z'), c('t')]).exact,
            Some(C::ScrollTop)
        );
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
        assert_eq!(lookup(Context::Help, c('f')), Some(C::HelpPageDown));
        assert_eq!(lookup(Context::Visual, c('j')), Some(C::LineDown));
        assert_eq!(lookup(Context::Visual, c('y')), Some(C::VisualYank));
        assert_eq!(lookup(Context::Visual, c('Y')), Some(C::VisualYankText));
        assert_eq!(
            lookup(Context::Visual, k(KeyCode::Esc)),
            Some(C::VisualExit)
        );
        assert_eq!(
            lookup(Context::Command, k(KeyCode::Enter)),
            Some(C::CommandRun)
        );
        assert_eq!(lookup(Context::Command, c('n')), None, "n types");
    }

    #[test]
    fn key_syntax() {
        let p = parse_keys;
        assert_eq!(p("j"), Ok(vec![c('j')]));
        assert_eq!(p("G"), Ok(vec![c('G')]));
        assert_eq!(p("'"), Ok(vec![c('\'')]));
        assert_eq!(p("-"), Ok(vec![c('-')]));
        assert_eq!(p("C-x"), Ok(vec![ctrl('x')]));
        assert_eq!(
            p("C-X"),
            Ok(vec![ctrl('x')]),
            "control letters are lowercase"
        );
        assert_eq!(
            p("M-x"),
            Ok(vec![Key {
                code: KeyCode::Char('x'),
                mods: Mods::ALT
            }])
        );
        assert_eq!(p("S-Tab"), Ok(vec![k(KeyCode::BackTab)]));
        assert_eq!(p("S-a"), Ok(vec![c('A')]));
        assert_eq!(p("Enter"), Ok(vec![k(KeyCode::Enter)]));
        assert_eq!(p("esc"), Ok(vec![k(KeyCode::Esc)]), "names ignore case");
        assert_eq!(p("Space"), Ok(vec![c(' ')]));
        assert_eq!(p("PgDn"), Ok(vec![k(KeyCode::PageDown)]));
        assert_eq!(p("PageDown"), Ok(vec![k(KeyCode::PageDown)]));
        assert_eq!(p("Up"), Ok(vec![k(KeyCode::Up)]));
        assert_eq!(p("F12"), Ok(vec![k(KeyCode::F(12))]));
        assert_eq!(p("gg"), Ok(vec![c('g'), c('g')]));
        assert_eq!(p("zt"), Ok(vec![c('z'), c('t')]));
        assert_eq!(p("g Home"), Ok(vec![c('g'), k(KeyCode::Home)]));
        assert_eq!(p("C-w j"), Ok(vec![ctrl('w'), c('j')]));
        assert_eq!(
            p("PgDwn"),
            Err("unknown key `PgDwn` (did you mean `PgDn`?)".into())
        );
        assert_eq!(p("Q-x"), Err("unknown key `Q-x`".into()));
        assert_eq!(p("C-C-x"), Err("unknown key `C-C-x`".into()));
        assert!(p("").unwrap_err().contains("Space"));
        assert!(p("   ").is_err());
        assert_eq!(
            p("F25"),
            Ok(vec![c('F'), c('2'), c('5')]),
            "no such key: characters"
        );
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
        assert_eq!(seq_label(&[c('g'), c('g')]), "gg");
        assert_eq!(seq_label(&[ctrl('w'), c('j')]), "^W j");
        let b = BINDINGS
            .iter()
            .find(|b| b.command == Command::Count)
            .unwrap();
        assert_eq!(keys_label(b), "0-9");
        let top = BINDINGS.iter().find(|b| b.name == "top").unwrap();
        assert_eq!(keys_label(top), "gg g Home <");
    }

    #[test]
    fn help_covers_every_binding() {
        let lines = Keymap::default().help_lines();
        let entries = lines
            .iter()
            .filter(|l| matches!(l, HelpLine::Entry { keys, .. } if !keys.is_empty()))
            .count();
        assert_eq!(entries, BINDINGS.len());
        assert_eq!(lines.first(), Some(&HelpLine::Title("Scrolling")));
        let md = markdown(false);
        assert!(md.contains("| `j` `↓` `^E` `^N` | down a line |"), "{md}");
        assert!(md.contains("| `0`–`9` |"));
        assert!(md.contains("| `gg` `g` `Home` `<` |"), "{md}");
        assert_eq!(md.matches("| Keys | Action |").count(), Section::ALL.len());
        let named = markdown(true);
        assert!(
            named.contains("| `Space` `PgDn` `^F` | down a page | `page-down` |"),
            "{named}"
        );
    }

    #[test]
    fn help_shows_the_keys_in_effect() {
        let (map, _) = Keymap::new(&overrides(&[("quit", &["Q", "ZZ"])]));
        let lines = map.help_lines();
        assert!(
            lines.contains(&HelpLine::Entry {
                keys: "Q ZZ".into(),
                help: "quit"
            }),
            "{lines:?}"
        );
    }

    #[test]
    fn remapping() {
        // New keys replace the defaults.
        let (map, issues) = Keymap::new(&overrides(&[("page-down", &["x", "C-v"])]));
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(
            map.command(Context::Normal, c('x')),
            Some(Command::PageDown)
        );
        assert_eq!(
            map.command(Context::Normal, ctrl('v')),
            Some(Command::PageDown)
        );
        assert_eq!(map.command(Context::Normal, c(' ')), None, "Space is gone");
        // Sequences, and no keys at all.
        let (map, issues) = Keymap::new(&overrides(&[("quit", &["ZZ"]), ("help", &[])]));
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(
            map.lookup(Context::Normal, &[c('Z'), c('Z')]).exact,
            Some(Command::Quit)
        );
        assert_eq!(map.command(Context::Normal, c('q')), None);
        assert_eq!(map.command(Context::Normal, c('h')), None);
        // A default key taken by another action moves, with a warning.
        let (map, issues) = Keymap::new(&overrides(&[("page-down", &["f"])]));
        assert_eq!(
            map.command(Context::Normal, c('f')),
            Some(Command::PageDown)
        );
        assert_eq!(
            issues,
            [KeyIssue {
                action: "page-down".into(),
                message: "`f` is a default key of `hints-follow`, which loses it to `page-down`"
                    .into()
            }]
        );
        // Other contexts keep their keys.
        assert_eq!(
            map.command(Context::Help, c('f')),
            Some(Command::HelpPageDown)
        );
        // An action that moves away first leaves no conflict.
        let (_, issues) = Keymap::new(&overrides(&[
            ("page-down", &["f"]),
            ("hints-follow", &["F"]),
        ]));
        assert!(issues.is_empty(), "{issues:?}");
        // Bound twice in the config: the later one wins.
        let (map, issues) = Keymap::new(&overrides(&[("top", &["x"]), ("bottom", &["x"])]));
        assert_eq!(map.command(Context::Normal, c('x')), Some(Command::Bottom));
        assert_eq!(
            issues,
            [KeyIssue {
                action: "bottom".into(),
                message: "`x` is bound twice, to `top` and `bottom`: the later one, `bottom`, \
                          gets it"
                    .into()
            }]
        );
        // Keys that work everywhere clash everywhere.
        let (map, issues) = Keymap::new(&overrides(&[("help-close", &["C-l"])]));
        assert_eq!(
            map.command(Context::Help, ctrl('l')),
            Some(Command::HelpClose)
        );
        assert_eq!(map.command(Context::Normal, ctrl('l')), None);
        assert_eq!(issues.len(), 1, "{issues:?}");
        // Unknown actions and keys.
        let (map, issues) = Keymap::new(&overrides(&[
            ("pgae-down", &["x"]),
            ("count", &["x"]),
            ("top", &["PgDwn", "T"]),
        ]));
        let messages: Vec<&str> = issues.iter().map(|i| i.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "unknown action `pgae-down` (did you mean `page-down`?)",
                "the count digits cannot be changed",
                "unknown key `PgDwn` (did you mean `PgDn`?)",
            ]
        );
        assert_eq!(map.command(Context::Normal, c('T')), Some(Command::Top));
        assert_eq!(map.command(Context::Normal, c('x')), None);
    }
}
