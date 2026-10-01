//! A scripted terminal for tests.
//!
//! [`FakeTerminal`] plays a script of [`Step`]s (events, waits, resizes,
//! signals, arbitrary actions) against the pager and records everything it
//! does as [`Chunk`]s: each `write` call, each resize, each suspend. Tests
//! feed the recorded bytes to a screen model (the `vt100` crate) and look
//! at the screen.
//!
//! Time is virtual: a poll that finds no event advances the clock by its
//! timeout (or by what is left of a scripted wait), so debouncing and file
//! watching run instantly and deterministically. A poll timeout outside
//! [`MIN_POLL`]`..=`[`MAX_POLL`] is an error, and so is polling long after
//! the script ran out (a test that forgot to quit fails instead of
//! hanging).

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use super::{
    ENTER, EXIT, Key, KeyCode, MAX_POLL, MIN_POLL, MOUSE_ON, Mouse, Signals, TermEvent, Terminal,
};

/// How many empty polls after the end of the script are tolerated.
const IDLE_LIMIT: u32 = 5_000;

/// One step of a [`FakeTerminal`] script.
pub enum Step {
    /// Deliver an event.
    Event(TermEvent),
    /// Let this much (virtual) time pass without input.
    Wait(Duration),
    /// Change the size and deliver the resize event.
    Resize(u16, u16),
    /// Raise a signal on the attached [`Signals`].
    Signal(i32),
    /// Run an action, e.g. change a watched file.
    Run(Box<dyn FnOnce()>),
    /// Record [`Chunk::Mark`] (so a test can look at the screen as it was
    /// at this point).
    Mark(String),
}

impl fmt::Debug for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::Event(e) => f.debug_tuple("Event").field(e).finish(),
            Step::Wait(d) => f.debug_tuple("Wait").field(d).finish(),
            Step::Resize(c, r) => f.debug_tuple("Resize").field(c).field(r).finish(),
            Step::Signal(s) => f.debug_tuple("Signal").field(s).finish(),
            Step::Run(_) => f.write_str("Run(..)"),
            Step::Mark(m) => f.debug_tuple("Mark").field(m).finish(),
        }
    }
}

/// What the pager did to a [`FakeTerminal`], in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chunk {
    /// One `write` call.
    Write(Vec<u8>),
    /// The terminal was resized to `(columns, rows)`.
    Resize(u16, u16),
    /// The pager stopped the process (Ctrl-Z) and was continued.
    Suspend,
    /// A [`Step::Mark`] was played.
    Mark(String),
}

/// A scripted terminal; see the module docs.
#[derive(Debug)]
pub struct FakeTerminal {
    cols: u16,
    rows: u16,
    script: VecDeque<Step>,
    start: Instant,
    elapsed: Duration,
    chunks: Vec<Chunk>,
    raw: bool,
    signals: Option<Signals>,
    idle: u32,
    polls: Vec<Duration>,
}

impl FakeTerminal {
    /// A `cols` × `rows` terminal with an empty script.
    pub fn new(cols: u16, rows: u16) -> FakeTerminal {
        FakeTerminal {
            cols,
            rows,
            script: VecDeque::new(),
            start: Instant::now(),
            elapsed: Duration::ZERO,
            chunks: Vec::new(),
            raw: false,
            signals: None,
            idle: 0,
            polls: Vec::new(),
        }
    }

    /// Scripted [`Step::Signal`]s raise on these flags.
    pub fn with_signals(mut self, signals: Signals) -> FakeTerminal {
        self.signals = Some(signals);
        self
    }

    /// Append a step to the script.
    pub fn step(mut self, step: Step) -> FakeTerminal {
        self.script.push_back(step);
        self
    }

    /// Append an event.
    pub fn event(self, event: TermEvent) -> FakeTerminal {
        self.step(Step::Event(event))
    }

    /// Append a key press.
    pub fn key(self, key: Key) -> FakeTerminal {
        self.event(TermEvent::Key(key))
    }

