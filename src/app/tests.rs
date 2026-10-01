//! Tests of the application pipeline's decisions.

use clap::Parser as _;

use super::*;
use crate::source::Origin;

fn cli(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("emde").chain(args.iter().copied())).unwrap()
}

fn loaded(set: &[&str]) -> Loaded {
    config::load(&LoadOptions {
        set: set.iter().map(|s| (*s).to_owned()).collect(),
        ..LoadOptions::default()
    })
}

#[test]
fn width_precedence() {
    let env = Env::from_pairs(&[("COLUMNS", "132")]);
    let mut caps = Caps::plain();
    assert_eq!(terminal_width(Some(60), false, &env, &mut caps), 60);
    assert_eq!(terminal_width(None, false, &env, &mut caps), 132);
    assert_eq!(terminal_width(None, false, &Env::default(), &mut caps), 80);
    let bad = Env::from_pairs(&[("COLUMNS", "wide")]);
    assert_eq!(terminal_width(None, false, &bad, &mut caps), 80);
    let zero = Env::from_pairs(&[("COLUMNS", "0")]);
    assert_eq!(terminal_width(Some(0), false, &zero, &mut caps), 80);
    assert_eq!(caps.size, None, "no terminal size without a terminal");
}

#[test]
fn the_terminal_size_counts_unless_it_is_zero() {
    let env = Env::from_pairs(&[("COLUMNS", "132")]);
    let mut caps = Caps::full();
    assert_eq!(width_for(None, Some((100, 30)), &env, &mut caps), 100);
    assert_eq!(caps.size, Some((100, 30)));
    // --width wins, and the screen is still known.
    let mut caps = Caps::full();
    assert_eq!(width_for(Some(60), Some((100, 30)), &env, &mut caps), 60);
    assert_eq!(caps.size, Some((100, 30)));
    // A 0×0 pseudo-terminal is an unknown size: $COLUMNS, and no screen
    // height to cap figures by or to decide on the pager with.
    for screen in [(0, 0), (100, 0), (0, 30)] {
        let mut caps = Caps::full();
        assert_eq!(width_for(None, Some(screen), &env, &mut caps), 132);
        assert_eq!(caps.size, None, "{screen:?}");
    }
}

#[test]
fn stream_mode_caps_figures() {
    let piped = Caps::plain();
    let rows = |render: &RenderOptions, caps: &Caps| stream_options(render, caps).images.max_height;
    assert_eq!(rows(&RenderOptions::default(), &piped), Height::Rows(30));
    let mut tall = RenderOptions::default();
    tall.images.max_height = Height::Rows(50);
    assert_eq!(rows(&tall, &piped), Height::Rows(30));
    let mut short = RenderOptions::default();
    short.images.max_height = Height::Rows(5);
    assert_eq!(rows(&short, &piped), Height::Rows(5));
    // On a terminal, never taller than the screen.
    let small_screen = Caps {
        size: Some((80, 12)),
        ..Caps::full()
    };
    assert_eq!(rows(&tall, &small_screen), Height::Rows(11));
    let tiny = Caps {
        size: Some((80, 1)),
        ..Caps::full()
    };
    assert_eq!(rows(&tall, &tiny), Height::Rows(1));
}

#[test]
fn broken_pipe_is_success() {
    let e = io::Error::from(io::ErrorKind::BrokenPipe);
    assert_eq!(write_failed(&e), ExitCode::SUCCESS);
}

#[test]
fn the_probe_asks_only_for_what_is_needed() {
    let config = loaded(&[]).config;
    let images = Hints {
        images: true,
        ..Hints::default()
    };
    let needs = probe_needs(&config, Hints::default(), ColorDepth::TrueColor);
    assert!(needs.background && !needs.graphics, "background = auto");
    let needs = probe_needs(&config, images, ColorDepth::TrueColor);
    assert_eq!(needs.graphics, cfg!(feature = "images"));
    // Without colours the background does not matter; without escapes
    // nothing does.
    let needs = probe_needs(&config, images, ColorDepth::Mono);
    assert!(!needs.background);
    assert_eq!(needs.graphics, cfg!(feature = "images"));
    assert!(!probe_needs(&config, images, ColorDepth::None).any());
    // A chosen background and no images: nothing to ask.
    let dark = loaded(&["theme.background=dark", "render.images=none"]).config;
    assert!(!probe_needs(&dark, images, ColorDepth::TrueColor).any());
}

