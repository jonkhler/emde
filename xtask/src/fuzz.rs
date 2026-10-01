//! `cargo xtask fuzz [--secs N] [--jobs N] [--asan] [TARGET...]`: run emde's
//! fuzz targets (`fuzz/`: cargo-fuzz and libFuzzer) for N seconds each
//! (default 60; libFuzzer's first pass over the corpus comes on top), one
//! after the other; with no TARGET, all of them.
//!
//! It needs the nightly toolchain and cargo-fuzz:
//!
//! ```sh
//! rustup toolchain install nightly --profile minimal
//! cargo install --locked cargo-fuzz
//! ```
//!
//! Each target first gets seeds made from the test fixtures and the
//! documentation (`tests/fixtures/`, `assets/`, the TOML examples of
//! `README.md` and `CONFIG.md`, recorded terminal replies) in
//! `$CARGO_TARGET_DIR/fuzz/seeds/<target>/`. libFuzzer reads them together
//! with `fuzz/corpus/<target>/`, where it keeps the inputs that reach new
//! code. A panic or a failed check stops the target; the input is saved in
//! `fuzz/artifacts/<target>/` and can be replayed with the same build:
//! `rustup run nightly cargo fuzz run --fuzz-dir fuzz -s none --target-dir
//! $CARGO_TARGET_DIR/fuzz/build <target> <file>` (`fuzz/README.md` has more).
//!
//! The targets are built without a sanitizer: emde has no unsafe code and
//! its checks are panics, and AddressSanitizer makes the runs about ten times
//! slower. `--asan` builds with it anyway. `--jobs N` runs N processes per
//! target (libFuzzer's fork mode).
//!
//! Fuzzing is not part of `cargo xtask ci`.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::docs::examples::blocks;

/// The fuzz targets, in the order they run.
const TARGETS: [&str; 5] = ["render", "math", "probe", "html", "config"];
/// Seconds per target unless `--secs` says otherwise.
const DEFAULT_SECS: u64 = 60;
/// The toolchain cargo-fuzz runs on (it needs nightly compiler flags).
const TOOLCHAIN: &str = "nightly";
/// A single input taking this long (in the instrumented build) is a finding.
const INPUT_TIMEOUT_SECS: u64 = 10;

/// The longest input libFuzzer makes for a target (longer seeds are cut).
/// Documents of more than a few kilobytes mostly make runs slower; TeX is
/// shown raw beyond 4 KiB anyway, and terminal replies are short.
fn max_len(target: &str) -> usize {
    match target {
        "math" => 4200,
        "probe" => 2048,
        _ => 8192,
    }
}

/// What to run.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Options {
    secs: u64,
    jobs: u32,
    asan: bool,
    targets: Vec<&'static str>,
}

/// Parse the arguments after `fuzz`.
fn options(args: &[String]) -> Result<Options, String> {
    let mut opts = Options {
        secs: DEFAULT_SECS,
        jobs: 1,
        asan: false,
        targets: Vec::new(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| args.next().cloned())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "--secs" => {
                opts.secs = value("--secs")?
                    .parse()
                    .map_err(|e| format!("--secs: {e}"))?;
            }
            "--jobs" => {
                opts.jobs = value("--jobs")?
                    .parse()
                    .ok()
                    .filter(|&j| j > 0)
                    .ok_or("--jobs needs a positive number")?;
            }
            "--asan" => opts.asan = true,
            name => match TARGETS.iter().find(|&&t| t == name) {
                Some(&t) if !opts.targets.contains(&t) => opts.targets.push(t),
                Some(_) => {}
                None => {
                    return Err(format!(
                        "unknown fuzz option or target `{name}` (targets: {})",
                        TARGETS.join(", ")
                    ));
                }
            },
        }
    }
    if opts.targets.is_empty() {
        opts.targets = TARGETS.to_vec();
    }
    Ok(opts)
}

