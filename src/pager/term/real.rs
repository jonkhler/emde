//! The real terminal: crossterm for raw mode, size and input; output is
//! written to standard output as plain bytes.
//!
//! crossterm is built with `use-dev-tty`, so its event reader uses
//! `/dev/tty` whenever standard input is not a terminal: the Markdown can
//! come through a pipe and keys still work.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode as CtKeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};

use crate::term::probe::{InputUnit, LateReplyFilter};

use super::{
    Button, ENTER, Key, KeyCode, MOUSE_ON, Mods, Mouse, MouseKind, TermEvent, Terminal, clamp_poll,
    mark_active, restore_into,
};

/// The terminal the pager runs on; see the module docs.
pub struct CrosstermTerminal {
    out: io::Stdout,
    /// Swallows probe replies that arrive after the probe gave up.
    late: LateReplyFilter,
}

impl CrosstermTerminal {
    /// A terminal writing to standard output. `late` is the probe's
    /// [`LateReplyFilter`] (inactive when the probe completed or did not
    /// run).
    pub fn new(late: LateReplyFilter) -> CrosstermTerminal {
        CrosstermTerminal {
            out: io::stdout(),
            late,
        }
    }
}

impl Terminal for CrosstermTerminal {
    fn size(&mut self) -> io::Result<(u16, u16)> {
        crossterm::terminal::size()
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    fn poll(&mut self, timeout: Duration) -> io::Result<Option<TermEvent>> {
        if !crossterm::event::poll(clamp_poll(timeout))? {
            return Ok(None);
        }
        let event = crossterm::event::read()?;
        if let Event::Key(key) = &event
            && self.late.swallow(Instant::now(), InputUnit::from_key(key))
        {
            return Ok(None);
        }
        Ok(convert(&event))
    }

    fn enter(&mut self, mouse: bool) -> io::Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        mark_active();
        let mut bytes = ENTER.to_vec();
        if mouse {
            bytes.extend_from_slice(MOUSE_ON);
        }
        self.out.write_all(&bytes)?;
        self.out.flush()
    }

    fn leave(&mut self) -> io::Result<()> {
        if restore_into(&mut self.out) {
            crossterm::terminal::disable_raw_mode()?;
        }
        Ok(())
    }

    fn suspend(&mut self) -> io::Result<()> {
        // No handler is registered for SIGTSTP, so this stops the process
        // (unless its process group is orphaned, when it is discarded) and
        // returns once it is continued.
        signal_hook::low_level::raise(signal_hook::consts::SIGTSTP)
    }
}

impl Drop for CrosstermTerminal {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

/// A crossterm event in emde's terms; `None` for events the pager ignores
/// (key releases, unknown keys).
fn convert(event: &Event) -> Option<TermEvent> {
    match event {
        Event::Key(key) => convert_key(key).map(TermEvent::Key),
        Event::Mouse(mouse) => convert_mouse(mouse).map(TermEvent::Mouse),
        Event::Resize(cols, rows) => Some(TermEvent::Resize {
            cols: *cols,
            rows: *rows,
        }),
        Event::FocusGained => Some(TermEvent::Focus(true)),
        Event::FocusLost => Some(TermEvent::Focus(false)),
        Event::Paste(text) => Some(TermEvent::Paste(text.clone())),
    }
}

/// A key press; releases (only reported with keyboard enhancements) are
/// dropped. `^H`, which some terminals send for Backspace, is Backspace.
fn convert_key(key: &KeyEvent) -> Option<Key> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let mut mods = Mods::empty();
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        mods |= Mods::SHIFT;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        mods |= Mods::CTRL;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        mods |= Mods::ALT;
    }
    let code = match key.code {
        CtKeyCode::Char('h') if mods == Mods::CTRL => {
            return Some(Key::plain(KeyCode::Backspace));
        }
        CtKeyCode::Char(c) => {
            // The character is already shifted; control letters come
            // lowercase.
            mods.remove(Mods::SHIFT);
            let c = if mods.contains(Mods::CTRL) {
                c.to_ascii_lowercase()
            } else {
                c
            };
            KeyCode::Char(c)
        }
        CtKeyCode::Enter => KeyCode::Enter,
        CtKeyCode::Esc => KeyCode::Esc,
        CtKeyCode::Backspace => KeyCode::Backspace,
        CtKeyCode::Tab => KeyCode::Tab,
        CtKeyCode::BackTab => {
            mods.remove(Mods::SHIFT);
            KeyCode::BackTab
        }
        CtKeyCode::Up => KeyCode::Up,
        CtKeyCode::Down => KeyCode::Down,
        CtKeyCode::Left => KeyCode::Left,
        CtKeyCode::Right => KeyCode::Right,
        CtKeyCode::Home => KeyCode::Home,
        CtKeyCode::End => KeyCode::End,
        CtKeyCode::PageUp => KeyCode::PageUp,
        CtKeyCode::PageDown => KeyCode::PageDown,
        CtKeyCode::Insert => KeyCode::Insert,
        CtKeyCode::Delete => KeyCode::Delete,
        CtKeyCode::F(n) => KeyCode::F(n),
        _ => return None,
    };
    Some(Key { code, mods })
}

