//! Orchestration: CLI → sources → configuration → terminal → layout → sink.
//!
//! A run, in order:
//!
//! 1. **Setup**: the panic hook, the environment snapshot and whether
//!    stdout is a terminal. Commands that only print (`--print-default-config`,
//!    `--list-*`, `--credits`, `--check-config`, `--doctor`) run and exit.
//! 2. **Sources**: every input is read, and a scan of a few microseconds
//!    (`survey::Hints`) looks for fenced code, images and math. With code, the
//!    syntax set starts loading on a background thread right away, so it
//!    overlaps everything below.
//! 3. **Configuration**: defaults, theme, config file, `--set` and flags
//!    ([`config::load`]); problems go to standard error, one line each
//!    (in full with `-v`).
//! 4. **Terminal**: the colour decision from the environment; then, on a
//!    terminal and only when something needs it (the background colour for
//!    `theme.background = "auto"`, images), the tmux query and the probe, on
//!    a thread while the documents are parsed; then [`caps::decide`]. Pipes
//!    get neither: no probe, no tmux subprocess.
//! 5. **Look**: the theme for the terminal, and a highlighter only when a
//!    document has code in some language; with several code blocks they are
//!    highlighted in parallel before layout (`prehighlight`).
//! 6. **Images**: an [`ImageStore`] for each document with figures, when
//!    images are shown at all.
//! 7. **Output**: the pager when it applies (`run_pager`; not part of
//!    this build yet), else stream mode: every document laid out and
//!    written to standard output, figures decoded in parallel just before.
//!
//! Exit status: 0 on success (a reader that goes away, `| head`, included),
//! 1 when an input could not be read (the other inputs are still shown) or
//! `--check-config` found problems, 2 for usage errors (bad flags, no input
//! at all).
//!
//! `EMDE_TRACE=1` prints the milliseconds each stage took on standard error.

mod prehighlight;
mod survey;

use std::fmt::Write as _;
use std::io::{self, IsTerminal as _, Write as _};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cli::{Cli, DoctorArg, DumpArg, FileArg};
use crate::config::{self, BackgroundMode, Config, ConfigEnv, LoadOptions, Loaded};
use crate::gfx::size;
use crate::gfx::store::{ImageStore, StoreOptions};
use crate::highlight::{self, Highlighter, PlainHighlighter};
use crate::ir::Document;
use crate::layout::{ImageSizer, Layout, NoImages, layout};
use crate::options::{FrontMatterMode, Height, ImageMode, RenderOptions, When};
use crate::pager::{self, FileLoader, PagerDoc, PagerSession};
use crate::parse::{ParseOptions, parse_source};
use crate::render::{ImageRows, RenderConfig, StreamSink, is_broken_pipe};
use crate::source::{Input, Source, SourceError};
use crate::term::env::Env;
use crate::term::probe::{self, LateReplyFilter, Needs, ProbeOutcome, ProbeRequest};
use crate::term::tmux::{self, TmuxInfo};
use crate::term::{Caps, ColorDepth, Graphics, caps, color, doctor};
use crate::theme::Theme;
use prehighlight::Prehighlighted;
use survey::{Hints, Survey, code_blocks};

/// Width when nothing says otherwise.
const DEFAULT_WIDTH: u16 = 80;

/// Configuration problems shown without `-v`, one line each.
const BRIEF_DIAGNOSTICS: usize = 3;

/// When the pager runs: `--paging`, `pager.enabled` (`--plain` is
/// [`Paging::Never`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Paging {
    /// On a terminal, for documents taller than the screen (like `less -F`).
    #[default]
    Auto,
    /// On a terminal, whatever the length.
    Always,
    /// Never: stream mode.
    Never,
}

impl From<When> for Paging {
    fn from(w: When) -> Paging {
        match w {
            When::Auto => Paging::Auto,
            When::Always => Paging::Always,
            When::Never => Paging::Never,
        }
    }
}

/// What the pager is asked for besides showing the documents. Stream mode
/// only uses `paging`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PagerRequest {
    /// Whether the pager runs.
    pub paging: Paging,
    /// `--toc`: open with the outline shown.
    pub toc: bool,
    /// `--anchor SLUG`, else the first `FILE#SLUG`: open at that heading.
    pub anchor: Option<String>,
}