/// Run the fuzz targets.
pub(crate) fn run(args: &[String]) -> Result<(), String> {
    let opts = options(args)?;
    let root = crate::repo_root()?;
    let work = crate::target_dir()?.join("fuzz");
    check_tools()?;
    for target in &opts.targets {
        let seeds = work.join("seeds").join(target);
        let count = write_seeds(&seeds, &seeds_for(&root, target)?)?;
        let corpus = root.join("fuzz").join("corpus").join(target);
        fs::create_dir_all(&corpus).map_err(|e| format!("{}: {e}", corpus.display()))?;
        // Per target, so that runs of different targets can overlap.
        let tmp = work.join("tmp").join(target);
        fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let result = crate::run(
            &format!("fuzz {target} for {} s ({count} seeds)", opts.secs),
            fuzz_command(&root, &work, &opts, target, &corpus, &seeds).env("TMPDIR", &tmp),
        );
        let _ = fs::remove_dir_all(&tmp);
        result.map_err(|e| {
            format!("{e}; when a check failed, the input is in fuzz/artifacts/{target}/")
        })?;
    }
    Ok(())
}

/// `cargo fuzz run` for one target.
fn fuzz_command(
    root: &Path,
    work: &Path,
    opts: &Options,
    target: &str,
    corpus: &Path,
    seeds: &Path,
) -> Command {
    // Builds with and without the sanitizer differ in every crate: separate
    // directories, so switching does not rebuild everything.
    let build = if opts.asan { "build-asan" } else { "build" };
    let mut cmd = nightly_cargo();
    cmd.current_dir(root)
        .args(["fuzz", "run", "--fuzz-dir", "fuzz", "--target-dir"])
        .arg(work.join(build));
    if !opts.asan {
        cmd.args(["--sanitizer", "none"]);
    }
    if opts.jobs > 1 {
        cmd.args(["--jobs", &opts.jobs.to_string()]);
    }
    cmd.arg(target).arg(corpus).arg(seeds).arg("--").args([
        format!("-max_total_time={}", opts.secs),
        format!("-max_len={}", max_len(target)),
        format!("-timeout={INPUT_TIMEOUT_SECS}"),
        "-rss_limit_mb=4096".to_owned(),
        "-print_final_stats=1".to_owned(),
    ]);
    // Nothing reads standard input; a target must never wait on it.
    cmd.stdin(Stdio::null());
    cmd
}

/// `cargo` on the nightly toolchain. `$CARGO` is the stable toolchain's
/// cargo, and the outer cargo's `RUSTUP_TOOLCHAIN` would win over
/// `rustup run`, so go through rustup without it.
fn nightly_cargo() -> Command {
    let mut cmd = Command::new("rustup");
    cmd.args(["run", TOOLCHAIN, "cargo"])
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO");
    cmd
}

