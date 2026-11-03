//! Terminal-signal handling for the tether (item #827).
//!
//! The tether runs its child in a separate process group so the whole tree can be
//! killed, which also means a Ctrl+C / SIGHUP aimed at the tether's group never reaches
//! the child: the tether would exit and orphan it. Handlers here only record the signal
//! in an atomic; `run_tether`'s poll loop sees it and tears the child tree down.
#![allow(unsafe_code)] // libc / Win32 FFI only; every unsafe block below states its invariant.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Last signal seen (Unix signal number; Windows maps every console event to SIGINT = 2).
static PENDING: AtomicI32 = AtomicI32::new(0);
/// Set once the child tree is gone, so a blocking Windows close-event handler can return.
static TEARDOWN_DONE: AtomicBool = AtomicBool::new(false);

/// The pending signal, cleared by the read. `run_tether` polls this every 50 ms.
pub(crate) fn take_pending() -> Option<i32> {
    match PENDING.swap(0, Ordering::Relaxed) {
        0 => None,
        sig => Some(sig),
    }
}

/// Tells a blocked Windows close-event handler that teardown finished.
pub(crate) fn teardown_done() {
    TEARDOWN_DONE.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
const HANDLED: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

#[cfg(unix)]
extern "C" fn on_signal(sig: libc::c_int) {
    PENDING.store(sig, Ordering::Relaxed); // async-signal-safe: a single atomic store
}

/// Installs the handlers for the tether's lifetime and restores the previous ones on drop.
pub(crate) struct Guard {
    #[cfg(unix)]
    prev: [libc::sighandler_t; 3],
}

impl Guard {
    #[cfg(unix)]
    pub(crate) fn install() -> Self {
        PENDING.store(0, Ordering::Relaxed);
        let mut prev = [libc::SIG_ERR; 3];
        for (slot, sig) in prev.iter_mut().zip(HANDLED) {
            // SAFETY: `on_signal` is an `extern "C"` fn that only does an atomic store.
            let old = unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
            if old == libc::SIG_IGN {
                // Ignored on entry (nohup, background job): keep it ignored.
                // SAFETY: re-installing the disposition `signal` just returned.
                unsafe { libc::signal(sig, old) };
            } else {
                *slot = old;
            }
        }
        Self { prev }
    }

    #[cfg(windows)]
    pub(crate) fn install() -> Self {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        PENDING.store(0, Ordering::Relaxed);
        TEARDOWN_DONE.store(false, Ordering::Relaxed);
        // SAFETY: `on_ctrl` matches PHANDLER_ROUTINE and lives for the whole process.
        unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
        Self {}
    }
}

impl Drop for Guard {
    #[cfg(unix)]
    fn drop(&mut self) {
        for (prev, sig) in self.prev.iter().zip(HANDLED) {
            if *prev != libc::SIG_ERR {
                // SAFETY: restoring the handler `signal` returned for this signal.
                unsafe { libc::signal(sig, *prev) };
            }
        }
    }

    #[cfg(windows)]
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        // Any exit path (including errors) has torn the child down by now.
        teardown_done();
        // SAFETY: removes the handler added in `install`.
        unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 0) };
    }
}

/// Windows console event: CTRL_C (0), CTRL_BREAK (1), CLOSE (2), LOGOFF (5), SHUTDOWN (6).
#[cfg(windows)]
pub(crate) unsafe extern "system" fn on_ctrl(ctrl_type: u32) -> i32 {
    PENDING.store(2, Ordering::Relaxed);
    // For close/logoff/shutdown Windows kills the process the moment the handler returns
    // (or after ~5 s), so hold it until the child tree is gone.
    if ctrl_type >= 2 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while !TEARDOWN_DONE.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    1 // handled: do not run the default handler, which would exit before teardown
}

/// Gives the child's process group the terminal while the tether runs, so an interactive
/// child reading stdin is not stopped by SIGTTIN, and takes it back on drop.
#[cfg(unix)]
pub(crate) struct Foreground {
    tether_pgrp: libc::pid_t,
}

#[cfg(unix)]
impl Foreground {
    /// `None` when stdin is not a terminal or the tether is not its foreground group.
    pub(crate) fn hand_to(child_pgrp: u32) -> Option<Self> {
        // SAFETY: plain terminal-control calls on fd 0; no memory is shared.
        unsafe {
            if libc::isatty(0) != 1 {
                return None;
            }
            let tether_pgrp = libc::tcgetpgrp(0);
            if tether_pgrp < 0 || tether_pgrp != libc::getpgrp() {
                return None;
            }
            let child = child_pgrp as libc::pid_t;
            if !set_foreground(child) {
                return None;
            }
            // The child may have read stdin before the handoff and been stopped.
            libc::killpg(child, libc::SIGCONT);
            Some(Self { tether_pgrp })
        }
    }
}

#[cfg(unix)]
impl Drop for Foreground {
    fn drop(&mut self) {
        // SAFETY: see `hand_to`.
        unsafe { set_foreground(self.tether_pgrp) };
    }
}

/// `tcsetpgrp` from a background group raises SIGTTOU, so ignore it for the call.
#[cfg(unix)]
unsafe fn set_foreground(pgrp: libc::pid_t) -> bool {
    unsafe {
        let old = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        let ok = libc::tcsetpgrp(0, pgrp) == 0;
        if old != libc::SIG_ERR {
            libc::signal(libc::SIGTTOU, old);
        }
        ok
    }
}
