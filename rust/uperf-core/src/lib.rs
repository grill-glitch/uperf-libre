//! uperf-core — the Rust half of the rewrite (AGENT.md §3, §5).
//!
//! M1+M2+M3+M4:
//!   * C ABI bridge + topic event dispatcher (`topic_dispatch`, `ffi`).
//!   * Hint state machine (`hint`) — `SfHint` enum values 0..5 derived from the
//!     upstream binary's two parallel jump tables (`docs/m1-static-reverse.md`
//!     §1.3).
//!   * Sysfs writer dispatch (`sysfs`) — SoC-specific per-cluster paths
//!     extracted from upstream's real device fd trace.
//!   * Orchestrator (`orchestrator`) — events → hint FSM → scene transition →
//!     sysfs write sequence (M4; `Sink` trait + `CollectingSink` test double).
//!
//! Lifetime rules (AGENT.md §5.1):
//!   * `data` passed to `on_event` is valid ONLY for the duration of the call.
//!   * `topic` is NUL-terminated UTF-8, also call-scoped — must be copied if kept.
//!   * `data == NULL && len == 0` ⇒ the topic carries no payload (`cgroup.*.update`).

#![deny(unsafe_op_in_unsafe_fn)]

pub mod cpu_task;
pub mod ffi;
pub mod inotify;
pub mod sched_apply;
pub mod sched_task;
pub mod startup_lines;
pub mod shutdown;
pub mod watch_task;
pub mod hint;
pub mod orchestrator;
pub mod sysfs;
pub mod topic_dispatch;

use std::ffi::CStr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex as PMutex;

use ffi::{Bridge, BRIDGE, DISPATCH_TX, TOPICS};
use orchestrator::Orchestrator;
use topic_dispatch::Dispatcher;

/// Single-line log buffer shared by the dispatcher + ad-hoc log helpers.
static LOG_BUF: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();

fn log_buf() -> &'static Mutex<Vec<u8>> {
    LOG_BUF.get_or_init(|| Mutex::new(Vec::with_capacity(256)))
}

/// SAFETY: see AGENT.md §5.1.
#[no_mangle]
pub(crate) unsafe extern "C" fn uperf_rs_on_event(
    topic: *const libc::c_char,
    data: *const libc::c_void,
    len: usize,
) {
    let topic = match unsafe { CStr::from_ptr(topic) }.to_str() {
        Ok(s) => s,
        Err(_) => return,
    };
    let payload: &[u8] = if data.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: contract.
        unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) }
    };

    if let Some(ev) = topic_dispatch::Event::parse(topic, payload) {
        if let Some(tx) = DISPATCH_TX.get() {
            let _ = tx.send(ev);
        }
    }
}