/// Fail early, with instructions, when nightly or cargo-fuzz is missing.
fn check_tools() -> Result<(), String> {
    let ok = nightly_cargo()
        .args(["fuzz", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        Ok(())
    } else {
        Err(format!(
            "fuzzing needs the {TOOLCHAIN} toolchain and cargo-fuzz: \
             `rustup toolchain install {TOOLCHAIN} --profile minimal` and \
             `cargo install --locked cargo-fuzz`"
        ))
    }
}

/// Replace the contents of `dir` with `seeds`, one file each, named by a
/// hash of the content; returns how many distinct seeds were written.
fn write_seeds(dir: &Path, seeds: &[Vec<u8>]) -> Result<usize, String> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut names = BTreeSet::new();
    for seed in seeds {
        let name = format!("seed-{:016x}", fnv1a(seed));
        if names.insert(name.clone()) {
            let path = dir.join(name);
            fs::write(&path, seed).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(names.len())
}

/// The 64-bit FNV-1a hash.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The seeds of a target.
fn seeds_for(root: &Path, target: &str) -> Result<Vec<Vec<u8>>, String> {
    let docs = Docs::read(root)?;
    Ok(match target {
        "render" => render_seeds(&docs),
        "math" => math_seeds(&docs),
        "probe" => probe_seeds(),
        "html" => html_seeds(&docs),
        "config" => config_seeds(root, &docs)?,
        other => return Err(format!("no seeds for `{other}`")),
    })
}

/// The Markdown the seeds are made from.
struct Docs {
    /// `tests/fixtures/**/*.md`, `README.md` and `CONFIG.md`.
    files: Vec<String>,
    /// The examples of the pulldown-cmark test suite.
    examples: Vec<String>,
}

impl Docs {
    fn read(root: &Path) -> Result<Docs, String> {
        let mut paths = Vec::new();
        for dir in ["tests/fixtures/md", "tests/fixtures/images"] {
            let dir = root.join(dir);
            let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "md") {
                    paths.push(path);
                }
            }
        }
        paths.sort();
        paths.extend(["README.md", "CONFIG.md"].map(|f| root.join(f)));
        let files = paths
            .iter()
            .filter_map(|p| fs::read_to_string(p).ok())
            .collect();
        let suite = root.join("tests/fixtures/pulldown-cmark-suite.txt");
        let suite = fs::read_to_string(&suite).map_err(|e| format!("{}: {e}", suite.display()))?;
        Ok(Docs {
            files,
            examples: suite_examples(&suite),
        })
    }
}

/// The examples of the pulldown-cmark suite file: each starts after a line
/// `⸻⸻⸻ example <file>:<n>`.
fn suite_examples(suite: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in suite.split_inclusive('\n') {
        if line.starts_with("⸻⸻⸻ example ") {
            out.extend(current.take());
            current = Some(String::new());
        } else if let Some(c) = &mut current {
            c.push_str(line);
        }
    }
    out.extend(current);
    out
}

/// `header` followed by `body`.
fn with_header(header: &[u8], body: &str) -> Vec<u8> {
    let mut seed = header.to_vec();
    seed.extend_from_slice(body.as_bytes());
    seed
}

/// Render seeds: every document under a few settings (see the option bytes
/// in `fuzz/fuzz_targets/render.rs`), every suite example under one.
fn render_seeds(docs: &Docs) -> Vec<Vec<u8>> {
    const HEADERS: [[u8; 4]; 3] = [
        [79, 1 << 5, 0, 39],
        [23, 0, 0, 119],
        [39, (3 << 5) | 0b1101, 0b0101_0101, 7],
    ];
    let mut seeds = Vec::new();
    for file in &docs.files {
        seeds.extend(HEADERS.iter().map(|h| with_header(h, file)));
    }
    seeds.extend(docs.examples.iter().map(|e| with_header(&HEADERS[0], e)));
    seeds
}

/// Formulas written for the seeds: every construction of the 2D layout.
const FORMULAS: &[&str] = &[
    r"\sum_{i=1}^n i^2 = \frac{n(n+1)(2n+1)}{6}",
    r"A = \begin{pmatrix} a & b \\ c & d \end{pmatrix}",
    r"f(x) = \begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}",
    r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
    r"\sqrt[3]{x^2+1} + \sqrt{\frac{a}{b}}",
    r"\mathbb{R}^n \times \mathcal{C} \to \mathfrak{g}",
    r"\hat a \not\in \bar{B}, \quad \vec v \ne 0",
    r"\overbrace{a+b+c}^{n} = \underbrace{x+y}_{k}",
    r"\begin{aligned} a &= b + c \\ d &\le e \end{aligned} \tag{1}",
    r"\left( \frac{a}{b} \right)^{-1} \left[ x \right]",
    r"\lim_{x \to 0} \frac{\sin x}{x} = 1",
    r"\binom{n}{k} \equiv a \pmod{n}",
    r"\operatorname*{argmax}_x f(x) \cdot \mathbf{x}^\top",
    r"\begin{array}{c|c} 1 & 2 \\ \hline 3 & 4 \end{array}",
    r"x^{y^{z^{w}}} + a_{i_{j_{k}}} + e^{i\pi} + 1 = 0",
    r"\frac{1}{1+\frac{1}{1+\frac{1}{x}}}",
];

