//! Teardown signals, handled in **thread context** instead of a signal handler.
//!
//! `uperf_rs_stop` does not belong in a handler: it joins threads, takes
//! `parking_lot`/`std` mutexes, allocates, writes a status file, and calls into
//! spdlog. None of that is async-signal-safe, and a signal can land on the very thread
//! that already holds one of those locks:
//!
//! * on the dispatcher, mid-`send`: the handler re-enters the log buffer, and the
//!   *C++* spdlog sink lock underneath it has no `try_lock` — the process stops there
//!   with the CPU governor still in `userspace`;
//! * on the cpu task, inside the writer critical section: `CpuTask::stop`'s timeout
//!   fallback calls `writer.lock()` on the same thread — a self-deadlock on a
//!   non-reentrant mutex.
//!
//! The fix is the textbook one, and it also explains the shape of
//! `shutdown_reentrancy_tests` in `lib.rs`: the handler must not *do* the work.
//!
//! 1. The worker blocks `SIGTERM` / `SIGINT` / `SIGUSR1` **before it creates any
//!    thread** (`app_main.cpp::BlockTerminationSignals`, in the fork's child branch).
//!    Every thread made afterwards inherits the mask — C++'s listeners included, which
//!    is why this cannot be done from Rust: `StartPlatform()` has already spawned
//!    threads by the time `uperf_rs_start` runs.
//! 2. One dedicated thread does a blocking `sigwait(3)` and, when it returns, runs the
//!    ordinary stop path and `_exit(0)`s.
//!
//! Because the signals stay blocked, a second delivery cannot interrupt the stop that
//! is already running (the reason `killall uperf` used to run the teardown twice on
//! one thread), and `uperf_rs_stop` is idempotent anyway.

use std::sync::OnceLock;

/// The three signals that mean "shut down", in the order the supervisor uses them.
///
/// `TERM_SIG` (SIGUSR1) is how the daemon tells its worker to stop; SIGTERM/SIGINT are
/// what a user or `killall` sends.
pub const TEARDOWN_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGUSR1];

/// Start the listener once per process. Returns `false` when the thread could not be
/// spawned — in that case the signals are put back to their default disposition, so a
/// wedged daemon can still be killed (leaving the governor armed, which is the old
/// behaviour, but at least it is not unkillable).
pub fn spawn_termination_listener() -> bool {
    static STARTED: OnceLock<bool> = OnceLock::new();
    *STARTED.get_or_init(|| spawn_blocking_listener())
}

fn spawn_blocking_listener() -> bool {
    // SAFETY: `sigset_t` is a plain C struct; `sigemptyset`/`sigaddset` initialise it
    // and the set outlives every use below (it is moved into the thread).
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut set);
        for sig in TEARDOWN_SIGNALS {
            libc::sigaddset(&mut set, sig);
        }
    }

    // Also block them here, in the caller's thread. Normally the C++ side already did
    // it (the worker blocks before creating any thread, and this runs inside the
    // worker) — doing it again is idempotent, and it makes the listener correct for a
    // caller that starts the daemon from a host test without the C++ wrapper.
    // SAFETY: valid set, no output pointer needed.
    let rc = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };

    let spawned = std::thread::Builder::new()
        .name("uperf-signal".into())
        .spawn(move || {
            loop {
                let mut sig: libc::c_int = 0;
                // SAFETY: `sigwait` only reads the set and writes one int.
                let rc = unsafe { libc::sigwait(&set, &mut sig) };
                if rc != 0 {
                    // EINTR/EINVAL: not a teardown signal. Keep waiting rather than
                    // exiting without disarming.
                    continue;
                }
                crate::log_msg(&format!(
                    "Rust: caught signal {sig} on the listener thread; stopping"
                ));
                crate::uperf_rs_stop();
                // The C++ main thread is parked in `Sleep(UINT32_MAX)` and will never
                // return; every thread that mattered has been joined or detached by
                // the stop above, and spdlog flushes at info level, so exiting here
                // loses nothing.
                // SAFETY: `_exit` — no destructors, no locks, cannot fail.
                unsafe { libc::_exit(0) };
            }
        })
        .is_ok();

    if !spawned {
        restore_default_handlers();
        crate::log_msg(
            "Rust: could not start the signal listener; teardown signals restored to their \
             default disposition (a SIGTERM will now kill without disarming)",
        );
    } else if rc != 0 {
        crate::log_msg(&format!(
            "Rust: pthread_sigmask failed (rc={rc}); relying on the caller's mask"
        ));
    }
    spawned
}

/// Give the teardown signals back to the kernel (used only when the listener could not
/// start, so the process stays killable).
fn restore_default_handlers() {
    for sig in TEARDOWN_SIGNALS {
        // SAFETY: plain disposition, installed from the thread that owns startup.
        unsafe { libc::signal(sig, libc::SIG_DFL) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mask the listener relies on must be expressible without touching the
    /// process: this asserts the set contents, which is what the C++ side blocks too
    /// (`app_main.cpp` uses the same three).
    #[test]
    fn teardown_set_is_standalone_signal_usr1_term_int() {
        assert_eq!(TEARDOWN_SIGNALS, [libc::SIGTERM, libc::SIGINT, libc::SIGUSR1]);
    }
}
