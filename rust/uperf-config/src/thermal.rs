//! Thermal feedback onto the power budget (AGENT.md §11 queue item ⑥).
//!
//! Borrowed from fas-rs's `core_temp_thresh`: when the SoC runs hot, ease the *budget*
//! rather than the frequency points. The lever here is **PL1** (`slow_limit_power`),
//! the sustained power envelope the governor caps the CPU with; the burst allowance
//! (PL2) and the OPP targets are deliberately left alone, which is the "先削 PL1 不先削
//! 频点" rule.
//!
//! Scaling PL1 bites in two places in the governor: the short-term pool drains faster
//! once `est_power` is over the (now smaller) PL1, and the hard power cap that runs
//! after `guideCap`/`limitEfficiency` uses the same value — so no frequency *point* is
//! ever selected by temperature.
//!
//! The temperature read is device-only (`/sys/class/thermal`); everything else is pure
//! and host-tested.

use std::path::{Path, PathBuf};

/// A zone whose `type` contains this is a CPU temperature. Both families on this
/// device are live silicon sensors: `cpu-0-0-usr` (the per-core user-visible ones)
/// and `cpu-0-0-step` (the ones the thermal engine's step logic uses).
pub const DEFAULT_ZONE_MATCH: &str = "cpu-";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermalPolicy {
    /// At or below this, PL1 is untouched.
    pub thresh_c: f64,
    /// Degrees above the threshold over which the budget ramps down to `floor`.
    pub span_c: f64,
    /// PL1 is never scaled below this fraction of the configured value.
    pub floor: f64,
}

impl Default for ThermalPolicy {
    fn default() -> Self {
        Self {
            thresh_c: 60.0,
            span_c: 20.0,
            floor: 0.5,
        }
    }
}

impl ThermalPolicy {
    /// Overridable so the numbers are not guesses baked into the binary.
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            thresh_c: env_f64("UPERF_THERMAL_THRESH_C", d.thresh_c),
            span_c: env_f64("UPERF_THERMAL_SPAN_C", d.span_c).max(0.1),
            floor: env_f64("UPERF_THERMAL_FLOOR", d.floor).clamp(0.05, 1.0),
        }
    }
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key).ok().and_then(|s| s.parse::<f64>().ok()).unwrap_or(default)
}

/// PL1 multiplier for a temperature: exactly `1.0` at or below the threshold, ramping
/// linearly to `floor` at `thresh + span`, clamped to `[floor, 1.0]`.
pub fn pl1_scale(temp_c: f64, p: ThermalPolicy) -> f64 {
    if temp_c <= p.thresh_c {
        return 1.0;
    }
    let over = (temp_c - p.thresh_c) / p.span_c;
    (1.0 - over * (1.0 - p.floor)).clamp(p.floor, 1.0)
}

/// The `cpu-*` thermal zones' `temp` files under `root` (default
/// `/sys/class/thermal`), sorted so the choice is deterministic.
pub fn discover_cpu_zones(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let dir = e.path();
        let ty = std::fs::read_to_string(dir.join("type")).unwrap_or_default();
        let temp = dir.join("temp");
        if ty.trim().contains(DEFAULT_ZONE_MATCH) && temp.exists() {
            out.push(temp);
        }
    }
    out.sort();
    out
}

/// The hottest zone in `paths`, in °C. Kernels report millidegrees; a zone that cannot
/// be read or parsed is skipped rather than guessed at.
pub fn read_max_temp_c(paths: &[PathBuf]) -> Option<f64> {
    let mut best: Option<f64> = None;
    for p in paths {
        let Ok(s) = std::fs::read_to_string(p) else {
            continue;
        };
        let Ok(milli) = s.trim().parse::<i64>() else {
            continue;
        };
        let c = milli as f64 / 1000.0;
        best = Some(best.map_or(c, |b: f64| b.max(c)));
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_is_one_at_or_below_the_threshold() {
        let p = ThermalPolicy::default(); // 60 / 20 / 0.5
        assert_eq!(pl1_scale(30.0, p), 1.0);
        assert_eq!(pl1_scale(60.0, p), 1.0);
    }

    #[test]
    fn scale_ramps_linearly_and_clamps_at_the_floor() {
        let p = ThermalPolicy::default();
        // halfway through the span -> halfway to the floor
        let mid = pl1_scale(70.0, p);
        assert!((mid - 0.75).abs() < 1e-9, "got {mid}");
        // at and past the end of the span it sits on the floor
        assert!((pl1_scale(80.0, p) - 0.5).abs() < 1e-9);
        assert!((pl1_scale(200.0, p) - 0.5).abs() < 1e-9, "never below the floor");
    }

    #[test]
    fn a_configured_floor_and_span_are_honoured() {
        let p = ThermalPolicy { thresh_c: 50.0, span_c: 10.0, floor: 0.2 };
        assert_eq!(pl1_scale(50.0, p), 1.0);
        assert!((pl1_scale(55.0, p) - 0.6).abs() < 1e-9);
        assert!((pl1_scale(60.0, p) - 0.2).abs() < 1e-9);
    }

    #[test]
    fn zones_are_discovered_by_type_and_missing_ones_are_skipped() {
        let dir = std::env::temp_dir().join(format!("uperf_thermal_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (z, ty, t) in [
            ("thermal_zone0", "cpu-0-0-usr\n", "31400\n"),
            ("thermal_zone1", "gpuss-0-usr\n", "29000\n"),
            ("thermal_zone2", "cpu-1-3-usr\n", "30600\n"),
            ("thermal_zone3", "cpu-0-0-step\n", "31800\n"),
        ] {
            let d = dir.join(z);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("type"), ty).unwrap();
            std::fs::write(d.join("temp"), t).unwrap();
        }
        // a cpu zone that exists as a directory but has no temp file
        let empty = dir.join("thermal_zone9");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::write(empty.join("type"), "cpu-9-9-usr\n").unwrap();

        let zones = discover_cpu_zones(&dir);
        // both cpu families match (usr + step); gpu and the temp-less cpu zone do not
        assert_eq!(zones.len(), 3, "cpu zones with a temp file: {zones:?}");
        assert_eq!(read_max_temp_c(&zones), Some(31.8), "the hottest zone wins");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_or_garbage_zones_do_not_invent_a_temperature() {
        let dir = std::env::temp_dir().join(format!("uperf_thermal_bad_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("temp");
        std::fs::write(&bad, "not-a-number\n").unwrap();
        assert_eq!(read_max_temp_c(std::slice::from_ref(&bad)), None);
        assert_eq!(read_max_temp_c(&[dir.join("nope")]), None);
        assert_eq!(read_max_temp_c(&[]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
