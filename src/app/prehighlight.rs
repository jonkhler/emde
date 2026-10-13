//! Highlighting code blocks in parallel, ahead of layout.
//!
//! Layout highlights code blocks one after another as it meets them. With
//! syntect, the first block of each language pays for loading its grammar
//! and compiling its regexes (several milliseconds), and every block then
//! costs tens of microseconds per line. [`Prehighlighted`] starts that work
//! on background threads, the first block of each language first, so
//! languages are compiled side by side, and returns at once: layout goes
//! on meanwhile and takes each block's result when it gets there, waiting
//! for one being made, or making it itself when no thread has started it
//! yet. A block without a result (it was too large, or its language
//! unknown) is handled by the wrapped highlighter as usual.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::highlight::{CodeColors, Highlighter, HlBlock, LangId};
use crate::ir::CodeBlock;
use crate::panic::guarded;

/// Most threads used for highlighting (the layout thread included).
const MAX_THREADS: usize = 8;

/// Where a block's highlighting is.
enum Slot {
    /// Nobody started it.
    Waiting,
    /// A thread is making it.
    Running,
    /// Made: its language and highlighting (`None`: an unknown language,
    /// or highlighting panicked).
    Done(Option<(LangId, HlBlock)>),
}

/// What the threads share.
struct Shared {
    inner: Box<dyn Highlighter>,
    /// The blocks to highlight, in the order they are started.
    jobs: Vec<(Box<str>, Arc<str>)>,
    slots: Mutex<Vec<Slot>>,
    /// Signalled whenever a slot becomes [`Slot::Done`].
    done: Condvar,
}

impl Shared {
    fn slots(&self) -> MutexGuard<'_, Vec<Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Highlight job `i` (claimed by the caller) and record the result.
    fn run(&self, i: usize) -> Option<(LangId, HlBlock)> {
        let (token, code) = &self.jobs[i];
        let result = guarded(|| {
            let lang = self.inner.resolve(token)?;
            Some((lang, self.inner.highlight(lang, code)))
        })
        .flatten();
        self.slots()[i] = Slot::Done(result.clone());
        self.done.notify_all();
        result
    }

    /// Claim the next waiting job, if any.
    fn claim_next(&self) -> Option<usize> {
        let mut slots = self.slots();
        let i = slots.iter().position(|s| matches!(s, Slot::Waiting))?;
        slots[i] = Slot::Running;
        Some(i)
    }

    /// The result of job `i`: made already, waited for, or made now.
    fn result(&self, i: usize) -> Option<(LangId, HlBlock)> {
        let mut slots = self.slots();
        loop {
            match &slots[i] {
                Slot::Done(result) => return result.clone(),
                Slot::Running => {
                    slots = self
                        .done
                        .wait(slots)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                Slot::Waiting => {
                    slots[i] = Slot::Running;
                    drop(slots);
                    return self.run(i);
                }
            }
        }
    }
}

/// A highlighter that answers from highlights made ahead, on other threads.
pub(super) struct Prehighlighted {
    shared: Arc<Shared>,
    /// The jobs of each code text.
    by_code: HashMap<Arc<str>, Vec<usize>>,
}

impl Prehighlighted {
    /// Start highlighting `blocks` with `inner` in the background. Blocks
    /// larger than `max_bytes` are left alone, as layout leaves them
    /// unhighlighted.
    pub(super) fn new(
        inner: Box<dyn Highlighter>,
        blocks: &[&CodeBlock],
        max_bytes: usize,
    ) -> Prehighlighted {
        let jobs: Vec<(Box<str>, Arc<str>)> = schedule(blocks, max_bytes)
            .into_iter()
            .map(|(token, code)| (token.into(), code.into()))
            .collect();
        let mut by_code: HashMap<Arc<str>, Vec<usize>> = HashMap::new();
        for (i, (_, code)) in jobs.iter().enumerate() {
            by_code.entry(Arc::clone(code)).or_default().push(i);
        }
        let slots = jobs.iter().map(|_| Slot::Waiting).collect();
        let shared = Arc::new(Shared {
            inner,
            jobs,
            slots: Mutex::new(slots),
            done: Condvar::new(),
        });
        // The layout thread is one of the threads: it makes what nobody
        // started when it gets there.
        let helpers = crate::parallel::cpus()
            .min(MAX_THREADS)
            .min(shared.jobs.len())
            .saturating_sub(1);
        for _ in 0..helpers {
            let shared = Arc::clone(&shared);
            let spawned = std::thread::Builder::new()
                .name("emde-highlight".into())
                .spawn(move || {
                    while let Some(i) = shared.claim_next() {
                        shared.run(i);
                    }
                });
            if spawned.is_err() {
                break;
            }
        }
        Prehighlighted { shared, by_code }
    }

