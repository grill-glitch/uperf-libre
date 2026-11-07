//! uperf-core — the Rust half of the rewrite (AGENT.md §3, §5).
//!
//! Layering, outermost first:
//!
//! * **FFI entry points** (this file): the four `extern "C"` functions the vendored
//!   C++ platform calls. They translate C pointers into Rust values and hand off;
//!   they hold no policy.
//! * [`settings`] — the run's knobs, parsed from the environment **once**, here at
//!   the edge.
//! * [`daemon`] — which subsystems exist, and in what order they start and stop.
//! * [`task`] — the lifecycle contract the daemon's registry is built on.
//! * the task modules themselves (`cpu_task`, `sched_task`, `watch_task`, …) and
//!   [`orchestrator`] (hint FSM → scene → sysfs plan).
//!
//! Lifetime rules (AGENT.md §5.1):
//!   * `data` passed to `on_event` is valid ONLY for the duration of the call.
//!   * `topic` is NUL-terminated UTF-8, also call-scoped — must be copied if kept.
//!   * `data == NULL && len == 0` ⇒ the topic carries no payload (`cgroup.*.update`).

#![deny(unsafe_op_in_unsafe_fn)]
// Every `unsafe` in this crate is load-bearing: a libc/`extern "C"` call, a raw
// pointer into a buffer the platform layer owns, or the binder mmap. None of them
// is decorative, so a block that stops needing `unsafe` must fail the build
// rather than linger (audit: `docs/audit/unsafe-audit-report.md`).
#![deny(unused_unsafe)]

pub mod cpu_task;
pub mod ctl_socket;
pub mod daemon;
/// dfps business logic — its own crate (`uperf-dfps`), re-exported under the name the
/// rest of this crate already uses. The subtree at `rust/uperf-dfps` and this crate are
/// the same boundary.
pub use uperf_dfps as dfps_rs;
pub mod ffi;
pub mod foreground;
pub mod frame_state;
pub mod hint;
pub mod inotify;
pub mod orchestrator;
pub mod procfs;
pub mod real_sink;
pub mod recorder;
pub mod sched_apply;
pub mod sched_task;
pub mod settings;
pub mod sf_binder;
pub mod signals;
pub mod sf_uprobe;
pub mod shutdown;
pub mod startup_lines;
pub mod status;
pub mod sysfs;
pub mod sysfs_ledger;
pub mod task;
pub mod topic_dispatch;
pub mod watch_task;

use std::ffi::CStr;
use std::sync::{Mutex, OnceLock};

use parking_lot::Mutex as PMutex;

use daemon::Daemon;
use ffi::{Bridge, BRIDGE};

/// Single-line log buffer shared by the dispatcher + ad-hoc log helpers.
static LOG_BUF: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();

fn log_buf() -> &'static Mutex<Vec<u8>> {
    LOG_BUF.get_or_init(|| Mutex::new(Vec::with_capacity(256)))
}

/// SAFETY: see AGENT.md §5.1. `topic` must be non-null and NUL-terminated; `data` is
/// borrowed only for the duration of the call (`data == null` is allowed iff `len == 0`).
#[no_mangle]
pub(crate) unsafe extern "C" fn uperf_rs_on_event(
    topic: *const libc::c_char,
    data: *const libc::c_void,
    len: usize,
) {
    // A NULL topic would be immediate UB in CStr::from_ptr; this is a plain function
    // pointer reachable from C++, so guard it instead of trusting the caller.
    if topic.is_null() {
        return;
    }
    let topic = match unsafe { CStr::from_ptr(topic) }.to_str() {
        Ok(s) => s,
        Err(_) => return,
    };
    if data.is_null() && len != 0 {
        // Contract violation: a length without a buffer. Dropping it beats inventing an
        // empty payload that looks like a real event body.
        return;
    }
    let payload: &[u8] = if data.is_null() {
        &[]
    } else {
        // SAFETY: contract — `data` points to `len` readable bytes for this call.
        unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) }
    };

    if let Some(ev) = topic_dispatch::Event::parse(topic, payload) {
        ffi::send_event(ev);
    }
}

/// Initialize the bridge handle. C++ must call this exactly once at boot, before
/// any other entry point. Subsequent calls are a no-op.
///
/// # Safety
///
/// `bridge` must be non-null and point to a `Bridge` that stays valid for the whole
/// process (the C++ side installs a `static` one). The `assert!` below is a
/// diagnostic for a broken caller, not a substitute for this contract.
#[no_mangle]
pub(crate) unsafe extern "C" fn uperf_rs_init(bridge: *const Bridge) {
    // `assert!` inside `extern "C"` aborts the whole process (a panic cannot unwind across
    // the C ABI): reject a broken caller instead of taking the daemon down with it.
    if bridge.is_null() {
        log_msg("uperf_rs_init: null bridge pointer, ignoring");
        return;
    }
    // SAFETY: contract — `bridge` points to a live, process-lifetime `Bridge`.
    let b = unsafe { (*bridge).clone() };
    if BRIDGE.set(b).is_err() {
        // Distinguish a real double-init from the documented no-op.
        log_msg("uperf_rs_init: bridge already initialized, keeping the first one");
    }
}

