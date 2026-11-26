//! Tracks the virtual keys injected by shell chords so held modifiers can
//! always be released, even when a chord fails halfway through or a session
//! lock swallows the key-up events.
//!
//! `taskbar::send_chord` records every successful `SendInput` keydown with
//! [`note_down`] and every key-up with [`note_up`]. On any partial failure it
//! drains the tracker with [`release_all`] and injects the remaining key-ups
//! (finally semantics), so a stuck Win/Ctrl/Alt can never leak into the next
//! user interaction. After an unlock, the app can wire `release_all()` into its
//! `session::install_shell_hooks`/`WM_SESSION` handler through the public
//! `taskbar::release_held_keys()` helper.

use std::sync::Mutex;

/// Virtual keys currently believed to be held down by one of our chords.
/// Insertion order is preserved so key-ups can be injected in reverse order.
static HELD_KEYS: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// Records a successful keydown. Duplicate downs are ignored so a single key
/// never produces more than one key-up.
pub fn note_down(vk: u16) {
    if let Ok(mut keys) = HELD_KEYS.lock()
        && !keys.contains(&vk)
    {
        keys.push(vk);
    }
}

/// Records a key-up (successful or best-effort) and forgets the key.
pub fn note_up(vk: u16) {
    if let Ok(mut keys) = HELD_KEYS.lock() {
        keys.retain(|&held| held != vk);
    }
}

/// Drains and returns every still-held key, in the original press order.
/// Callers inject a `KEYEVENTF_KEYUP` for each returned key. A second call
/// returns an empty vector, which makes this safe to use as a finally path.
pub fn release_all() -> Vec<u16> {
    HELD_KEYS
        .lock()
        .map(|mut keys| std::mem::take(&mut *keys))
        .unwrap_or_default()
}

/// Number of keys currently tracked. Exposed for diagnostics/tests.
pub fn held_count() -> usize {
    HELD_KEYS.lock().map_or(0, |keys| keys.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The tracker is process-global; serialize the tests so they cannot
    // observe each other's key state when cargo runs them in parallel.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    const VK_LWIN: u16 = 0x5B;
    const VK_CONTROL: u16 = 0x11;
    const VK_MENU: u16 = 0x12;

    #[test]
    fn notes_down_and_up_and_deduplicates() {
        let _guard = TEST_LOCK.lock().unwrap();
        release_all();

        note_down(VK_LWIN);
        note_down(VK_LWIN); // duplicate must not be recorded twice
        note_down(VK_CONTROL);
        assert_eq!(held_count(), 2);

        note_up(VK_LWIN);
        assert_eq!(held_count(), 1);
        note_up(VK_LWIN); // unknown/up-again is a no-op
        assert_eq!(held_count(), 1);
        note_up(VK_CONTROL);
        assert_eq!(held_count(), 0);
        release_all();
    }

    #[test]
    fn release_all_drains_in_press_order_and_is_idempotent() {
        let _guard = TEST_LOCK.lock().unwrap();
        release_all();

        note_down(VK_CONTROL);
        note_down(VK_LWIN);
        note_down(VK_MENU);
        note_up(VK_LWIN); // released normally; must not appear in the drain

        assert_eq!(release_all(), vec![VK_CONTROL, VK_MENU]);
        assert_eq!(release_all(), Vec::<u16>::new());
        assert_eq!(held_count(), 0);

        note_down(VK_LWIN);
        assert_eq!(release_all(), vec![VK_LWIN]);
    }
}
