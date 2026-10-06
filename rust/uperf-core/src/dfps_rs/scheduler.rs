//! DfpsScheduler — event routing + the delayed transitions upstream implements
//! with `DelayedWorker` (`dynamic_fps.cpp:226-282`).
//!
//! ## What upstream does
//!
//! `DynamicFps` subscribes to five uperf topics and on each either applies a
//! change immediately or schedules one for later through a `DelayedWorker`:
//!
//! | topic | immediate | delayed |
//! |---|---|---|
//! | `input.touch` / `input.btn` | `active_ = true; SwitchRefreshRate()` | release -> `active_ = false; SwitchRefreshRate()` after `touchSlackMs` |
//! | `input.state` (gesture) | `overridedApp_ = "*"; SwitchRefreshRate()` | resume -> clear override after `gestureSlackMs` |
//! | `topapp.pkgName` | `curApp_ = pkg; SwitchRefreshRate(force=true)` | — |
//! | `offscreen.state` | off -> `overridedApp_ = "-"; SwitchRefreshRate(true)` | wake -> clear override after `gestureSlackMs` |
//!
//! `DwSetWork(handle, fn, deadline)` cancels the handle's pending item by
//! **replacing** it. With one timer thread reading the slot under the same
//! lock that the events write it, replacing the value *is* the cancellation —
//! no generation counter is needed, which is why there is none here.
//!
//! ## Threading
//!
//! One `Mutex<State>` + one `Condvar`, paired. Every event mutates state and
//! `notify_all()`s; the single timer thread computes the earliest pending
//! deadline under the lock and `wait_for`s it. Because the deadline is read
//! and waited on under the same lock, an event cannot slip between the check
//! and the wait and be lost.
//!
//! ## I/O
//!
//! The task itself is pure. Everything that touches the device goes through
//! [`RefreshSink`]: production uses [`RealSink`] (`settings put` + the notify
//! file), tests use [`RecordingSink`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::dfps_rs::config::{FpsRule, RuleTable, OFFSCREEN_PKG, UNIVERSAL_PKG};
use crate::dfps_rs::task::DfpsTask;

/// Where an effective refresh-rate change lands, and where the brightness
/// sample comes from. Production writes the SettingsProvider keys and
/// `dfps_cur.txt`; tests record and return a fixed brightness.
pub trait RefreshSink: Send + Sync {
    /// Called once per *actual* change (the dedupe gate already ran).
    fn on_switch(&self, from: Option<i32>, to: i32);

    /// `screen_brightness` for the anti-flicker gate. `None` means the read
    /// failed; the task maps that to upstream's `-1` (treated as "low").
    fn screen_brightness(&self) -> Option<i32>;
}

/// The production sink: upstream `SysPeakRefreshRate` + `NotifyRefreshRate`.
pub struct RealSink;

impl RefreshSink for RealSink {
    fn on_switch(&self, from: Option<i32>, to: i32) {
        crate::log_msg(&format!(
            "Dfps: switch {} -> {to} Hz",
            match from {
                Some(p) => p.to_string(),
                None => "-".to_string(),
            }
        ));
        // Order mirrors upstream: notify file, then the settings writes.
        if let Err(e) = crate::dfps_rs::notifier::write_cur_hz(to) {
            crate::log_msg(&format!("Dfps: notify write failed: {e}"));
        }
        if crate::dfps_rs::sys_settings::write_peak_refresh_rate(to) == 0 {
            crate::log_msg("Dfps: settings put did not spawn (no /system/bin/cmd?)");
        }
    }

    fn screen_brightness(&self) -> Option<i32> {
        crate::dfps_rs::sys_settings::get_screen_brightness()
    }
}

/// Test double: records every switch, touches nothing. Brightness defaults to
/// `Some(255)` (bright → not low), so idle behaviour is the default in tests.
pub struct RecordingSink {
    pub events: Mutex<Vec<(Option<i32>, i32)>>,
    pub brightness: Mutex<Option<i32>>,
}

impl Default for RecordingSink {
    fn default() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            brightness: Mutex::new(Some(255)),
        }
    }
}