/// Reset the `SIGCHLD` disposition so `std::process::Command` works from the
/// worker.
///
/// The vendored dfps supervisor installs `signal(SIGCHLD, DaemonSigHandler)` and
/// that handler calls `wait(2)` to notice a worker dying. The worker is forked
/// *after* the handler is installed, so it inherits it — and the inherited
/// handler then reaps children that the worker's own `Command` calls are waiting
/// for. `Command::output()` fails with `ECHILD` ("No child processes"), which is
/// exactly what the device dry run reported when resolving the home package:
///
/// ```text
/// Rust: cannot resolve the home package (cmd: spawn failed: No child processes (os error 10))
/// ```
///
/// The worker supervises nothing (the daemon does that), so restoring the default
/// disposition is safe and is what makes the resolution work.
fn reset_sigchld_for_command() {
    // SAFETY: signal() with a plain disposition, no handlers installed here.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
    }
}

/// # Safety
///
/// Both paths must be NUL-terminated UTF-8 that stays valid for the call (C++
/// passes `std::string::c_str()` values from `app_main`); they are only read here.
#[no_mangle]
pub(crate) unsafe extern "C" fn uperf_rs_start(
    config_path: *const libc::c_char,
    log_path: *const libc::c_char,
) -> libc::c_int {
    reset_sigchld_for_command();
    // SAFETY: contract — both pointers are NUL-terminated for the duration of the call.
    let cfg = unsafe { CStr::from_ptr(config_path) };
    let log = unsafe { CStr::from_ptr(log_path) };

    // `to_string_lossy` would replace invalid UTF-8 with U+FFFD and hand a *different*
    // (non-existent) path to every consumer, surfacing far away as "config not found".
    let (Ok(cfg_str), Ok(log_str)) = (cfg.to_str(), log.to_str()) else {
        return -1;
    };

    // The one place the environment is read (`settings.rs`). Parsing it here — at the
    // outermost edge, before any thread exists — is what lets the rest of the crate
    // take its knobs as parameters, and what removes the `set_var` calls this
    // function used to make to publish derived paths: writing `environ` while the
    // previous generation's threads are still alive is exactly the data race Rust
    // 2024 marks `set_var` unsafe for.
    let settings = settings::Settings::from_env(cfg_str, log_str);
    *settings_slot().lock() = Some(settings.clone());

    // A re-start must not leave the previous generation running beside the new one.
    // This used to be eight `slot.lock().take().stop()` blocks in a hand-maintained
    // order; `Daemon::stop` owns that now.
    stop_existing_daemon();

    // Machine-readable status (`uperf.state`, M9). The module's watchdog establishes
    // liveness from /proc itself — both our processes are renamed to `uperf`, so the
    // process table cannot tell them apart — and only *reads* this file; it exists so
    // `state` / `takeover` / `armed` are observable without parsing the log, and so a
    // stale `state=running` with no live process is the documented signature of a kill
    // (which no in-process path survives).
    let cfg_path = settings.config_path.clone();
    let log_path_shown = settings.log_path.clone();
    let loaded = settings.config_dir.is_some();
    let status_target = settings.status.clone();
    let take_over = settings.cpu_governor;
    *daemon_slot().lock() = Some(Daemon::start(settings));

    // Publish `state=running` only now that a daemon is really in the slot. Writing it
    // first meant a startup panic (or a signal before the listener exists) left a status
    // file claiming a live, armed daemon while none existed — the signature the module's
    // watchdog interprets as a kill.
    if let Some(target) = status_target.as_ref() {
        status::write(
            &target.path,
            &status::Snapshot {
                state: "running",
                takeover: take_over,
                armed: &[],
                config: &target.config,
            },
        );
    }

    // Only now, with a daemon in the slot: a signal that arrived during startup stayed
    // pending (the worker blocks the teardown set before creating any thread), so it is
    // delivered to the listener below and handled with the daemon fully built. Starting
    // the listener first would let a signal interrupt `Daemon::start` between arming the
    // governor and publishing the daemon, and `_exit` there would leave the policies in
    // `userspace`.
    signals::spawn_termination_listener();

    log_msg(&format!(
        "uperf_rs_start: cfg={cfg_path} log={log_path_shown} (daemon started, config_dir_known={loaded})"
    ));
    0
}

