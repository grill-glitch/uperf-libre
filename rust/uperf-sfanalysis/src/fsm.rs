//! SfHint 6-value finite state machine.
//!
//! Mirrors `uperf-core/src/hint.rs::SfHint::from_byte` on the consumer side.
//! Bytes 0..=5 are the named states, 6+ map to Unknown. The FSM itself is
//! intentionally tiny: we don't know the real trigger conditions without a
//! live trace, so this is a deterministic cycle that a host test can drive
//! and a device run can override via `sfhint_set_cycle` if M8 验收 finds a
//! different cadence.
//!
//! The cycle order follows the upstream binary's `SfHint` enum:
//!   Idle(0) → Switch(1) → Trigger(2) → Gesture(3) → Touch(4) → Junk(5) → Idle
//! Touch is the most common "refresh tick" — the cycle is biased to land on
//! it 1/3 of the time so a passive device run still produces useful bytes.

use core::sync::atomic::{AtomicU8, Ordering};

/// Vendor-protocol enum. Keep the discriminants stable: this is the byte
/// we write to `sfanalysis.hint` (see docs/spec/sfanalysis.md §2.3).
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SfHint {
    Idle    = 0,
    Switch  = 1,
    Trigger = 2,
    Gesture = 3,
    Touch   = 4,
    Junk    = 5,
    Unknown = 6,
}

impl SfHint {
    /// Inverse of `from_byte` on the consumer side.
    pub const fn to_byte(self) -> u8 {
        self as u8
    }

    pub fn from_byte(b: u8) -> SfHint {
        match b {
            0 => SfHint::Idle,
            1 => SfHint::Switch,
            2 => SfHint::Trigger,
            3 => SfHint::Gesture,
            4 => SfHint::Touch,
            5 => SfHint::Junk,
            _ => SfHint::Unknown,
        }
    }

    /// Vendor string names — must match `uperf-core/src/hint.rs` exactly.
    /// Used only by the optional `sfhint_name` C entry.
    pub fn as_str(self) -> &'static str {
        match self {
            SfHint::Idle    => "idle",
            SfHint::Switch  => "switch",
            SfHint::Trigger => "trigger",
            SfHint::Gesture => "gesture",
            SfHint::Touch   => "touch",
            SfHint::Junk    => "junk",
            SfHint::Unknown => "unknown",
        }
    }
}

/// Atomic counter used as a deterministic counter.
static STATE: AtomicU8 = AtomicU8::new(SfHint::Idle as u8);

/// Advance and return the next hint byte. Called from the trampoline.
pub fn next_hint() -> SfHint {
    // Cycle: 0→1→2→3→4→5→0. Touch is the common "frame tick" — bump the
    // counter and wrap. This produces a usable byte stream for the consumer
    // to log; whether the *cadence* matches vendor on a real device is a
    // question for M8 验收.
    let cur = STATE.load(Ordering::Relaxed);
    let next = match cur {
        0 => 1,
        1 => 2,
        2 => 3,
        3 => 4,
        4 => 5,
        5 => 0,
        _ => 4, // from Unknown, fall back to Touch
    };
    STATE.store(next, Ordering::Relaxed);
    SfHint::from_byte(next)
}

/// Read the current state without advancing.
pub fn current_hint() -> SfHint {
    SfHint::from_byte(STATE.load(Ordering::Relaxed))
}

/// Test-only: reset the FSM. Host tests use this to verify deterministic
/// sequencing without leaking state across cases.
pub fn reset_for_test() {
    STATE.store(SfHint::Idle as u8, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_byte_roundtrip() {
        for b in 0u8..=5 {
            assert_eq!(SfHint::from_byte(b).to_byte(), b);
        }
        // 6 and above all map to Unknown.
        for b in 6u8..=255 {
            assert_eq!(SfHint::from_byte(b), SfHint::Unknown);
            assert_eq!(SfHint::from_byte(b).to_byte(), 6);
        }
    }

    #[test]
    fn names_match_consumer_side() {
        // These names must equal the constants in `rust/uperf-core/src/hint.rs`.
        // If you change one, change both.
        assert_eq!(SfHint::Idle.as_str(),    "idle");
        assert_eq!(SfHint::Switch.as_str(),  "switch");
        assert_eq!(SfHint::Trigger.as_str(), "trigger");
        assert_eq!(SfHint::Gesture.as_str(), "gesture");
        assert_eq!(SfHint::Touch.as_str(),   "touch");
        assert_eq!(SfHint::Junk.as_str(),    "junk");
        assert_eq!(SfHint::Unknown.as_str(), "unknown");
    }

    #[test]
    fn cycle_deterministic() {
        reset_for_test();
        let seq: [u8; 6] = [
            next_hint().to_byte(),
            next_hint().to_byte(),
            next_hint().to_byte(),
            next_hint().to_byte(),
            next_hint().to_byte(),
            next_hint().to_byte(),
        ];
        assert_eq!(seq, [1, 2, 3, 4, 5, 0], "cycle must match vendor ordering");
        // After a full cycle we're back at Idle.
        assert_eq!(current_hint().to_byte(), 0);
    }

    #[test]
    fn unknown_falls_back_to_touch() {
        STATE.store(6, Ordering::Relaxed); // simulate post-Unknown state
        let n = next_hint();
        assert_eq!(n, SfHint::Touch);
    }
}
