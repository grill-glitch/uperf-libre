//! Topic parsing + dispatcher thread.
//!
//! `Event::parse` converts the raw payload bytes the C++ bridge hands us into a
//! typed value. The dispatcher thread is the single owner of the spdlog sink —
//! the C++ side only ever calls `uperf_bridge_write_log` from this thread, so log
//! lines cannot race. M3 will swap the print for the switcher / state machine.

use std::ffi::{c_char, CStr};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::ffi::DISPATCH_TX;
use crate::orchestrator::Sink;

// ---------------------------------------------------------------------------
//  Topic — typed identifier matching cpp/uperf/bridge.cpp::dispatch()
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Topic {
    InputTouch,
    InputBtn,
    InputState,
    TopappPkgName,
    OffscreenState,
    CgroupTaList,
    CgroupFgList,
    CgroupBgList,
    CgroupReList,
    CgroupTaUpdate,
    CgroupFgUpdate,
    CgroupBgUpdate,
    CgroupReUpdate,
}

impl Topic {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::InputTouch => "input.touch",
            Self::InputBtn => "input.btn",
            Self::InputState => "input.state",
            Self::TopappPkgName => "topapp.pkgName",
            Self::OffscreenState => "offscreen.state",
            Self::CgroupTaList => "cgroup.ta.list",
            Self::CgroupFgList => "cgroup.fg.list",
            Self::CgroupBgList => "cgroup.bg.list",
            Self::CgroupReList => "cgroup.re.list",
            Self::CgroupTaUpdate => "cgroup.ta.update",
            Self::CgroupFgUpdate => "cgroup.fg.update",
            Self::CgroupBgUpdate => "cgroup.bg.update",
            Self::CgroupReUpdate => "cgroup.re.update",
        }
    }
}

// ---------------------------------------------------------------------------
//  Event — typed payload
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum Event {
    Touch(bool),
    Btn(bool),
    InputState { hold: bool, swipe: bool, gesture: bool },
    Topapp(String),
    Offscreen(bool),
    CgroupList { topic: Topic, pids: Vec<i32> },
    CgroupUpdate(Topic),
}

impl Event {
    pub(crate) fn parse(topic: &str, payload: &[u8]) -> Option<Self> {
        use Topic::*;
        let t = match topic {
            "input.touch" => InputTouch,
            "input.btn" => InputBtn,
            "input.state" => InputState,
            "topapp.pkgName" => TopappPkgName,
            "offscreen.state" => OffscreenState,
            "cgroup.ta.list" => CgroupTaList,
            "cgroup.fg.list" => CgroupFgList,
            "cgroup.bg.list" => CgroupBgList,
            "cgroup.re.list" => CgroupReList,
            "cgroup.ta.update" => CgroupTaUpdate,
            "cgroup.fg.update" => CgroupFgUpdate,
            "cgroup.bg.update" => CgroupBgUpdate,
            "cgroup.re.update" => CgroupReUpdate,
            _ => return None,
        };
        match t {
            InputTouch => Some(Event::Touch(decode_bool(payload))),
            InputBtn => Some(Event::Btn(decode_bool(payload))),
            InputState => decode_input_state(payload).map(|(hold, swipe, gesture)| {
                Event::InputState { hold, swipe, gesture }
            }),
            TopappPkgName => {
                let s = std::str::from_utf8(payload).ok()?;
                Some(Event::Topapp(s.trim_end_matches('\0').to_owned()))
            }
            OffscreenState => Some(Event::Offscreen(decode_bool(payload))),
            CgroupTaList | CgroupFgList | CgroupBgList | CgroupReList => {
                let pids = decode_pid_list(payload).unwrap_or_default();
                Some(Event::CgroupList { topic: t, pids })
            }
            CgroupTaUpdate | CgroupFgUpdate | CgroupBgUpdate | CgroupReUpdate => {
                Some(Event::CgroupUpdate(t))
            }
        }
    }
}

fn decode_bool(payload: &[u8]) -> bool {
    if payload.len() < 4 {
        return false;
    }
    i32::from_ne_bytes(payload[0..4].try_into().unwrap()) != 0
}

fn decode_input_state(payload: &[u8]) -> Option<(bool, bool, bool)> {
    if payload.len() < 12 {
        return None;
    }
    let hold = i32::from_ne_bytes(payload[0..4].try_into().ok()?) != 0;
    let swipe = i32::from_ne_bytes(payload[4..8].try_into().ok()?) != 0;
    let gesture = i32::from_ne_bytes(payload[8..12].try_into().ok()?) != 0;
    Some((hold, swipe, gesture))
}

