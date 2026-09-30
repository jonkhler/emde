//! The built-in themes, embedded as TOML (`assets/themes/*.toml`).

/// Built-in theme names with their TOML sources, in listing order.
const BUILTIN: &[(&str, &str)] = &[
    ("emde", include_str!("../../assets/themes/emde.toml")),
    ("ansi", include_str!("../../assets/themes/ansi.toml")),
    ("mono", include_str!("../../assets/themes/mono.toml")),
];

/// The default theme.
pub const DEFAULT: &str = "emde";
/// The theme picked on 16-colour terminals.
pub const ANSI: &str = "ansi";
/// The theme picked under `NO_COLOR`.
pub const MONO: &str = "mono";

/// Names of the built-in themes.
pub fn names() -> impl Iterator<Item = &'static str> {
    BUILTIN.iter().map(|(n, _)| *n)
}

/// The canonical name and TOML source of a built-in theme.
pub(crate) fn get(name: &str) -> Option<(&'static str, &'static str)> {
    BUILTIN.iter().copied().find(|(n, _)| *n == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup() {
        assert_eq!(names().collect::<Vec<_>>(), [DEFAULT, ANSI, MONO]);
        assert!(get("emde").is_some_and(|(n, src)| n == "emde" && src.contains("name = \"emde\"")));
        assert!(get("Emde").is_none());
        assert!(get("nord").is_none());
    }
}
