//! Visual mode: a selection of whole lines, vim-like.
//!
//! The selection runs from an anchor line to a cursor line (both kept as
//! [`Place`]s, so they survive a new layout); the motions move the cursor
//! and the view follows it. Copying takes either the Markdown source of
//! the top-level blocks the selection touches ([`yank`]: one slice of the
//! file from the first block to the last, or of the list items when the
//! selection is inside one list), or the text of exactly the selected lines
//! as they are shown, without decorations ([`yank_text`]).

use std::ops::Range;

use crate::ir::Block;
use crate::layout::{Layout, LineKind, SpanFlags};

use super::hints::{self, Copied, Kind, block_at, count, element_lines, line_count};
use super::keymap::Command;
use super::state::{Mode, Place, State, Visual};
use super::update::Effect;

/// Select `lines` (the cursor on the last one).
pub(crate) fn start(state: &mut State, lines: Range<usize>) {
    let last = lines.end.saturating_sub(1).max(lines.start);
    state.mode = Mode::Visual(Visual {
        anchor: Place::of(&state.layout, lines.start),
        cursor: Place::of(&state.layout, last),
    });
    follow_cursor(state, last);
}

/// `V`: select the first line of the first block that starts on screen
/// (else the first line on screen that shows something).
pub(crate) fn enter(state: &mut State) {
    let layout = &state.layout;
    let shown = state.top..(state.top + state.view_rows()).min(layout.len());
    let content = |i: &usize| {
        layout
            .lines
            .get(*i)
            .is_some_and(|l| l.kind != LineKind::Blank)
    };
    let starts = shown
        .clone()
        .find(|&i| content(&i) && layout.block_lines.iter().any(|r| r.start as usize == i));
    match starts.or_else(|| shown.clone().find(content)) {
        Some(line) => start(state, line..line + 1),
        None => state.say("nothing to select"),
    }
}

/// The cursor line.
fn cursor(state: &State) -> Option<usize> {
    match &state.mode {
        Mode::Visual(v) => Some(
            v.cursor
                .line(&state.layout)
                .min(state.layout.len().saturating_sub(1)),
        ),
        _ => None,
    }
}

/// Move the cursor to `line` and scroll as little as needed to show it.
fn set_cursor(state: &mut State, line: usize) {
    let line = line.min(state.layout.len().saturating_sub(1));
    let place = Place::of(&state.layout, line);
    if let Mode::Visual(v) = &mut state.mode {
        v.cursor = place;
    }
    follow_cursor(state, line);
}

fn follow_cursor(state: &mut State, line: usize) {
    let rows = state.view_rows().max(1);
    if line < state.top {
        state.top = line;
    } else if line >= state.top + rows {
        state.top = line + 1 - rows;
    }
    state.clamp_top();
}

/// The last line of block `b` that shows something, and its first line.
fn block_span(layout: &Layout, b: usize) -> Option<(usize, usize)> {
    let r = layout.block_lines.get(b)?;
    let (start, mut end) = (r.start as usize, r.end as usize);
    while end > start + 1
        && layout
            .lines
            .get(end - 1)
            .is_none_or(|l| l.kind == LineKind::Blank)
    {
        end -= 1;
    }
    (end > start).then_some((start, end - 1))
}