fn decode_pid_list(payload: &[u8]) -> Option<Vec<i32>> {
    // C layout: uperf_pid_list_t { const int32_t* pids, size_t len }.
    // On arm64 that is 8 + 8 = 16 bytes.
    if payload.len() < 16 {
        return None;
    }
    let pids_ptr = u64::from_ne_bytes(payload[0..8].try_into().unwrap()) as *const i32;
    let len = u64::from_ne_bytes(payload[8..16].try_into().unwrap()) as usize;
    if pids_ptr.is_null() || len == 0 {
        return Some(Vec::new());
    }
    // SAFETY: the C++ bridge guarantees `pids` points to `len` valid i32s whose
    // lifetime outlives this call.
    let pids = unsafe { std::slice::from_raw_parts(pids_ptr, len) };
    Some(pids.to_vec())
}

// ---------------------------------------------------------------------------
//  Dispatcher thread
// ---------------------------------------------------------------------------

/// Spawn the dispatcher thread. The thread reads from `rx` and writes each
/// event as a single log line through the C++ spdlog sink. It exits when both
/// senders (the FFI sender in DISPATCH_TX and the keep-alive) are dropped.
///
/// `dfps` carries the optional dfps-rs scheduler: when present, the five
/// refresh-rate topics are routed into it in addition to the orchestrator
/// (upstream `DynamicFps::AddReactor`, `dynamic_fps.cpp:202-209`).
pub(crate) fn spawn(
    orch: std::sync::Arc<parking_lot::Mutex<crate::orchestrator::Orchestrator>>,
    dfps: Option<crate::dfps_rs::DfpsScheduler>,
) -> Dispatcher {
    let (tx, rx): (Sender<Event>, Receiver<Event>) = std::sync::mpsc::channel();
    // Install a clone so the FFI entry point can also send events.
    let tx_for_ffi = tx.clone();
    DISPATCH_TX.get_or_init(|| tx_for_ffi);
    let thread = thread::Builder::new()
        .name("uperf-rs".into())
        .spawn(move || run(rx, orch, dfps))
        .expect("spawn uperf-rs dispatcher");
    Dispatcher {
        _keep_alive: tx,
        thread: Some(thread),
    }
}

pub(crate) struct Dispatcher {
    /// Keep this sender alive until `join_timeout` is called. The cloned sender
    /// in `DISPATCH_TX` keeps the FFI entry point usable; once both are dropped
    /// the receiver hits `RecvError` and the thread exits.
    _keep_alive: Sender<Event>,
    thread: Option<JoinHandle<()>>,
}

impl Dispatcher {
    pub(crate) fn join_timeout(&mut self, d: Duration) {
        drop(std::mem::replace(&mut self._keep_alive, dummy_sender()));
        let start = Instant::now();
        if let Some(t) = self.thread.take() {
            while !t.is_finished() && start.elapsed() < d {
                thread::sleep(Duration::from_millis(10));
            }
            if !t.is_finished() {
                eprintln!("uperf-rs dispatcher did not exit in {d:?}; leaking the thread");
            }
        }
    }
}

fn dummy_sender() -> Sender<Event> {
    let (tx, _rx) = std::sync::mpsc::channel::<Event>();
    tx
}

fn run(
    rx: Receiver<Event>,
    orch: std::sync::Arc<parking_lot::Mutex<crate::orchestrator::Orchestrator>>,
    dfps: Option<crate::dfps_rs::DfpsScheduler>,
) {
    // M4: writes go under a fake root so device validation never touches the
    // real sysfs. Set `UPERF_FAKE_ROOT` to enable file emission (e.g.
    // /data/local/tmp/uperf_fake); otherwise we only log the planned sequence.
    let fake_root = std::env::var("UPERF_FAKE_ROOT").ok();
    while let Ok(ev) = rx.recv() {
        write_event(&ev);

        {
            let mut g = orch.lock();
            g.on_event(&ev);
        }
        apply_pending(&orch, fake_root.as_deref());

        // Refresh-rate topics fan out to dfps-rs as a second subscriber
        // (T03: same process, no IPC). Ordering after the orchestrator is
        // deliberate — the orchestrator owns sysfs/CPU knobs, dfps owns the
        // refresh rate, and nothing is shared between them.
        if let Some(d) = dfps.as_ref() {
            route_dfps(&ev, d);
        }
    }
}

/// Upstream `DynamicFps::AddReactor` subscribes to exactly these five topics
/// (`dynamic_fps.cpp:202-209`). Payload shapes come from `Event::parse`.
fn route_dfps(ev: &Event, d: &crate::dfps_rs::DfpsScheduler) {
    match ev {
        Event::Touch(pressed) => d.on_touch(*pressed),
        Event::Btn(pressed) => d.on_btn(*pressed),
        Event::InputState { gesture, .. } => d.on_input_state(*gesture),
        Event::Topapp(pkg) => d.on_top_app(pkg),
        Event::Offscreen(off) => d.on_offscreen(*off),
        // cgroup.* topics are the orchestrator's; dfps does not read them.
        _ => {}
    }
}

