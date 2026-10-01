//! The terminal behind the pager: a small [`Terminal`] trait, its events,
//! the setup and teardown sequences, signal flags and the emergency
//! restore used by the panic hook.
//!
//! Two implementations exist:
//!
//! * [`CrosstermTerminal`]: raw mode and input through crossterm (reading
//!   `/dev/tty`, so standard input can be the Markdown pipe), output as
//!   plain byte writes to standard output;
//! * [`FakeTerminal`]: scripted events and a virtual clock for tests; what
//!   the pager writes is recorded so tests can feed it to a screen model.
//!
//! # Setup and teardown
//!
//! Entering writes [`ENTER`] (alternate screen, hidden cursor, autowrap
//! off) and, with the mouse on, [`MOUSE_ON`]: button events (1000) in SGR
//! encoding (1006) only. Any-motion tracking (1003, which crossterm's
//! `EnableMouseCapture` turns on) would flood an SSH connection.
//!
//! Leaving writes the cleanup hook's bytes (kitty image deletion, once
//! images use it) and [`EXIT`], then returns to cooked mode. It runs from
//! the pager's drop guard, after SIGTERM/SIGHUP/SIGINT (checked by the
//! event loop) and from the panic hook ([`install_panic_hook`]; for a
//! panic on the pager's own thread), and it is idempotent: a process-wide
//! flag records whether the real terminal is set up, so the bytes are
//! written at most once however the pager ends.

mod fake;
mod real;

use std::cell::Cell;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, TryLockError};
use std::time::{Duration, Instant};

use bitflags::bitflags;

pub use fake::{Chunk, FakeTerminal, Step};
pub use real::CrosstermTerminal;

/// Entering the pager: alternate screen, cursor hidden, autowrap off.
pub const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?7l";
/// Mouse reporting on: button presses and releases (1000), SGR encoding
/// (1006). Never any-motion tracking (1003).
pub const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1006h";
/// Mouse reporting off.
pub const MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1000l";
/// Leaving the pager: synchronized output off, mouse off, scroll region
/// reset, autowrap on, attributes reset, cursor shown, main screen.
pub const EXIT: &[u8] =
    b"\x1b[?2026l\x1b[?1006l\x1b[?1000l\x1b[r\x1b[?7h\x1b[0m\x1b[?25h\x1b[?1049l";

/// Shortest poll timeout. Never zero: crossterm 0.29 does not return events
/// it has already buffered when polled with `Duration::ZERO` (bug #839).
pub const MIN_POLL: Duration = Duration::from_millis(1);
/// Longest poll timeout. crossterm retries `poll(2)` after `EINTR`, so a
/// signal only ends a wait when it times out: this cap is how quickly the
/// event loop notices SIGTERM and friends.
pub const MAX_POLL: Duration = Duration::from_millis(250);

/// A poll timeout within [`MIN_POLL`]`..=`[`MAX_POLL`].
pub fn clamp_poll(timeout: Duration) -> Duration {
    timeout.clamp(MIN_POLL, MAX_POLL)
}

/// What the pager needs from a terminal.
pub trait Terminal {
    /// The size in cells: `(columns, rows)`.
    fn size(&mut self) -> io::Result<(u16, u16)>;

    /// Write bytes. The pager writes each frame with one call.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Flush written bytes.
    fn flush(&mut self) -> io::Result<()>;

    /// Wait up to `timeout` (always within [`MIN_POLL`]`..=`[`MAX_POLL`])
    /// for the next event; `None` when none arrived.
    fn poll(&mut self, timeout: Duration) -> io::Result<Option<TermEvent>>;

    /// Set the terminal up: raw mode, then [`ENTER`] and, with `mouse`,
    /// [`MOUSE_ON`].
    fn enter(&mut self, mouse: bool) -> io::Result<()>;

    /// Put the terminal back: cleanup bytes, [`EXIT`], cooked mode. Does
    /// nothing when the terminal is not set up.
    fn leave(&mut self) -> io::Result<()>;

    /// Stop the process until it is continued (Ctrl-Z); called after
    /// [`Terminal::leave`], and followed by [`Terminal::enter`].
    fn suspend(&mut self) -> io::Result<()>;

    /// The current time (a virtual clock in tests).
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// An input event, in emde's own terms (no crossterm types leak out).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    /// A key press.
    Key(Key),
    /// A mouse event (SGR 1006 reports).
    Mouse(Mouse),
    /// The terminal was resized.
    Resize {
        /// New width in columns.
        cols: u16,
        /// New height in rows.
        rows: u16,
    },
    /// The terminal gained (`true`) or lost focus.
    Focus(bool),
    /// Pasted text (bracketed paste).
    Paste(String),
}