/// A document ready to be shown.
#[derive(Debug)]
pub struct Doc {
    /// Its text and where it came from (the pager reloads and watches that
    /// file, and resolves relative links from it).
    pub source: Source,
    /// The parsed document.
    pub doc: Document,
    /// The heading to open at (`FILE#anchor`).
    pub anchor: Option<String>,
    /// The images of its figures; `None` when it has none or images are
    /// not shown.
    pub images: Option<ImageStore>,
    /// What else it uses.
    survey: Survey,
}

/// What a run decided before showing anything: shared by all documents,
/// and handed to the pager as a whole.
pub struct Setup {
    /// The environment snapshot every decision was made from.
    pub env: Env,
    /// The resolved configuration.
    pub config: Config,
    /// What the terminal can show.
    pub caps: Caps,
    /// The theme for this terminal.
    pub theme: Theme,
    /// Syntax highlighting (plain when no document has code in a language;
    /// shared, as the pager keeps it across layouts).
    pub highlighter: Arc<dyn Highlighter>,
    /// The tmux query, when it ran.
    pub tmux: Option<TmuxInfo>,
    /// The probe, when it ran: after a timeout the pager's input layer must
    /// swallow late replies (`term::probe::LateReplyFilter::after`).
    pub probe: Option<ProbeOutcome>,
    /// Total width in columns.
    pub width: u16,
    /// `-v`: explain every problem in full.
    pub verbose: bool,
}

/// Run emde with parsed arguments.
pub fn run(cli: &Cli) -> ExitCode {
    // Stream mode leaves the terminal as it found it: nothing to restore.
    crate::panic::install_hook(|| {});
    let mut trace = Trace::from_env();
    let env = Env::from_process();
    let is_tty = io::stdout().is_terminal();
    let load = cli.load_options(ConfigEnv::from_process());
    if let Some(code) = print_command(cli, &load) {
        return code;
    }
    if let Some(format) = cli.doctor {
        return run_doctor(cli, format, env, is_tty, &load);
    }

    let (sources, mut status) = match read_sources(cli) {
        Ok(read) => read,
        Err(code) => return code,
    };
    if sources.is_empty() {
        // Every input failed (and was reported): nothing to set up for.
        return status;
    }
    let hints = sources
        .iter()
        .fold(Hints::default(), |h, (_, s)| h.union(Hints::of(&s.text)));
    if hints.code {
        highlight::prewarm();
    }
    trace.stage("read", &hints);

    let loaded = config::load(&load);
    report_config(&mut io::stderr().lock(), &loaded, cli.verbose);
    trace.stage("config", &hints);

    let (setup, docs) = prepare(cli, env, is_tty, loaded, sources, hints, &mut trace);
    let request = pager_request(cli, &setup.config, &docs);
    let layouts = lay_out(&setup, &docs);
    trace.stage("layout", &hints);
    let lines: usize = layouts.iter().map(Layout::len).sum();
    let code = if cli.dump.is_none() && wants_pager(&request, &setup.caps, lines) {
        run_pager(setup, docs, layouts, request)
    } else {
        let mut docs = docs;
        stream(&setup, &mut docs, &layouts, cli.dump)
    };
    trace.stage("output", &hints);
    if code != ExitCode::SUCCESS {
        status = code;
    }
    report_caught(cli.verbose);
    status
}

