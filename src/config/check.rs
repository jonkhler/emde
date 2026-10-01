//! Checks of colour names, code themes and `[code.aliases]` targets across
//! all layers.
//!
//! Colour names in styles and palettes can only be resolved once every
//! layer is known (a user style may use a colour from the theme's palette,
//! and the other way round), so these checks run after all documents are
//! read. Each problem is reported at the line that caused it, at most
//! [`MAX_REPORTED`](super::de::MAX_REPORTED) per document.

use std::collections::{BTreeMap, BTreeSet};

use super::de::{Deferred, Parsed};
use super::layer::ConfigLayer;
use super::suggest::did_you_mean;
use super::{Diagnostic, Severity};
use crate::theme::Variant;
use crate::theme::color::{
    ColorSpec, PaletteProblem, SURFACE, ansi_index, ansi_names, known_names, name_resolves,
    resolve_palette,
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

/// Warn about colour names that resolve in no palette they apply to, about
/// palette entries that refer to each other in a loop, and about user
/// palette entries no style can use.
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
    // Everything about one document is reported together, in line order.
    let (user_loops, theme_loops) = palette_loops(&palettes, theme_docs, user_docs);
    for (doc, loops) in theme_docs.iter().zip(theme_loops) {
        let mut items = check_tables(&Tables::of_theme(&doc.value), &known);
        items.extend(loops);
        doc.report(Severity::Warning, items, diags);
    }
    for (doc, loops) in user_docs.iter().zip(user_loops) {
        let mut items = check_tables(&Tables::of_config(&doc.value), &known);
        items.extend(unreachable_entries(&doc.value.palette));
        items.extend(loops);
        doc.report(Severity::Warning, items, diags);
    }
}

/// Palette entries that change no style: in styles, `surface` always means
/// the panel colour derived from the page background, so an entry of that
/// name (which the built-in themes keep for their own use) cannot be reached.
fn unreachable_entries(palette: &PaletteTable) -> Vec<Deferred<'_>> {
    palette
        .entries()
        .filter(|(_, key, _)| key.as_str() == SURFACE)
        .map(|(only, key, _)| {
            let path = palette_path(variant_prefix(only), key);
            let dotted = path.join(".");
            let message = move || {
                format!(
                    "{dotted}: has no effect: `surface` in styles is the panel colour emde \
                     derives from the page background (set `bg` in [style.<element>] tables \
                     instead)"
                )
            };
            (path, Box::new(message) as Box<dyn FnOnce() -> String>)
        })
        .collect()
}

/// Unresolvable colour names in one document's tables, with their paths.
/// Messages (and their suggestions) are only built for problems shown.
fn check_tables<'a>(tables: &Tables<'a>, known: &'a [BTreeSet<String>; 2]) -> Vec<Deferred<'a>> {
    let mut out: Vec<Deferred<'a>> = Vec::new();
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
                let Some(&first) = missing.first() else {
                    continue;
                };
                let mut path: Vec<&str> = variant_prefix(only);
                path.extend(["style", element.name(), field]);
                let dotted = path.join(".");
                let message = move || {
                    let pool = known
                        .get(first.index())
                        .map(known_names)
                        .unwrap_or_default();
                    format!("{dotted}: {}", unknown_colour(name, &missing, only, &pool))
                };
                out.push((path, Box::new(message)));
            }
        }
    }
    for (only, key, spec) in tables.palette.entries() {
        let path = palette_path(variant_prefix(only), key);
        let dotted = path.join(".");
        let message: Box<dyn FnOnce() -> String + 'a> = match spec {
            ColorSpec::Tint { .. } => {
                Box::new(move || format!("{dotted}: tints can only be used in styles"))
            }
            ColorSpec::Name(name) if name == key => {
                if ansi_index(name).is_some() {
                    continue;
                }
                Box::new(move || {
                    format!(
                        "{dotted}: unknown colour `{name}` (a name equal to its key must be an \
                         ANSI colour)"
                    )
                })
            }
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
                let Some(&first) = missing.first() else {
                    continue;
                };
                Box::new(move || {
                    let pool: Vec<String> = known
                        .get(first.index())
                        .map(|k| k.iter().cloned().chain(ansi_names()).collect())
                        .unwrap_or_default();
                    format!("{dotted}: {}", unknown_colour(name, &missing, only, &pool))
                })
            }
            _ => continue,
        };
        out.push((path, message));
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

/// Problems to report, one list per document.
type PerDocument<'a> = Vec<Vec<Deferred<'a>>>;