/// Run a motion (`cmd`, `n` times, with the `count` typed): returns
/// whether `cmd` is one.
pub(crate) fn motion(state: &mut State, cmd: Command, n: usize, count: Option<usize>) -> bool {
    let Some(c) = cursor(state) else {
        return false;
    };
    let last = state.layout.len().saturating_sub(1);
    let rows = state.view_rows().max(1);
    let half = (rows / 2).max(1).saturating_mul(n);
    let target = match cmd {
        Command::LineDown => c.saturating_add(n),
        Command::LineUp => c.saturating_sub(n),
        Command::HalfDown => {
            state.top = state.top.saturating_add(half);
            state.clamp_top();
            c.saturating_add(half)
        }
        Command::HalfUp => {
            state.top = state.top.saturating_sub(half);
            c.saturating_sub(half)
        }
        Command::Top => count.map_or(0, |c| c.saturating_sub(1)),
        Command::Bottom => count.map_or(last, |c| c.saturating_sub(1)),
        Command::NextBlock => {
            let mut at = c;
            for _ in 0..n {
                let b = block_at(&state.layout, at);
                at = match block_span(&state.layout, b) {
                    Some((_, end)) if end > at => end,
                    _ => (b + 1..state.layout.block_lines.len())
                        .find_map(|b| block_span(&state.layout, b))
                        .map_or(at, |(_, end)| end),
                };
            }
            at
        }
        Command::PrevBlock => {
            let mut at = c;
            for _ in 0..n {
                let b = block_at(&state.layout, at);
                at = match block_span(&state.layout, b) {
                    Some((start, _)) if start < at => start,
                    _ => (0..b)
                        .rev()
                        .find_map(|b| block_span(&state.layout, b))
                        .map_or(at, |(start, _)| start),
                };
            }
            at
        }
        Command::NextHeading | Command::PrevHeading => {
            let down = cmd == Command::NextHeading;
            let lines: Vec<usize> = state
                .derived
                .headings
                .iter()
                .map(|&(l, _)| l as usize)
                .collect();
            let mut at = c;
            for _ in 0..n {
                let next = if down {
                    lines.iter().copied().find(|&l| l > at)
                } else {
                    lines.iter().rev().copied().find(|&l| l < at)
                };
                match next {
                    Some(l) => at = l,
                    None => break,
                }
            }
            if at == c {
                state.say(if down {
                    "no heading below"
                } else {
                    "no heading above"
                });
            }
            at
        }
        Command::NextMatch | Command::PrevMatch => {
            match_motion(state, c, n, cmd == Command::PrevMatch)
        }
        Command::ScrollCenter | Command::ScrollTop | Command::ScrollBottom => {
            super::update::place_line(state, c, cmd);
            return true;
        }
        _ => return false,
    };
    set_cursor(state, target.min(last));
    true
}

/// `n`/`N` in Visual mode: the cursor to the `n`-th match below (above).
fn match_motion(state: &mut State, c: usize, n: usize, up: bool) -> usize {
    let Some(s) = state.search.as_ref() else {
        state.say("no search: / searches");
        return c;
    };
    let up = up != s.backward;
    let lines: Vec<usize> = s
        .matches
        .iter()
        .map(|m| state.layout.line_at(m.pos()))
        .collect();
    let mut at = c;
    let mut current = None;
    for _ in 0..n {
        let next = if up {
            lines.iter().enumerate().rev().find(|&(_, &l)| l < at)
        } else {
            lines.iter().enumerate().find(|&(_, &l)| l > at)
        };
        match next {
            Some((i, &l)) => {
                at = l;
                current = Some(i);
            }
            None => break,
        }
    }
    match current {
        Some(i) => {
            if let Some(s) = state.search.as_mut() {
                s.current = Some(i);
            }
        }
        None => state.say(if up {
            "no match above"
        } else {
            "no match below"
        }),
    }
    at
}

/// `o`: the cursor to the other end.
pub(crate) fn swap(state: &mut State) {
    let line = match &mut state.mode {
        Mode::Visual(v) => {
            std::mem::swap(&mut v.anchor, &mut v.cursor);
            v.cursor.line(&state.layout)
        }
        _ => return,
    };
    follow_cursor(state, line);
}

/// The top-level blocks lines `lo..=hi` touch.
pub(crate) fn blocks_of(layout: &Layout, lo: usize, hi: usize) -> Range<usize> {
    block_at(layout, lo)..block_at(layout, hi) + 1
}

