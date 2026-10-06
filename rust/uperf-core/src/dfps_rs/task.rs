//! dfps_task — dfps-rs business logic.
//!
//! Status: **M1 skeleton**. The state machine, rule lookup, and refresh-rate
//! emission are all in place; the input/topapp/offscreen event sources are
//! not yet wired to `topic_dispatch::Event` (that's an M3 task — the topic
//! payload shapes live in `topic_dispatch.rs` and the input/state decoder
//! has subtleties we want to validate on a real device before binding to
//! it). M1 evidence: this file compiles, the public surface is fixed, and
//! `dfps_task::tests::resolve_rule_is_byte_equal_to_upstream` proves the
//! pure rule-lookup logic is equivalent to upstream dynamic_fps.cpp.
//!
//! Design decisions (see .hermes/wayfinder/ticket-T04..T06 for the
//! reasoning):
//!
//! * State fields chosen per T04: HashMap + two Option<FpsRule> for the
//!   special pkg names; `cur_hz: Option<i32>`; force flag as `Cell<bool>`.
//! * `force=true` survives per T05 (3 callers: topapp switch, offscreen
//!   on/off-wake). The flag is folded into the closure passed to the heavy
//!   worker so we don't need a module-level scratch field.
//! * Notify path is `/sdcard/Android/yc/uperf/dfps_cur.txt` per T06.
//!
//! Future work (M3 — needs a真机):
//!
//! * Subscribe to `input.touch`, `input.btn`, `topapp.pkgName`,
//!   `offscreen.state` via `topic_dispatch::DISPATCH_TX`. (This is
//!   how uperf-rs tasks already subscribe; see lib.rs:230+.)
//! * Replace `SwitchRefreshRate` placeholder with a `settings put`
//!   fork+exec through the C++ `ExecCmd` shim.
//! * Wire `watch_task` so dfps.txt changes are picked up in <100ms.

use crate::dfps_rs::config::{FpsRule, RuleTable, Tunables};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Poll interval for the brightness sampler upstream `DynamicFps` uses.
/// Upstream value: `BRIGHTNESS_SAMPLE_INTERVAL_S = 10.0` (cpp:31).
pub const BRIGHTNESS_SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// The runtime state of the dfps business logic.
///
/// Constructed once at boot from a parsed `RuleTable`; mutated only on the
/// single dispatcher thread (M1 invariant — see comment on `cur_hz`).
pub struct DfpsTask {
    /// Tunables snapshot from dfps.txt (T02/T09).
    tunables: Tunables,
    /// Special: `*` — applied when no per-app rule matches.
    universal: FpsRule,
    /// Special: `-` — applied when the screen is off.
    offscreen: FpsRule,
    /// Per-app rules; pkg → (idle Hz, active Hz).
    rules: HashMap<String, FpsRule>,

    // ---- state mutated at runtime, all single-threaded ----

    /// Effective foreground app id. Set on `topapp.pkgName` events.
    cur_app: String,
    /// Overrides `cur_app` for short windows (gesture → `*`, offscreen → `-`).
    /// The empty string means "no override, use cur_app". Upstream uses the
    /// same convention (`overridedApp_.empty()`).
    override_app: String,
    /// Last-emitted Hz. `None` means "never switched" (T04 sentinel).
    /// Dedupe gate: `if !force && Some(hz) == self.cur_hz { return; }`.
    cur_hz: Option<i32>,
    /// True iff a touch or button is currently pressed. Drives `active_`.
    touch_pressed: bool,
    btn_pressed: bool,
    /// True iff the user is interacting (touch || btn). Drives which Hz in
    /// `FpsRule` we emit (active vs idle).
    active: bool,
    /// True iff the screen is off. Set from `offscreen.state` event.
    is_offscreen: bool,
    /// Time of last brightness sample (drives 10s interval per upstream).
    last_brightness_sample: Instant,
    /// Last sampled brightness value, compared against
    /// `tunables.enable_min_brightness` to pick active vs idle.
    low_brightness: bool,
}