#[test]
fn paging_decisions() {
    let tty = Caps {
        size: Some((80, 24)),
        ..Caps::full()
    };
    let request = |paging| PagerRequest {
        paging,
        ..PagerRequest::default()
    };
    assert!(!wants_pager(&request(Paging::Auto), &tty, 23));
    assert!(wants_pager(&request(Paging::Auto), &tty, 24));
    assert!(wants_pager(&request(Paging::Always), &tty, 1));
    assert!(!wants_pager(&request(Paging::Never), &tty, 1000));
    let piped = Caps::plain();
    assert!(!wants_pager(&request(Paging::Always), &piped, 1000));
    let unknown = Caps {
        size: None,
        ..Caps::full()
    };
    assert!(!wants_pager(&request(Paging::Auto), &unknown, 1000));
    assert_eq!(Paging::from(When::Always), Paging::Always);
    assert_eq!(Paging::from(When::Never), Paging::Never);
    assert_eq!(Paging::from(When::Auto), Paging::Auto);
}

fn doc(anchor: Option<&str>) -> Doc {
    Doc {
        source: Source::from_text(""),
        doc: Document::default(),
        anchor: anchor.map(str::to_owned),
        images: None,
        survey: Survey::default(),
    }
}

#[test]
fn pager_requests_carry_the_pager_flags() {
    let config = loaded(&[]).config;
    let docs = [doc(Some("intro")), doc(Some("other"))];
    let r = pager_request(&cli(&["--toc"]), &config, &docs);
    assert_eq!(
        r,
        PagerRequest {
            paging: Paging::Always,
            toc: true,
            anchor: Some("intro".into()),
        }
    );
    let r = pager_request(&cli(&["--anchor", "usage"]), &config, &docs);
    assert_eq!(r.anchor.as_deref(), Some("usage"), "--anchor wins");
    // `--plain` and `--paging` reach the request through the configuration.
    let plain = config::load(&cli(&["-p"]).load_options(ConfigEnv::default())).config;
    assert_eq!(pager_request(&cli(&[]), &plain, &[]).paging, Paging::Never);
    let set = loaded(&["pager.enabled=always"]).config;
    assert_eq!(pager_request(&cli(&[]), &set, &[]).paging, Paging::Always);
}

#[test]
fn the_pager_opens_for_short_documents_on_a_terminal_by_default() {
    let tty = Caps {
        size: Some((80, 24)),
        ..Caps::full()
    };
    let paging = |args: &[&str], set: &[&str]| {
        let cli = cli(args);
        let mut opts = cli.load_options(ConfigEnv::default());
        opts.set = set.iter().map(|s| (*s).to_owned()).collect();
        let config = config::load(&opts).config;
        pager_request(&cli, &config, &[])
    };
    // The default: a one-line document opens in the pager on a terminal.
    let default = paging(&[], &[]);
    assert_eq!(default.paging, Paging::Always);
    assert_eq!(PagerRequest::default().paging, Paging::Always);
    assert!(wants_pager(&default, &tty, 1));
    assert!(wants_pager(&default, &tty, 0), "even an empty one");
    // Never when piped, with --plain or with --paging=never.
    assert!(!wants_pager(&default, &Caps::plain(), 1000));
    for args in [&["-p"][..], &["--plain"], &["--paging", "never"]] {
        let r = paging(args, &[]);
        assert!(!wants_pager(&r, &tty, 1000), "{args:?}");
    }
    assert!(!wants_pager(
        &paging(&[], &["pager.enabled=never"]),
        &tty,
        1000
    ));
    // `auto` keeps its meaning: only documents taller than the screen.
    let auto = paging(&["--paging", "auto"], &[]);
    assert!(!wants_pager(&auto, &tty, 23));
    assert!(wants_pager(&auto, &tty, 24));
    let auto = paging(&[], &["pager.enabled=auto"]);
    assert!(!wants_pager(&auto, &tty, 1));
}