/// The source the selection covers: `(text, blocks, what)`, or `None`
/// when no source is known for it.
fn source_of(state: &State, lo: usize, hi: usize) -> Option<(String, usize, &'static str)> {
    let doc = &state.page.doc;
    let text = &state.page.source.text;
    let blocks = blocks_of(&state.layout, lo, hi);
    // Inside one list: its items.
    if blocks.len() == 1
        && let Some(Block::List(_)) = doc.blocks.get(blocks.start)
    {
        let mut range: Option<Range<u32>> = None;
        let mut items = 0;
        hints::elements(doc, blocks.clone(), |el| {
            let Some(item) = el.item.filter(|i| i.src.start < i.src.end) else {
                return;
            };
            let lines = element_lines(&state.layout, &el);
            if lines.start > hi || lines.end <= lo {
                return;
            }
            if range.as_ref().is_none_or(|r| item.src.start >= r.end) {
                items += 1;
            }
            range = Some(match range.take() {
                Some(r) => r.start.min(item.src.start)..r.end.max(item.src.end),
                None => item.src.clone(),
            });
        });
        if let Some(r) = range
            && let Some(s) = text.get(r.start as usize..r.end as usize)
        {
            return Some((s.to_owned(), items, "list item"));
        }
    }
    let mut range: Option<Range<usize>> = None;
    let mut n = 0;
    for b in blocks {
        if let Some(r) = doc.block_source(crate::ir::BlockId(u32::try_from(b).unwrap_or(u32::MAX)))
        {
            n += 1;
            range = Some(match range {
                Some(all) => all.start.min(r.start)..all.end.max(r.end),
                None => r,
            });
        }
    }
    let r = range?;
    Some((text.get(r)?.to_owned(), n, "block"))
}

/// `y`: copy the Markdown source of the blocks the selection touches and
/// leave Visual mode.
pub(crate) fn yank(state: &mut State) -> Vec<Effect> {
    let Some((lo, hi)) = state.selection() else {
        return Vec::new();
    };
    state.mode = Mode::Normal;
    let copied = match source_of(state, lo, hi) {
        Some((text, n, what)) => Copied {
            what: format!(
                "copied {} of Markdown ({})",
                count(n, what),
                count(line_count(&text), "line")
            ),
            text,
        },
        None => {
            // No source (the footnotes): their text.
            let doc = &state.page.doc;
            let blocks = blocks_of(&state.layout, lo, hi);
            let text = doc.blocks_text(doc.blocks.get(blocks).unwrap_or(&[]));
            Copied::lines(text, "text")
        }
    };
    copied.effect()
}

/// The characters of table borders and rules (`|`, `-`, `+`, `=` and `:`
/// too in ASCII tables).
fn is_border(c: char, ascii: bool) -> bool {
    ('\u{2500}'..='\u{257f}').contains(&c) || ascii && matches!(c, '-' | '=' | '+' | '|' | ':')
}

/// The text of line `i` without its decorations: quote bars, table
/// borders (cells are separated by tabs); `None` for a line that is all
/// decoration (a border, a rule).
#[inline(never)]
fn line_text(state: &State, i: usize) -> Option<String> {
    let layout = &state.layout;
    let quote = state.settings.glyphs.0.as_str();
    let table = layout.lines.get(i)?.kind == LineKind::Table;
    let ascii = state.settings.ascii && table;
    let text = layout.line_text(i);
    // Quote bars at the start, after any indentation, each with the space
    // after it.
    let indent = text.len() - text.trim_start_matches(' ').len();
    let mut rest = text.get(indent..).unwrap_or("");
    while let Some(after) = rest.strip_prefix(quote).filter(|_| !quote.is_empty()) {
        rest = after.strip_prefix(' ').unwrap_or(after);
    }
    let rest = rest.trim_end();
    if rest.chars().all(|c| c == ' ' || is_border(c, ascii)) && !rest.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(indent + rest.len());
    out.extend(std::iter::repeat_n(' ', indent));
    if table {
        // `│ a │ b │` is `a`, tab, `b`.
        let bar = |c: char| matches!(c, '│' | '┃' | '║') || ascii && c == '|';
        let cells = rest.trim_start_matches(bar).trim_end_matches(bar);
        for (n, cell) in cells.split(bar).enumerate() {
            if n > 0 {
                out.push('\t');
            }
            out.push_str(cell.trim());
        }
    } else {
        out.push_str(rest);
    }
    Some(out)
}

