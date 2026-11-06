//! C ABI bindings declared in `cpp/include/uperf_rs.h` (AGENT.md §5).

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int};
use std::sync::{mpsc::Sender, OnceLock};

use parking_lot::Mutex as PMutex;

use crate::topic_dispatch::Event;

/// Opaque C++ log line — `msg` is NUL-terminated UTF-8 valid for the call only.
#[repr(C)]
#[derive(Copy, Clone)]
pub(crate) struct LogLine {
    msg: *const c_char,
}

#[allow(dead_code)]
impl LogLine {
    /// SAFETY: caller must guarantee `msg` is a NUL-terminated UTF-8 byte slice.
    pub(crate) unsafe fn msg(&self) -> &'static str {
        if self.msg.is_null() {
            return "";
        }
        // SAFETY: contract.
        unsafe { std::ffi::CStr::from_ptr(self.msg) }
            .to_str()
            .unwrap_or("(non-utf8 log)")
    }
}

/// Process-lifetime handle to the C++ bridge. C++ gives us a single `Bridge*` at
/// boot via `uperf_rs_init`; Rust subscribes / writes logs through it.
///
/// SAFETY: the C++ side guarantees this pointer outlives the process.
#[repr(C)]
#[derive(Copy, Clone)]
pub(crate) struct Bridge {
    subscribe: unsafe extern "C" fn(*const c_char) -> c_int,
    write_log: unsafe extern "C" fn(*const c_char, *const c_char, usize),
}

impl Bridge {
    pub(crate) fn subscribe(&self, topic: &str) {
        let cstr = match std::ffi::CString::new(topic) {
            Ok(c) => c,
            Err(_) => return,
        };
        // SAFETY: subscribe is a valid C extern fn pointer per the C++ contract.
        unsafe { (self.subscribe)(cstr.as_ptr()) };
    }
}

/// Installed by C++ at `uperf_rs_init`.
pub(crate) static BRIDGE: OnceLock<Bridge> = OnceLock::new();

/// Topic set Rust subscribes to at boot. Mirrors the M0 tap exactly.
pub(crate) static TOPICS: once_cell::sync::Lazy<Vec<&'static str>> =
    once_cell::sync::Lazy::new(|| {
        vec![
            "input.touch",
            "input.btn",
            "input.state",
            "topapp.pkgName",
            "offscreen.state",
            "cgroup.ta.list",
            "cgroup.fg.list",
            "cgroup.bg.list",
            "cgroup.re.list",
            "cgroup.ta.update",
            "cgroup.fg.update",
            "cgroup.bg.update",
            "cgroup.re.update",
        ]
    });

/// Channel sender installed by `topic_dispatch::spawn`; `uperf_rs_on_event` forwards
/// every parsed event through it.
///
/// Replaceable, not a `OnceLock`: on a re-start the new dispatcher installs a new
/// channel, and a `OnceLock` would keep the first sender forever — every event after
/// the re-start would be sent into a channel whose receiver is gone and dropped
/// silently. `parking_lot::Mutex` keeps the uncontended send at tens of nanoseconds,
/// which is nothing beside the FFI hop that just happened.
static DISPATCH_TX: OnceLock<PMutex<Option<Sender<Event>>>> = OnceLock::new();

fn dispatch_slot() -> &'static PMutex<Option<Sender<Event>>> {
    DISPATCH_TX.get_or_init(|| PMutex::new(None))
}

pub(crate) fn install_dispatch_tx(tx: Sender<Event>) {
    *dispatch_slot().lock() = Some(tx);
}

/// Forward one event to the dispatcher, if one is running.
pub(crate) fn send_event(ev: Event) {
    let guard = dispatch_slot().lock();
    if let Some(tx) = guard.as_ref() {
        let _ = tx.send(ev);
    }
}

extern "C" {
    pub(crate) fn uperf_bridge_write_log(tag: *const c_char, msg: *const c_char, len: usize);
    /// `modules.log.level` — applied from Rust once the config is parsed.
    pub(crate) fn uperf_bridge_set_log_level(level: *const c_char);
    /// `modules.input.{swipeThd,gestureThdX,gestureThdY}` — applied from Rust once
    /// the config is parsed, to the listener the platform layer registered.
    pub(crate) fn uperf_bridge_set_input_thresholds(
        swipe_thd: f32,
        gesture_thd_x: f32,
        gesture_thd_y: f32,
    );
    /// `modules.atrace.enable` — opens the ftrace trace_marker and toggles the
    /// vendored ATRACE_* markers.
    pub(crate) fn uperf_bridge_set_atrace(enable: bool);
}