#[no_mangle]
pub(crate) extern "C" fn uperf_rs_reload() {
    log_msg("uperf_rs_reload: re-subscribing");
    if let Some(bridge) = BRIDGE.get() {
        for topic in ffi::TOPICS.iter() {
            bridge.subscribe(topic);
        }
    }
}

#[no_mangle]
pub(crate) extern "C" fn uperf_rs_stop() {
    stop_existing_daemon();

    // Report the clean stop. Every governor is restored by now (the cpu task's own
    // disarm, or the stop-path fallback in `CpuTask::stop`), so from here on a
    // status file still saying `state=running` means the daemon was killed rather
    // than stopped — which is exactly what the module's watchdog keys off.
    let settings = settings_slot().lock().clone();
    if let Some(target) = settings.as_ref().and_then(|s| s.status.as_ref()) {
        // Ownership-checked: after a fast restart this write may belong to the
        // previous worker and must not overwrite the new one's `running`.
        status::write_if_owner(
            &target.path,
            &status::Snapshot {
                state: "stopped",
                takeover: settings.as_ref().is_some_and(|s| s.cpu_governor),
                armed: &[],
                config: &target.config,
            },
        );
    }
    log_msg("uperf_rs_stop: dispatcher joined");
}

/// Take the daemon out of its slot and stop it.
///
/// Idempotent, and safe from any thread: the C++ supervisor can deliver a second
/// signal while the first stop is still winding down, and the second call must not
/// deadlock behind the first (the slot lock is only held for the take).
fn stop_existing_daemon() {
    if let Some(mut daemon) = daemon_slot().lock().take() {
        daemon.stop();
    }
}

/// The run's knob snapshot, holding the last `uperf_rs_start`. A slot rather than a
/// constant because `uperf_rs_stop` has no arguments and must still know where the
/// status file is; `parking_lot::Mutex` keeps the read cheap on the stop path.
static SETTINGS: OnceLock<PMutex<Option<settings::Settings>>> = OnceLock::new();

fn settings_slot() -> &'static PMutex<Option<settings::Settings>> {
    SETTINGS.get_or_init(|| PMutex::new(None))
}

/// The live daemon: every task, the orchestrator, the dfps scheduler and the
/// dispatcher. One slot, so "what is running" has exactly one owner — the eight
/// per-subsystem slots this replaces could (and did) disagree after a re-start.
static DAEMON: OnceLock<PMutex<Option<Daemon>>> = OnceLock::new();

fn daemon_slot() -> &'static PMutex<Option<Daemon>> {
    DAEMON.get_or_init(|| PMutex::new(None))
}

pub(crate) fn log_msg(s: &str) {
    // `try_lock`, not `lock`: this is on the shutdown path, and a signal handler
    // can re-enter that path (SIGTERM plus the supervisor's SIGUSR1). A plain
    // `lock()` deadlocks against itself there and the process never exits,
    // leaving the CPU governor armed. A dropped log line beats a hung daemon.
    let Ok(mut buf) = log_buf().try_lock() else { return };
    buf.clear();
    buf.extend_from_slice(s.as_bytes());
    buf.push(b'\n');
    // SAFETY: spdlog file sink copies before returning.
    unsafe { ffi::uperf_bridge_write_log(std::ptr::null(), buf.as_ptr().cast(), buf.len()) };
}

#[cfg(test)]
mod shutdown_reentrancy_tests {
    use super::*;

    /// Regression for the deadlock that made `killall uperf` leave the device
    /// pinned in `userspace`.
    ///
    /// `killall` delivers SIGTERM to the daemon and the worker at once, and the
    /// daemon's own handler then forwards SIGUSR1 to the worker — so the shutdown
    /// path runs twice, concurrently, on the same thread. Both runs log, and the
    /// shared line buffer is a plain `Mutex`, which is not reentrant: the second
    /// `lock()` blocked on the first and the process never exited.
    ///
    /// This test reproduces the shape exactly — hold the buffer, then log — so a
    /// future change back to `lock()` hangs here instead of on a phone.
    #[test]
    fn logging_never_blocks_when_the_buffer_is_already_held() {
        let guard = log_buf().lock().unwrap();
        log_msg("reentrant log line: must be dropped, not blocked on");
        drop(guard);
        // Still usable afterwards.
        log_msg("after the guard is released");
    }

    /// The same guarantee for the per-module log helpers, which each own their own
    /// static buffer.
    #[test]
    fn concurrent_logging_completes() {
        let threads: Vec<_> = (0..8)
            .map(|i| {
                std::thread::spawn(move || {
                    for n in 0..200 {
                        log_msg(&format!("thread {i} line {n}"));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("a logging thread must not panic or hang");
        }
    }
}