/// Decide everything about the terminal and the look, and parse the
/// sources meanwhile.
fn prepare(
    cli: &Cli,
    env: Env,
    is_tty: bool,
    loaded: Loaded,
    sources: Vec<(FileArg, Source)>,
    hints: Hints,
    trace: &mut Trace,
) -> (Setup, Vec<Doc>) {
    let config = loaded.config;
    let mut base = color::decide(
        &env,
        is_tty,
        config.terminal.color,
        config.terminal.hyperlinks,
    );
    let width = terminal_width(config.render.width, is_tty, &env, &mut base);
    let needs = probe_needs(&config, hints, base.color);
    let parse_opts = ParseOptions::from(&config.render);
    let front_matter = config.render.front_matter;
    let ask = || ask_terminal(&env, needs, &config, cli.reprobe);
    let (docs, answers) = std::thread::scope(|s| {
        let asked = (is_tty && needs.any()).then(|| {
            std::thread::Builder::new()
                .name("emde-probe".into())
                .spawn_scoped(s, ask)
        });
        let docs: Vec<Doc> = sources
            .into_iter()
            .map(|(arg, source)| parse_doc(arg, source, &parse_opts, front_matter))
            .collect();
        let answers = match asked {
            Some(Ok(probing)) => probing.join().unwrap_or_default(),
            // No thread to spare: ask now.
            Some(Err(_)) => ask(),
            None => Answers::default(),
        };
        (docs, answers)
    });
    trace.stage("parse+probe", &hints);

    let replies = answers.probe.as_ref().and_then(ProbeOutcome::answers);
    let caps = caps::decide(
        &env,
        base,
        answers.tmux.as_ref(),
        replies,
        &config.render.images,
    );
    let theme = config::build_theme(&config, caps.background, caps.color);
    let (highlighter, stores) =
        highlighter_and_images(&docs, &caps, &config, &theme, &loaded.diagnostics);
    let mut docs = docs;
    for (doc, store) in docs.iter_mut().zip(stores) {
        doc.images = store;
    }
    trace.stage("setup", &hints);
    let setup = Setup {
        env,
        config,
        caps,
        theme,
        highlighter: Arc::from(highlighter),
        tmux: answers.tmux,
        probe: answers.probe,
        width,
        verbose: cli.verbose,
    };
    (setup, docs)
}

// --- Commands that only print ------------------------------------------------

/// `--print-default-config`, `--list-*`, `--credits` and `--check-config`:
/// the exit status when one of them was asked for.
fn print_command(cli: &Cli, load: &LoadOptions) -> Option<ExitCode> {
    if cli.print_default_config {
        return Some(print(config::DEFAULT_CONFIG));
    }
    if cli.list_themes {
        return Some(print(&theme_list(load)));
    }
    if cli.list_code_themes {
        let mut names = highlight::list_code_themes().join("\n");
        names.push('\n');
        return Some(print(&names));
    }
    if cli.list_languages {
        let mut out = String::new();
        for (name, tokens) in highlight::list_languages() {
            let _ = writeln!(out, "{name}: {}", tokens.join(", "));
        }
        return Some(print(&out));
    }
    if cli.credits {
        return Some(print(&credits()));
    }
    if cli.check_config {
        let report = config::check(load);
        let written = print(&format!("{report}\n"));
        return Some(if written == ExitCode::SUCCESS {
            ExitCode::from(report.exit_code())
        } else {
            written
        });
    }
    None
}

/// `--list-themes`: built-in themes, then installed ones with their files.
fn theme_list(load: &LoadOptions) -> String {
    let mut out = String::new();
    for t in crate::theme::list_themes(&load.env) {
        match t.path {
            Some(path) => {
                let _ = writeln!(out, "{:<16} {}", t.name, path.display());
            }
            None => {
                let _ = writeln!(out, "{:<16} built-in", t.name);
            }
        }
    }
    out
}

/// `--credits`: what emde bundles, and the licences it comes under.
fn credits() -> String {
    let notices = highlight::credits();
    if notices.is_empty() {
        return "emde: this build bundles no third-party syntaxes or code themes.\n".into();
    }
    format!(
        "emde bundles syntax definitions and code themes from two-face (bat), \
         under these licences:\n\n{notices}"
    )
}

/// `--doctor[=json]`: every terminal decision with its reason. The probe
/// asks everything, but still only on a terminal.
fn run_doctor(
    cli: &Cli,
    format: DoctorArg,
    env: Env,
    is_tty: bool,
    load: &LoadOptions,
) -> ExitCode {
    let loaded = config::load(load);
    report_config(&mut io::stderr().lock(), &loaded, cli.verbose);
    let config = loaded.config;
    let mut base = color::decide(
        &env,
        is_tty,
        config.terminal.color,
        config.terminal.hyperlinks,
    );
    terminal_width(config.render.width, is_tty, &env, &mut base);
    let answers = if is_tty {
        ask_terminal(&env, Needs::ALL, &config, cli.reprobe)
    } else {
        Answers::default()
    };
    let replies = answers.probe.as_ref().and_then(ProbeOutcome::answers);
    let caps = caps::decide(
        &env,
        base,
        answers.tmux.as_ref(),
        replies,
        &config.render.images,
    );
    let tmux = answers.tmux.as_ref();
    let probe = answers.probe.as_ref();
    print(&match format {
        DoctorArg::Text => doctor::report(&env, &caps, tmux, probe),
        DoctorArg::Json => doctor::json(&env, &caps, tmux, probe),
    })
}

