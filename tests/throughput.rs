//! Throughput on a generated 10 MB prose document (source → parse → layout
//! → bytes), per stage and end to end. The target is ≥ 50 MB/s.
//!
//! Ignored by default; run it optimised:
//!
//! ```sh
//! cargo test --release --test throughput -- --ignored --nocapture
//! ```

mod common;

use std::io::Write as _;
use std::time::Instant;

use emde::highlight::PlainHighlighter;
use emde::layout::{NoImages, layout};
use emde::options::RenderOptions;
use emde::parse::{ParseOptions, parse_source};
use emde::render::{RenderConfig, to_bytes};
use emde::source::{Origin, Source};
use emde::term::Caps;
use emde::theme::Theme;

/// About `bytes` of prose: paragraphs of pseudo-random words with a little
/// inline markup, and a heading now and then.
fn prose(bytes: usize) -> String {
    const WORDS: &[&str] = &[
        "the",
        "quick",
        "brown",
        "fox",
        "jumps",
        "over",
        "lazy",
        "dog",
        "terminal",
        "markdown",
        "reader",
        "renders",
        "every",
        "paragraph",
        "with",
        "care",
        "and",
        "speed",
        "while",
        "wrapping",
        "lines",
        "at",
        "word",
        "boundaries",
        "so",
        "that",
        "text",
        "stays",
        "readable",
    ];
    let mut out = String::with_capacity(bytes + 1024);
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut para = 0;
    while out.len() < bytes {
        if para % 20 == 0 {
            out.push_str("## Section heading\n\n");
        }
        let words = 60 + next() % 80;
        for i in 0..words {
            let w = WORDS[(next() % WORDS.len() as u64) as usize];
            match next() % 40 {
                0 => {
                    out.push('*');
                    out.push_str(w);
                    out.push('*');
                }
                1 => {
                    out.push_str("**");
                    out.push_str(w);
                    out.push_str("**");
                }
                _ => out.push_str(w),
            }
            out.push(if i + 1 == words { '.' } else { ' ' });
        }
        out.push_str("\n\n");
        para += 1;
    }
    out
}

fn mbs(bytes: usize, secs: f64) -> f64 {
    bytes as f64 / 1e6 / secs.max(1e-9)
}

#[test]
#[ignore = "benchmark: cargo test --release --test throughput -- --ignored --nocapture"]
fn prose_throughput() {
    let md = prose(10 * 1024 * 1024);
    let n = md.len();
    let theme = Theme::test();
    let opts = RenderOptions::default();
    let mut report = String::new();
    // Warm up (allocator, page cache, CPU clocks) before measuring.
    {
        let source = Source::from_bytes(md.as_bytes().to_vec(), Origin::Memory);
        let doc = parse_source(&source, &ParseOptions::default());
        let caps = Caps::plain();
        let l = layout(
            &doc,
            100,
            &theme,
            &caps,
            &opts,
            &PlainHighlighter,
            &NoImages,
        );
        let _ = to_bytes(&doc, &l, &RenderConfig::from_caps(&caps));
    }
    for (label, caps) in [("piped", Caps::plain()), ("truecolor", Caps::full())] {
        // Best of three, to keep scheduling noise out.
        let mut best = [f64::MAX; 5];
        let mut out_len = 0;
        for _ in 0..3 {
            let t0 = Instant::now();
            let source = Source::from_bytes(md.as_bytes().to_vec(), Origin::Memory);
            let t1 = Instant::now();
            let doc = parse_source(&source, &ParseOptions::default());
            let t2 = Instant::now();
            let l = layout(
                &doc,
                100,
                &theme,
                &caps,
                &opts,
                &PlainHighlighter,
                &NoImages,
            );
            let t3 = Instant::now();
            let cfg = RenderConfig::from_caps(&caps);
            let bytes = to_bytes(&doc, &l, &cfg);
            let t4 = Instant::now();
            out_len = bytes.len();
            let times = [
                (t1 - t0).as_secs_f64(),
                (t2 - t1).as_secs_f64(),
                (t3 - t2).as_secs_f64(),
                (t4 - t3).as_secs_f64(),
                (t4 - t0).as_secs_f64(),
            ];
            for (b, t) in best.iter_mut().zip(times) {
                *b = b.min(t);
            }
        }
        report.push_str(&format!(
            "{label:>9}: {:.1} MB in, {:.1} MB out | source {:.0} MB/s, parse {:.0} MB/s, layout {:.0} MB/s, render {:.0} MB/s | end to end {:.1} MB/s\n",
            n as f64 / 1e6,
            out_len as f64 / 1e6,
            mbs(n, best[0]),
            mbs(n, best[1]),
            mbs(n, best[2]),
            mbs(n, best[3]),
            mbs(n, best[4]),
        ));
    }
    let _ = std::io::stdout().write_all(report.as_bytes());
}

#[test]
fn generator_makes_prose() {
    let p = prose(10_000);
    assert!(p.len() >= 10_000);
    assert!(p.contains("## Section heading"));
    let doc = common::parse(&p);
    assert!(doc.blocks.len() > 3);
}

#[test]
#[ignore = "benchmark: cargo test --release --test throughput -- --ignored --nocapture"]
fn wrapping_costs() {
    use emde::text::{Constraints, WrapOptions, Wrapper, break_opportunities};
    let md = prose(10 * 1024 * 1024);
    let paras: Vec<&str> = md.split("\n\n").collect();
    let n = md.len();
    let t = Instant::now();
    let mut breaks = Vec::new();
    let mut count = 0usize;
    for p in &paras {
        break_opportunities(p, &[], &mut breaks);
        count += breaks.len();
    }
    let t_breaks = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let mut w = Wrapper::new();
    let mut lines = Vec::new();
    let mut total = 0usize;
    for p in &paras {
        w.wrap_into(
            p,
            Constraints::default(),
            WrapOptions::uniform(96),
            &mut lines,
        );
        total += lines.len();
    }
    let t_wrap = t.elapsed().as_secs_f64();
    let _ = writeln!(
        std::io::stdout(),
        "breaks {:.0} MB/s ({count}), wrap incl. breaks {:.0} MB/s ({total} lines)",
        mbs(n, t_breaks),
        mbs(n, t_wrap)
    );
}