    /// Append a key press without modifiers.
    pub fn code(self, code: KeyCode) -> FakeTerminal {
        self.key(Key::plain(code))
    }

    /// Append one key press per character: `\n` is Enter, `\x1b` Esc,
    /// `\t` Tab and `\x7f` Backspace.
    pub fn keys(mut self, text: &str) -> FakeTerminal {
        for c in text.chars() {
            let key = match c {
                '\n' => Key::plain(KeyCode::Enter),
                '\x1b' => Key::plain(KeyCode::Esc),
                '\t' => Key::plain(KeyCode::Tab),
                '\x7f' => Key::plain(KeyCode::Backspace),
                c => Key::char(c),
            };
            self = self.key(key);
        }
        self
    }

    /// Append a mouse event.
    pub fn mouse(self, mouse: Mouse) -> FakeTerminal {
        self.event(TermEvent::Mouse(mouse))
    }

    /// Append a quiet period.
    pub fn wait(self, d: Duration) -> FakeTerminal {
        self.step(Step::Wait(d))
    }

    /// Append a resize.
    pub fn resize(self, cols: u16, rows: u16) -> FakeTerminal {
        self.step(Step::Resize(cols, rows))
    }

    /// Append an action.
    pub fn run(self, f: impl FnOnce() + 'static) -> FakeTerminal {
        self.step(Step::Run(Box::new(f)))
    }

    /// Append a mark after a quiet period longer than the resize
    /// debounce, so that it is recorded after the frame for the steps
    /// before it.
    pub fn mark(self, name: &str) -> FakeTerminal {
        self.wait(Duration::from_millis(50))
            .step(Step::Mark(name.to_owned()))
    }

    /// Everything that happened, in order.
    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    /// Every byte written, concatenated.
    pub fn output(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for c in &self.chunks {
            if let Chunk::Write(b) = c {
                out.extend_from_slice(b);
            }
        }
        out
    }

    /// The `write` calls, in order.
    pub fn writes(&self) -> Vec<&[u8]> {
        self.chunks
            .iter()
            .filter_map(|c| match c {
                Chunk::Write(b) => Some(b.as_slice()),
                _ => None,
            })
            .collect()
    }

    /// Forget what was recorded so far (the script stays).
    pub fn clear_output(&mut self) {
        self.chunks.clear();
    }

    /// Whether the terminal is set up (between `enter` and `leave`).
    pub fn is_raw(&self) -> bool {
        self.raw
    }

    /// The timeouts the pager polled with.
    pub fn poll_timeouts(&self) -> &[Duration] {
        &self.polls
    }

    /// Virtual time since the terminal was made.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Steps not played yet.
    pub fn remaining(&self) -> usize {
        self.script.len()
    }

    fn advance(&mut self, d: Duration) {
        self.elapsed = self.elapsed.saturating_add(d);
    }
}