impl RefreshSink for RecordingSink {
    fn on_switch(&self, from: Option<i32>, to: i32) {
        self.events.lock().push((from, to));
    }
    fn screen_brightness(&self) -> Option<i32> {
        *self.brightness.lock()
    }
}

/// Which delayed transition a deadline belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    /// Input release -> back to idle.
    Input,
    /// Gesture end -> drop the `*` override.
    Gesture,
    /// Screen wake -> drop the `-` override.
    Wakeup,
}

struct State {
    task: DfpsTask,
    /// Pending deadline per handle; `None` = nothing scheduled.
    input_deadline: Option<Instant>,
    gesture_deadline: Option<Instant>,
    wakeup_deadline: Option<Instant>,
    shutting_down: bool,
}

impl State {
    /// Earliest pending deadline across the three handles.
    fn earliest(&self) -> Option<Instant> {
        [self.input_deadline, self.gesture_deadline, self.wakeup_deadline]
            .into_iter()
            .flatten()
            .min()
    }
}

struct Inner {
    state: Mutex<State>,
    cv: Condvar,
    sink: Arc<dyn RefreshSink>,
}

/// Fan-in of one dfps task's event surface. Cheap to clone (one `Arc`).
#[derive(Clone)]
pub struct DfpsScheduler {
    inner: Arc<Inner>,
}

impl DfpsScheduler {
    /// Build around a parsed table, with the production sink.
    pub fn new(table: RuleTable) -> Self {
        Self::with_sink(table, Arc::new(RealSink))
    }

