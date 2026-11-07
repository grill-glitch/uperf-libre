//! Topic parsing + dispatcher thread.
//!
//! `Event::parse` converts the raw payload bytes the C++ bridge hands us into a
//! typed value. The dispatcher thread is the single owner of the spdlog sink —
//! the C++ side only ever calls `uperf_bridge_write_log` from this thread, so log
//! lines cannot race. M3 will swap the print for the switcher / state machine.


use std::sync::mpsc::{Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};


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
    // Width from the target, not a hardcoded 8+8: a 32-bit build would read garbage.
    const PTR_BYTES: usize = std::mem::size_of::<usize>();
    if payload.len() < 2 * PTR_BYTES {
        return None;
    }
    let pids_ptr = usize::from_ne_bytes(payload[0..PTR_BYTES].try_into().ok()?) as *const i32;
    let len = usize::from_ne_bytes(payload[PTR_BYTES..2 * PTR_BYTES].try_into().ok()?);
    if pids_ptr.is_null() || len == 0 {
        return Some(Vec::new());
    }
    // A corrupt length must not drive an unbounded read/allocation, and `from_raw_parts`
    // requires the pointer to satisfy the alignment of `i32`.
    const MAX_PIDS: usize = 1 << 16;
    if len > MAX_PIDS || (pids_ptr as usize) % std::mem::align_of::<i32>() != 0 {
        return None;
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
///
/// `foreground` is the optional top-app file source (queue item ④): every tick it
/// is re-read and, when it names a new (fresh) package, injected as a
/// `topapp.pkgName` event through the same path as a C++-published one. When it is
/// absent or stale the loop simply carries the C++ events, so the vendored monitor
/// stays the fallback.
pub(crate) fn spawn(
    orch: std::sync::Arc<parking_lot::Mutex<crate::orchestrator::Orchestrator>>,
    dfps: Option<crate::dfps_rs::DfpsScheduler>,
    foreground: Option<crate::foreground::ForegroundFile>,
    settings: crate::settings::Settings,
    sink: Option<std::sync::Arc<parking_lot::Mutex<crate::real_sink::RealSink>>>,
) -> Dispatcher {
    let (tx, rx): (Sender<Event>, Receiver<Event>) = std::sync::mpsc::channel();
    // Install a clone so the FFI entry point can also send events. Replaces any
    // previous generation's sender: its receiver died with that dispatcher.
    let generation = crate::ffi::install_dispatch_tx(tx.clone());
    let thread = thread::Builder::new()
        .name("uperf-rs".into())
        .spawn(move || run(rx, orch, dfps, foreground, settings, sink))
        .expect("spawn uperf-rs dispatcher");
    Dispatcher {
        _keep_alive: tx,
        generation,
        thread: Some(thread),
    }
}

pub(crate) struct Dispatcher {
    /// Keep this sender alive until `join_timeout` is called. The cloned sender in
    /// `ffi::DISPATCH_TX` keeps the FFI entry point usable; once both are dropped the
    /// receiver hits `RecvError` and the thread exits.
    _keep_alive: Sender<Event>,
    /// The generation this dispatcher installed in `ffi::DISPATCH_TX`.
    generation: u64,
    thread: Option<JoinHandle<()>>,
}

impl Dispatcher {
    pub(crate) fn join_timeout(&mut self, d: Duration) {
        // Every sender must go: the FFI one lives in `ffi::DISPATCH_TX`, so dropping
        // only this one leaves the receiver connected and the thread parked in
        // `recv_timeout` until the whole budget is spent.
        crate::ffi::clear_dispatch_tx(self.generation);
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
    mut foreground: Option<crate::foreground::ForegroundFile>,
    settings: crate::settings::Settings,
    sink: Option<std::sync::Arc<parking_lot::Mutex<crate::real_sink::RealSink>>>,
) {
    // The wake interval comes from the one `Settings` snapshot: the foreground poll
    // when that source is armed, otherwise a full second — long enough to notice a
    // dropped sender promptly, since the join path waits at most two seconds.
    let tick = settings.dispatcher_tick();
    // Poll the file at most once per tick, independent of how busy the channel is. The old
    // code only polled on `Timeout`, so a steady stream of C++ events (a touch storm shorter
    // than the tick) returned `Ok` every iteration and the top-app file was never re-read —
    // contradicting the documented "every tick" behaviour.
    let mut next_foreground_poll = std::time::Instant::now();
    let poll_foreground = |foreground: &mut Option<crate::foreground::ForegroundFile>,
                           orch: &std::sync::Arc<parking_lot::Mutex<crate::orchestrator::Orchestrator>>,
                           dfps: Option<&crate::dfps_rs::DfpsScheduler>,
                           settings: &crate::settings::Settings,
                           sink: Option<&std::sync::Arc<parking_lot::Mutex<crate::real_sink::RealSink>>>| {
        // A stale/absent file yields nothing and the C++ monitor's own `topapp.pkgName`
        // events carry on.
        if let Some(pkg) = foreground.as_mut().and_then(|f| f.poll()) {
            handle(&Event::Topapp(pkg), orch, dfps, settings, sink.map(|s| s.as_ref()));
        }
    };
    loop {
        // Never block longer than the next foreground deadline.
        let wait = next_foreground_poll
            .saturating_duration_since(std::time::Instant::now())
            .min(tick);
        let received = rx.recv_timeout(if wait.is_zero() { std::time::Duration::from_millis(0) } else { wait });
        match received {
            Ok(ev) => handle(&ev, &orch, dfps.as_ref(), &settings, sink.as_deref()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if std::time::Instant::now() >= next_foreground_poll {
            next_foreground_poll = std::time::Instant::now() + tick;
            poll_foreground(&mut foreground, &orch, dfps.as_ref(), &settings, sink.as_ref());
        }
    }
}

/// One event, through every subscriber. Both the FFI entry point and the
/// foreground tick feed this, so a file-sourced top-app is handled byte-for-byte
/// like a C++-published one.
fn handle(
    ev: &Event,
    orch: &std::sync::Arc<parking_lot::Mutex<crate::orchestrator::Orchestrator>>,
    dfps: Option<&crate::dfps_rs::DfpsScheduler>,
    settings: &crate::settings::Settings,
    sink: Option<&parking_lot::Mutex<crate::real_sink::RealSink>>,
) {
    write_event(ev);

    {
        let mut g = orch.lock();
        g.on_event(ev);
    }
    apply_pending(orch, settings, sink);

    // Refresh-rate topics fan out to dfps-rs as a second subscriber
    // (T03: same process, no IPC). Ordering after the orchestrator is
    // deliberate — the orchestrator owns sysfs/CPU knobs, dfps owns the
    // refresh rate, and nothing is shared between them.
    if let Some(d) = dfps {
        route_dfps(ev, d);
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
    settings: &crate::settings::Settings,
    sink: Option<&parking_lot::Mutex<crate::real_sink::RealSink>>,
) -> (usize, usize) {
    let mut collected = crate::orchestrator::CollectingSink::default();
    // One lock: the plan *and* the config flag that decides whether the plan may be
    // written to live nodes come from the same orchestrator state.
    let knobs_enabled = {
        let mut g = orch.lock();
        g.drain(&mut collected);
        g.sysfs_enabled()
    };
    if collected.writes.is_empty() {
        return (0, 0);
    }
    for w in &collected.writes {
        log_sysfs_write(w);
    }
    match settings.fake_root.as_deref() {
        Some(root) => {
            // Record what each knob held before we touch it, so a later stop (or the
            // watchdog's dead-man path) can put it back. Without a status target there
            // is nowhere to record, and we write without one rather than inventing a
            // location.
            let mut files = match settings.sysfs_orig.clone() {
                Some(p) => crate::orchestrator::UnderRootSink::with_ledger(
                    root,
                    crate::sysfs_ledger::SysfsLedger::load(p),
                ),
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
        // Live tree, and only when the config says the knob table is on. A config that
        // ships `modules.sysfs.enable: false` (all 63 of them do) therefore cannot write a
        // single kernel node, whatever its knob table contains.
        None if knobs_enabled => {
            let Some(sink) = sink else {
                // No sink means the daemon was started without one; never silently
                // claim a write.
                for w in collected.writes {
                    log_msg(&format!("Rust: would-write {} = {}", w.path, w.value));
                }
                return (0, 0);
            };
            // The sink lives in the daemon: its fd cache and its dedup map are what make
            // a second identical scene switch free, so it must not be rebuilt per plan.
            let mut files = sink.lock();
            let (w0, f0, i0) = (files.written, files.failed.len(), files.ineffective.len());
            for w in collected.writes {
                files.write(&w);
            }
            for (path, err) in files.failed[f0..].iter() {
                log_msg(&format!("Rust: sysfs write failed {path}: {err}"));
            }
            for (path, wrote, read) in files.ineffective[i0..].iter() {
                log_msg(&format!(
                    "Rust: sysfs write of {path}={wrote} read back as '{read}' (driver kept its value)"
                ));
            }
            let (written, failed) = (files.written - w0, files.failed.len() - f0);
            log_msg(&format!(
                "Rust: sysfs real-write {written} ok, {failed} failed ({})",
                files.summary()
            ));
            (written, failed)
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


#[cfg(test)]
mod foreground_tick_tests {
    //! Queue item ④: a fresh line in the foreground file must reach the
    //! orchestrator exactly like a C++-published `topapp.pkgName`, and a stale
    //! one must reach it not at all (the vendored monitor stays the fallback).

    use super::*;
    use crate::hint::HintDurations;
    use crate::orchestrator::Orchestrator;
    use std::sync::Arc;

    fn unique_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "uperf_td_{}_{}_{}.txt",
            tag,
            std::process::id(),
            crate::foreground::monotonic_ms()
        ))
    }

    fn spawn_with(
        fg: Option<crate::foreground::ForegroundFile>,
    ) -> (Arc<parking_lot::Mutex<Orchestrator>>, Dispatcher) {
        let orch = Arc::new(parking_lot::Mutex::new(Orchestrator::new(
            HintDurations::default(),
            "balance",
        )));
        let settings = crate::settings::Settings::default();
        let d = spawn(orch.clone(), None, fg, settings, None);
        (orch, d)
    }

    fn top_of(orch: &Arc<parking_lot::Mutex<Orchestrator>>) -> Option<String> {
        orch.lock().top_app().map(str::to_string)
    }

    #[test]
    fn a_fresh_foreground_file_becomes_the_top_app() {
        let path = unique_path("fresh");
        std::fs::write(
            &path,
            format!("com.example.top {}\n", crate::foreground::monotonic_ms()),
        )
        .unwrap();
        let fg = crate::foreground::ForegroundFile::new(
            &path,
            crate::foreground::DEFAULT_MAX_AGE_MS,
        );
        let (orch, mut d) = spawn_with(Some(fg));

        // The tick is 500 ms; give the first tick a generous margin.
        let mut got = None;
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(50));
            got = top_of(&orch);
            if got.is_some() {
                break;
            }
        }
        d.join_timeout(Duration::from_secs(2));
        let _ = std::fs::remove_file(&path);
        assert_eq!(got.as_deref(), Some("com.example.top"));
    }

    #[test]
    fn a_stale_foreground_file_reports_nothing() {
        let path = unique_path("stale");
        // A timestamp far in the past: the helper stopped writing.
        std::fs::write(&path, "com.example.gone 1\n").unwrap();
        let fg = crate::foreground::ForegroundFile::new(&path, 500);
        let (orch, mut d) = spawn_with(Some(fg));

        std::thread::sleep(Duration::from_millis(1200));
        let got = top_of(&orch);
        d.join_timeout(Duration::from_secs(2));
        let _ = std::fs::remove_file(&path);
        assert_eq!(got, None, "a stale file must never invent a top app");
    }
}