bitflags! {
    /// Key modifiers. Character keys never carry `SHIFT`: the character
    /// itself is already shifted (`G`, `?`, `}`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub struct Mods: u8 {
        const SHIFT = 1 << 0;
        const CTRL = 1 << 1;
        const ALT = 1 << 2;
    }
}

/// A key other than a modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// A character; control characters arrive as the letter plus
    /// [`Mods::CTRL`] (`^E` is `Char('e')`).
    Char(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    /// Shift+Tab.
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// A function key (`F(1)` is F1).
    F(u8),
}

/// A key with its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub mods: Mods,
}

impl Key {
    /// A key without modifiers.
    pub const fn plain(code: KeyCode) -> Key {
        Key {
            code,
            mods: Mods::empty(),
        }
    }

    /// A character key without modifiers.
    pub const fn char(c: char) -> Key {
        Key::plain(KeyCode::Char(c))
    }

    /// Control plus a (lowercase) letter.
    pub const fn ctrl(c: char) -> Key {
        Key {
            code: KeyCode::Char(c),
            mods: Mods::CTRL,
        }
    }

    /// The character this key types into a prompt, if any: printable
    /// characters without Ctrl or Alt.
    pub fn text(self) -> Option<char> {
        match self.code {
            KeyCode::Char(c)
                if !self.mods.intersects(Mods::CTRL | Mods::ALT) && !c.is_control() =>
            {
                Some(c)
            }
            _ => None,
        }
    }
}

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// What happened with the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseKind {
    Press(Button),
    Release(Button),
    Drag(Button),
    Moved,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

/// A mouse event at a cell (0-based column and row).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Mouse {
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
}

// ---------------------------------------------------------------------------
// Emergency restore
// ---------------------------------------------------------------------------

/// Whether the real terminal is set up (raw mode, alternate screen).
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Bytes written before [`EXIT`] when leaving (kitty image deletion).
static CLEANUP: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Set the bytes written before [`EXIT`] whenever the terminal is put back
/// (the image layer registers its kitty deletions here).
pub fn set_cleanup(bytes: Vec<u8>) {
    let mut slot = CLEANUP.lock().unwrap_or_else(|e| e.into_inner());
    *slot = bytes;
}

/// How many times the real terminal was set up. The thread that did it
/// last keeps the count in its [`OWNED`]: that is how the panic hook tells
/// the pager's thread from the others.
static OWNER: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// [`OWNER`] as it was when this thread last set the terminal up (0:
    /// never).
    static OWNED: Cell<usize> = const { Cell::new(0) };
}

/// Record that the real terminal is set up, by the current thread.
pub(crate) fn mark_active() {
    let count = OWNER.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    let _ = OWNED.try_with(|owned| owned.set(count));
    ACTIVE.store(true, Ordering::SeqCst);
}

/// Whether the current thread is the one that set the terminal up last (or
/// that cannot be told). Safe in a panic hook: no lock, no allocation.
fn on_owner_thread() -> bool {
    let owner = OWNER.load(Ordering::SeqCst);
    OWNED.try_with(|owned| owned.get() == owner).unwrap_or(true)
}

/// Write the cleanup bytes and [`EXIT`] to `out` if the terminal is set
/// up, and record that it no longer is. Returns whether anything was
/// written: a second call (from the panic hook, then the drop guard) does
/// nothing.
pub(crate) fn restore_into(out: &mut dyn Write) -> bool {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return false;
    }
    // `try_lock`: the panic hook may run on a thread that holds the lock.
    let cleanup = match CLEANUP.try_lock() {
        Ok(bytes) => bytes.clone(),
        Err(TryLockError::Poisoned(e)) => e.into_inner().clone(),
        Err(TryLockError::WouldBlock) => Vec::new(),
    };
    let mut bytes = cleanup;
    bytes.extend_from_slice(EXIT);
    let _ = out.write_all(&bytes);
    let _ = out.flush();
    true
}