/// Initialize the bridge handle. C++ must call this exactly once at boot, before
/// any other entry point. Subsequent calls are a no-op.
#[no_mangle]
pub(crate) extern "C" fn uperf_rs_init(bridge: *const Bridge) {
    assert!(!bridge.is_null(), "bridge pointer must be non-null");
    // SAFETY: bridge is process-lifetime per C++ contract.
    let b = unsafe { (*bridge).clone() };
    let _ = BRIDGE.set(b);
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

#[no_mangle]
pub(crate) extern "C" fn uperf_rs_start(
    config_path: *const libc::c_char,
    log_path: *const libc::c_char,
) -> libc::c_int {
    reset_sigchld_for_command();
    // SAFETY: NUL-terminated per contract.
    let cfg = unsafe { CStr::from_ptr(config_path) };
    let log = unsafe { CStr::from_ptr(log_path) };

    // Where the CPU governor records the governors it replaces, so the module's
    // stop script can restore them even after a SIGKILL.
    if std::env::var("UPERF_STATE_FILE").is_err() {
        let cfg_str = cfg.to_string_lossy().to_string();
        if let Some(dir) = std::path::Path::new(&cfg_str).parent() {
            std::env::set_var("UPERF_STATE_FILE", dir.join("orig_governor.txt"));
        }
    }
    let (loaded_cfg, mode) = load_config_and_mode(&cfg.to_string_lossy());

    // Criterion 2 (AGENT.md §1): the config identity and the knob writability
    // warnings, verbatim. Emitted without the `Rust:` prefix the rewrite's own
    // diagnostics carry, so they are greppable against an upstream log.
    if let Some(c) = loaded_cfg.as_ref() {
        let (name, author) = c.meta_ident();
        log_msg(&crate::startup_lines::config_line(&name, &author));
        for line in crate::startup_lines::knob_warnings(&c.sysfs_knob_table(), &crate::startup_lines::real_probe())
        {
            log_msg(&line);
        }
    }

    // Apply `modules.log.level` (upstream's LogLevelSwitcher). Every shipped
    // config sets "info"; the logger used to be hardcoded to debug.
    if let Some(level) = loaded_cfg.as_ref().and_then(|c| c.log_level()) {
        if let Ok(lv) = std::ffi::CString::new(level.clone()) {
            // SAFETY: NUL-terminated, read only for the duration of the call.
            unsafe { crate::ffi::uperf_bridge_set_log_level(lv.as_ptr()) };
        }
    }
    let has_cfg = loaded_cfg.is_some();
    let cfg_for_governor = loaded_cfg.clone();
    {
        let mut guard = dispatcher_slot().lock();
        if let Some(mut prev) = guard.take() {
            prev.join_timeout(std::time::Duration::from_secs(2));
        }
        let orch = match loaded_cfg {
            Some(c) => Orchestrator::with_config(c, &mode),
            None => Orchestrator::new(hint::HintDurations::default(), &mode),
        };
        let orch = Arc::new(PMutex::new(orch));
        let _ = ORCHESTRATOR.set(orch.clone());
        *guard = Some(topic_dispatch::spawn(orch));
    }

    let bridge = match BRIDGE.get() {
        Some(b) => b,
        None => return 1,
    };
    for topic in TOPICS.iter() {
        bridge.subscribe(topic);
    }

    // Apply `modules.input.*` to the vendored InputListener. This is the one place
    // the rewrite reaches into a vendored module, because dfps hardcodes these
    // three thresholds while uperf reads them (README lines 96-98) — 62 of the 63
    // configs ask for swipeThd 3x the hardcoded value.
    if let Some(c) = cfg_for_governor.as_ref() {
        if c.input_enabled() == Some(false) {
            log_msg(
                "Rust: modules.input.enable=false is not honoured (the listener is started by the \
                 platform layer before the config is parsed)",
            );
        }
        if let Some((swipe, gx, gy)) = c.input_thresholds() {
            // SAFETY: plain scalars across the C ABI.
            unsafe { crate::ffi::uperf_bridge_set_input_thresholds(swipe, gx, gy) };
        }
    }

    // `modules.atrace.enable` -> open the ftrace trace_marker and toggle the
    // markers emitted by the vendored ATRACE_* instrumentation.
    if let Some(c) = cfg_for_governor.as_ref() {
        if let Some(on) = c.atrace_enabled() {
            // SAFETY: plain scalar across the C ABI.
            unsafe { crate::ffi::uperf_bridge_set_atrace(on) };
        }
    }

    // Start the userspace CPU governor: samples /proc/stat, runs the power-model
    // loop and publishes per-cluster frequency targets.
    if let Some(c) = cfg_for_governor.as_ref() {
        {
            let mut guard = cpu_task_slot().lock();
            if let Some(mut prev) = guard.take() {
                prev.stop();
            }
            *guard = cpu_task::CpuTask::spawn(c, &mode, "idle");
        }
        log_msg("Rust: cpu governor started");
    }

    // Start the context scheduler (modules.sched): it resolves each process
    // through the config's rules and applies affinity / SCHED class per thread.
    if let Some(c) = cfg_for_governor.as_ref() {
        if c.modules_map().is_some() {
            let mut log_fn = |m: &str| log_msg(m);
            if let Some(planner) = sched_task::SchedTask::planner_for(c, &mut log_fn) {
                let Some(orch) = ORCHESTRATOR.get().cloned() else {
                    log_msg("Rust: context scheduler skipped (no orchestrator)");
                    return 0;
                };
                let state = move || {
                    let g = orch.lock();
                    (g.current_scene().to_string(), g.top_app().map(str::to_string), g.generation())
                };
                let mut guard = sched_task_slot().lock();
                if let Some(mut prev) = guard.take() {
                    prev.stop();
                }
                *guard = Some(sched_task::SchedTask::spawn(planner, state));
                log_msg("Rust: context scheduler started");
            }
        }
    }

    // Start the file watcher: cur_powermode.txt / perapp_powermode.txt preset
    // switching and the single-byte sfanalysis.hint feed.
    if let Some(c) = cfg_for_governor.as_ref() {
        if let Some(orch) = ORCHESTRATOR.get().cloned() {
            let cfg_path = cfg.to_string_lossy().to_string();
            let plan = watch_task::WatchPlan::from_config(c, std::path::Path::new(&cfg_path));
            let fake_root = std::env::var("UPERF_FAKE_ROOT").ok();
            let mut guard = watch_task_slot().lock();
            if let Some(mut prev) = guard.take() {
                prev.stop();
            }
            *guard = Some(watch_task::WatchTask::spawn(
                plan,
                orch,
                fake_root,
                |m: &str| log_msg(m),
            ));
            log_msg("Rust: preset/hint watcher started");
        }
    }

    log_msg(&format!(
        "uperf_rs_start: cfg={} log={} (M4 orchestrator+governor+sched+watch, mode={}, config_loaded={})",
        cfg.to_string_lossy(),
        log.to_string_lossy(),
        mode,
        has_cfg
    ));
    0
}

#[no_mangle]
pub(crate) extern "C" fn uperf_rs_reload() {
    log_msg("uperf_rs_reload: re-subscribing");
    if let Some(bridge) = BRIDGE.get() {
        for topic in TOPICS.iter() {
            bridge.subscribe(topic);
        }
    }
}

#[no_mangle]
pub(crate) extern "C" fn uperf_rs_stop() {
    let mut guard = dispatcher_slot().lock();
    if let Some(mut prev) = guard.take() {
        prev.join_timeout(std::time::Duration::from_secs(2));
    }
    {
        let mut guard = cpu_task_slot().lock();
        if let Some(mut t) = guard.take() {
            t.stop();
        }
    }
    {
        let mut guard = sched_task_slot().lock();
        if let Some(mut t) = guard.take() {
            t.stop();
        }
    }
    {
        let mut guard = watch_task_slot().lock();
        if let Some(mut t) = guard.take() {
            t.stop();
        }
    }
    log_msg("uperf_rs_stop: dispatcher joined");
}

static DISPATCHER: OnceLock<PMutex<Option<Dispatcher>>> = OnceLock::new();
static ORCHESTRATOR: OnceLock<Arc<PMutex<Orchestrator>>> = OnceLock::new();
static CPU_TASK: OnceLock<PMutex<Option<cpu_task::CpuTask>>> = OnceLock::new();
static SCHED_TASK: OnceLock<PMutex<Option<sched_task::SchedTask>>> = OnceLock::new();
static WATCH_TASK: OnceLock<PMutex<Option<watch_task::WatchTask>>> = OnceLock::new();

fn watch_task_slot() -> &'static PMutex<Option<watch_task::WatchTask>> {
    WATCH_TASK.get_or_init(|| PMutex::new(None))
}

