//! Shutdown helpers shared by the background tasks.
//!
//! Every task thread owns resources that must be released *in a defined order*
//! (the CPU governor has to disarm, restoring the kernel's original governors),
//! so the stop path joins its thread. An unbounded join is a liability: a task
//! that hangs — one did, on a mutex taken twice in one expression — makes
//! `uperf_rs_stop()` block forever, and a worker killed by the supervisor then
//! never restores the governor. Observed on device: `kill -TERM` left four
//! workers alive.
//!
//! So every join is bounded, and a task that owns something that must be undone
//! gets a way to undo it from the stopping thread too.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a task gets to notice the stop flag and finish its cleanup. The
/// longest poll interval in the tasks is 250 ms, so this is ~8x the worst case.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// Signal `flag` and join `handle`, giving up after `timeout`.
///
/// Returns `true` when the thread finished on its own. On `false` the thread is
/// **deliberately detached**: the caller is on the shutdown path, and letting a
/// stuck task block process exit (and therefore the supervisor) is worse than
/// leaking a thread that dies with the process.
pub fn stop_and_join(
    flag: &Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
    timeout: Duration,
    on_timeout: impl FnOnce(),
) -> bool {
    flag.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            on_timeout();
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = handle.join();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_a_thread_that_finishes() {
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let h = std::thread::spawn(move || {
            while !f.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let mut timed_out = false;
        let ok = stop_and_join(&flag, h, Duration::from_secs(2), || timed_out = true);
        assert!(ok);
        assert!(!timed_out);
    }

    #[test]
    fn gives_up_on_a_stuck_thread_and_reports_it() {
        // A thread that ignores the flag entirely, as a deadlocked one would.
        let flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let h = std::thread::spawn(move || {
            let _ = rx.recv(); // blocks until we drop tx
        });
        let mut notified = false;
        let t0 = Instant::now();
        let ok = stop_and_join(&flag, h, Duration::from_millis(150), || notified = true);
        assert!(!ok, "a stuck thread must report a timeout");
        assert!(notified, "and must run the on_timeout hook");
        assert!(t0.elapsed() < Duration::from_secs(2), "must not hang");
        drop(tx); // let the detached thread finish so it is not leaked in-process
    }
}
