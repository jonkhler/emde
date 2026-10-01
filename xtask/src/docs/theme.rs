//! The theme reference: the built-in themes, the default palette, and every
//! styleable element with its parent and its style in the default theme.
//!
//! Element names and parents come from `emde::theme::Element`; the
//! descriptions are written here, in [`ELEMENT_GROUPS`], and must cover
//! exactly the elements emde has. Themes, palettes and styles are read from
//! `assets/themes/*.toml`.

use std::collections::BTreeSet;
use std::path::Path;

use emde::theme::{Element, builtin};

use super::{markdown, read};

/// The elements by group, each with a description. Every element appears
/// exactly once.
const ELEMENT_GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Text and headings",
        &[
            (
                "text",
                "Body text: the root every other element inherits from.",
            ),
            ("heading", "All headings."),
            (
                "h1",
                "Level-1 headings: the full-width bar (its `bg`, fading to `bg_to`), \
                 or styled text with `heading.h1 = \"underline\"` or `\"plain\"`.",
            ),
            ("h2", "Level-2 headings."),
            ("h3", "Level-3 headings."),
            ("h4", "Level-4 headings."),
            ("h5", "Level-5 headings."),
            ("h6", "Level-6 headings."),
            (
                "heading_rule",
                "Heading decorations: the rule under h2, markers and numbers.",
            ),
        ],
    ),
    (
        "Inline",
        &[
            ("emph", "*Emphasis*."),
            ("strong", "**Strong** text."),
            ("strike", "~~Strikethrough~~."),
            ("mark", "`<mark>` highlights."),
            ("kbd", "`<kbd>` keys."),
            ("link", "Links."),
            (
                "link_url",
                "URLs shown as text: numbered references, and links where OSC 8 is off.",
            ),
            (
                "link_focus",
                "The link focused in the pager (Tab, Shift-Tab).",
            ),
            ("link_ref", "`[1]`-style link reference numbers."),
            ("footnote_ref", "Footnote references and back-links."),
        ],
    ),
    (
        "Code",
        &[
            ("code", "Code: inline code and code blocks."),
            ("code_inline", "Inline code."),
            ("code_block", "Code blocks (the panel)."),
            (
                "code_label",
                "The title and language label of a code block.",
            ),
            ("code_gutter", "Line numbers, wrap markers and code frames."),
        ],
    ),
    (
        "Blocks",
        &[
            ("quote", "Block quotes."),
            ("quote_bar", "The bar left of quotes and alerts."),
            ("alert", "GitHub alerts: the five kinds below."),
            ("alert_note", "`> [!NOTE]`."),
            ("alert_tip", "`> [!TIP]`."),
            ("alert_important", "`> [!IMPORTANT]`."),
            ("alert_warning", "`> [!WARNING]`."),
            ("alert_caution", "`> [!CAUTION]`."),
            ("list_marker", "List bullets and numbers."),
            ("task_done", "The marker of a done task."),
            ("task_todo", "The marker of an open task."),
            ("table_border", "Table borders."),
            ("table_header", "Table header rows."),
            (
                "table_zebra",
                "The background of every other table row (24-bit colour only).",
            ),
            ("rule", "Horizontal rules."),
            ("footnote", "The footnotes section."),
            ("def_term", "Terms of definition lists."),
            ("front_matter_key", "Keys of the front matter card."),
            ("front_matter_value", "Values of the front matter card."),
            ("html", "HTML shown as text (`render.html = \"raw\"`)."),
        ],
    ),
    (
        "Images",
        &[
            ("image_alt", "The alt text of an image that is not shown."),
            ("image_caption", "Figure captions."),
            (
                "image_frame",
                "The box an image is drawn in while it loads or when it cannot be shown, \
                 and the image chip glyph.",
            ),
        ],
    ),
    (
        "Math",
        &[
            ("math", "Math: every role below."),
            ("math_var", "Variables."),
            ("math_num", "Numbers."),
            ("math_op", "Operators (`+`, `∑`)."),
            ("math_rel", "Relations (`=`, `≤`, `→`)."),
            ("math_func", "Function names (`sin`, `lim`)."),
            ("math_text", "`\\text{…}` and equation tags."),
            ("math_delim", "Delimiters, fraction bars and radical signs."),
            (
                "math_error",
                "TeX that could not be rendered, shown as written.",
            ),
        ],
    ),
    (
        "Decorations and the pager",
        &[
            (
                "muted",
                "De-emphasised decorations: labels, borders, rules.",
            ),
            ("status", "The pager's status bar."),
            ("status_msg", "Messages in the status bar."),
            ("search_match", "Search matches."),
            ("search_current", "The current search match."),
            ("prompt", "The search prompt."),
            ("hint", "Link hint labels (`o`)."),
            ("toc", "The outline (`t`)."),
            ("toc_current", "The current section in the outline."),
        ],
    ),
];

