//! Snapshot of the environment variables relevant to terminal detection.
//!
//! Detection code never reads the process environment directly: it takes an
//! [`Env`], so every decision is a pure function that tests can drive with
//! recorded values.

use std::collections::BTreeMap;

/// Variables captured by [`Env::from_process`].
pub const VARS: &[&str] = &[
    "TERM",
    "COLORTERM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "TMUX",
    "SSH_CONNECTION",
    "SSH_TTY",
    "NO_COLOR",
    "FORCE_COLOR",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "COLORFGBG",
    "KITTY_WINDOW_ID",
    "WEZTERM_EXECUTABLE",
    "GHOSTTY_RESOURCES_DIR",
    "WT_SESSION",
    "VTE_VERSION",
    "KONSOLE_VERSION",
    "VSCODE_INJECTION",
    "COLUMNS",
    "XDG_RUNTIME_DIR",
    "DISPLAY",
    "WAYLAND_DISPLAY",
];

/// An immutable snapshot of environment variables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<String, String>,
}

impl Env {
    /// Capture [`VARS`] from the current process.
    pub fn from_process() -> Env {
        let vars = VARS
            .iter()
            .filter_map(|&k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
            .collect();
        Env { vars }
    }

    /// Build a snapshot from explicit pairs (tests, recorded fixtures).
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Env {
        Env {
            vars: pairs
                .iter()
                .map(|&(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// The value of a variable, if set.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    /// The value of a variable if it is set and non-empty.
    pub fn non_empty(&self, key: &str) -> Option<&str> {
        self.get(key).filter(|v| !v.is_empty())
    }

    /// Whether a variable is set to a non-empty value.
    pub fn is_set(&self, key: &str) -> bool {
        self.non_empty(key).is_some()
    }

    /// All captured variables (for `--doctor`).
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_and_lookup() {
        let e = Env::from_pairs(&[("TERM", "tmux-256color"), ("NO_COLOR", "")]);
        assert_eq!(e.get("TERM"), Some("tmux-256color"));
        assert_eq!(e.get("NO_COLOR"), Some(""));
        assert!(!e.is_set("NO_COLOR"));
        assert!(e.is_set("TERM"));
        assert_eq!(e.get("COLORTERM"), None);
    }
}