impl Terminal for FakeTerminal {
    fn size(&mut self) -> io::Result<(u16, u16)> {
        Ok((self.cols, self.rows))
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.chunks.push(Chunk::Write(bytes.to_vec()));
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn poll(&mut self, timeout: Duration) -> io::Result<Option<TermEvent>> {
        self.polls.push(timeout);
        if !(MIN_POLL..=MAX_POLL).contains(&timeout) {
            return Err(io::Error::other(format!(
                "poll timeout {timeout:?} outside {MIN_POLL:?}..={MAX_POLL:?}"
            )));
        }
        let Some(step) = self.script.pop_front() else {
            self.idle += 1;
            if self.idle > IDLE_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the fake terminal's script ran out (did the test forget to quit?)",
                ));
            }
            self.advance(timeout);
            return Ok(None);
        };
        self.idle = 0;
        match step {
            Step::Event(e) => Ok(Some(e)),
            Step::Resize(cols, rows) => {
                self.cols = cols;
                self.rows = rows;
                self.chunks.push(Chunk::Resize(cols, rows));
                Ok(Some(TermEvent::Resize { cols, rows }))
            }
            Step::Wait(d) if d > timeout => {
                self.advance(timeout);
                self.script.push_front(Step::Wait(d - timeout));
                Ok(None)
            }
            Step::Wait(d) => {
                self.advance(d);
                Ok(None)
            }
            Step::Signal(sig) => {
                if let Some(s) = &self.signals {
                    s.raise(sig);
                }
                Ok(None)
            }
            Step::Run(f) => {
                f();
                Ok(None)
            }
            Step::Mark(name) => {
                self.chunks.push(Chunk::Mark(name));
                Ok(None)
            }
        }
    }

    fn enter(&mut self, mouse: bool) -> io::Result<()> {
        self.raw = true;
        let mut bytes = ENTER.to_vec();
        if mouse {
            bytes.extend_from_slice(MOUSE_ON);
        }
        self.write(&bytes)
    }

    fn leave(&mut self) -> io::Result<()> {
        if !self.raw {
            return Ok(());
        }
        self.raw = false;
        self.write(EXIT)
    }

    fn suspend(&mut self) -> io::Result<()> {
        self.chunks.push(Chunk::Suspend);
        Ok(())
    }

    fn now(&self) -> Instant {
        self.start + self.elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_play_in_order_with_virtual_time() {
        let mut t = FakeTerminal::new(80, 24)
            .keys("j\n")
            .wait(Duration::from_millis(300))
            .resize(100, 30);
        let t0 = t.now();
        assert_eq!(
            t.poll(MAX_POLL).unwrap(),
            Some(TermEvent::Key(Key::char('j')))
        );
        assert_eq!(
            t.poll(MAX_POLL).unwrap(),
            Some(TermEvent::Key(Key::plain(KeyCode::Enter)))
        );
        // A 300 ms wait takes two polls of at most 250 ms.
        assert_eq!(t.poll(MAX_POLL).unwrap(), None);
        assert_eq!(t.poll(MAX_POLL).unwrap(), None);
        assert_eq!(t.now() - t0, Duration::from_millis(300));
        assert_eq!(
            t.poll(MIN_POLL).unwrap(),
            Some(TermEvent::Resize {
                cols: 100,
                rows: 30
            })
        );
        assert_eq!(t.size().unwrap(), (100, 30));
        assert_eq!(t.remaining(), 0);
        assert!(t.poll(Duration::ZERO).is_err(), "zero timeouts are refused");
    }

    #[test]
    fn enter_and_leave_are_recorded() {
        let mut t = FakeTerminal::new(10, 5);
        t.enter(true).unwrap();
        assert!(t.is_raw());
        t.leave().unwrap();
        t.leave().unwrap();
        assert!(!t.is_raw());
        let mut want = ENTER.to_vec();
        want.extend_from_slice(MOUSE_ON);
        assert_eq!(t.writes(), [want.as_slice(), EXIT]);
    }

    #[test]
    fn an_exhausted_script_ends_with_an_error() {
        let mut t = FakeTerminal::new(10, 5);
        let err = (0..=IDLE_LIMIT + 1)
            .map(|_| t.poll(MAX_POLL))
            .find_map(Result::err);
        assert!(err.is_some());
    }

    #[test]
    fn signals_and_actions() {
        use std::cell::Cell;
        use std::rc::Rc;
        let signals = Signals::new();
        let ran = Rc::new(Cell::new(false));
        let flag = Rc::clone(&ran);
        let mut t = FakeTerminal::new(10, 5)
            .with_signals(signals.clone())
            .step(Step::Signal(15))
            .run(move || flag.set(true));
        assert_eq!(t.poll(MIN_POLL).unwrap(), None);
        assert_eq!(signals.exit_requested(), Some(15));
        assert_eq!(t.poll(MIN_POLL).unwrap(), None);
        assert!(ran.get());
    }
}
