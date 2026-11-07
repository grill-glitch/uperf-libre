//! Hint state machine — translates (cgroup_top_app pidlist, input events, sf hint)
//! into `HintState` transitions + emits preset/scene change commands.
//!
//! AGENT.md §8.1 mermaid spec; SfHint enum values 0..5 derived in
//! `docs/records/m1-static-reverse.md` §1.3 from the upstream v3 binary's two jump
//! tables (one for old_hint, one for new_hint). Both tables index the SAME
//! 6 string-construction cases, so the mapping is:
//!
//! | SfHint (i8) | name       | what it means (upstream code path)         |
//! |-------------|------------|--------------------------------------------|
//! | 0           | idle       | no hint; switcher may apply '*' defaults    |
//! | 1           | switch     | window switch (animated / window change)   |
//! | 2           | trigger    | finger up / start-of-slide                  |
//! | 3           | gesture    | fullscreen gesture detected                 |
//! | 4           | touch      | finger down / hold                           |
//! | 5           | junk       | drop frame (sfanalysis hint                 |
//! | 6+          | unknown    | error / out-of-range; treat as idle          |
//!
//! `durations[scene]` is `cfg.switcher.hintDuration[scene]` (ms).


use serde_json::{Map, Value};
use std::time::{Duration, Instant};

/// The 6 SfHint values from upstream, ordered by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i8)]
pub enum SfHint {
    Idle = 0,
    Switch = 1,
    Trigger = 2,
    Gesture = 3,
    Touch = 4,
    Junk = 5,
    /// any value >= 6
    Unknown = 6,
}

impl SfHint {
    pub fn from_byte(b: i8) -> Self {
        match b {
            0 => Self::Idle,
            1 => Self::Switch,
            2 => Self::Trigger,
            3 => Self::Gesture,
            4 => Self::Touch,
            5 => Self::Junk,
            _ => Self::Unknown,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Switch => "switch",
            Self::Trigger => "trigger",
            Self::Gesture => "gesture",
            Self::Touch => "touch",
            Self::Junk => "junk",
            Self::Unknown => "unknown",
        }
    }
}

/// Hint duration source: looks up `modules.switcher.hintDuration.<scene>` (ms).
#[derive(Debug, Clone)]
pub struct HintDurations {
    pub idle_ms: u64,
    pub touch_ms: u64,
    pub trigger_ms: u64,
    pub gesture_ms: u64,
    pub switch_ms: u64,
    pub junk_ms: u64,
}

impl Default for HintDurations {
    fn default() -> Self {
        Self { idle_ms: 0, touch_ms: 4000, trigger_ms: 30, gesture_ms: 100, switch_ms: 400, junk_ms: 60 }
    }
}

impl HintDurations {
    pub fn from_modules(modules: &Map<String, Value>) -> Self {
        // Build on the defaults instead of repeating them: two copies of the numbers would
        // drift, and `HintDurations::default()` is what the no-config path uses.
        let mut d = Self::default();
        let mut junk_explicit: Option<u64> = None;
        let mut swjunk_ms: Option<u64> = None;
        if let Some(sw) = modules.get("switcher").and_then(|m| m.as_object()) {
            if let Some(h) = sw.get("hintDuration").and_then(|d| d.as_object()) {
                for (k, v) in h {
                    // The values are *seconds* (`"touch": 4.0` == 4 s) — including the
                    // integer spellings (`"touch": 4`), so `as_f64` is the only conversion
                    // needed. A missing/non-numeric/negative entry keeps the default instead
                    // of collapsing the timeout to 0.
                    let Some(secs) = v.as_f64().filter(|f| f.is_finite() && *f >= 0.0) else {
                        continue;
                    };
                    let ms = (secs * 1000.0).round() as u64;
                    match k.as_str() {
                        "idle" => d.idle_ms = ms,
                        "touch" => d.touch_ms = ms,
                        "trigger" => d.trigger_ms = ms,
                        "gesture" => d.gesture_ms = ms,
                        "switch" => d.switch_ms = ms,
                        "junk" => junk_explicit = Some(ms),
                        // UGT-specific extra (m1-static-reverse §1.1 strings). Merged with
                        // `junk` deterministically after the loop: `min` applied inside the
                        // loop made the result depend on the map's iteration order.
                        "swjunk" => swjunk_ms = Some(ms.max(1)),
                        _ => {}
                    }
                }
            }
        }
        d.junk_ms = match (junk_explicit, swjunk_ms) {
            (Some(j), Some(s)) => j.min(s),
            (Some(j), None) => j,
            (None, Some(s)) => s,
            (None, None) => d.junk_ms,
        };
        d
    }
    pub fn for_hint(&self, h: SfHint) -> Duration {
        Duration::from_millis(match h {
            SfHint::Idle => self.idle_ms,
            SfHint::Touch => self.touch_ms,
            SfHint::Trigger => self.trigger_ms,
            SfHint::Gesture => self.gesture_ms,
            SfHint::Switch => self.switch_ms,
            SfHint::Junk => self.junk_ms,
            // An out-of-range vendor hint is "idle" upstream (see the module header and
            // `Orchestrator::scene_for_hint`), not a touch that expires after 4 s.
            SfHint::Unknown => self.idle_ms,
        })
    }
}