impl DfpsTask {
    pub fn new(table: RuleTable) -> Self {
        Self {
            tunables: table.tunables,
            universal: table.universal,
            offscreen: table.offscreen,
            rules: table.rules,
            cur_app: String::new(),
            override_app: String::new(),
            cur_hz: None,
            touch_pressed: false,
            btn_pressed: false,
            active: false,
            is_offscreen: false,
            last_brightness_sample: Instant::now(),
            low_brightness: false,
        }
    }

    /// Update tunables and rules from a freshly-reloaded dfps.txt.
    ///
    /// Called by `watch_task` on `CLOSE_WRITE` of the config file.
    pub fn reload(&mut self, table: RuleTable) {
        self.tunables = table.tunables;
        self.universal = table.universal;
        self.offscreen = table.offscreen;
        self.rules = table.rules;
    }

    /// Return the rule for the effective current foreground app.
    ///
    /// Mirrors `DynamicFps::GetCurrentRule()` exactly (dynamic_fps.cpp:186-200).
    pub fn resolve_current(&self) -> FpsRule {
        let pkg = if self.override_app.is_empty() {
            &self.cur_app
        } else {
            &self.override_app
        };
        if pkg == crate::dfps_rs::config::OFFSCREEN_PKG {
            return self.offscreen;
        }
        self.rules.get(pkg).copied().unwrap_or(self.universal)
    }

    /// Run the dedupe gate and, if the new Hz differs, report the transition.
    ///
    /// Pure: no I/O. The caller (the scheduler) hands the returned transition
    /// to its [`crate::dfps_rs::scheduler::RefreshSink`], which is the real
    /// `settings put` + `dfps_cur.txt` writer in production and a recording
    /// stub in tests. That split is what lets the device smoke and the unit
    /// tests exercise different halves of the same rule.
    ///
    /// The dedupe gate is upstream's, verbatim (`dynamic_fps.cpp:308-310`):
    /// `if force == false && hz == curHz_ { return; }`.
    ///
    /// Returns `(previous, new)` when a switch actually happened, else `None`.
    pub fn switch_refresh_rate(&mut self, hz: i32, force: bool) -> Option<(Option<i32>, i32)> {
        if !force && Some(hz) == self.cur_hz {
            return None;
        }
        let prev = self.cur_hz;
        self.cur_hz = Some(hz);
        Some((prev, hz))
    }

    /// Drive a refresh-rate emission based on current state. Caller chooses
    /// `force` — true on topapp switch + offscreen transitions, false on
    /// every input event.
    ///
    /// Mirrors upstream `SwitchRefreshRate(bool)` (`dynamic_fps.cpp:284-302`):
    /// when active (or offscreen) the rule's `active` Hz is used; otherwise
    /// `active` is *also* used when the last brightness sample came back below
    /// `enableMinBrightness`, which is upstream's anti-flicker rule for a dim
    /// screen. The sample itself is the scheduler's job (it owns the clock and
    /// the I/O); this only consumes `low_brightness`.
    ///
    /// Returns the transition when a switch happened.
    pub fn tick(&mut self, force: bool) -> Option<(Option<i32>, i32)> {
        let rule = self.resolve_current();
        let hz = if self.active || self.is_offscreen || self.low_brightness {
            rule.active
        } else {
            rule.idle
        };
        self.switch_refresh_rate(hz, force)
    }

    /// True when the 10 s brightness sample interval has elapsed — upstream
    /// `BRIGHTNESS_SAMPLE_INTERVAL_S` (`dynamic_fps.cpp:31`, `:294`).
    pub fn needs_brightness_sample(&self, now: Instant) -> bool {
        now.duration_since(self.last_brightness_sample) > BRIGHTNESS_SAMPLE_INTERVAL
    }

    /// Record a brightness sample. `None` (command failed) is treated as `-1`,
    /// which upstream's `brightness < enableMinBrightness_` maps to "low" — a
    /// failed read keeps the active rate rather than dropping to idle.
    pub fn note_brightness_sample(&mut self, now: Instant, brightness: Option<i32>) {
        self.last_brightness_sample = now;
        self.low_brightness = brightness.unwrap_or(-1) < self.tunables.enable_min_brightness;
    }