/// The order style fields are shown in (`StyleSpec`'s field order).
const STYLE_FIELDS: &[&str] = &[
    "fg",
    "bg",
    "bg_to",
    "underline",
    "underline_color",
    "bold",
    "italic",
    "dim",
    "strikethrough",
    "reverse",
    "overline",
];

/// The built-in theme files, parsed.
pub(crate) struct Themes {
    /// `(name, the file's text, the file parsed)`, in listing order.
    files: Vec<(&'static str, String, toml::Table)>,
}

impl Themes {
    /// Read every built-in theme from `assets/themes/`.
    pub(crate) fn load(root: &Path) -> Result<Themes, String> {
        let mut files = Vec::new();
        for name in builtin::names() {
            let path = root.join("assets/themes").join(format!("{name}.toml"));
            let text = read(&path)?;
            let table = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            files.push((name, text, table));
        }
        Ok(Themes { files })
    }

    /// The default theme.
    fn default_theme(&self) -> Result<&toml::Table, String> {
        self.files
            .iter()
            .find(|(n, _, _)| *n == builtin::DEFAULT)
            .map(|(_, _, t)| t)
            .ok_or_else(|| format!("no built-in theme `{}`", builtin::DEFAULT))
    }

    /// A table of the built-in themes, each described by the first line of
    /// its file (`# NAME: description`).
    pub(crate) fn builtin_markdown(&self) -> Result<String, String> {
        let mut rows = Vec::new();
        for (name, text, _) in &self.files {
            let about = text
                .lines()
                .next()
                .and_then(|l| l.strip_prefix(&format!("# {name}: ")))
                .ok_or_else(|| {
                    format!("assets/themes/{name}.toml must start with `# {name}: <description>`")
                })?;
            rows.push(vec![
                markdown::code(name),
                markdown::text(&capitalised(about)),
            ]);
        }
        Ok(markdown::table(&["Theme", "What it is"], &rows))
    }

    /// The default theme's palette, with both variants.
    pub(crate) fn palette_markdown(&self) -> Result<String, String> {
        let theme = self.default_theme()?;
        let variant = |v: &str| {
            theme
                .get("palette")
                .and_then(|p| p.get(v))
                .and_then(toml::Value::as_table)
                .ok_or_else(|| format!("the default theme has no [palette.{v}]"))
        };
        let (dark, light) = (variant("dark")?, variant("light")?);
        let names: Vec<&String> = dark.keys().collect();
        if names != light.keys().collect::<Vec<_>>() {
            return Err(
                "the default theme's dark and light palettes name different colours".into(),
            );
        }
        let value = |t: &toml::Table, k: &str| {
            t.get(k)
                .and_then(scalar)
                .map(|v| markdown::code(&v))
                .ok_or_else(|| format!("palette entry `{k}` is missing or not a colour"))
        };
        let mut rows = Vec::new();
        for name in names {
            rows.push(vec![
                markdown::code(name),
                value(dark, name)?,
                value(light, name)?,
            ]);
        }
        Ok(markdown::table(&["Name", "Dark", "Light"], &rows))
    }

    /// Every element, by group: its parent, what it styles, and its style in
    /// the default theme.
    pub(crate) fn elements_markdown(&self) -> Result<String, String> {
        check_groups()?;
        let theme = self.default_theme()?;
        let mut out = String::new();
        for (group, elements) in ELEMENT_GROUPS {
            out.push_str(&format!("#### {group}\n\n"));
            let mut rows = Vec::new();
            for &(name, about) in *elements {
                let element = Element::from_name(name)
                    .ok_or_else(|| format!("`{name}` is not an element"))?;
                let parent = element
                    .parent()
                    .map_or_else(|| "—".to_owned(), |p| markdown::code(p.name()));
                rows.push(vec![
                    markdown::code(name),
                    parent,
                    about.to_owned(),
                    default_style(theme, name)?,
                ]);
            }
            out.push_str(&markdown::table(
                &["Element", "Inherits from", "Styles", "In the emde theme"],
                &rows,
            ));
            out.push('\n');
        }
        Ok(out)
    }
}

/// [`ELEMENT_GROUPS`] must name every element exactly once.
fn check_groups() -> Result<(), String> {
    let mut described = BTreeSet::new();
    for (_, elements) in ELEMENT_GROUPS {
        for (name, _) in *elements {
            if !described.insert(*name) {
                return Err(format!("element `{name}` is described twice"));
            }
        }
    }
    let actual: BTreeSet<&str> = Element::ALL.iter().map(|e| e.name()).collect();
    let missing: Vec<&&str> = actual.difference(&described).collect();
    let unknown: Vec<&&str> = described.difference(&actual).collect();
    if missing.is_empty() && unknown.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "describe every element in xtask/src/docs/theme.rs: missing {missing:?}, \
             not elements {unknown:?}"
        ))
    }
}

