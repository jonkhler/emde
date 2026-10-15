//! Paths typed into the picker and the `:` prompt: which queries are
//! paths, the directory they name, its entries, and what Tab completes
//! (the same engine for both).
//!
//! A query is a path when it starts with `/`, `~`, `./` or `../`, or has a
//! `/` in it. Its directory part (up to the last `/`) names the directory
//! to list, relative to the current document's directory unless absolute
//! or under `~`; the rest filters the entries.

use std::borrow::Cow;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::super::fuzzy::{self, Pattern, Scratch};

/// Whether `query` is a path (see the module docs).
pub(crate) fn is_path(query: &str) -> bool {
    query.starts_with(['/', '~'])
        || query.starts_with("./")
        || query.starts_with("../")
        || query.contains('/')
}

/// `query` split after its last `/`: the directory part and the name
/// typed after it. `~` alone is a directory part.
pub(crate) fn split(query: &str) -> (&str, &str) {
    if query == "~" {
        return (query, "");
    }
    match query.rfind('/') {
        Some(i) => query.split_at(i + 1),
        None => ("", query),
    }
}

/// The query that names `name` in directory part `dir`.
pub(crate) fn join(dir: &str, name: &str) -> String {
    let mut out = dir.to_owned();
    if !out.is_empty() && !out.ends_with('/') {
        out.push('/');
    }
    out.push_str(name);
    out
}

/// What is wrong with a directory part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unresolved {
    /// `~` without a home directory.
    NoHome,
    /// `~user`, which is not supported.
    OtherHome,
}

impl Unresolved {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Unresolved::NoHome => "~ needs $HOME",
            Unresolved::OtherHome => "~user is not supported",
        }
    }
}

/// The path that `typed` (a directory part, or a whole path) names: `~`
/// is the home directory, and a relative path is relative to `base`.
pub(crate) fn resolve(typed: &str, base: &Path, home: Option<&Path>) -> Result<PathBuf, Unresolved> {
    if let Some(rest) = typed.strip_prefix('~') {
        let rest = match rest.strip_prefix('/') {
            Some(r) => r,
            None if rest.is_empty() => "",
            None => return Err(Unresolved::OtherHome),
        };
        let home = home.ok_or(Unresolved::NoHome)?;
        return Ok(if rest.is_empty() {
            home.to_path_buf()
        } else {
            home.join(rest)
        });
    }
    if typed.is_empty() {
        return Ok(base.to_path_buf());
    }
    Ok(base.join(typed))
}

/// `path` as the reader would type it: under `home` as `~/…`.
pub(crate) fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.to_string_lossy()),
        None => path.to_string_lossy().into_owned(),
    }
}

/// A directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The name (lossy UTF-8, as shown).
    pub(crate) name: Box<str>,
    pub(crate) path: PathBuf,
    /// A directory (or a link to one).
    pub(crate) dir: bool,
    /// Its name starts with a dot.
    pub(crate) hidden: bool,
}

/// The entries of `dir`, directories first, then by name; at most `cap`.
pub(crate) fn list(dir: &Path, cap: usize) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)?.take(cap) {
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into();
        let path = entry.path();
        let dir = match entry.file_type() {
            Ok(t) if t.is_symlink() => fs::metadata(&path).is_ok_and(|m| m.is_dir()),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        let hidden = entry.file_name().as_encoded_bytes().starts_with(b".");
        out.push(Entry {
            name,
            path,
            dir,
            hidden,
        });
    }
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// Whether entry `e` is listed for `leaf`: hidden entries only with
/// `hidden`, or when the leaf starts with a dot.
fn shown(e: &Entry, leaf: &str, hidden: bool) -> bool {
    !e.hidden || hidden || leaf.starts_with('.')
}

/// The entries matching `leaf`, fuzzily (directories first, then the best
/// matches): `(index, score)`.
pub(crate) fn filter(entries: &[Entry], leaf: &str, hidden: bool) -> Vec<(usize, i32)> {
    let pattern = Pattern::new(leaf);
    let mut scratch = Scratch::default();
    let mut out: Vec<(usize, i32)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| shown(e, leaf, hidden))
        .filter_map(|(i, e)| {
            let score = pattern.score(&e.name, fuzzy::mask(&e.name), 0, &mut scratch)?;
            Some((i, score))
        })
        .collect();
    out.sort_by_key(|&(i, score)| (!entries[i].dir, std::cmp::Reverse(score), i));
    out
}

