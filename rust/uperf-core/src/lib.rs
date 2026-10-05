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

#[no_mangle]
pub(crate) extern "C" fn uperf_rs_start(
    config_path: *const libc::c_char,
    log_path: *const libc::c_char,
) -> libc::c_int {
    // SAFETY: NUL-terminated per contract.
    let cfg = unsafe { CStr::from_ptr(config_path) };
    let log = unsafe { CStr::from_ptr(log_path) };

    let (loaded_cfg, mode) = load_config_and_mode(&cfg.to_string_lossy());
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

    log_msg(&format!(
        "uperf_rs_start: cfg={} log={} (M4 orchestrator+governor, mode={}, config_loaded={})",
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
    log_msg("uperf_rs_stop: dispatcher joined");
}

static DISPATCHER: OnceLock<PMutex<Option<Dispatcher>>> = OnceLock::new();
static ORCHESTRATOR: OnceLock<Arc<PMutex<Orchestrator>>> = OnceLock::new();
static CPU_TASK: OnceLock<PMutex<Option<cpu_task::CpuTask>>> = OnceLock::new();

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
    let mut buf = log_buf().lock().unwrap();
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