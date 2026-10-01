//! Highlighting code blocks in parallel before layout.
//!
//! Layout highlights code blocks one after another as it meets them. With
//! syntect, the first block of each language pays for loading its grammar
//! and compiling its regexes (several milliseconds), and every block then
//! costs tens of microseconds per line. [`Prehighlighted`] does that work up
//! front on several threads, the first block of each language first, so
//! languages are compiled side by side; layout then reads the results. A
//! block without a result (it was too large, or its language unknown) is
//! handled by the wrapped highlighter as usual.

use std::collections::{HashMap, HashSet};

use crate::highlight::{CodeColors, Highlighter, HlBlock, LangId};
use crate::ir::CodeBlock;

/// Most threads used for highlighting.
const MAX_THREADS: usize = 8;

/// A highlighter that answers from highlights made in advance.
pub(super) struct Prehighlighted {
    inner: Box<dyn Highlighter>,
    /// Results by language, then by code.
    done: HashMap<LangId, HashMap<Box<str>, HlBlock>>,
}

impl Prehighlighted {
    /// Highlight `blocks` with `inner` in parallel. Blocks larger than
    /// `max_bytes` are left alone, as layout leaves them unhighlighted.
    pub(super) fn new(
        inner: Box<dyn Highlighter>,
        blocks: &[&CodeBlock],
        max_bytes: usize,
    ) -> Prehighlighted {
        let jobs = schedule(blocks, max_bytes);
        let results = crate::parallel::map(&jobs, MAX_THREADS, |&(token, code)| {
            let lang = inner.resolve(token)?;
            Some((lang, inner.highlight(lang, code)))
        });
        let mut done: HashMap<LangId, HashMap<Box<str>, HlBlock>> = HashMap::new();
        for ((_, code), result) in jobs.iter().zip(results) {
            if let Some(Some((lang, block))) = result {
                done.entry(lang).or_default().insert((*code).into(), block);
            }
        }
        Prehighlighted { inner, done }
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
        self.inner.resolve(token)
    }

    fn highlight(&self, lang: LangId, code: &str) -> HlBlock {
        match self.done.get(&lang).and_then(|by_code| by_code.get(code)) {
            Some(block) => block.clone(),
            None => self.inner.highlight(lang, code),
        }
    }

    fn colors(&self) -> CodeColors {
        self.inner.colors()
    }

    fn language_name(&self, lang: LangId) -> String {
        self.inner.language_name(lang)
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
        assert_eq!(h.done.len(), 2, "go is unknown");
        assert_eq!(count(), 2);
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
}
