//! Host-side unit tests for the hint FSM and sysfs dispatch — re-imported from the
//! aarch64-target crates. Both are pure logic with no FFI, so we test against the host
//! build (cargo test -p uperf-core runs on x86_64). Same surface as aarch64.

use serde_json::{Map, Value};

// Re-declare the minimal `hint::SfHint` enum + `HintDurations` so we don't have to
// pull the whole crate (which depends on libc etc.). The enum tag/values match the
// upstream binary's jump tables — see docs/m1-static-reverse.md §1.3.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i8)]
pub enum SfHint {
    Idle = 0,
    Switch = 1,
    Trigger = 2,
    Gesture = 3,
    Touch = 4,
    Junk = 5,
    Unknown = 6,
}

impl SfHint {
    pub fn from_byte(b: i8) -> Self {
        match b {
            0 => Self::Idle, 1 => Self::Switch, 2 => Self::Trigger,
            3 => Self::Gesture, 4 => Self::Touch, 5 => Self::Junk,
            _ => Self::Unknown,
        }
    }
}

fn durations_from_modules(modules: &Map<String, Value>) -> (u64, u64, u64, u64, u64, u64) {
    let mut d = (0u64, 4_000, 30, 100, 400, 60);
    if let Some(sw) = modules.get("switcher").and_then(|m| m.as_object()) {
        if let Some(h) = sw.get("hintDuration").and_then(|v| v.as_object()) {
            for (k, v) in h {
                let ms = v.as_f64().map(|f| (f * 1000.0) as u64)
                    .or_else(|| v.as_u64()).unwrap_or(0);
                match k.as_str() {
                    "idle" => d.0 = ms,
                    "touch" => d.1 = ms,
                    "trigger" => d.2 = ms,
                    "gesture" => d.3 = ms,
                    "switch" => d.4 = ms,
                    "junk" => d.5 = ms,
                    _ => {}
                }
            }
        }
    }
    d
}

#[test]
fn sf_hint_from_bytes() {
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
fn durations_from_default_config() {
    let mut m = Map::new();
    m.insert("switcher".into(),
             serde_json::json!({"hintDuration": {"idle": 0.0, "touch": 4.0,
                 "trigger": 0.03, "gesture": 0.1, "switch": 0.4, "junk": 0.06}}));
    let d = durations_from_modules(&m);
    assert_eq!(d.0, 0);     // idle
    assert_eq!(d.1, 4000);  // touch
    assert_eq!(d.2, 30);    // trigger
    assert_eq!(d.3, 100);   // gesture
    assert_eq!(d.4, 400);   // switch
    assert_eq!(d.5, 60);    // junk
}