/// Math seeds: the formulas of the documents and [`FORMULAS`], under two
/// settings (see `fuzz/fuzz_targets/math.rs`).
fn math_seeds(docs: &Docs) -> Vec<Vec<u8>> {
    const HEADERS: [[u8; 3]; 2] = [[0, 80, 12], [0b10_0101, 20, 3]];
    let mut formulas: BTreeSet<String> = FORMULAS.iter().map(|f| (*f).to_owned()).collect();
    for file in &docs.files {
        formulas.extend(formulas_in(file));
    }
    formulas
        .iter()
        .flat_map(|f| HEADERS.iter().map(move |h| with_header(h, f)))
        .collect()
}

/// The TeX of `$…$`, `$$…$$`, `\(…\)`, `\[…\]` and ```` ```math ```` blocks
/// (roughly: seeds need not be exact).
fn formulas_in(markdown: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fence: Option<String> = None;
    for line in markdown.lines() {
        if let Some(body) = &mut fence {
            if line.trim_start().starts_with("```") {
                out.extend(fence.take());
            } else {
                body.push_str(line);
                body.push('\n');
            }
            continue;
        }
        if line.trim_start().starts_with("```math") {
            fence = Some(String::new());
        }
    }
    for (open, close) in [("$$", "$$"), (r"\[", r"\]"), (r"\(", r"\)"), ("$", "$")] {
        let mut rest = markdown;
        while let Some(start) = rest.find(open) {
            let after = &rest[start + open.len()..];
            let Some(end) = after.find(close) else {
                break;
            };
            let tex = after[..end].trim();
            if !tex.is_empty() && tex.len() < 500 {
                out.push(tex.to_owned());
            }
            rest = &after[end + close.len()..];
        }
    }
    out
}

/// Terminal replies, recorded or rebuilt from each terminal's source (the
/// same fixtures as the tests of `src/term/probe.rs`).
const REPLIES: &[&[u8]] = &[
    b"\x1b_Gi=31;OK\x1b\\\x1bP>|kitty(0.49.1)\x1b\\\x1b[6;36;17t\x1b[4;1800;2720t\x1b[8;50;160t\
      \x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?62;52;c",
    b"\x1b_Gi=31;OK\x1b\\\x1bP>|ghostty 1.3.1\x1b\\\x1b[6;38;18t\x1b[4;1900;2880t\x1b[8;50;160t\
      \x1b]11;rgb:2828/2c2c/3434\x1b\\\x1b[?62;22;52c",
    b"\x1b_Gi=31;OK\x1b\\\x1bP>|iTerm2 3.6.9\x1b\\\x1b[4;864;1712t\x1b[8;54;214t\
      \x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?62;1;2;4;6;22;28c",
    b"\x1b_Gi=31;OK\x1b\\\x1bP>|WezTerm 20240203-110809-5046fc22\x1b\\\x1b[6;20;10t\
      \x1b[4;1000;1600t\x1b[8;50;160t\x1b]11;rgb:1f1f/1f1f/2828\x1b\\\x1b[?65;4;6;18;22c",
    b"\x1b_Gi=31;OK\x1b\\\x1bP>|xterm.js(6.0.0)\x1b\\\x1b[6;17;8t\x1b[4;850;1200t\x1b[8;50;150t\
      \x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\\x1b[?62;4;9;22c",
    b"\x1bP>|foot(1.20.2)\x1b\\\x1b[6;20;10t\x1b[4;1000;1600t\x1b[8;50;160t\
      \x1b]11;rgb:2424/2424/2424\x1b\\\x1b[?62;4;22;28c",
    b"\x1bP>|XTerm(390)\x1b\\\x1b[6;17;9t\x1b[4;408;720t\x1b[8;24;80t\
      \x1b]11;rgb:ffff/ffff/ffff\x07\x1b[?63;1;2;4;6;9;15;16;22;28c",
];