/// The panic hook's restore: the exit sequence straight to `/dev/tty`
/// (standard output may be a locked or broken stream by now), then cooked
/// mode. Only for a panic on the pager's own thread: a panic on another
/// thread leaves the pager running, so the terminal must stay as it needs
/// it.
fn restore_after_panic() {
    if !on_owner_thread() {
        return;
    }
    let tty = std::fs::OpenOptions::new().write(true).open("/dev/tty");
    let restored = match tty {
        Ok(mut tty) => restore_into(&mut tty),
        Err(_) => restore_into(&mut io::stdout()),
    };
    if restored {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Install the panic hook that puts the terminal back before the panic
/// message is printed (once per process). Panics inside
/// [`crate::panic::guarded`] sections never restore anything: the hook
/// only records them.
pub fn install_panic_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| crate::panic::install_hook(restore_after_panic));
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// The flags behind [`Signals`].
#[derive(Clone, Debug, Default)]
struct Flags {
    /// Number of the signal that asked the pager to quit (0: none).
    exit: Arc<AtomicUsize>,
    /// SIGCONT arrived: the process was stopped and continued.
    cont: Arc<AtomicBool>,
    /// No pager is running: the default actions apply.
    released: Arc<AtomicBool>,
}

/// Gives the signals their default actions back when the last [`Signals`]
/// clone goes.
#[derive(Debug, Default)]
struct Release(Flags);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.released.store(true, Ordering::SeqCst);
    }
}

/// The flags the process's signal handlers set. The handlers are installed
/// once and then kept for every later pager: signal-hook cannot take a
/// handler back without leaving its signal ignored, and a second set of
/// handlers would leave the first set's "default action once released" in
/// place, ending the process (with the terminal still set up) on a signal
/// meant for the later pager.
static HANDLERS: Mutex<Option<Flags>> = Mutex::new(None);

/// Install the handlers that set `flags`.
fn install_handlers(flags: &Flags) -> io::Result<()> {
    use signal_hook::consts::{SIGCONT, SIGHUP, SIGINT, SIGTERM};
    use signal_hook::flag;
    for sig in [SIGTERM, SIGHUP, SIGINT] {
        // While no pager runs, the signal gets its default action.
        flag::register_conditional_default(sig, Arc::clone(&flags.released))?;
        let value = usize::try_from(sig).unwrap_or(1);
        flag::register_usize(sig, Arc::clone(&flags.exit), value)?;
    }
    flag::register(SIGCONT, Arc::clone(&flags.cont))?;
    Ok(())
}

/// Signals the event loop checks at least every [`MAX_POLL`].
///
/// [`Signals::register`] turns SIGTERM, SIGHUP and SIGINT into a request
/// to quit (the pager then restores the terminal on its normal way out)
/// and notes SIGCONT, after which the terminal is set up again and fully
/// redrawn. Once the last clone is dropped, the three signals get their
/// default action back. [`Signals::new`] registers nothing; tests use
/// [`Signals::raise`] to simulate a signal.
#[derive(Clone, Debug, Default)]
pub struct Signals {
    flags: Arc<Release>,
}

impl Signals {
    /// Flags without any signal handler (tests).
    pub fn new() -> Signals {
        Signals::default()
    }

    /// Flags set by the process's signal handlers (installed by the first
    /// call and kept for later pagers), cleared for a new pager. For one
    /// pager at a time: dropping the last clone gives the signals their
    /// default actions back.
    pub fn register() -> io::Result<Signals> {
        let mut slot = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
        let flags = match slot.as_ref() {
            Some(flags) => flags.clone(),
            None => {
                let flags = Flags::default();
                install_handlers(&flags)?;
                slot.insert(flags).clone()
            }
        };
        flags.exit.store(0, Ordering::SeqCst);
        flags.cont.store(false, Ordering::SeqCst);
        flags.released.store(false, Ordering::SeqCst);
        Ok(Signals {
            flags: Arc::new(Release(flags)),
        })
    }

    /// Simulate the arrival of signal `sig` (SIGCONT, or a signal that
    /// asks to quit).
    pub fn raise(&self, sig: i32) {
        if sig == signal_hook::consts::SIGCONT {
            self.flags.0.cont.store(true, Ordering::SeqCst);
        } else {
            let value = usize::try_from(sig).unwrap_or(1);
            self.flags.0.exit.store(value, Ordering::SeqCst);
        }
    }

    /// The signal that asked the pager to quit, if any.
    pub(crate) fn exit_requested(&self) -> Option<i32> {
        match self.flags.0.exit.load(Ordering::SeqCst) {
            0 => None,
            n => Some(i32::try_from(n).unwrap_or(i32::MAX)),
        }
    }

