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
    // `sigset_t` is opaque and `sigemptyset` initialises it: build it in `MaybeUninit` so
    // the safety argument is local instead of relying on "all-zero is valid on this libc".
    let mut set = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: `sigemptyset` initialises the whole value before it is read.
    unsafe {
        libc::sigemptyset(set.as_mut_ptr());
    }
    // SAFETY: initialised above.
    let mut set = unsafe { set.assume_init() };
    unsafe {
        for sig in TEARDOWN_SIGNALS {
            libc::sigaddset(&mut set, sig);
        }
    }

    // A copy for the failure path: `set` is moved into the closure (and dropped with the
    // builder when the spawn fails), so the unblock below needs its own.
    // SAFETY: initialised above; both copies are plain values.
    let set_copy = unsafe { std::ptr::read(&set) };

    // Also block them here, in the caller's thread. Normally the C++ side already did
    // it (the worker blocks before creating any thread, and this runs inside the
    // worker) — doing it again is idempotent, and it makes the listener correct for a
    // caller that starts the daemon from a host test without the C++ wrapper.
    // SAFETY: valid set, no output pointer needed.
    let rc = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };

    let spawned = std::thread::Builder::new()
        .name("uperf-signal".into())
        .spawn(move || {
            let mut errors = 0u32;
            loop {
                let mut sig: libc::c_int = 0;
                // SAFETY: `sigwait` only reads the set and writes one int.
                let rc = unsafe { libc::sigwait(&set, &mut sig) };
                if rc != 0 {
                    // `sigwait` does not report EINTR, so a non-zero return is a real error
                    // (EINVAL); retrying it forever spun the listener at 100% CPU and teardown
                    // never ran. Give up loudly after a bounded number of attempts.
                    errors += 1;
                    crate::log_msg(&format!(
                        "Rust: sigwait failed (rc={rc}), attempt {errors}/3"
                    ));
                    if errors >= 3 {
                        // Nothing can stop the daemon any more: make the failure terminal.
                        unsafe { libc::_exit(1) };
                    }
                    continue;
                }
                crate::log_msg(&format!(
                    "Rust: caught signal {sig} on the listener thread; stopping"
                ));
                // A panic here used to kill the listener thread while the C++ main thread
                // stayed parked in `Sleep(UINT32_MAX)` with every teardown signal still
                // blocked — an unkillable process, exactly what this module exists to prevent.
                let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::uperf_rs_stop()
                }))
                .is_ok();
                if !stopped {
                    crate::log_msg("Rust: uperf_rs_stop panicked; exiting without a clean stop");
                    unsafe { libc::_exit(1) };
                }
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
        // The disposition alone is not enough: these signals are still *blocked* here (and
        // in every thread the C++ side created before us), and a blocked signal with SIG_DFL
        // stays pending instead of terminating the process. Unblock them so a SIGTERM is
        // actually delivered — otherwise the fallback produced an unkillable daemon.
        // SAFETY: valid set, no output pointer.
        unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &set_copy, std::ptr::null_mut()) };
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

    /// What the listener actually depends on: the same three signals the C++ side blocks,
    /// with no duplicates.
    #[test]
    fn teardown_set_is_term_int_usr1_without_duplicates() {
        let mut seen = std::collections::BTreeSet::new();
        for sig in TEARDOWN_SIGNALS {
            assert!(seen.insert(sig), "signal {sig} is listed twice");
        }
        assert_eq!(seen.len(), 3);
        assert!(seen.contains(&libc::SIGTERM));
        assert!(seen.contains(&libc::SIGINT));
        assert!(seen.contains(&libc::SIGUSR1));
    }
}