/// Write `text` to standard output; a reader that went away is fine.
fn print(text: &str) -> ExitCode {
    let mut out = io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => write_failed(&e),
    }
}

// --- Sources -----------------------------------------------------------------

/// Read every input. A missing input is reported and skipped (status 1);
/// no input at all is a usage error (status 2).
fn read_sources(cli: &Cli) -> Result<(Vec<(FileArg, Source)>, ExitCode), ExitCode> {
    let args = if cli.files.is_empty() {
        vec![None]
    } else {
        cli.file_args().into_iter().map(Some).collect()
    };
    let mut status = ExitCode::SUCCESS;
    let mut sources = Vec::with_capacity(args.len());
    for arg in args {
        let input = match Input::from_arg(arg.as_ref().map(|a| a.path.as_path())) {
            Ok(input) => input,
            Err(e) => {
                error(&e);
                return Err(ExitCode::from(2));
            }
        };
        match Source::load(&input) {
            Ok(source) => {
                let arg = arg.unwrap_or_else(|| FileArg {
                    path: "-".into(),
                    anchor: None,
                });
                sources.push((arg, source));
            }
            Err(e @ SourceError::NoInput) => {
                error(&e);
                return Err(ExitCode::from(2));
            }
            Err(e) => {
                error(&e);
                status = ExitCode::from(1);
            }
        }
    }
    Ok((sources, status))
}

/// Parse one source; `front_matter` is how its front matter is shown.
fn parse_doc(
    arg: FileArg,
    source: Source,
    opts: &ParseOptions,
    front_matter: FrontMatterMode,
) -> Doc {
    let doc = parse_source(&source, opts);
    let survey = Survey::of(&doc, front_matter);
    Doc {
        source,
        doc,
        anchor: arg.anchor,
        images: None,
        survey,
    }
}

// --- The terminal ------------------------------------------------------------

/// What the probe needs to ask: the background for `theme.background =
/// "auto"` (when there are colours to choose), graphics when a document
/// may have images to show.
fn probe_needs(config: &Config, hints: Hints, depth: ColorDepth) -> Needs {
    let images_on = config.render.images.mode != ImageMode::None && cfg!(feature = "images");
    Needs {
        background: config.theme.background == BackgroundMode::Auto && depth >= ColorDepth::Ansi16,
        graphics: hints.images && images_on && depth != ColorDepth::None,
    }
}

/// The answers of the tmux query and the probe.
#[derive(Debug, Default)]
struct Answers {
    tmux: Option<TmuxInfo>,
    probe: Option<ProbeOutcome>,
}

/// The tmux query (inside tmux, when graphics matter) and the probe
/// (unless `terminal.probe = false`), for what `needs` asks. The probe
/// itself refuses to run unless stdout is a terminal.
fn ask_terminal(env: &Env, needs: Needs, config: &Config, reprobe: bool) -> Answers {
    let tmux = if needs.graphics && color::in_tmux(env) {
        tmux::query(env)
    } else {
        None
    };
    let terminal = &config.terminal;
    let probe = if terminal.probe {
        // 0 is automatic: the probe picks the deadline for the session.
        let timeout = (terminal.probe_timeout_ms > 0)
            .then(|| Duration::from_millis(u64::from(terminal.probe_timeout_ms)));
        probe::probe(&ProbeRequest {
            env,
            tmux: tmux.as_ref(),
            needs,
            timeout,
            reprobe,
            tmux_passthrough: config.render.images.tmux_passthrough,
        })
    } else {
        None
    };
    Answers { tmux, probe }
}