/// Drain whatever the orchestrator has planned into the configured sink.
///
/// Shared by the event loop and the mode watcher, so a preset switched from
/// `cur_powermode.txt` produces byte-identical writes to a preset switched by an
/// event. Returns `(written, failed)`.
pub(crate) fn apply_pending(
    orch: &std::sync::Arc<parking_lot::Mutex<crate::orchestrator::Orchestrator>>,
    fake_root: Option<&str>,
) -> (usize, usize) {
    let mut collected = crate::orchestrator::CollectingSink::default();
    {
        let mut g = orch.lock();
        g.drain(&mut collected);
    }
    if collected.writes.is_empty() {
        return (0, 0);
    }
    for w in &collected.writes {
        log_sysfs_write(w);
    }
    match fake_root {
        Some(root) => {
            // Record what each knob held before we touch it, so a later stop (or the
            // watchdog's dead-man path) can put it back. Without a status target there
            // is nowhere to record, and we write without one rather than inventing a
            // location.
            let mut files = match crate::sysfs_ledger::SysfsLedger::for_status() {
                Some(l) => crate::orchestrator::UnderRootSink::with_ledger(root, l),
                None => crate::orchestrator::UnderRootSink::new(root),
            };
            for w in collected.writes {
                files.write(&w);
            }
            log_msg(&format!(
                "Rust: fake-write root={} ok={} failed={}",
                root,
                files.written.len(),
                files.failed.len()
            ));
            (files.written.len(), files.failed.len())
        }
        None => {
            for w in collected.writes {
                log_msg(&format!("Rust: would-write {} = {}", w.path, w.value));
            }
            (0, 0)
        }
    }
}

fn log_sysfs_write(w: &crate::orchestrator::SysfsWrite) {
    use std::fmt::Write as _;
    let mut buf = String::with_capacity(96);
    let _ = write!(buf, "Rust: SysfsWrite path={} value={}", w.path, w.value);
    write_log_line(&buf);
}

fn log_msg(s: &str) {
    use std::sync::{Mutex, OnceLock};
    static BUF: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
    let buf = BUF.get_or_init(|| Mutex::new(Vec::with_capacity(160)));
    // `try_lock`, not `lock`: this buffer is written from the shutdown path too,
    // and a signal handler can re-enter that path (SIGTERM + the supervisor's
    // SIGUSR1). A plain `lock()` there deadlocks on itself and the process never
    // exits. A dropped log line is always better than a hung daemon.
    let Ok(mut b) = buf.try_lock() else { return };
    b.clear();
    b.extend_from_slice(s.as_bytes());
    b.push(b'\n');
    // SAFETY: the C++ sink copies before returning.
    unsafe {
        crate::ffi::uperf_bridge_write_log(std::ptr::null(), b.as_ptr().cast(), b.len());
    }
}

fn write_event(ev: &Event) {
    use std::fmt::Write as _;
    let mut buf = String::with_capacity(128);
    let _ = match ev {
        Event::Touch(b) => write!(buf, "Rust: input.touch = {}", b),
        Event::Btn(b) => write!(buf, "Rust: input.btn = {}", b),
        Event::InputState { hold, swipe, gesture } => write!(
            buf,
            "Rust: input.state = hold:{} swipe:{} gesture:{}",
            hold, swipe, gesture
        ),
        Event::Topapp(s) => write!(buf, "Rust: topapp.pkgName = {}", s),
        Event::Offscreen(b) => write!(buf, "Rust: offscreen.state = {}", b),
        Event::CgroupList { topic, pids } => {
            let mut preview = String::new();
            for (i, p) in pids.iter().take(8).enumerate() {
                if i > 0 {
                    preview.push(' ');
                }
                let _ = write!(preview, "{}", p);
            }
            write!(
                buf,
                "Rust: {} = {} pid(s) [{}]",
                topic.as_str(),
                pids.len(),
                preview
            )
        }
        Event::CgroupUpdate(t) => write!(buf, "Rust: {} (no payload)", t.as_str()),
    };
    write_log_line(&buf);
}

fn write_log_line(s: &str) {
    let cstr = match std::ffi::CString::new(s) {
        Ok(c) => c,
        Err(_) => return,
    };
    // SAFETY: the FFI copies the bytes.
    unsafe {
        crate::ffi::uperf_bridge_write_log(std::ptr::null(), cstr.as_ptr(), s.len());
    }
}

#[allow(dead_code)]
fn _cstr_assert(c: *const c_char) -> Option<&'static str> {
    if c.is_null() {
        return None;
    }
    // SAFETY: for diagnostics only.
    unsafe { CStr::from_ptr(c) }.to_str().ok()
}