fn convert_button(b: MouseButton) -> Button {
    match b {
        MouseButton::Left => Button::Left,
        MouseButton::Middle => Button::Middle,
        MouseButton::Right => Button::Right,
    }
}

fn convert_mouse(m: &MouseEvent) -> Option<Mouse> {
    let kind = match m.kind {
        MouseEventKind::Down(b) => MouseKind::Press(convert_button(b)),
        MouseEventKind::Up(b) => MouseKind::Release(convert_button(b)),
        MouseEventKind::Drag(b) => MouseKind::Drag(convert_button(b)),
        MouseEventKind::Moved => MouseKind::Moved,
        MouseEventKind::ScrollUp => MouseKind::WheelUp,
        MouseEventKind::ScrollDown => MouseKind::WheelDown,
        MouseEventKind::ScrollLeft => MouseKind::WheelLeft,
        MouseEventKind::ScrollRight => MouseKind::WheelRight,
    };
    Some(Mouse {
        kind,
        col: m.column,
        row: m.row,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: CtKeyCode, modifiers: KeyModifiers) -> Option<Key> {
        convert_key(&KeyEvent::new(code, modifiers))
    }

    #[test]
    fn keys_are_normalised() {
        assert_eq!(
            key(CtKeyCode::Char('G'), KeyModifiers::SHIFT),
            Some(Key::char('G'))
        );
        assert_eq!(
            key(CtKeyCode::Char('e'), KeyModifiers::CONTROL),
            Some(Key::ctrl('e'))
        );
        assert_eq!(
            key(CtKeyCode::Char('h'), KeyModifiers::CONTROL),
            Some(Key::plain(KeyCode::Backspace))
        );
        assert_eq!(
            key(CtKeyCode::BackTab, KeyModifiers::SHIFT),
            Some(Key::plain(KeyCode::BackTab))
        );
        assert_eq!(
            key(CtKeyCode::F(1), KeyModifiers::NONE),
            Some(Key::plain(KeyCode::F(1)))
        );
        assert_eq!(key(CtKeyCode::CapsLock, KeyModifiers::NONE), None);
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            ..KeyEvent::new(CtKeyCode::Char('q'), KeyModifiers::NONE)
        };
        assert_eq!(convert_key(&release), None);
    }

    #[test]
    fn events_are_converted() {
        let wheel = Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 3,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            convert(&wheel),
            Some(TermEvent::Mouse(Mouse {
                kind: MouseKind::WheelDown,
                col: 3,
                row: 4
            }))
        );
        assert_eq!(
            convert(&Event::Resize(100, 30)),
            Some(TermEvent::Resize {
                cols: 100,
                rows: 30
            })
        );
        assert_eq!(convert(&Event::FocusLost), Some(TermEvent::Focus(false)));
        let click = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            convert(&click),
            Some(TermEvent::Mouse(Mouse {
                kind: MouseKind::Press(Button::Left),
                ..
            }))
        ));
    }
}