/// tmux's query output with each passthrough setting.
const TMUX_OUTPUT: &[&str] = &[
    "3.4|iTerm2 3.6.9|xterm-256color|RGB,hyperlinks,usstyle,sixel,sync,mouse|0x0|off|external",
    "3.4|iTerm2 3.6.9|xterm-256color|RGB,hyperlinks,usstyle,sixel,sync,mouse|9x18|on|on",
    "3.3a|xterm.js(6.0.0)|xterm-256color|RGB,mouse|0x0|all|off",
];

/// The batch tmux answers itself, then the outer terminal's answers to the
/// wrapped queries.
const TMUX_REPLIES: &[u8] = b"\x1bP>|tmux 3.4\x1b\\\x1b[6;32;16t\x1b[4;1728;3424t\x1b[8;54;214t\
    \x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?1;2;4c\x1b_Gi=31;OK\x1b\\\x1b[0n";

/// Probe seeds (see `fuzz/fuzz_targets/probe.rs` for the option bytes).
fn probe_seeds() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    for (i, replies) in REPLIES.iter().enumerate() {
        let env = u8::try_from(i % 8).unwrap_or(0) << 2;
        let mut seed = vec![env, 100];
        seed.extend_from_slice(replies);
        seeds.push(seed);
    }
    for (i, tmux) in TMUX_OUTPUT.iter().enumerate() {
        let wrapped = u8::from(i == 1) << 1;
        let mut seed = vec![1 | wrapped | (1 << 2), 77];
        seed.extend_from_slice(TMUX_REPLIES);
        seed.push(0);
        seed.extend_from_slice(tmux.as_bytes());
        seeds.push(seed);
    }
    seeds
}

/// HTML written for the seeds: the subset emde renders, and broken markup.
const HTML: &[&str] = &[
    r#"<p align="center"><img src="logo.png" alt="Logo" width="200" height="50%"></p>"#,
    "<details><summary>More</summary>\n\nBody <b>bold</b> <i>it</i> <kbd>Ctrl</kbd>+<kbd>C</kbd>\n</details>",
    r#"<picture><source media="(prefers-color-scheme: dark)" srcset="d.png"><img src="l.png"></picture>"#,
    r#"<a href="https://example.org/?a=1&amp;b=2" name='x' id=y>link</a><a name="anchor"></a>"#,
    "<!-- comment --><!DOCTYPE html><?xml x?><![CDATA[<x>]]><br/><hr>",
    "H<sub>2</sub>O x<sup>2</sup> <mark>m</mark> <s>s</s> <del>d</del> <ins>i</ins> <u>u</u>",
    "&amp; &lt; &gt; &quot; &#39; &#x1F600; &#0; &#27; &#x9b; &nosuch; &#99999999999;",
    r#"<div align="center"><h1>Title</h1><center>c</center></div><code>x</code>"#,
    r#"<img src="a.png" alt='x > y' title="unterminated>"#,
];

/// HTML seeds: whole documents, and [`HTML`].
fn html_seeds(docs: &Docs) -> Vec<Vec<u8>> {
    docs.files
        .iter()
        .map(String::as_str)
        .chain(HTML.iter().copied())
        .map(|s| s.as_bytes().to_vec())
        .collect()
}

