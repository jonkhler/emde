//! Checks of colour names and code themes across all layers.
//!
//! Colour names in styles and palettes can only be resolved once every
//! layer is known (a user style may use a colour from the theme's palette,
//! and the other way round), so these checks run after all documents are
//! read. Each problem is reported at the line that caused it.

use std::collections::{BTreeMap, BTreeSet};

use super::de::Parsed;
use super::layer::ConfigLayer;
use super::suggest::did_you_mean;
use super::{Diagnostic, Severity};
use crate::theme::Variant;
use crate::theme::color::{
    ColorSpec, PaletteProblem, ansi_index, ansi_names, known_names, name_resolves, resolve_palette,
};
use crate::theme::spec::{PaletteTable, StyleTable, ThemeFile, ThemePatch};

/// The theme tables of one document.
struct Tables<'a> {
    palette: &'a PaletteTable,
    style: &'a StyleTable,
    dark: &'a StyleTable,
    light: &'a StyleTable,
}

impl<'a> Tables<'a> {
    fn of_theme(f: &'a ThemeFile) -> Tables<'a> {
        Tables {
            palette: &f.palette,
            style: &f.style,
            dark: &f.dark.style,
            light: &f.light.style,
        }
    }

    fn of_config(l: &'a ConfigLayer) -> Tables<'a> {
        Tables {
            palette: &l.palette,
            style: &l.style,
            dark: &l.dark.style,
            light: &l.light.style,
        }
    }
}

/// The variants a table entry applies to.
fn variants(only: Option<Variant>) -> Vec<Variant> {
    only.map_or_else(|| Variant::ALL.to_vec(), |v| vec![v])
}

/// Warn about colour names that resolve in no palette they apply to, and
/// about palette entries that refer to each other in a loop.
pub(super) fn check_colours(
    theme_docs: &[Parsed<ThemeFile>],
    user_docs: &[Parsed<ConfigLayer>],
    theme_patch: &ThemePatch,
    diags: &mut Vec<Diagnostic>,
) {
    // The palettes once every layer is merged.
    let mut palettes = theme_patch.palette.clone();
    for doc in user_docs {
        for (only, key, spec) in doc.value.palette.entries() {
            for v in variants(only) {
                if let Some(p) = palettes.get_mut(v.index()) {
                    p.insert(key.clone(), spec.clone());
                }
            }
        }
    }
    let known: [BTreeSet<String>; 2] = palettes.clone().map(|p| p.into_keys().collect());
    for doc in theme_docs {
        let items = check_tables(&Tables::of_theme(&doc.value), &known);
        doc.report(Severity::Warning, items, diags);
    }
    for doc in user_docs {
        let items = check_tables(&Tables::of_config(&doc.value), &known);
        doc.report(Severity::Warning, items, diags);
    }
    check_palette_loops(&palettes, theme_docs, user_docs, diags);
}

/// Unresolvable colour names in one document's tables, with their paths.
fn check_tables<'a>(
    tables: &Tables<'a>,
    known: &[BTreeSet<String>; 2],
) -> Vec<(Vec<&'a str>, String)> {
    let mut out = Vec::new();
    let styles = [
        (None, tables.style),
        (Some(Variant::Dark), tables.dark),
        (Some(Variant::Light), tables.light),
    ];
    for (only, table) in styles {
        for (element, spec) in &table.0 {
            for (field, color) in spec.colors() {
                let Some(name) = color.name() else { continue };
                let missing: Vec<Variant> = variants(only)
                    .into_iter()
                    .filter(|v| known.get(v.index()).is_none_or(|k| !name_resolves(name, k)))
                    .collect();
                let Some(first) = missing.first() else {
                    continue;
                };
                let mut path: Vec<&str> = variant_prefix(only);
                path.extend(["style", element.name(), field]);
                let pool = known
                    .get(first.index())
                    .map(known_names)
                    .unwrap_or_default();
                let message = format!(
                    "{}: {}",
                    path.join("."),
                    unknown_colour(name, &missing, only, &pool)
                );
                out.push((path, message));
            }
        }
    }
    for (only, key, spec) in tables.palette.entries() {
        let mut path = vec!["palette"];
        path.extend(variant_prefix(only));
        path.push(key);
        let message = match spec {
            ColorSpec::Tint { .. } => Some("tints can only be used in styles".to_owned()),
            ColorSpec::Name(name) if name == key => ansi_index(name).is_none().then(|| {
                format!("unknown colour `{name}` (a name equal to its key must be an ANSI colour)")
            }),
            ColorSpec::Name(name) => {
                let missing: Vec<Variant> = variants(only)
                    .into_iter()
                    .filter(|v| {
                        known
                            .get(v.index())
                            .is_none_or(|k| !k.contains(name.as_str()))
                            && ansi_index(name).is_none()
                    })
                    .collect();
                missing.first().map(|first| {
                    let pool: Vec<String> = known
                        .get(first.index())
                        .map(|k| k.iter().cloned().chain(ansi_names()).collect())
                        .unwrap_or_default();
                    unknown_colour(name, &missing, only, &pool)
                })
            }
            _ => None,
        };
        if let Some(message) = message {
            let message = format!("{}: {message}", path.join("."));
            out.push((path, message));
        }
    }
    out
}

/// `["dark"]`, `["light"]` or nothing.
fn variant_prefix(only: Option<Variant>) -> Vec<&'static str> {
    match only {
        Some(Variant::Dark) => vec!["dark"],
        Some(Variant::Light) => vec!["light"],
        None => Vec::new(),
    }
}

