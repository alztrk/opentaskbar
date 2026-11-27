//! Shared COM apartment guard for the platform modules that call shell/WinRT
//! objects from worker threads.
//!
//! `CoInitializeEx` returns `RPC_E_CHANGED_MODE` when the thread is already in
//! an apartment of a different kind. That is not a failure: the thread can
//! still use COM, but it must **not** call `CoUninitialize` because it does not
//! own that initialization. The guard records exactly that, so every caller
//! balances its own initialization without disturbing a caller's apartment.

use std::ptr::null;

use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize,
};

const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;

/// Owns a successful COM initialization until dropped. A thread already in an
/// apartment yields a guard with `owns() == false`; dropping it is a no-op.
#[must_use = "dropping the guard immediately would end the apartment"]
#[derive(Debug)]
pub struct ComApartment {
    owns: bool,
}

impl ComApartment {
    /// Initializes a single-threaded apartment (needed by `IShellLinkW`,
    /// `ICustomDestinationList` and most shell objects).
    pub fn sta() -> Option<Self> {
        Self::new(COINIT_APARTMENTTHREADED as u32)
    }

    /// Initializes a multi-threaded apartment.
    pub fn mta() -> Option<Self> {
        Self::new(COINIT_MULTITHREADED as u32)
    }

    fn new(mode: u32) -> Option<Self> {
        // SAFETY: no out-pointers; COM apartment state is per-thread and the
        // matching `CoUninitialize` happens in `Drop` only when we own it.
        let hr = unsafe { CoInitializeEx(null(), mode) };
        if hr < 0 && hr != RPC_E_CHANGED_MODE {
            return None;
        }
        Some(Self { owns: hr >= 0 })
    }

    /// Whether this guard performed (and will balance) the initialization.
    pub fn owns(&self) -> bool {
        self.owns
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owns {
            // SAFETY: balances the successful `CoInitializeEx` in `new`.
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_initialization_does_not_unbalance_a_foreign_apartment() {
        // Run on a dedicated thread so the apartment state is deterministic.
        std::thread::spawn(|| {
            let first = ComApartment::sta().expect("fresh thread can initialize COM");
            assert!(first.owns());
            // A second init of the *same* mode returns S_FALSE and owns a
            // reference that must be balanced too.
            let second = ComApartment::sta().expect("same-mode init succeeds");
            assert!(second.owns());
            drop(second);
            drop(first);

            // A different mode returns RPC_E_CHANGED_MODE: usable, but the
            // guard must not uninitialize a foreign apartment.
            let outer = ComApartment::mta().expect("fresh thread can initialize COM");
            assert!(outer.owns());
            let inner = ComApartment::sta().expect("a changed mode is not a failure");
            assert!(
                !inner.owns(),
                "a mismatched-mode init must be a no-op guard"
            );
            drop(inner);
            drop(outer);
        })
        .join()
        .expect("guard drops must not fault");
    }
}