/// What Tab does with `leaf` among `entries`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Completion {
    /// One entry starts with it: its name, and whether it is a directory.
    Unique(String, bool),
    /// Several do, and they share more than `leaf`: that much.
    Longer(String),
    /// Several do and share no more: their names.
    Several(Vec<String>),
    /// None does.
    Nothing,
}

/// Tab after `leaf`: the entries whose names start with it (ignoring case
/// unless it has an uppercase letter), and how far they agree.
pub(crate) fn complete(entries: &[Entry], leaf: &str, hidden: bool) -> Completion {
    let sensitive = leaf.chars().any(char::is_uppercase);
    let fold = |s: &str| -> Vec<char> {
        if sensitive {
            s.chars().collect()
        } else {
            s.chars().flat_map(char::to_lowercase).collect()
        }
    };
    let want = fold(leaf);
    let found: Vec<&Entry> = entries
        .iter()
        .filter(|e| shown(e, leaf, hidden) && fold(&e.name).starts_with(&want))
        .collect();
    let Some(first) = found.first() else {
        return Completion::Nothing;
    };
    if let [only] = found.as_slice() {
        return Completion::Unique(only.name.to_string(), only.dir);
    }
    // The longest prefix all share (in the first one's spelling).
    let chars: Vec<char> = first.name.chars().collect();
    let mut n = chars.len();
    for e in &found[1..] {
        let other: Vec<char> = e.name.chars().collect();
        n = n.min(
            chars
                .iter()
                .zip(&other)
                .take_while(|(a, b)| fold(&a.to_string()) == fold(&b.to_string()))
                .count(),
        );
    }
    let prefix: String = chars.get(..n).unwrap_or_default().iter().collect();
    if prefix.chars().count() > leaf.chars().count() {
        Completion::Longer(prefix)
    } else {
        Completion::Several(found.iter().map(|e| e.name.to_string()).collect())
    }
}