/// The text lines `lo..=hi` show: code as its code (no gutter, padding or
/// wrap markers, wrapped lines joined), decorations left out, the common
/// indentation removed, trailing spaces trimmed.
#[inline(never)]
pub(crate) fn selected_text(state: &State, lo: usize, hi: usize) -> String {
    let layout = &state.layout;
    let mut out: Vec<String> = Vec::new();
    let mut code_prev = false;
    for i in lo..=hi.min(layout.len().saturating_sub(1)) {
        let Some(line) = layout.lines.get(i) else {
            break;
        };
        match line.kind {
            LineKind::Code { byte0, .. } => {
                let code: String = layout
                    .line_spans(i)
                    .iter()
                    .filter(|s| s.flags.contains(SpanFlags::CODE))
                    .map(|s| layout.span_text(s))
                    .collect();
                match out.last_mut() {
                    Some(last) if byte0 > 0 && code_prev => last.push_str(&code),
                    _ => out.push(code),
                }
                code_prev = true;
                continue;
            }
            LineKind::Image { .. } => continue,
            _ => code_prev = false,
        }
        match line_text(state, i) {
            // Padding of panels and frames: nothing to copy.
            Some(text) if text.trim().is_empty() && line.kind != LineKind::Blank => {}
            Some(text) => out.push(text),
            None => {}
        }
    }
    for l in &mut out {
        let kept = l.trim_end().len();
        l.truncate(kept);
    }
    let indent = out
        .iter()
        .filter(|l| !l.is_empty())
        .map(|l| l.len() - l.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    let lines: Vec<&str> = out.iter().map(|l| l.get(indent..).unwrap_or("")).collect();
    lines.join("\n")
}

/// `Y`: copy the text of the selected lines and leave Visual mode.
pub(crate) fn yank_text(state: &mut State) -> Vec<Effect> {
    let Some((lo, hi)) = state.selection() else {
        return Vec::new();
    };
    state.mode = Mode::Normal;
    let text = selected_text(state, lo, hi);
    Copied {
        what: format!("copied the text of {}", count(hi - lo + 1, "line")),
        text,
    }
    .effect()
}

/// Line `line` of `text` (1-based) holding byte `byte`.
pub(crate) fn source_line(text: &str, byte: usize) -> usize {
    let head = text.get(..byte.min(text.len())).unwrap_or(text);
    head.bytes().filter(|&b| b == b'\n').count() + 1
}

/// The line of the file to open the editor at: where the block shown on
/// layout line `line` starts (the innermost list item there, in a list).
pub(crate) fn edit_line(state: &State, line: usize) -> usize {
    let doc = &state.page.doc;
    let text = &state.page.source.text;
    let layout = &state.layout;
    let b = block_at(layout, line);
    let mut item_start: Option<u32> = None;
    hints::elements(doc, b..b + 1, |el| {
        if let (Kind::Item, Some(item)) = (el.kind, el.item)
            && item.src.start < item.src.end
            && element_lines(layout, &el).contains(&line)
            && item_start.is_none_or(|s| item.src.start > s)
        {
            item_start = Some(item.src.start);
        }
    });
    let start = item_start.map(|s| s as usize).or_else(|| {
        (0..=b)
            .rev()
            .find_map(|b| doc.block_source(crate::ir::BlockId(u32::try_from(b).ok()?)))
            .map(|r| r.start)
    });
    source_line(text, start.unwrap_or(0))
}

/// The status bar's description of the selection: `3 lines, 2 blocks`.
pub(crate) fn describe(state: &State) -> Option<String> {
    let (lo, hi) = state.selection()?;
    let blocks = blocks_of(&state.layout, lo, hi).len();
    Some(format!(
        "{}, {}",
        count(hi - lo + 1, "line"),
        count(blocks, "block")
    ))
}