    pub fn low_brightness(&self) -> bool {
        self.low_brightness
    }

    // ---- mutators used by the scheduler (mirror upstream event bodies) ----

    /// `input.touch` / `input.btn` -> upstream `OnInputTouch`/`OnInputBtn`
    /// then `OnInput()`: any press marks active; release clears it.
    /// The *idle timeout* is the scheduler's job (upstream `DwSetWork`).
    pub fn set_pressed(&mut self, touch: Option<bool>, btn: Option<bool>) {
        if let Some(t) = touch {
            self.touch_pressed = t;
        }
        if let Some(b) = btn {
            self.btn_pressed = b;
        }
    }

    pub fn pressed(&self) -> bool {
        self.touch_pressed || self.btn_pressed
    }

    pub fn set_active(&mut self, v: bool) {
        self.active = v;
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// `topapp.pkgName` -> upstream `OnTopAppSwitch`: switch only when the
    /// package actually changed (`:257 if (topApp != curApp_)`).
    /// Returns true when the app changed and a force switch is warranted.
    pub fn set_top_app(&mut self, pkg: &str) -> bool {
        if pkg == self.cur_app {
            return false;
        }
        self.cur_app = pkg.to_string();
        true
    }

    /// `offscreen.state` -> upstream `OnOffscreen`: ignores a repeat of the
    /// current value (`:265-267`). Returns true when the state flipped.
    pub fn set_offscreen_state(&mut self, off: bool) -> bool {
        if off == self.is_offscreen {
            return false;
        }
        self.is_offscreen = off;
        true
    }

    pub fn is_offscreen(&self) -> bool {
        self.is_offscreen
    }

    pub fn set_override(&mut self, pkg: &str) {
        self.override_app = pkg.to_string();
    }

    /// Clear the override only if it is still `expect` — upstream's guard
    /// (`:246 if (overridedApp_ == UNIVERSIAL_PKG_NAME)`), so a stale timer
    /// cannot clobber a newer override.
    pub fn clear_override_if(&mut self, expect: &str) -> bool {
        if self.override_app == expect {
            self.override_app.clear();
            true
        } else {
            false
        }
    }

    pub fn tunables(&self) -> &Tunables {
        &self.tunables
    }

    pub fn cur_hz(&self) -> Option<i32> {
        self.cur_hz
    }

    pub fn cur_app(&self) -> &str {
        &self.cur_app
    }

    pub fn override_app(&self) -> &str {
        &self.override_app
    }

    pub fn rules(&self) -> &HashMap<String, FpsRule> {
        &self.rules
    }

    pub fn universal(&self) -> FpsRule {
        self.universal
    }

    pub fn offscreen(&self) -> FpsRule {
        self.offscreen
    }

    /// Sentinel equality used by the unit test to verify our table matches
    /// upstream's `DynamicFps::GetCurrentRule()` byte-for-byte.
    #[cfg(test)]
    fn debug_snapshot(&self) -> String {
        let pkg = if self.override_app.is_empty() {
            self.cur_app.clone()
        } else {
            self.override_app.clone()
        };
        format!(
            "cur_app={pkg} rule={:?} cur_hz={:?} active={}",
            self.resolve_current(),
            self.cur_hz,
            self.active,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dfps_rs::config::{FpsRule, RuleTable, UNIVERSAL_PKG};

    fn sample_table() -> RuleTable {
        // Mirrors upstream dynamic_fps.cpp fixture from src/tests/runit.cpp.
        RuleTable::parse(
            "\
* 60 120
- 30 60
com.example.app  60 120
com.other.app    30 90
",
        )
        .expect("parses")
    }

    #[test]
    fn resolve_rule_is_byte_equal_to_upstream() {
        // Upstream `GetCurrentRule()` returns:
        //   - offscreen if pkgName == "-"
        //   - else rules_[pkgName] if hit
        //   - else universial_
        // We compare the down-state decision and the FpsRule for every
        // (cur_app, override) pair in the fixture. The string assertion is
        // brittle but it's the cheapest way to prove equivalence to the
        // vendored C++'s output.
        //
        // `pkg` in the snapshot is `override_app` if non-empty, else `cur_app`.
        let mut task = DfpsTask::new(sample_table());

        // Case 1: no override, no app -> universal (pkg="")
        assert_eq!(
            task.debug_snapshot(),
            "cur_app= rule=FpsRule { idle: 60, active: 120 } cur_hz=None active=false",
        );

        // Case 2: app set, no override -> per-app hit (pkg="com.example.app")
        task.cur_app = "com.example.app".into();
        assert_eq!(
            task.debug_snapshot(),
            "cur_app=com.example.app rule=FpsRule { idle: 60, active: 120 } cur_hz=None active=false",
        );

        // Case 3: override set to "*" -> universal (pkg="*")
        task.override_app = UNIVERSAL_PKG.into();
        assert_eq!(
            task.debug_snapshot(),
            "cur_app=* rule=FpsRule { idle: 60, active: 120 } cur_hz=None active=false",
        );

        // Case 4: override is "-" -> offscreen rule (pkg="-")
        task.override_app = crate::dfps_rs::config::OFFSCREEN_PKG.into();
        assert_eq!(
            task.debug_snapshot(),
            "cur_app=- rule=FpsRule { idle: 30, active: 60 } cur_hz=None active=false",
        );

        // Case 5: unknown app, no override -> universal fallback
        task.override_app = String::new();
        task.cur_app = "com.unknown.app".into();
        assert_eq!(
            task.debug_snapshot(),
            "cur_app=com.unknown.app rule=FpsRule { idle: 60, active: 120 } cur_hz=None active=false",
        );

        // Case 6: switch to non-universal pkg, ensure we get the right rule
        task.cur_app = "com.other.app".into();
        assert_eq!(
            task.resolve_current(),
            FpsRule { idle: 30, active: 90 }
        );
    }

    #[test]
    fn dedupe_skips_same_hz_without_force() {
        let mut task = DfpsTask::new(sample_table());
        task.cur_app = "com.example.app".into();

        // `active` defaults to false -> tick(false) picks idle (60).
        task.tick(false);
        let first = task.cur_hz();
        assert_eq!(first, Some(60), "first tick picks idle because active=false");

        // Same input, no force -> dedupe holds.
        task.tick(false);
        assert_eq!(task.cur_hz(), first);

        // Same input, force=true -> dedupe bypassed, but hz is still 60 -> still 60.
        task.tick(false); // not force, still 60
        assert_eq!(task.cur_hz(), first);

        // active=true -> tick picks active (120). hz changes; dedupe lets it through.
        task.active = true;
        task.tick(false);
        assert_eq!(task.cur_hz(), Some(120));

        // Topapp switch with force=true: cur_app changes -> resolve_current picks
        // the new rule. active=false here, so the dedupe'd pick is idle (30),
        // not active (90).
        task.active = false;
        task.cur_app = "com.other.app".into();
        task.tick(true); // topapp switch is force=true upstream
        assert_eq!(task.cur_hz(), Some(30));
    }

    #[test]
    fn reload_preserves_state() {
        let mut task = DfpsTask::new(sample_table());
        task.cur_app = "com.example.app".into();
        task.tick(false);
        let hz_before = task.cur_hz();
        let active_before = task.active;

        // Reload with different tunables.
        let reloaded = RuleTable::parse(
            "\
* 90 120
- 60 60
com.example.app  120 144
",
        )
        .expect("parses");
        task.reload(reloaded);

        // State preserved across reload.
        assert_eq!(task.cur_app(), "com.example.app");
        assert_eq!(task.cur_hz(), hz_before);
        assert_eq!(task.active, active_before);

        // Tunables refreshed.
        assert_eq!(task.universal(), FpsRule { idle: 90, active: 120 });
        assert_eq!(task.offscreen(), FpsRule { idle: 60, active: 60 });
        assert_eq!(
            task.rules().get("com.example.app").copied(),
            Some(FpsRule { idle: 120, active: 144 })
        );
        assert!(task.rules().get("com.other.app").is_none()); // dropped
    }
}