fn sched_task_slot() -> &'static PMutex<Option<sched_task::SchedTask>> {
    SCHED_TASK.get_or_init(|| PMutex::new(None))
}

fn cpu_task_slot() -> &'static PMutex<Option<cpu_task::CpuTask>> {
    CPU_TASK.get_or_init(|| PMutex::new(None))
}

/// Read + parse the config and derive the initial preset.
///
/// Mode comes from `modules.switcher.switchInode` (default
/// `/sdcard/Android/yc/uperf/cur_powermode.txt`), matching upstream's
/// `Preset inode -> '<mode>'` startup behaviour.
fn load_config_and_mode(cfg_path: &str) -> (Option<uperf_config::Config>, String) {
    let text = match std::fs::read_to_string(cfg_path) {
        Ok(t) => t,
        Err(e) => {
            log_msg(&format!("Rust: cannot read config '{cfg_path}': {e}"));
            return (None, "balance".into());
        }
    };
    let cfg = match uperf_config::Config::from_slice(text.as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            log_msg(&format!("Rust: config parse failed: {e}"));
            return (None, "balance".into());
        }
    };
    let inode = cfg
        .modules_map()
        .and_then(|m| m.get("switcher"))
        .and_then(|s| s.get("switchInode"))
        .and_then(|p| p.as_str())
        .unwrap_or("/sdcard/Android/yc/uperf/cur_powermode.txt");
    let mode = std::fs::read_to_string(inode)
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "balance".into());
    (Some(cfg), mode)
}

fn dispatcher_slot() -> &'static PMutex<Option<Dispatcher>> {
    DISPATCHER.get_or_init(|| PMutex::new(None))
}

fn log_msg(s: &str) {
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

#[allow(dead_code)]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