#[test]
fn configuration_problems_are_brief_unless_verbose() {
    let bad = loaded(&[
        "render.max_widht=80",
        "render.margin=wide",
        "glyphs.quote=",
        "pager.mouse=maybe",
    ]);
    assert_eq!(bad.diagnostics.len(), 4, "{:?}", bad.diagnostics);
    let mut brief = Vec::new();
    report_config(&mut brief, &bad, false);
    let brief = String::from_utf8(brief).unwrap();
    let lines: Vec<&str> = brief.lines().collect();
    assert_eq!(lines.len(), 4, "{brief}");
    assert!(lines[0].starts_with("emde: warning: --set render.max_widht=80: "));
    assert!(lines[0].contains("did you mean `render.max_width`"));
    assert_eq!(
        lines[3],
        "emde: 4 configuration problems in all; `emde --check-config` shows each in full"
    );
    let mut full = Vec::new();
    report_config(&mut full, &bad, true);
    let full = String::from_utf8(full).unwrap();
    assert_eq!(full.matches("emde: ").count(), 4);
    assert!(!full.contains("in all"));
    // One short problem needs no pointer to more.
    let one = loaded(&["render.max_widht=80"]);
    let mut out = Vec::new();
    report_config(&mut out, &one, false);
    assert_eq!(String::from_utf8(out).unwrap().lines().count(), 1);
    let mut none = Vec::new();
    report_config(&mut none, &loaded(&[]), false);
    assert!(none.is_empty());
}

#[test]
fn content_problems_only_with_verbose() {
    let source = Source::from_bytes(b"bad \xff byte".to_vec(), Origin::Stdin);
    let d = parse_doc(
        FileArg {
            path: "-".into(),
            anchor: None,
        },
        source,
        &ParseOptions::default(),
        FrontMatterMode::Card,
    );
    let docs = [d];
    let mut quiet = Vec::new();
    report_content(&mut quiet, &docs, false);
    assert!(quiet.is_empty());
    let mut verbose = Vec::new();
    report_content(&mut verbose, &docs, true);
    let text = String::from_utf8(verbose).unwrap();
    assert!(
        text.starts_with("emde: <stdin>: 1 invalid byte sequence replaced"),
        "{text}"
    );
}

#[test]
fn the_highlighter_is_only_made_for_code_in_a_language() {
    let parse_opts = ParseOptions::default();
    let make = |md: &str| {
        let source = Source::from_text(md);
        let arg = FileArg {
            path: "-".into(),
            anchor: None,
        };
        parse_doc(arg, source, &parse_opts, FrontMatterMode::Card)
    };
    let config = loaded(&[]).config;
    let theme = Theme::fallback(crate::theme::Variant::Dark, None);
    let plain = [make("text\n\n    indented\n\n```\nno language\n```")];
    let h = highlighter_for(&plain, &theme, &config, &[]);
    assert_eq!(h.resolve("rust"), None, "plain highlighter");
    let code = [make("```rust\nfn a() {}\n```\n\n```python\nx = 1\n```")];
    let h = highlighter_for(&code, &theme, &config, &[]);
    if cfg!(feature = "highlight") {
        let rust = h.resolve("rust").unwrap();
        assert!(!h.highlight(rust, "fn a() {}").lines[0].is_empty());
    }
    // Nested front matter is shown as YAML code, so it is highlighted too.
    let front = [make("---\ntitle: A\ntags:\n  - x\n---\n\ntext")];
    let h = highlighter_for(&front, &theme, &config, &[]);
    assert_eq!(h.resolve("yaml").is_some(), cfg!(feature = "highlight"));
}

#[test]
fn credits_say_what_is_bundled() {
    let text = credits();
    if cfg!(feature = "highlight") {
        assert!(text.starts_with("emde bundles syntax definitions"));
        assert!(text.contains("──"), "licence sections");
    } else {
        assert!(text.contains("no third-party"));
    }
}

#[test]
fn theme_list_names_builtins_and_files() {
    let dir = std::env::temp_dir().join(format!("emde-app-themes-{}", std::process::id()));
    let themes = dir.join("emde").join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    std::fs::write(themes.join("nord.toml"), "name = \"nord\"\n").unwrap();
    let load = LoadOptions {
        env: ConfigEnv {
            xdg_config_home: Some(dir.clone()),
            ..ConfigEnv::default()
        },
        ..LoadOptions::default()
    };
    let text = theme_list(&load);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("emde") && l.ends_with("built-in"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("nord") && l.ends_with("nord.toml")),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tracing_is_off_unless_asked_for() {
    // The test process does not set EMDE_TRACE.
    if std::env::var_os("EMDE_TRACE").is_none() {
        assert!(Trace::from_env().last.is_none());
    }
}