    /// Wait until every block is highlighted (for tests).
    #[cfg(test)]
    fn wait_all(&self) {
        for i in 0..self.shared.jobs.len() {
            self.shared.result(i);
        }
    }
}

/// The distinct (token, code) pairs to highlight, the first block of each
/// language first (in reading order), then the second of each, and so on.
fn schedule<'b>(blocks: &[&'b CodeBlock], max_bytes: usize) -> Vec<(&'b str, &'b str)> {
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut per_token: HashMap<&str, usize> = HashMap::new();
    let mut ranked: Vec<(usize, (&str, &str))> = Vec::new();
    for cb in blocks {
        let Some(token) = cb.lang.as_deref() else {
            continue;
        };
        let code = cb.code.as_str();
        if code.len() > max_bytes || !seen.insert((token, code)) {
            continue;
        }
        let rank = per_token.entry(token).or_default();
        ranked.push((*rank, (token, code)));
        *rank += 1;
    }
    // Stable: equal ranks keep their reading order.
    ranked.sort_by_key(|&(rank, _)| rank);
    ranked.into_iter().map(|(_, job)| job).collect()
}

impl Highlighter for Prehighlighted {
    fn resolve(&self, token: &str) -> Option<LangId> {
        self.shared.inner.resolve(token)
    }

    fn highlight(&self, lang: LangId, code: &str) -> HlBlock {
        for &i in self.by_code.get(code).into_iter().flatten() {
            if let Some((made, block)) = self.shared.result(i)
                && made == lang
            {
                return block;
            }
        }
        self.shared.inner.highlight(lang, code)
    }

    fn colors(&self) -> CodeColors {
        self.shared.inner.colors()
    }

    fn language_name(&self, lang: LangId) -> String {
        self.shared.inner.language_name(lang)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::highlight::HlSpan;
    use crate::style::Style;

    /// A highlighter that records what it highlights and marks each block
    /// with its length.
    #[derive(Default)]
    struct Recording {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Highlighter for Recording {
        fn resolve(&self, token: &str) -> Option<LangId> {
            match token {
                "rust" => Some(LangId(1)),
                "python" => Some(LangId(2)),
                _ => None,
            }
        }

        fn highlight(&self, lang: LangId, code: &str) -> HlBlock {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(format!("{}:{code}", lang.0));
            }
            HlBlock {
                lines: vec![vec![HlSpan {
                    end: u32::try_from(code.len()).unwrap_or(0),
                    style: Style::PLAIN,
                }]],
            }
        }

        fn colors(&self) -> CodeColors {
            CodeColors::default()
        }

        fn language_name(&self, lang: LangId) -> String {
            format!("lang{}", lang.0)
        }
    }

    fn block(lang: &str, code: &str) -> CodeBlock {
        CodeBlock {
            lang: Some(lang.into()),
            info: lang.into(),
            title: None,
            code: code.into(),
        }
    }

    #[test]
    fn first_blocks_of_each_language_go_first() {
        let blocks = [
            block("rust", "a"),
            block("rust", "b"),
            block("python", "c"),
            block("rust", "a"),
            block("python", "d"),
            block("go", "e"),
        ];
        let refs: Vec<&CodeBlock> = blocks.iter().collect();
        let jobs = schedule(&refs, 100);
        assert_eq!(
            jobs,
            [
                ("rust", "a"),
                ("python", "c"),
                ("go", "e"),
                ("rust", "b"),
                ("python", "d")
            ]
        );
        // Too large to highlight: left alone.
        assert_eq!(schedule(&refs, 0), []);
    }

    #[test]
    fn answers_come_from_the_advance_work() {
        let blocks = [
            block("rust", "fn x"),
            block("python", "y = 1"),
            block("go", "z"),
        ];
        let refs: Vec<&CodeBlock> = blocks.iter().collect();
        let recording = Recording::default();
        let calls = Arc::clone(&recording.calls);
        let count = || calls.lock().map(|c| c.len()).unwrap_or(0);
        let h = Prehighlighted::new(Box::new(recording), &refs, 1000);
        h.wait_all();
        assert_eq!(count(), 2, "go is unknown");
        let rust = h.resolve("rust").unwrap();
        assert_eq!(h.highlight(rust, "fn x").lines[0][0].end, 4);
        assert_eq!(count(), 2, "answered from the advance work");
        // Something not done in advance is highlighted then.
        assert_eq!(h.highlight(rust, "other").lines[0][0].end, 5);
        assert_eq!(count(), 3);
        assert_eq!(h.language_name(rust), "lang1");
        assert_eq!(h.colors(), CodeColors::default());
        assert_eq!(h.resolve("go"), None);
    }

    #[test]
    fn layout_takes_results_as_they_come() {
        // Many blocks, asked for at once while the threads work: every
        // answer is what the highlighter itself gives, whoever made it.
        let blocks: Vec<CodeBlock> = (0..200)
            .map(|i| {
                let lang = if i % 3 == 0 { "python" } else { "rust" };
                block(lang, &"x".repeat(i % 17 + 1))
            })
            .chain([block("python", "shared"), block("rust", "shared")])
            .collect();
        let refs: Vec<&CodeBlock> = blocks.iter().collect();
        for _ in 0..5 {
            let h = Prehighlighted::new(Box::new(Recording::default()), &refs, 1000);
            let reference = Recording::default();
            for b in &blocks {
                let lang = h.resolve(b.lang.as_deref().unwrap()).unwrap();
                assert_eq!(
                    h.highlight(lang, &b.code),
                    reference.highlight(lang, &b.code)
                );
            }
            // The same code in two languages: each gets its own.
            let python = h.resolve("python").unwrap();
            let rust = h.resolve("rust").unwrap();
            assert_ne!(python, rust);
            assert_eq!(
                h.highlight(python, "shared"),
                reference.highlight(python, "shared")
            );
        }
    }
}