/// State machine: tracks the currently bound hint + how long it's been active.
#[derive(Debug)]
pub struct HintState {
    current: SfHint,
    bound_at: Instant,
    durations: HintDurations,
    /// Number of times we've transitioned in `process_event`. For parity logging.
    transitions: u64,
}

impl HintState {
    pub fn new(durations: HintDurations) -> Self {
        Self {
            current: SfHint::Idle,
            bound_at: Instant::now(),
            durations,
            transitions: 0,
        }
    }
    pub fn current(&self) -> SfHint {
        self.current
    }
    pub fn transitions(&self) -> u64 {
        self.transitions
    }

    /// Apply an inbound SfHint. If the new hint is the same kind AND we're still
    /// within its duration, the timer is extended (touch refresh). If it differs,
    /// the timer is reset to the new hint's duration. The exact edge semantics are
    /// documented in AGENT.md §8.1.
    pub fn process(&mut self, incoming: SfHint) -> Option<HintTransition> {
        if incoming == self.current {
            // Same hint = a refresh, not a transition: counting it here inflated the
            // counter that parity logging compares against emitted transitions.
            self.bound_at = Instant::now();
            return None;
        }
        self.transitions += 1;
        let from = self.current;
        let to = incoming;
        self.current = incoming;
        self.bound_at = Instant::now();
        Some(HintTransition { from, to })
    }

    /// Has the current hint expired? Caller polls this and triggers a re-bind on
    /// idle/scene transition.
    pub fn expired(&self) -> bool {
        let dur = match self.current {
            // Idle is unbounded by default (idle_ms == 0 means "no expiry" upstream); an
            // explicit idle duration still applies. An out-of-range hint is idle upstream,
            // so it follows the same rule instead of expiring after the touch timeout.
            SfHint::Idle | SfHint::Unknown if self.durations.idle_ms == 0 => return false,
            _ => self.durations.for_hint(self.current),
        };
        Instant::now()
            .checked_duration_since(self.bound_at)
            .map(|d| d >= dur)
            .unwrap_or(false)
    }
}

/// One observed transition in the hint FSM. The orchestrator (the Switcher in
/// upstream terms) logs these and propagates them to the Sysfs writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HintTransition {
    pub from: SfHint,
    pub to: SfHint,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn durations() -> HintDurations {
        let mut m = Map::new();
        m.insert(
            "switcher".into(),
            json!({
                "hintDuration": {
                    "idle": 0.0, "touch": 4.0, "trigger": 0.03, "gesture": 0.1,
                    "switch": 0.4, "junk": 0.06
                }
            }),
        );
        HintDurations::from_modules(&m)
    }

    #[test]
    fn enum_mapping_matches_upstream() {
        // The SfHint enum value 0..5 mapping was extracted from upstream's two
        // parallel jump tables in docs/records/m1-static-reverse.md §1.3.
        assert_eq!(SfHint::from_byte(0), SfHint::Idle);
        assert_eq!(SfHint::from_byte(1), SfHint::Switch);
        assert_eq!(SfHint::from_byte(2), SfHint::Trigger);
        assert_eq!(SfHint::from_byte(3), SfHint::Gesture);
        assert_eq!(SfHint::from_byte(4), SfHint::Touch);
        assert_eq!(SfHint::from_byte(5), SfHint::Junk);
        assert_eq!(SfHint::from_byte(6), SfHint::Unknown);
        assert_eq!(SfHint::from_byte(-1), SfHint::Unknown);
    }

    #[test]
    fn same_hint_refreshes_timer_only() {
        // Idle -> Touch IS a transition (returning Some(HintTransition)).
        let mut s = HintState::new(durations());
        let t = s.process(SfHint::Touch);
        assert!(t.is_some(), "Idle -> Touch should be a transition");
        let t1 = s.bound_at;
        let t = s.process(SfHint::Touch);
        // A repeat of the same hint yields no transition (timer refresh).
        assert!(t.is_none(), "Touch -> Touch should be a refresh, no transition");
        assert!(s.bound_at >= t1);
    }

    #[test]
    fn different_hint_yields_transition() {
        let mut s = HintState::new(durations());
        let _ = s.process(SfHint::Touch);
        let t = s.process(SfHint::Junk);
        assert_eq!(t, Some(HintTransition { from: SfHint::Touch, to: SfHint::Junk }));
        assert_eq!(s.transitions(), 2);
    }

    #[test]
    fn idle_does_not_expire() {
        let s = HintState::new(durations());
        assert!(!s.expired());
    }

    #[test]
    fn touch_with_zero_duration_expires_immediately() {
        let mut d = durations();
        d.touch_ms = 0;
        let mut s = HintState::new(d);
        let _ = s.process(SfHint::Touch);
        assert!(s.expired());
    }
}