/// The total width: `--width`, else the terminal's (recorded in
/// `caps.size`), else `$COLUMNS`, else 80.
fn terminal_width(forced: Option<u16>, is_tty: bool, env: &Env, caps: &mut Caps) -> u16 {
    let screen = if is_tty {
        crossterm::terminal::size().ok()
    } else {
        None
    };
    width_for(forced, screen, env, caps)
}

/// [`terminal_width`] for a terminal of `screen` (columns, rows), if
/// known. A size of zero (some pseudo-terminals report 0×0) is unknown.
fn width_for(forced: Option<u16>, screen: Option<(u16, u16)>, env: &Env, caps: &mut Caps) -> u16 {
    if let Some((cols, rows)) = screen.filter(|&(cols, rows)| cols > 0 && rows > 0) {
        caps.size = Some((cols, rows));
        if forced.is_none() {
            return cols;
        }
    }
    forced
        .filter(|&w| w > 0)
        .or_else(|| columns(env))
        .unwrap_or(DEFAULT_WIDTH)
}

/// `$COLUMNS`, if it is a positive number.
fn columns(env: &Env) -> Option<u16> {
    env.non_empty("COLUMNS")?
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|&w| w > 0)
}

// --- Highlighting and images -------------------------------------------------

/// The highlighter and the image stores of the documents, made side by
/// side when there is work for both (fetching images can take a while).
fn highlighter_and_images(
    docs: &[Doc],
    caps: &Caps,
    config: &Config,
    theme: &Theme,
    reported: &[config::Diagnostic],
) -> (Box<dyn Highlighter>, Vec<Option<ImageStore>>) {
    let highlighter = || highlighter_for(docs, theme, config, reported);
    let stores = || {
        docs.iter()
            .map(|d| image_store(d, caps, config, theme))
            .collect::<Vec<_>>()
    };
    let code = docs.iter().any(|d| !d.survey.langs.is_empty());
    let figures = docs.iter().any(|d| !d.survey.figures.is_empty());
    if !(code && figures && caps.graphics != Graphics::None) {
        return (highlighter(), stores());
    }
    std::thread::scope(|s| {
        let highlighting = std::thread::Builder::new().spawn_scoped(s, highlighter);
        let stores = stores();
        let highlighter = match highlighting {
            Ok(h) => h.join().unwrap_or_else(|_| Box::new(PlainHighlighter)),
            Err(_) => highlighter(),
        };
        (highlighter, stores)
    })
}

/// The highlighter: syntect with the theme's code theme when a document
/// has code in some language, else plain. With several code blocks they
/// are highlighted in parallel right away ([`Prehighlighted`]). A code
/// theme that cannot be loaded is reported, unless the configuration
/// already did.
fn highlighter_for(
    docs: &[Doc],
    theme: &Theme,
    config: &Config,
    reported: &[config::Diagnostic],
) -> Box<dyn Highlighter> {
    if docs.iter().all(|d| d.survey.langs.is_empty()) {
        return Box::new(PlainHighlighter);
    }
    let code = &config.render.code;
    let (highlighter, warning) = highlight::create(&theme.code_theme, code);
    if let Some(w) = warning
        && !reported.iter().any(|d| d.message.contains(&w))
    {
        warn(&w);
    }
    let blocks: Vec<_> = docs.iter().flat_map(|d| code_blocks(&d.doc)).collect();
    if blocks.len() < 2 || !cfg!(feature = "highlight") {
        return highlighter;
    }
    Box::new(Prehighlighted::new(
        highlighter,
        &blocks,
        code.max_highlight_bytes,
    ))
}

/// The image store of a document with figures, when images are shown.
fn image_store(doc: &Doc, caps: &Caps, config: &Config, theme: &Theme) -> Option<ImageStore> {
    if doc.survey.figures.is_empty() || caps.graphics == Graphics::None {
        return None;
    }
    let opts = StoreOptions::new(caps, &config.render.images, theme);
    Some(ImageStore::load(&doc.doc, &doc.survey.figures, opts))
}

// --- Output --------------------------------------------------------------------

