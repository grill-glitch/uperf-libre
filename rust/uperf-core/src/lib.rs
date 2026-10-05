//! uperf-core — the Rust half of the rewrite (AGENT.md §3, §5).
//!
//! M1 scope:
//!   * Export the C ABI declared in `cpp/include/uperf_rs.h`.
//!   * Subscribe to the platform's events via the C++ bridge, log every event on
//!     stdout (actually through the same spdlog file sink as the C++ side), and
//!     survive `start()` / `reload()` / `stop()` cycles. No policy yet (M2+).
//!
//! Lifetime rules (AGENT.md §5.1):
//!   * `data` passed to `on_event` is valid ONLY for the duration of the call.
//!   * `topic` is NUL-terminated UTF-8, also call-scoped — must be copied if kept.
//!   * `data == NULL && len == 0` ⇒ the topic carries no payload (`cgroup.*.update`).

#![deny(unsafe_op_in_unsafe_fn)]

mod ffi;
mod topic_dispatch;

use std::ffi::CStr;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex as PMutex;

use ffi::{Bridge, BRIDGE, DISPATCH_TX, TOPICS};
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
            // Send and ignore errors (receiver gone = dispatcher exited cleanly).
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

    // (Re)start the dispatcher.
    {
        let mut guard = dispatcher_slot().lock();
        if let Some(mut prev) = guard.take() {
            prev.join_timeout(std::time::Duration::from_secs(2));
        }
        *guard = Some(topic_dispatch::spawn());
    }

    // Subscribe to every topic we cover in M1.
    let bridge = match BRIDGE.get() {
        Some(b) => b,
        None => {
            return 1;
        }
    };
    for topic in TOPICS.iter() {
        bridge.subscribe(topic);
    }

    log_msg(&format!(
        "uperf_rs_start: cfg={} log={} (M1: log-only, no policy yet)",
        cfg.to_string_lossy(),
        log.to_string_lossy()
    ));
    0
}

#[no_mangle]
pub(crate) extern "C" fn uperf_rs_reload() {
    log_msg("uperf_rs_reload: re-subscribing (M1 just re-subscribes)");
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
    log_msg("uperf_rs_stop: dispatcher joined");
}

// ---------------------------------------------------------------------------
//  Globals
// ---------------------------------------------------------------------------

static DISPATCHER: OnceLock<PMutex<Option<Dispatcher>>> = OnceLock::new();

fn dispatcher_slot() -> &'static PMutex<Option<Dispatcher>> {
    DISPATCHER.get_or_init(|| PMutex::new(None))
}

// ---------------------------------------------------------------------------
//  Helpers
// ---------------------------------------------------------------------------

fn log_msg(s: &str) {
    let mut buf = log_buf().lock().unwrap();
    buf.clear();
    buf.extend_from_slice(s.as_bytes());
    buf.push(b'\n');
    // SAFETY: spdlog file sink copies before returning.
    unsafe { ffi::uperf_bridge_write_log(buf.as_ptr().cast(), buf.len()) };
}

#[allow(dead_code)]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}