    /// Whether SIGCONT arrived since the last call.
    pub(crate) fn take_continued(&self) -> bool {
        self.flags.0.cont.swap(false, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests that use the process-wide restore flag.
    static ACTIVE_LOCK: Mutex<()> = Mutex::new(());

    /// Serialises the tests that install real signal handlers and raise
    /// real signals (a SIGTERM while no pager holds the flags ends the
    /// test process).
    static SIGNALS_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn enter_and_exit_sequences() {
        let mut enter = ENTER.to_vec();
        enter.extend_from_slice(MOUSE_ON);
        assert_eq!(
            enter, b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[?1000h\x1b[?1006h",
            "only 1000 and 1006: never any-motion tracking"
        );
        assert!(!String::from_utf8_lossy(&enter).contains("1003"));
        assert_eq!(
            EXIT,
            b"\x1b[?2026l\x1b[?1006l\x1b[?1000l\x1b[r\x1b[?7h\x1b[0m\x1b[?25h\x1b[?1049l"
        );
    }

    #[test]
    fn poll_timeouts_are_clamped() {
        assert_eq!(clamp_poll(Duration::ZERO), MIN_POLL);
        assert_eq!(clamp_poll(Duration::from_secs(5)), MAX_POLL);
        assert_eq!(
            clamp_poll(Duration::from_millis(30)),
            Duration::from_millis(30)
        );
    }

    #[test]
    fn restore_writes_the_exit_sequence_once() {
        let _lock = ACTIVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // The panic hook and the drop guard both call this; only the first
        // call writes anything.
        set_cleanup(b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\".to_vec());
        mark_active();
        let mut first = Vec::new();
        assert!(restore_into(&mut first));
        let mut want = b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\".to_vec();
        want.extend_from_slice(EXIT);
        assert_eq!(first, want);
        let mut second = Vec::new();
        assert!(!restore_into(&mut second), "idempotent");
        assert!(second.is_empty());
        set_cleanup(Vec::new());
    }

    #[test]
    fn panics_in_guarded_sections_do_not_restore() {
        let _lock = ACTIVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        install_panic_hook();
        mark_active();
        let caught: Option<()> = crate::panic::guarded(|| panic!("inside a guarded section"));
        assert!(caught.is_none());
        let mut out = Vec::new();
        assert!(
            restore_into(&mut out),
            "a guarded panic left the terminal alone"
        );
        assert!(out.ends_with(EXIT));
    }

    #[test]
    fn panics_on_other_threads_leave_the_terminal_alone() {
        let _lock = ACTIVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        install_panic_hook();
        // This thread runs the pager; a worker thread panics.
        mark_active();
        let worker = std::thread::spawn(|| panic!("a worker thread panics"));
        assert!(worker.join().is_err());
        let mut out = Vec::new();
        assert!(
            restore_into(&mut out),
            "the pager's terminal is still set up"
        );
        assert!(out.ends_with(EXIT));
    }

    #[test]
    fn simulated_signals() {
        let s = Signals::new();
        assert_eq!(s.exit_requested(), None);
        assert!(!s.take_continued());
        s.raise(signal_hook::consts::SIGCONT);
        assert!(s.take_continued());
        assert!(!s.take_continued(), "taken");
        s.raise(signal_hook::consts::SIGTERM);
        assert_eq!(s.exit_requested(), Some(signal_hook::consts::SIGTERM));
        let clone = s.clone();
        assert_eq!(clone.exit_requested(), Some(signal_hook::consts::SIGTERM));
    }

    #[test]
    fn signal_handlers_serve_one_pager_after_another() {
        use signal_hook::consts::{SIGCONT, SIGTERM};
        let _lock = SIGNALS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // A first pager comes and goes …
        drop(Signals::register().unwrap());
        // … and the next one still gets SIGTERM as a request to quit. (With
        // a second set of handlers, the first set's default action would
        // end this process here.)
        let second = Signals::register().unwrap();
        assert_eq!(second.exit_requested(), None);
        signal_hook::low_level::raise(SIGTERM).unwrap();
        assert_eq!(second.exit_requested(), Some(SIGTERM));
        signal_hook::low_level::raise(SIGCONT).unwrap();
        assert!(second.take_continued());
        drop(second);
        // A new pager starts with clear flags.
        let third = Signals::register().unwrap();
        assert_eq!(third.exit_requested(), None);
        assert!(!third.take_continued());
    }

    #[test]
    fn key_text() {
        assert_eq!(Key::char('a').text(), Some('a'));
        assert_eq!(Key::ctrl('a').text(), None);
        assert_eq!(Key::plain(KeyCode::Enter).text(), None);
        let alt = Key {
            code: KeyCode::Char('x'),
            mods: Mods::ALT,
        };
        assert_eq!(alt.text(), None);
    }
}