/// The pager request: `--paging`/`--plain` through the configuration,
/// `--toc`, and `--anchor` or else the first document's `#anchor`.
fn pager_request(cli: &Cli, config: &Config, docs: &[Doc]) -> PagerRequest {
    PagerRequest {
        paging: config.pager.enabled.into(),
        toc: cli.toc,
        anchor: cli
            .anchor
            .clone()
            .or_else(|| docs.first().and_then(|d| d.anchor.clone())),
    }
}

/// Whether the pager would run: on a terminal, unless paging is off, for
/// output taller than the screen or when paging is `always`.
fn wants_pager(request: &PagerRequest, caps: &Caps, lines: usize) -> bool {
    if !caps.is_tty {
        return false;
    }
    match request.paging {
        Paging::Never => false,
        Paging::Always => true,
        Paging::Auto => caps
            .size
            .is_some_and(|(_, rows)| lines >= usize::from(rows)),
    }
}

/// Show the documents in the built-in pager.
///
/// The pager shows one document at a time: the first one opens, and linked
/// local documents load in-app. `layouts` are the stream-mode layouts the
/// paging decision was made on (figures capped for stream mode), so the
/// pager lays out for itself with the configured image heights.
fn run_pager(
    setup: Setup,
    mut docs: Vec<Doc>,
    layouts: Vec<Layout>,
    request: PagerRequest,
) -> ExitCode {
    drop(layouts);
    if docs.is_empty() {
        return ExitCode::SUCCESS;
    }
    let first = docs.swap_remove(0);
    let Doc {
        source,
        doc,
        anchor,
        images,
        ..
    } = first;
    let render = setup.config.render.clone();
    let parse = ParseOptions::from(&render);
    let mut session =
        PagerSession::new(PagerDoc::new(source, doc), setup.theme, setup.caps, render);
    session.loader = Box::new(FileLoader::new(parse));
    session.highlighter = setup.highlighter;
    session.pager = setup.config.pager.clone();
    session.anchor = request.anchor.or(anchor);
    session.open_toc = request.toc;
    session.env = setup.env;
    session.tmux = setup.tmux;
    session.late_replies = LateReplyFilter::after(setup.probe.as_ref(), Instant::now());
    if let Some(store) = images {
        session.images = Box::new(FirstDocImages(store));
    }
    match pager::run(session) {
        Ok(exit) => ExitCode::from(exit.code()),
        Err(e) => {
            let _ = writeln!(io::stderr(), "emde: pager: {e}");
            ExitCode::from(1)
        }
    }
}

/// Figure sizes for the pager from the first document's image store.
/// Linked documents opened in the pager show their figures as alt boxes.
struct FirstDocImages(ImageStore);

impl pager::ImageProvider for FirstDocImages {
    fn figure_cells(
        &self,
        doc: &Document,
        image: crate::ir::ImageId,
        max_cols: u16,
        max_rows: u16,
    ) -> Option<(u16, u16)> {
        // Only the store's own document has its images loaded.
        if !self.0.belongs_to(doc) {
            return None;
        }
        self.0.cells(image, max_cols, max_rows)
    }
}

/// Stream mode shows figures at most [`size::STREAM_MAX_ROWS`] rows tall,
/// and on a terminal never taller than the screen (a pixel image must fit
/// on it to be drawn over its rows).
fn stream_options(render: &RenderOptions, caps: &Caps) -> RenderOptions {
    let mut opts = render.clone();
    let mut rows = size::max_rows(opts.images.max_height, None);
    if caps.is_tty
        && let Some((_, screen)) = caps.size
    {
        rows = rows.min(screen.saturating_sub(1).max(1));
    }
    opts.images.max_height = Height::Rows(rows);
    opts
}

/// Lay every document out for stream mode.
fn lay_out(setup: &Setup, docs: &[Doc]) -> Vec<Layout> {
    let opts = stream_options(&setup.config.render, &setup.caps);
    docs.iter()
        .map(|d| {
            let sizer: &dyn ImageSizer = match &d.images {
                Some(store) => store,
                None => &NoImages,
            };
            layout(
                &d.doc,
                setup.width,
                &setup.theme,
                &setup.caps,
                &opts,
                &*setup.highlighter,
                sizer,
            )
        })
        .collect()
}

