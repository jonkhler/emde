//! Panic containment.
//!
//! Third-party hot spots (syntect regex compilation, the TeX parser, image
//! decoders) run inside [`guarded`]. A panic there must degrade the affected
//! content, not tear down the reader. Because a panic hook runs *before*
//! unwinding — even for panics that `catch_unwind` later catches — the hook
//! installed by [`install_hook`] checks a thread-local depth counter: inside a
//! guarded section it only records the message; otherwise it restores the
//! terminal and defers to the previous hook.

use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

static CAUGHT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Run `f`, turning a panic into `None`. The panic message is recorded and
/// can be read with [`take_caught`].
pub fn guarded<T>(f: impl FnOnce() -> T) -> Option<T> {
    DEPTH.with(|d| d.set(d.get() + 1));
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    DEPTH.with(|d| d.set(d.get() - 1));
    result.ok()
}

/// Whether the current thread is inside [`guarded`].
pub fn in_guarded() -> bool {
    DEPTH.with(|d| d.get() > 0)
}

/// Messages of panics caught by [`guarded`] since the last call.
pub fn take_caught() -> Vec<String> {
    CAUGHT
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default()
}

/// Install the process panic hook. `restore` must put the terminal back into
/// a sane state (leave the alternate screen, show the cursor, …); it runs for
/// unguarded panics only, before the default hook prints the message.
pub fn install_hook(restore: fn()) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if in_guarded() {
            let payload = info.payload();
            let msg = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("panic")
                .to_string();
            let loc = info
                .location()
                .map(|l| format!(" at {}:{}", l.file(), l.line()))
                .unwrap_or_default();
            if let Ok(mut v) = CAUGHT.lock() {
                v.push(format!("{msg}{loc}"));
            }
            return;
        }
        restore();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarded_catches_and_restores_depth() {
        assert!(!in_guarded());
        let r: Option<u32> = guarded(|| panic!("boom"));
        assert_eq!(r, None);
        assert!(!in_guarded());
        assert_eq!(guarded(|| 7), Some(7));
        let nested = guarded(|| guarded(in_guarded));
        assert_eq!(nested, Some(Some(true)));
    }
}
