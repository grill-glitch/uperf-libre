//! C ABI bindings declared in `cpp/include/uperf_rs.h` (AGENT.md §5).

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int};
use std::sync::{mpsc::Sender, OnceLock};

use parking_lot::Mutex as PMutex;

use crate::topic_dispatch::Event;

/// Process-lifetime handle to the C++ bridge. C++ gives us a single `Bridge*` at
/// boot via `uperf_rs_init`; Rust subscribes / writes logs through it.
///
/// SAFETY: the C++ side guarantees this pointer outlives the process.
#[repr(C)]
#[derive(Copy, Clone)]
pub(crate) struct Bridge {
    subscribe: unsafe extern "C" fn(*const c_char) -> c_int,
    /// Layout parity with `uperf_bridge_t` in `cpp/include/uperf_rs_bridge.h`. Logging goes
    /// through the `uperf_bridge_write_log` extern instead (one path, not two), so this field
    /// is never read — it must stay for the ABI and is therefore not dead code.
    #[allow(dead_code)]
    write_log: unsafe extern "C" fn(*const c_char, *const c_char, usize),
}

impl Bridge {
    /// Subscribe a topic; returns whether the C++ side accepted it.
    pub(crate) fn subscribe(&self, topic: &str) -> bool {
        let cstr = match std::ffi::CString::new(topic) {
            Ok(c) => c,
            Err(_) => {
                log_line(&format!("subscribe: topic {topic:?} contains a NUL"));
                return false;
            }
        };
        // SAFETY: subscribe is a valid C extern fn pointer per the C++ contract.
        let rc = unsafe { (self.subscribe)(cstr.as_ptr()) };
        if rc != 0 {
            // Documented as `-1` for an unknown topic: a typo used to disable an entire data
            // source with zero diagnostics.
            log_line(&format!("subscribe: {topic} rejected by the bridge (rc={rc})"));
        }
        rc == 0
    }
}

fn log_line(s: &str) {
    match std::ffi::CString::new(s.replace('\0', " ")) {
        Ok(msg) => unsafe {
            // SAFETY: `tag` is explicitly allowed to be null; `msg` is NUL-terminated and
            // borrowed only for the call.
            uperf_bridge_write_log(std::ptr::null(), msg.as_ptr(), msg.as_bytes().len());
        },
        Err(_) => {}
    }
}

/// Installed by C++ at `uperf_rs_init`.
pub(crate) static BRIDGE: OnceLock<Bridge> = OnceLock::new();

/// Topic set Rust subscribes to at boot. Mirrors the M0 tap exactly.
///
/// A plain `&'static [&'static str]`: the list is fixed at compile time and only iterated, so
/// a lazily-allocated `Vec` bought an allocation and a synchronisation point for nothing.
pub(crate) static TOPICS: &[&str] = &[
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
];

/// Channel sender installed by `topic_dispatch::spawn`; `uperf_rs_on_event` forwards
/// every parsed event through it.
///
/// Replaceable, not a `OnceLock`: on a re-start the new dispatcher installs a new
/// channel, and a `OnceLock` would keep the first sender forever — every event after
/// the re-start would be sent into a channel whose receiver is gone and dropped
/// silently. `parking_lot::Mutex` keeps the uncontended send at tens of nanoseconds,
/// which is nothing beside the FFI hop that just happened.
static DISPATCH_TX: OnceLock<PMutex<Option<(u64, Sender<Event>)>>> = OnceLock::new();

/// Monotonic generation so a late `clear_dispatch_tx` from the previous generation cannot
/// wipe the sender a new dispatcher just installed.
static DISPATCH_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn dispatch_slot() -> &'static PMutex<Option<(u64, Sender<Event>)>> {
    DISPATCH_TX.get_or_init(|| PMutex::new(None))
}

/// Install a new sender and return its generation id.
pub(crate) fn install_dispatch_tx(tx: Sender<Event>) -> u64 {
    let gen = DISPATCH_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    *dispatch_slot().lock() = Some((gen, tx));
    gen
}

/// Drop the installed sender. The dispatcher's `join_timeout` needs this: the receiver
/// only sees `Disconnected` once *every* sender is gone, and since the slot holds one
/// for the FFI entry point, a stop that skipped this step waited out the whole 2 s
/// join budget and detached a thread that was perfectly able to exit (measured on
/// device: `caught signal` at :39, `task 'cpu' stopped` at :41).
pub(crate) fn clear_dispatch_tx(owner_gen: u64) {
    // Generation-checked: a late `stop()` from the previous generation must not wipe the
    // sender a *new* dispatcher just installed (every later event would be dropped until
    // the next restart).
    let mut guard = dispatch_slot().lock();
    if guard.as_ref().is_some_and(|(gen, _)| *gen == owner_gen) {
        *guard = None;
    }
}

/// Forward one event to the dispatcher, if one is running.
pub(crate) fn send_event(ev: Event) {
    let guard = dispatch_slot().lock();
    if let Some((_, tx)) = guard.as_ref() {
        if tx.send(ev).is_err() {
            // The receiver is gone (dispatcher stopped or failed to start): the event is
            // dropped, and a silent drop is indistinguishable from a healthy but quiet run.
            static DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if n == 1 || n % 1000 == 0 {
                log_line(&format!("event dropped: dispatcher receiver gone ({n} so far)"));
            }
        }
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