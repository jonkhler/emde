//! Fence-token aliases: common info-string words that the syntax set does
//! not know by that name (`console`, `jsonc`, `golang`, …).
//!
//! A token is first mapped through the user's `[code.aliases]`, then
//! through [`BUILTIN`]; the result is looked up as a syntax extension or
//! name. [`PLAIN`] means "show as plain text".

/// The token that means "no highlighting".
pub const PLAIN: &str = "plain";

/// Built-in aliases: `(fence token, syntax token)`.
pub const BUILTIN: &[(&str, &str)] = &[
    // Shell sessions and dialects.
    ("console", "bash"),
    ("shell", "bash"),
    ("sh", "bash"),
    ("zsh", "bash"),
    ("ksh", "bash"),
    ("shell-session", "bash"),
    ("shellsession", "bash"),
    // JSON dialects.
    ("jsonc", "json"),
    ("json5", "json"),
    ("jsonl", "json"),
    ("ndjson", "json"),
    ("yml", "yaml"),
    // Plain text, and languages the pure-Rust syntax set cannot highlight.
    ("text", PLAIN),
    ("txt", PLAIN),
    ("plaintext", PLAIN),
    ("ps", PLAIN),
    ("pwsh", PLAIN),
    ("powershell", PLAIN),
    ("mermaid", PLAIN),
    // Common names that are neither an extension nor a syntax name.
    ("golang", "go"),
    ("node", "js"),
    ("jsx", "js"),
    ("python3", "py"),
    ("objc", "objective-c"),
    ("csharp", "cs"),
    ("fsharp", "fs"),
    ("docker", "dockerfile"),
    ("terraform", "tf"),
    ("viml", "vim"),
    ("postgres", "sql"),
    ("postgresql", "sql"),
    ("psql", "sql"),
    ("mysql", "sql"),
    ("sqlite", "sql"),
    ("batch", "bat"),
    ("fortran", "f90"),
    ("scheme", "scm"),
    ("elisp", "el"),
    ("emacs-lisp", "el"),
    ("regex", "re"),
    ("graphviz", "dot"),
];

/// Normalise an info-string word: lowercase, without Pandoc's `{.lang}`
/// braces and dot, and without rustdoc's `,attributes`.
pub fn normalize(token: &str) -> String {
    let t = token.trim();
    let t = t.strip_prefix('{').unwrap_or(t);
    let t = t.strip_prefix('.').unwrap_or(t);
    let end = t.find([',', '}', ' ', '\t']).unwrap_or(t.len());
    t.get(..end).unwrap_or(t).to_lowercase()
}

/// Map a fence token through the user's aliases, then the built-in ones.
/// User alias keys are matched case-insensitively.
pub fn resolve(token: &str, user: &[(String, String)]) -> String {
    let token = normalize(token);
    let token = user
        .iter()
        .find(|(from, _)| from.eq_ignore_ascii_case(&token))
        .map_or(token, |(_, to)| normalize(to));
    BUILTIN
        .iter()
        .find(|(from, _)| *from == token)
        .map_or(token, |(_, to)| (*to).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation() {
        assert_eq!(normalize("Rust"), "rust");
        assert_eq!(normalize(" rust,ignore "), "rust");
        assert_eq!(normalize("{.python .numberLines}"), "python");
        assert_eq!(normalize("{r}"), "r");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn builtin_aliases() {
        assert_eq!(resolve("console", &[]), "bash");
        assert_eq!(resolve("JSONC", &[]), "json");
        assert_eq!(resolve("text", &[]), PLAIN);
        assert_eq!(resolve("rust", &[]), "rust");
    }

    #[test]
    fn user_aliases_come_first_and_chain_into_builtins() {
        let user = vec![
            ("Foo".to_owned(), "sh".to_owned()),
            ("console".to_owned(), "text".to_owned()),
        ];
        assert_eq!(resolve("foo", &user), "bash", "user alias, then built-in");
        assert_eq!(resolve("console", &user), PLAIN, "user alias wins");
    }

    #[test]
    fn table_is_well_formed() {
        for (i, (from, to)) in BUILTIN.iter().enumerate() {
            assert_eq!(normalize(from), *from);
            assert!(!to.is_empty());
            assert!(
                BUILTIN[..i].iter().all(|(f, _)| f != from),
                "duplicate alias {from}"
            );
            assert!(
                BUILTIN.iter().all(|(f, _)| f != to),
                "alias target {to} is itself an alias"
            );
        }
    }
}