    /// Build with an arbitrary sink (tests pass [`RecordingSink`]).
    pub fn with_sink(table: RuleTable, sink: Arc<dyn RefreshSink>) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    task: DfpsTask::new(table),
                    input_deadline: None,
                    gesture_deadline: None,
                    wakeup_deadline: None,
                    shutting_down: false,
                }),
                cv: Condvar::new(),
                sink,
            }),
        }
    }

    /// Spawn the timer thread; returns its join handle. Takes `&self` so the
    /// caller keeps using the scheduler.
    pub fn spawn(&self) -> std::thread::JoinHandle<()> {
        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("uperf-dfps".into())
            .spawn(move || timer_loop(inner))
            .expect("spawn dfps timer thread")
    }

    /// Signal the timer thread to exit.
    pub fn stop(&self) {
        let mut s = self.inner.state.lock();
        s.shutting_down = true;
        self.inner.cv.notify_all();
    }

    pub fn cur_hz(&self) -> Option<i32> {
        self.inner.state.lock().task.cur_hz()
    }

    pub fn cur_app(&self) -> String {
        self.inner.state.lock().task.cur_app().to_string()
    }

    // ---- topic handlers (called from the uperf-rs dispatcher thread) ----

    /// `input.touch` — upstream `OnInputTouch` + `OnInput`.
    pub fn on_touch(&self, pressed: bool) {
        let mut s = self.inner.state.lock();
        s.task.set_pressed(Some(pressed), None);
        apply_press(&mut s, &*self.inner.sink);
        drop(s);
        self.inner.cv.notify_all();
    }

    /// `input.btn` — upstream `OnInputBtn` + `OnInput`.
    pub fn on_btn(&self, pressed: bool) {
        let mut s = self.inner.state.lock();
        s.task.set_pressed(None, Some(pressed));
        apply_press(&mut s, &*self.inner.sink);
        drop(s);
        self.inner.cv.notify_all();
    }

    /// `input.state` — upstream `OnInputScene`.
    pub fn on_input_state(&self, gesture: bool) {
        let mut s = self.inner.state.lock();
        if gesture {
            // Gesture start: universal override, active, no idle timer.
            s.input_deadline = None;
            s.task.set_override(UNIVERSAL_PKG);
            s.task.set_active(true);
            tick_with_brightness(&mut s, &*self.inner.sink, false);
        } else {
            let slack = s.task.tunables().gesture_slack_ms;
            s.gesture_deadline = Some(Instant::now() + ms(slack));
        }
        drop(s);
        self.inner.cv.notify_all();
    }

    /// `topapp.pkgName` — upstream `OnTopAppSwitch`.
    pub fn on_top_app(&self, pkg: &str) {
        let mut s = self.inner.state.lock();
        if s.task.set_top_app(pkg) {
            // upstream :259 SwitchRefreshRate(true)
            tick_with_brightness(&mut s, &*self.inner.sink, true);
        }
        drop(s);
        self.inner.cv.notify_all();
    }

    /// `offscreen.state` — upstream `OnOffscreen`.
    pub fn on_offscreen(&self, off: bool) {
        let mut s = self.inner.state.lock();
        if !s.task.set_offscreen_state(off) {
            return;
        }
        if off {
            s.wakeup_deadline = None;
            s.task.set_override(OFFSCREEN_PKG);
            tick_with_brightness(&mut s, &*self.inner.sink, true);
        } else {
            let slack = s.task.tunables().gesture_slack_ms;
            s.wakeup_deadline = Some(Instant::now() + ms(slack));
        }
        drop(s);
        self.inner.cv.notify_all();
    }

    /// Reload `dfps.txt` (called by the uperf-rs inotify watcher).
    pub fn reload(&self, table: RuleTable) {
        let mut s = self.inner.state.lock();
        s.task.reload(table);
        drop(s);
        self.inner.cv.notify_all();
    }

    /// Parse and install a new rule table from raw `dfps.txt` text.
    ///
    /// On a parse error the **current table is kept** and the error is
    /// returned. Upstream throws and the daemon dies; a live refresh-rate
    /// controller must survive a half-written or mistyped config, so this is
    /// deliberately more forgiving than the boot path.
    ///
    /// Returns `(rule_count, universal_rule)` on success.
    pub fn reload_from_text(&self, text: &str) -> Result<(usize, FpsRule), String> {
        let table = crate::dfps_rs::parse_config(text).map_err(|e| e.to_string())?;
        let n = table.rules.len();
        let universal = table.universal;
        self.reload(table);
        Ok((n, universal))
    }

    // ---- timer internals ----

    /// Run every due delayed transition; returns how many fired.
    fn run_due(&self, now: Instant) -> usize {
        let mut s = self.inner.state.lock();
        let mut fired = 0;
        for handle in [Handle::Input, Handle::Gesture, Handle::Wakeup] {
            let slot = deadline_mut(&mut s, handle);
            if !matches!(*slot, Some(d) if d <= now) {
                continue;
            }
            *slot = None;
            match handle {
                Handle::Input => {
                    if !s.task.pressed() {
                        s.task.set_active(false);
                        tick_with_brightness(&mut s, &*self.inner.sink, false);
                        fired += 1;
                    }
                }
                Handle::Gesture => {
                    if s.task.clear_override_if(UNIVERSAL_PKG) {
                        tick_with_brightness(&mut s, &*self.inner.sink, false);
                        fired += 1;
                    }
                }
                Handle::Wakeup => {
                    if s.task.clear_override_if(OFFSCREEN_PKG) {
                        tick_with_brightness(&mut s, &*self.inner.sink, true);
                        fired += 1;
                    }
                }
            }
        }
        fired
    }

    /// Earliest pending deadline, or `None`. Public for the status surface and
    /// tests; the timer loop calls `State::earliest` under its own lock so it
    /// can go straight to the condvar wait without a second acquisition.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.inner.state.lock().earliest()
    }
}

/// Run a tick with upstream's brightness gate: only when the task is idle
/// (not active, not offscreen) does it sample `screen_brightness`, and only
/// every 10 s. Mirrors the `else` branch of `SwitchRefreshRate(bool)`
/// (`dynamic_fps.cpp:291-300`) without moving the I/O into the state machine.
fn tick_with_brightness(s: &mut State, sink: &dyn RefreshSink, force: bool) {
    if !s.task.active() && !s.task.is_offscreen() {
        let now = Instant::now();
        if s.task.needs_brightness_sample(now) {
            let b = sink.screen_brightness();
            s.task.note_brightness_sample(now, b);
        }
    }
    let ev = s.task.tick(force);
    dispatch(sink, ev);
}

