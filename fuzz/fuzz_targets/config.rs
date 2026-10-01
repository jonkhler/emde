//! Configuration text through emde's layering: a config file, two theme
//! files and `--set` arguments, loaded the way emde loads them, then the
//! theme built for a terminal (one of six, picked by the input's length) and
//! a document laid out with the result.
//!
//! Input: parts separated by NUL bytes: the config file, the theme files
//! `a.toml` and `b.toml` of the themes directory, then one `--set`
//! argument per part. The files live in a scratch directory under
//! `$TMPDIR` (`cargo xtask fuzz` points it into the target directory and
//! removes it afterwards).
//!
//! Checked:
//! * loading, checking (`--check-config`) and building themes never panic;
//! * diagnostics print without control characters (other than newlines and
//!   tabs), whatever the files contain;
//! * whatever the settings (glyphs, markers, widths…), a document laid out
//!   with them fits the width and its output holds only emde's escapes.

#![no_main]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use emde::config::{self, Config, ConfigEnv, LoadOptions};
use emde::highlight::PlainHighlighter;
use emde::layout::{NoImages, layout};
use emde::parse::{ParseOptions, parse_source};
use emde::render::{RenderConfig, to_bytes};
use emde::source::{Origin, Source};
use emde::style::Rgb;
use emde::term::{Caps, ColorDepth};
use emde::text::str_width;
use emde_fuzz::check_escapes;
use libfuzzer_sys::fuzz_target;

/// A document with an element of each kind the settings change.
const DOCUMENT: &str = "---\ntitle: T\n---\n\n# One\n\n## Two\n\n### Three\n\n\
    - a\n  - b\n    - c\n      - d\n- [ ] todo\n- [x] done\n\n1. one\n2. two\n\n\
    > quote\n>\n> > nested\n\n> [!NOTE]\n> An alert.\n\n```rust\nfn main() {\n\tlet x = 1;\n}\n```\n\n\
    | a | b |\n|---|:-:|\n| `c` | $x^2$ |\n\n---\n\nText[^1] with a [link](https://example.org), \
    <kbd>K</kbd> and $\\frac{a}{b}$.\n\n$$\\sum_{i=1}^n i^2$$\n\n![alt](missing.png)\n\n\
    Term\n: definition\n\n[^1]: A note.\n";

fuzz_target!(init: emde_fuzz::init(), |data: &[u8]| {
    let dir = scratch();
    let mut parts = data.split(|&b| b == 0);
    let file = dir.join("config.toml");
    write(&file, parts.next());
    let themes = dir.join("emde").join("themes");
    write(&themes.join("a.toml"), parts.next());
    write(&themes.join("b.toml"), parts.next());
    let set = parts.map(|p| String::from_utf8_lossy(p).into_owned()).collect();
    let opts = LoadOptions {
        config: Some(file),
        set,
        env: ConfigEnv {
            emde_config: None,
            xdg_config_home: Some(dir.to_path_buf()),
            home: None,
        },
        ..LoadOptions::default()
    };

    let loaded = config::load(&opts);
    let report = config::check(&opts);
    for d in loaded.diagnostics.iter().chain(&report.diagnostics) {
        let text = d.to_string();
        assert!(
            !text.chars().any(|c| c.is_control() && c != '\n' && c != '\t'),
            "a control character in a diagnostic: {text:?}"
        );
    }
    let _ = report.to_string();
    check_rendering(&loaded.config, data.len());
});

/// Build the theme for terminal `n` (of six) and lay a document out with it.
fn check_rendering(config: &Config, n: usize) {
    let dark = Some(Rgb(0x1e, 0x1e, 0x2e));
    let light = Some(Rgb(0xef, 0xf1, 0xf5));
    let terminals = [
        (ColorDepth::TrueColor, dark, 40),
        (ColorDepth::TrueColor, light, 7),
        (ColorDepth::Ansi256, None, 80),
        (ColorDepth::Ansi16, dark, 30),
        (ColorDepth::Mono, None, 13),
        (ColorDepth::None, None, 1),
    ];
    let (depth, background, width) = terminals[n % terminals.len()];
    let opts = &config.render;
    let source = Source::from_bytes(DOCUMENT.as_bytes().to_vec(), Origin::Memory);
    let doc = parse_source(&source, &ParseOptions::from(opts));
    let theme = config::build_theme(config, background, depth);
    let caps = Caps {
        color: depth,
        background,
        ..Caps::full()
    };
    let l = layout(
        &doc,
        width,
        &theme,
        &caps,
        opts,
        &PlainHighlighter,
        &NoImages,
    );
    let cfg = RenderConfig {
        link_id_prefix: "f".into(),
        ..RenderConfig::from_caps(&caps)
    };
    let bytes = to_bytes(&doc, &l, &cfg);
    let text = std::str::from_utf8(&bytes).expect("rendered bytes are UTF-8");
    for line in text.split('\n') {
        let visible = check_escapes(line).unwrap_or_else(|e| panic!("{e}"));
        let w = str_width(&visible, opts.ambiguous_wide);
        assert!(
            w <= usize::from(width),
            "{w} columns > {width}: {visible:?}"
        );
    }
}

/// This process's scratch directory, with an empty themes directory.
fn scratch() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("emde-fuzz-config-{}", std::process::id()));
        fs::create_dir_all(dir.join("emde").join("themes")).expect("a scratch directory");
        dir
    })
}

/// Write a part to `path`, or remove the file when there is no such part.
fn write(path: &Path, part: Option<&[u8]>) {
    match part {
        Some(bytes) => fs::write(path, bytes).expect("a scratch file"),
        None => {
            let _ = fs::remove_file(path);
        }
    }
}