/// Config seeds (parts separated by NUL: the config file, themes `a` and
/// `b`, `--set` arguments): the defaults, the built-in themes, every TOML
/// example of the documentation, an inheritance chain and some `--set`s.
fn config_seeds(root: &Path, docs: &Docs) -> Result<Vec<Vec<u8>>, String> {
    let read = |rel: &str| {
        let path = root.join(rel);
        fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
    };
    let parts = |list: &[&str]| list.join("\0").into_bytes();
    let use_a = "[theme]\nname = \"a\"\n";
    let mut seeds = vec![
        read("assets/default.toml")?.into_bytes(),
        parts(&[
            use_a,
            "inherits = \"b\"\n[palette]\naccent = \"#ff8800\"\n",
            "inherits = \"emde\"\n[style.h1]\nfg = \"accent\"\nunderline = \"curly\"\n",
        ]),
        parts(&[
            "",
            "",
            "",
            "render.max_width=80",
            "theme.code=Nord",
            "glyphs.bullets=[\"*\", \"-\"]",
            "heading.markers=[\"# \", \"## \", \"\", \"\", \"\", \"\"]",
        ]),
    ];
    for theme in ["emde", "ansi", "mono"] {
        seeds.push(parts(&[
            use_a,
            &read(&format!("assets/themes/{theme}.toml"))?,
        ]));
    }
    for file in &docs.files {
        for block in blocks(file) {
            let mut kind = block.info.split_whitespace();
            match (kind.next(), kind.next()) {
                (Some("toml"), None) => seeds.push(block.text.into_bytes()),
                (Some("toml"), Some("theme")) => seeds.push(parts(&[use_a, &block.text])),
                _ => {}
            }
        }
    }
    Ok(seeds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn option_parsing() {
        let all = options(&[]).unwrap();
        assert_eq!(all.secs, 60);
        assert_eq!(all.targets, TARGETS);
        let some = options(&args(&[
            "--secs", "300", "math", "--asan", "render", "math",
        ]))
        .unwrap();
        assert_eq!(
            some,
            Options {
                secs: 300,
                jobs: 1,
                asan: true,
                targets: vec!["math", "render"]
            }
        );
        assert_eq!(options(&args(&["--secs=5", "--jobs=4"])).unwrap().jobs, 4);
        assert!(options(&args(&["--secs"])).is_err());
        assert!(options(&args(&["--secs", "x"])).is_err());
        assert!(options(&args(&["--jobs", "0"])).is_err());
        assert!(options(&args(&["nope"])).is_err());
    }

    #[test]
    fn suite_examples_are_split() {
        let suite = "header\n⸻⸻⸻ example a.rs:1\n> quote\n⸻⸻⸻ example a.rs:2\n# h\n\nx\n";
        assert_eq!(suite_examples(suite), ["> quote\n", "# h\n\nx\n"]);
    }

    #[test]
    fn formulas_are_found() {
        let md = "Inline $a+b$ and \\(x^2\\), display $$\\frac12$$ and\n\\[ y \\]\n\n\
                  ```math\n\\sum_i i\n```\n";
        let found = formulas_in(md);
        for tex in ["a+b", "x^2", "\\frac12", "y", "\\sum_i i\n"] {
            assert!(found.iter().any(|f| f == tex), "{tex:?} in {found:?}");
        }
    }

    #[test]
    fn every_target_has_seeds() {
        let root = crate::repo_root().unwrap();
        for target in TARGETS {
            let seeds = seeds_for(&root, target).unwrap();
            assert!(seeds.len() >= 5, "{target}: {} seeds", seeds.len());
        }
        assert!(seeds_for(&root, "nope").is_err());
    }

    #[test]
    fn seeds_are_written_once_each() {
        let dir = std::env::temp_dir().join(format!("emde-xtask-seeds-{}", std::process::id()));
        let n = write_seeds(&dir, &[b"a".to_vec(), b"b".to_vec(), b"a".to_vec()]).unwrap();
        assert_eq!(n, 2);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        // Writing again replaces the old seeds.
        assert_eq!(write_seeds(&dir, &[b"c".to_vec()]).unwrap(), 1);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hashes() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