/// Upstream `OnInput()`: press -> active now; release -> idle after slack.
fn apply_press(s: &mut State, sink: &dyn RefreshSink) {
    if s.task.pressed() {
        // A new press cancels the pending release timeout
        // (upstream `DwSetWork(dwInput_, nullptr, SLEEP_TS)`).
        s.input_deadline = None;
        s.task.set_active(true);
        tick_with_brightness(s, sink, false);
    } else {
        let slack = s.task.tunables().touch_slack_ms;
        s.input_deadline = Some(Instant::now() + ms(slack));
    }
}

fn dispatch(sink: &dyn RefreshSink, ev: Option<(Option<i32>, i32)>) {
    if let Some((from, to)) = ev {
        sink.on_switch(from, to);
    }
}

fn deadline_mut(s: &mut State, h: Handle) -> &mut Option<Instant> {
    match h {
        Handle::Input => &mut s.input_deadline,
        Handle::Gesture => &mut s.gesture_deadline,
        Handle::Wakeup => &mut s.wakeup_deadline,
    }
}

fn ms(v: i64) -> Duration {
    Duration::from_millis(v.max(0) as u64)
}

fn timer_loop(inner: Arc<Inner>) {
    loop {
        if inner.state.lock().shutting_down {
            return;
        }
        // Run due work first …
        let fired = DfpsScheduler { inner: Arc::clone(&inner) }.run_due(Instant::now());
        if fired > 0 {
            crate::log_msg(&format!("Dfps: {fired} delayed transition(s) applied"));
        }
        // … then compute the wait and sleep on the *same* lock, so an event
        // that arrives between the two cannot be lost.
        let mut s = inner.state.lock();
        let now = Instant::now();
        let next: Option<Instant> = s.earliest();
        let wait = next
            .map(|d| d.saturating_duration_since(now))
            // Nothing pending: park until an event notifies. The cap is a
            // safety net, not a poll interval.
            .unwrap_or(Duration::from_secs(60));
        if !wait.is_zero() {
            inner.cv.wait_for(&mut s, wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> RuleTable {
        RuleTable::parse("* 60 120\n- 30 60\ncom.example.app 90 144\n").unwrap()
    }

    fn sched() -> (DfpsScheduler, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let s = DfpsScheduler::with_sink(table(), sink.clone());
        (s, sink)
    }

    #[test]
    fn press_marks_active_and_release_schedules_idle() {
        let (s, sink) = sched();
        s.on_top_app("com.example.app");
        assert_eq!(s.cur_hz(), Some(90)); // idle of com.example.app

        s.on_touch(true);
        assert_eq!(s.cur_hz(), Some(144)); // active

        s.on_touch(false);
        assert_eq!(s.cur_hz(), Some(144)); // only scheduled, not applied

        // Force the deadline due, then drain.
        s.inner.state.lock().input_deadline = Some(Instant::now() - Duration::from_millis(1));
        assert_eq!(s.run_due(Instant::now()), 1);
        assert_eq!(s.cur_hz(), Some(90));

        let ev = sink.events.lock().clone();
        assert_eq!(ev, vec![(None, 90), (Some(90), 144), (Some(144), 90)]);
    }

    #[test]
    fn repress_cancels_pending_idle() {
        let (s, _) = sched();
        s.on_top_app("com.example.app");
        s.on_touch(true);
        s.on_touch(false); // schedules idle
        assert!(s.inner.state.lock().input_deadline.is_some());
        s.on_touch(true); // must cancel it
        assert!(s.inner.state.lock().input_deadline.is_none());
    }

    #[test]
    fn offscreen_switches_then_wake_restores() {
        let (s, _) = sched();
        s.on_top_app("com.example.app");
        s.on_offscreen(true);
        assert_eq!(s.cur_hz(), Some(60)); // offscreen.active (- 30 60)
        assert!(s.inner.state.lock().wakeup_deadline.is_none());

        s.on_offscreen(false);
        s.inner.state.lock().wakeup_deadline = Some(Instant::now() - Duration::from_millis(1));
        assert_eq!(s.run_due(Instant::now()), 1);
        assert_eq!(s.cur_hz(), Some(90)); // back to the app's idle
        assert_eq!(s.inner.state.lock().task.override_app(), "");
    }

    #[test]
    fn gesture_overrides_to_universal_then_restores() {
        let (s, _) = sched();
        s.on_top_app("com.example.app");
        assert_eq!(s.cur_hz(), Some(90));

        s.on_input_state(true); // gesture start
        assert_eq!(s.cur_hz(), Some(120)); // universal.active (active_ is true)

        s.on_input_state(false); // gesture end -> schedule restore
        s.inner.state.lock().gesture_deadline = Some(Instant::now() - Duration::from_millis(1));
        s.run_due(Instant::now());
        assert_eq!(s.inner.state.lock().task.override_app(), "");
    }

    #[test]
    fn top_app_same_value_is_a_noop() {
        let (s, sink) = sched();
        s.on_top_app("com.example.app");
        let n = sink.events.lock().len();
        s.on_top_app("com.example.app");
        assert_eq!(sink.events.lock().len(), n);
    }

    #[test]
    fn repeated_offscreen_value_is_ignored() {
        let (s, sink) = sched();
        s.on_offscreen(false);
        assert!(sink.events.lock().is_empty());
    }

    #[test]
    fn stale_wake_cannot_clobber_newer_gesture_override() {
        // Upstream guard: clear only if the override is still OFFSCREEN_PKG.
        let (s, _) = sched();
        s.on_offscreen(true); // override = "-"
        s.on_input_state(true); // override = "*" (a newer event)
        // The wake timer fires late; it must not clear the "*" override.
        s.inner.state.lock().wakeup_deadline = Some(Instant::now() - Duration::from_millis(1));
        s.run_due(Instant::now());
        assert_eq!(s.inner.state.lock().task.override_app(), UNIVERSAL_PKG);
    }

    #[test]
    fn dim_screen_keeps_active_rate_when_idle() {
        // Upstream: when not active, a brightness below enableMinBrightness
        // (default 8) makes the idle path emit `active` instead of `idle`.
        let (s, sink) = sched();
        *sink.brightness.lock() = Some(3); // below the default 8
        // Force the sample interval to have elapsed.
        {
            let mut g = s.inner.state.lock();
            g.task
                .note_brightness_sample(Instant::now() - Duration::from_secs(60), Some(255));
        }
        s.on_top_app("com.example.app");
        // Idle path, but dim -> the active Hz (144), not the idle 90.
        assert_eq!(s.cur_hz(), Some(144));
        assert!(s.inner.state.lock().task.low_brightness());
    }

    #[test]
    fn bright_screen_uses_idle_rate() {
        let (s, sink) = sched();
        *sink.brightness.lock() = Some(200);
        {
            let mut g = s.inner.state.lock();
            g.task
                .note_brightness_sample(Instant::now() - Duration::from_secs(60), Some(255));
        }
        s.on_top_app("com.example.app");
        assert_eq!(s.cur_hz(), Some(90));
        assert!(!s.inner.state.lock().task.low_brightness());
    }

    #[test]
    fn reload_from_text_installs_a_new_table() {
        let (s, _) = sched();
        s.on_top_app("com.example.app");
        assert_eq!(s.cur_hz(), Some(90)); // old table: idle 90

        let (n, universal) = s
            .reload_from_text("* 0 240\n- 0 240\ncom.example.app 30 240\n")
            .expect("parses");
        assert_eq!(n, 1);
        assert_eq!(universal, FpsRule { idle: 0, active: 240 });
        // The new table's idle for the same app.
        s.on_top_app("com.example.other");
        assert_eq!(s.cur_hz(), Some(0));
    }

    #[test]
    fn reload_from_text_keeps_the_old_table_on_a_bad_edit() {
        let (s, _) = sched();
        s.on_top_app("com.example.app");
        assert_eq!(s.cur_hz(), Some(90));

        // Missing the offscreen rule -> ParseError::NoOffscreen.
        let err = s
            .reload_from_text("* 0 240\ncom.example.app 30 240\n")
            .unwrap_err();
        assert!(err.contains("offscreen"), "got: {err}");

        // The old table is still in force.
        s.on_top_app("com.example.other");
        assert_eq!(s.cur_hz(), Some(60)); // old universal idle
        s.on_top_app("com.example.app");
        assert_eq!(s.cur_hz(), Some(90)); // old per-app idle
    }
}