fn unknown_colour(
    name: &str,
    missing: &[Variant],
    only: Option<Variant>,
    pool: &[String],
) -> String {
    let mut msg = format!("unknown colour `{name}`");
    if only.is_none() && missing.len() == 1 {
        let v = match missing.first() {
            Some(Variant::Light) => "light",
            _ => "dark",
        };
        msg.push_str(&format!(" in the {v} palette"));
    }
    if let Some(s) = did_you_mean(name, pool.iter().map(String::as_str)) {
        msg.push_str(&format!(" (did you mean `{s}`?)"));
    }
    msg
}

/// Report palette entries that refer to each other in a loop, at the entry
/// that starts it (in the highest layer that defines it).
fn check_palette_loops(
    palettes: &[BTreeMap<String, ColorSpec>; 2],
    theme_docs: &[Parsed<ThemeFile>],
    user_docs: &[Parsed<ConfigLayer>],
    diags: &mut Vec<Diagnostic>,
) {
    let mut seen = BTreeSet::new();
    for pal in palettes {
        for problem in resolve_palette(pal).1 {
            let key = match &problem {
                PaletteProblem::Cycle(keys) => keys.first().cloned().unwrap_or_default(),
                PaletteProblem::TooDeep(key) => key.clone(),
                _ => continue,
            };
            let message = problem.to_string();
            if !seen.insert(message.clone()) {
                continue;
            }
            let defines = |p: &PaletteTable| -> Option<Vec<&'static str>> {
                if p.both.contains_key(&key) {
                    Some(vec![])
                } else if p.dark.contains_key(&key) {
                    Some(vec!["dark"])
                } else if p.light.contains_key(&key) {
                    Some(vec!["light"])
                } else {
                    None
                }
            };
            let located = user_docs
                .iter()
                .rev()
                .find_map(|d| Some(d.location(&palette_path(defines(&d.value.palette)?, &key))))
                .or_else(|| {
                    theme_docs.iter().find_map(|d| {
                        Some(d.location(&palette_path(defines(&d.value.palette)?, &key)))
                    })
                })
                .unwrap_or_else(|| "palette".to_owned());
            diags.push(Diagnostic {
                severity: Severity::Warning,
                location: located,
                message,
            });
        }
    }
}

fn palette_path<'a>(prefix: Vec<&'static str>, key: &'a str) -> Vec<&'a str> {
    let mut path = vec!["palette"];
    path.extend(prefix);
    path.push(key);
    path
}

/// Check that the configured code themes exist.
pub(super) fn check_code_themes(
    theme_docs: &[Parsed<ThemeFile>],
    user_docs: &[Parsed<ConfigLayer>],
    diags: &mut Vec<Diagnostic>,
) {
    for doc in user_docs {
        let code = doc.value.theme.code.as_deref();
        let Some(code) = code.filter(|c| !c.trim().eq_ignore_ascii_case("auto")) else {
            continue;
        };
        if let Err(message) = crate::highlight::check_code_theme(code) {
            let text = format!("theme.code: {message}");
            diags.push(doc.diagnostic(Severity::Warning, &["theme", "code"], text));
        }
    }
    for doc in theme_docs {
        let Some(code) = &doc.value.code else {
            continue;
        };
        let mut names: Vec<&str> = Variant::ALL.iter().filter_map(|&v| code.get(v)).collect();
        names.dedup();
        for name in names {
            if let Err(message) = crate::highlight::check_code_theme(name) {
                let text = format!("code: {message}");
                diags.push(doc.diagnostic(Severity::Warning, &["code"], text));
            }
        }
    }
}