/// Write every document to standard output (or its dump), then, with
/// `-v`, the problems met in the documents.
fn stream(setup: &Setup, docs: &mut [Doc], layouts: &[Layout], dump: Option<DumpArg>) -> ExitCode {
    let mut sink = StreamSink::new(io::stdout().lock(), RenderConfig::from_caps(&setup.caps));
    for (doc, layout) in docs.iter_mut().zip(layouts) {
        let written = match dump {
            Some(DumpArg::Ir) => sink.write_text(&doc.doc.dump()),
            Some(DumpArg::Lines) => sink.write_text(&layout.dump()),
            None => {
                if let Some(store) = doc.images.as_mut() {
                    store.prepare_stream(layout);
                }
                let images = doc.images.as_ref().map(|s| s as &dyn ImageRows);
                sink.write_document_with(&doc.doc, layout, images)
            }
        };
        if let Err(e) = written {
            return write_failed(&e);
        }
    }
    let status = match sink.finish() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => write_failed(&e),
    };
    report_content(&mut io::stderr().lock(), docs, setup.verbose);
    status
}

// --- Messages ------------------------------------------------------------------

/// Report configuration problems: in full with `-v`, else the first line
/// of the first few, and where to find the rest.
fn report_config(out: &mut dyn io::Write, loaded: &Loaded, verbose: bool) {
    let diags = &loaded.diagnostics;
    if verbose {
        for d in diags {
            let _ = writeln!(out, "emde: {d}");
        }
        return;
    }
    let mut cut = diags.len() > BRIEF_DIAGNOSTICS;
    for d in diags.iter().take(BRIEF_DIAGNOSTICS) {
        let text = d.to_string();
        let mut lines = text.lines();
        let _ = writeln!(out, "emde: {}", lines.next().unwrap_or_default());
        cut |= lines.next().is_some();
    }
    if cut {
        let _ = writeln!(
            out,
            "emde: {} configuration problem{} in all; `emde --check-config` shows each in full",
            diags.len(),
            if diags.len() == 1 { "" } else { "s" }
        );
    }
}

/// With `-v`, the content problems of every document (input decoding,
/// markup, images) on standard error.
fn report_content(out: &mut dyn io::Write, docs: &[Doc], verbose: bool) {
    if !verbose {
        return;
    }
    for d in docs {
        for diag in &d.doc.diagnostics {
            let _ = writeln!(out, "emde: {}: {diag}", d.source.origin);
        }
        for problem in d.images.iter().flat_map(ImageStore::problems) {
            let _ = writeln!(out, "emde: {}: image {problem}", d.source.origin);
        }
    }
}

/// With `-v`, panics that were caught (and worked around) along the way.
fn report_caught(verbose: bool) {
    let caught = crate::panic::take_caught();
    if !verbose || caught.is_empty() {
        return;
    }
    let mut err = io::stderr().lock();
    for message in caught {
        let _ = writeln!(err, "emde: recovered from an internal error: {message}");
    }
}

/// Print an error on standard error.
fn error(e: &dyn std::fmt::Display) {
    let _ = writeln!(io::stderr(), "emde: {e}");
}

/// Print a warning on standard error.
fn warn(message: &str) {
    let _ = writeln!(io::stderr(), "emde: warning: {message}");
}

/// The exit status after a failed write: a closed pipe is a normal end.
fn write_failed(e: &io::Error) -> ExitCode {
    if is_broken_pipe(e) {
        ExitCode::SUCCESS
    } else {
        error(e);
        ExitCode::from(1)
    }
}

/// `EMDE_TRACE=1`: the milliseconds each stage took, on standard error.
struct Trace {
    last: Option<Instant>,
}

impl Trace {
    fn from_env() -> Trace {
        let on = std::env::var_os("EMDE_TRACE").is_some_and(|v| !v.is_empty() && v != "0");
        Trace {
            last: on.then(Instant::now),
        }
    }

    fn stage(&mut self, name: &str, hints: &Hints) {
        let Some(last) = self.last.as_mut() else {
            return;
        };
        let now = Instant::now();
        let ms = now.duration_since(*last).as_secs_f64() * 1000.0;
        *last = now;
        let _ = writeln!(
            io::stderr(),
            "emde: trace {name:<12} {ms:>8.3} ms  {hints:?}"
        );
    }
}

#[cfg(test)]
mod tests;