/// An element's style in a theme: `[style.NAME]`, and the variant tables
/// when they differ. "—" when the theme leaves it to the parent.
fn default_style(theme: &toml::Table, name: &str) -> Result<String, String> {
    let style_of = |prefix: &[&str]| -> Result<Option<String>, String> {
        let mut t = theme;
        for key in prefix {
            match t.get(*key).and_then(toml::Value::as_table) {
                Some(next) => t = next,
                None => return Ok(None),
            }
        }
        t.get(name)
            .map(|v| {
                v.as_table()
                    .ok_or_else(|| format!("[{}.{name}] is not a table", prefix.join(".")))
                    .and_then(describe_style)
            })
            .transpose()
    };
    let both = style_of(&["style"])?;
    let dark = style_of(&["dark", "style"])?;
    let light = style_of(&["light", "style"])?;
    let mut parts = Vec::new();
    if let Some(s) = both {
        parts.push(markdown::code(&s));
    }
    for (variant, s) in [("dark", dark), ("light", light)] {
        if let Some(s) = s {
            parts.push(format!("{variant}: {}", markdown::code(&s)));
        }
    }
    Ok(if parts.is_empty() {
        "—".to_owned()
    } else {
        parts.join("; ")
    })
}

/// A style table in one line: `fg=accent bold`.
fn describe_style(style: &toml::Table) -> Result<String, String> {
    if let Some(unknown) = style.keys().find(|k| !STYLE_FIELDS.contains(&k.as_str())) {
        return Err(format!("unknown style field `{unknown}`"));
    }
    let mut parts = Vec::new();
    for &field in STYLE_FIELDS {
        match style.get(field) {
            None => {}
            Some(toml::Value::Boolean(true)) => parts.push(field.to_owned()),
            Some(v) => {
                let v = scalar(v).ok_or_else(|| format!("`{field}` is not a plain value"))?;
                parts.push(format!("{field}={v}"));
            }
        }
    }
    Ok(parts.join(" "))
}

/// A string, number or boolean as text (strings without quotes).
fn scalar(v: &toml::Value) -> Option<String> {
    match v {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

/// `s` with its first letter in upper case.
fn capitalised(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_element_is_described_once() {
        check_groups().unwrap();
    }

    #[test]
    fn styles_in_one_line() {
        let t: toml::Table =
            toml::from_str("fg = \"accent\"\nbold = true\nitalic = false\nunderline = \"curly\"")
                .unwrap();
        assert_eq!(
            describe_style(&t).unwrap(),
            "fg=accent underline=curly bold italic=false"
        );
        let bad: toml::Table = toml::from_str("colour = \"red\"").unwrap();
        assert!(describe_style(&bad).is_err());
    }

    #[test]
    fn variant_styles() {
        let theme: toml::Table = toml::from_str(
            "[style.h1]\nbold = true\n[dark.style.h1]\nfg = \"a\"\n[light.style.h2]\nfg = \"b\"",
        )
        .unwrap();
        assert_eq!(default_style(&theme, "h1").unwrap(), "`bold`; dark: `fg=a`");
        assert_eq!(default_style(&theme, "h2").unwrap(), "light: `fg=b`");
        assert_eq!(default_style(&theme, "h3").unwrap(), "—");
    }

    #[test]
    fn the_real_themes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let themes = Themes::load(root).unwrap();
        let builtin = themes.builtin_markdown().unwrap();
        assert!(
            builtin.contains("| `emde` | The default theme. |"),
            "{builtin}"
        );
        let palette = themes.palette_markdown().unwrap();
        assert!(
            palette.contains("| `accent` | `#89b4fa` | `#1e66f5` |"),
            "{palette}"
        );
        let elements = themes.elements_markdown().unwrap();
        assert!(
            elements.contains("| `h2` | `heading` | Level-2 headings. | `fg=mauve bold` |"),
            "{elements}"
        );
        assert!(elements.contains("| `text` | — |"));
    }

    #[test]
    fn capitals() {
        assert_eq!(capitalised("the default theme."), "The default theme.");
        assert_eq!(capitalised(""), "");
    }
}