/// Palette entries that refer to each other in a loop, each at the entry
/// starting it, in the highest layer that defines it: one list of problems
/// per user document, and one per theme document.
fn palette_loops<'a>(
    palettes: &[BTreeMap<String, ColorSpec>; 2],
    theme_docs: &'a [Parsed<ThemeFile>],
    user_docs: &'a [Parsed<ConfigLayer>],
) -> (PerDocument<'a>, PerDocument<'a>) {
    let mut user_items: PerDocument<'a> = user_docs.iter().map(|_| Vec::new()).collect();
    let mut theme_items: PerDocument<'a> = theme_docs.iter().map(|_| Vec::new()).collect();
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
            let in_user = user_docs
                .iter()
                .enumerate()
                .rev()
                .find_map(|(i, d)| Some((i, defining_path(&d.value.palette, &key)?)));
            let target = match in_user {
                Some((i, path)) => user_items.get_mut(i).map(|items| (items, path)),
                None => theme_docs
                    .iter()
                    .enumerate()
                    .find_map(|(i, d)| Some((i, defining_path(&d.value.palette, &key)?)))
                    .and_then(|(i, path)| theme_items.get_mut(i).map(|items| (items, path))),
            };
            if let Some((items, path)) = target {
                items.push((path, Box::new(move || message)));
            }
        }
    }
    (user_items, theme_items)
}

/// The path of the entry `key` in a document's palette, if it has one.
fn defining_path<'a>(p: &'a PaletteTable, key: &str) -> Option<Vec<&'a str>> {
    let (prefix, own_key) = if let Some((k, _)) = p.both.get_key_value(key) {
        (vec![], k)
    } else if let Some((k, _)) = p.dark.get_key_value(key) {
        (vec!["dark"], k)
    } else {
        (vec!["light"], p.light.get_key_value(key)?.0)
    };
    Some(palette_path(prefix, own_key))
}

fn palette_path<'a>(prefix: Vec<&'static str>, key: &'a str) -> Vec<&'a str> {
    let mut path = vec!["palette"];
    path.extend(prefix);
    path.push(key);
    path
}

/// How far the checks of code themes and languages go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Checks {
    /// Cheap enough for every run: code theme names must exist and files
    /// must be there (the highlighter itself reports a file that does not
    /// load).
    Quick,
    /// For `--check-config`: `.tmTheme` files are loaded, and
    /// `[code.aliases]` targets are looked up in the syntax set.
    Thorough,
}

/// Check that the configured code themes exist.
pub(super) fn check_code_themes(
    theme_docs: &[Parsed<ThemeFile>],
    user_docs: &[Parsed<ConfigLayer>],
    how: Checks,
    diags: &mut Vec<Diagnostic>,
) {
    let check = |spec: &str| match how {
        Checks::Quick => crate::highlight::check_code_theme(spec),
        Checks::Thorough => crate::highlight::validate_code_theme(spec),
    };
    for doc in user_docs {
        let code = doc.value.theme.code.as_deref();
        let Some(code) = code.filter(|c| !c.trim().eq_ignore_ascii_case("auto")) else {
            continue;
        };
        if let Err(message) = check(code) {
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
            if let Err(message) = check(name) {
                let text = format!("code: {message}");
                diags.push(doc.diagnostic(Severity::Warning, &["code"], text));
            }
        }
    }
}

/// Check `[pager.keys]`: action names, key names, and keys bound to two
/// actions (each document on its own, against the default keys).
pub(super) fn check_keys(user_docs: &[Parsed<ConfigLayer>], diags: &mut Vec<Diagnostic>) {
    for doc in user_docs {
        let keys = &doc.value.pager.keys.0;
        if keys.is_empty() {
            continue;
        }
        let (_, issues) = crate::pager::keymap::Keymap::new(keys);
        let items: Vec<Deferred<'_>> = issues
            .into_iter()
            .map(|issue| {
                // The action as written in the document, for its line.
                let action = keys
                    .iter()
                    .find(|(a, _)| *a == issue.action)
                    .map_or("", |(a, _)| a.as_str());
                let path = vec!["pager", "keys", action];
                let message = move || format!("pager.keys.{}: {}", issue.action, issue.message);
                (path, Box::new(message) as Box<dyn FnOnce() -> String>)
            })
            .collect();
        doc.report(Severity::Warning, items, diags);
    }
}

/// Check that `[code.aliases]` targets name languages (loads the syntax
/// set, so only for [`Checks::Thorough`]).
pub(super) fn check_aliases(user_docs: &[Parsed<ConfigLayer>], diags: &mut Vec<Diagnostic>) {
    for doc in user_docs {
        let items: Vec<Deferred<'_>> = doc
            .value
            .code
            .aliases
            .0
            .iter()
            .filter_map(|(from, to)| {
                let message = crate::highlight::validate_language(to).err()?;
                let path = vec!["code", "aliases", from.as_str()];
                let message = move || format!("code.aliases.{from}: {message}");
                Some((path, Box::new(message) as Box<dyn FnOnce() -> String>))
            })
            .collect();
        doc.report(Severity::Warning, items, diags);
    }
}