/// `name` as typed in a query: a directory with a `/` after it.
pub(crate) fn typed_name(name: &str, dir: bool) -> Cow<'_, str> {
    if dir {
        Cow::Owned(format!("{name}/"))
    } else {
        Cow::Borrowed(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory with these files (a trailing `/` makes a
    /// directory).
    fn tree(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emde-paths-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for f in files {
            let p = dir.join(f);
            if f.ends_with('/') {
                fs::create_dir_all(&p).unwrap();
            } else {
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(&p, "x").unwrap();
            }
        }
        dir
    }

    fn names(entries: &[Entry], hits: &[(usize, i32)]) -> Vec<String> {
        hits.iter()
            .map(|&(i, _)| {
                let e = &entries[i];
                typed_name(&e.name, e.dir).into_owned()
            })
            .collect()
    }

    #[test]
    fn which_queries_are_paths() {
        for q in ["/", "/etc", "~", "~/dev", "./x", "../x", "src/main", "a/"] {
            assert!(is_path(q), "{q}");
        }
        for q in ["", "readme", ".hidden", "..", "a b", "x~"] {
            assert!(!is_path(q), "{q}");
        }
        assert_eq!(split("~/dev/ne"), ("~/dev/", "ne"));
        assert_eq!(split("../"), ("../", ""));
        assert_eq!(split("~"), ("~", ""));
        assert_eq!(split("name"), ("", "name"));
        assert_eq!(join("~", "dev/"), "~/dev/");
        assert_eq!(join("", "x"), "x");
        assert_eq!(join("a/", "x"), "a/x");
    }

    #[test]
    fn resolving() {
        let base = Path::new("/docs/sub");
        let home = Some(Path::new("/home/u"));
        assert_eq!(resolve("~/dev/", base, home), Ok(PathBuf::from("/home/u/dev/")));
        assert_eq!(resolve("~", base, home), Ok(PathBuf::from("/home/u")));
        assert_eq!(resolve("~", base, None), Err(Unresolved::NoHome));
        assert_eq!(resolve("~bob/", base, home), Err(Unresolved::OtherHome));
        assert_eq!(resolve("../", base, home), Ok(PathBuf::from("/docs/sub/../")));
        assert_eq!(resolve("", base, home), Ok(PathBuf::from("/docs/sub")));
        assert_eq!(resolve("/etc/", base, home), Ok(PathBuf::from("/etc/")));
        assert_eq!(tilde(Path::new("/home/u/dev"), home), "~/dev");
        assert_eq!(tilde(Path::new("/home/u"), home), "~");
        assert_eq!(tilde(Path::new("/etc"), home), "/etc");
        assert_eq!(tilde(Path::new("/home/u/dev"), None), "/home/u/dev");
    }

    #[test]
    fn listing_and_filtering() {
        let dir = tree(
            "list",
            &["notes.md", "news/", "Nested/", ".hidden.md", "zebra.md", "new.md"],
        );
        let entries = list(&dir, 100).unwrap();
        let all = filter(&entries, "", false);
        assert_eq!(
            names(&entries, &all),
            ["Nested/", "news/", "new.md", "notes.md", "zebra.md"],
            "directories first, hidden ones left out"
        );
        let hidden = filter(&entries, "", true);
        assert_eq!(hidden.len(), 6);
        assert_eq!(names(&entries, &filter(&entries, ".h", false)), [".hidden.md"]);
        let ne = filter(&entries, "ne", false);
        assert_eq!(names(&entries, &ne), ["Nested/", "news/", "new.md", "notes.md"]);
        assert!(list(&dir.join("missing"), 100).is_err());
        assert_eq!(list(&dir, 2).unwrap().len(), 2, "capped");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tab_completes_the_common_prefix() {
        let dir = tree(
            "complete",
            &["project-alpha/", "project-beta/", "proposal.md", "README.md", "readme-old.md"],
        );
        let entries = list(&dir, 100).unwrap();
        assert_eq!(
            complete(&entries, "proj", false),
            Completion::Longer("project-".into())
        );
        assert_eq!(
            complete(&entries, "project-a", false),
            Completion::Unique("project-alpha".into(), true)
        );
        assert_eq!(
            complete(&entries, "pro", false),
            Completion::Several(vec![
                "project-alpha".into(),
                "project-beta".into(),
                "proposal.md".into()
            ])
        );
        // Ignoring case, in the first entry's spelling.
        assert_eq!(complete(&entries, "read", false), Completion::Longer("README".into()));
        assert_eq!(
            complete(&entries, "READ", false),
            Completion::Unique("README.md".into(), false)
        );
        assert_eq!(complete(&entries, "x", false), Completion::Nothing);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn home_and_parent_directories_list() {
        let dir = tree("home", &["dev/a.md", "dev/b/", "top.md"]);
        let home = Some(dir.as_path());
        let base = dir.join("dev");
        let (part, leaf) = split("~/de");
        let at = resolve(part, &base, home).unwrap();
        let entries = list(&at, 100).unwrap();
        assert_eq!(complete(&entries, leaf, false), Completion::Unique("dev".into(), true));
        let (part, _) = split("../");
        let up = list(&resolve(part, &base, home).unwrap(), 100).unwrap();
        let shown: Vec<_> = up.iter().map(|e| e.name.to_string()).collect();
        assert_eq!(shown, ["dev", "top.md"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn names_that_are_not_utf8() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = tree("utf8", &[]);
        fs::create_dir_all(&dir).unwrap();
        let odd = std::ffi::OsStr::from_bytes(b"caf\xe9.md");
        if fs::write(dir.join(odd), "x").is_err() {
            return; // the file system refuses such names
        }
        let entries = list(&dir, 100).unwrap();
        assert_eq!(&*entries[0].name, "caf\u{fffd}.md");
        assert_eq!(entries[0].path, dir.join(odd), "the real name opens");
        assert_eq!(filter(&entries, "caf", false).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
