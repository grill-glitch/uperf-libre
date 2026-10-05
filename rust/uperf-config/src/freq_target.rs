//! Cluster -> frequency-control target resolution.
//!
//! The config gives `modules.sysfs.knob` as `{knob: path}` but never says which
//! knob drives which cluster. Worse, which *mechanism* works is device-specific:
//!
//! * `/sys/devices/system/cpu/cpufreq/policyN/scaling_max_freq` is the obvious
//!   target, but on `qcom-cpufreq-hw` it is registered read-only (mode 444) and
//!   every write fails with EACCES even as root — measured on alioth, which is
//!   why the upstream binary aborts with "No CpufreqWriter supported for this
//!   platform" there;
//! * `scaling_governor` + `scaling_setspeed` *are* writable on the same device:
//!   switch the policy to the `userspace` governor and every write to
//!   `scaling_setspeed` takes effect immediately (verified on alioth);
//! * `/sys/module/msm_performance/parameters/cpu_max_freq` is the Qualcomm-wide
//!   fallback where the module exists.
//!
//! Resolution order therefore probes the filesystem instead of assuming:
//! `userspace` control → per-policy `scaling_max_freq` → msm_performance → None.

#![allow(dead_code)]

use std::collections::BTreeMap;

/// How a cluster's target frequency is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreqTarget {
    /// `scaling_governor = userspace` + `scaling_setspeed = <kHz>`.
    Userspace { policy_dir: String },
    /// A per-policy `scaling_max_freq` knob declared in the config.
    PolicyMax { knob: String, path: String },
    /// The global msm_performance knob (all clusters at once).
    MsmPerformance { knob: String, path: String },
    /// Nothing usable — the governor has nowhere to write.
    None,
}

impl FreqTarget {
    pub fn knob(&self) -> Option<&str> {
        match self {
            Self::PolicyMax { knob, .. } | Self::MsmPerformance { knob, .. } => Some(knob),
            _ => None,
        }
    }
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::PolicyMax { path, .. } | Self::MsmPerformance { path, .. } => Some(path),
            Self::Userspace { policy_dir } => Some(policy_dir),
            Self::None => None,
        }
    }
    pub fn is_global(&self) -> bool {
        matches!(self, Self::MsmPerformance { .. })
    }
    pub fn is_usable(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// A filesystem probe, injected so resolution is unit-testable.
pub trait FsProbe {
    fn exists(&self, path: &str) -> bool;
    fn is_writable(&self, path: &str) -> bool;
}

/// Real probe.
pub struct RealFs;
impl FsProbe for RealFs {
    fn exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }
    fn is_writable(&self, path: &str) -> bool {
        // `access(2)` reports writability from the *mode bits*; a file the
        // driver registered read-only (mode 444) still looks writable to root
        // because of CAP_DAC_OVERRIDE. Mode bits are therefore the honest check
        // here: 444 means "do not try", whatever the capability says. The real
        // proof is an attempted write, which the writer does and reports.
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o200 != 0)
            .unwrap_or(false)
    }
}

/// Policy directory for a cluster's first CPU id.
pub fn policy_dir(first_cpu: usize) -> String {
    format!("/sys/devices/system/cpu/cpufreq/policy{first_cpu}")
}

/// Resolve how to control the cluster whose first CPU id is `first_cpu`.
pub fn freq_target_for_cluster(knob_table: &BTreeMap<String, String>, first_cpu: usize) -> FreqTarget {
    freq_target_for_cluster_with(knob_table, first_cpu, &RealFs)
}

pub fn freq_target_for_cluster_with(
    knob_table: &BTreeMap<String, String>,
    first_cpu: usize,
    fs: &dyn FsProbe,
) -> FreqTarget {
    let dir = policy_dir(first_cpu);

    // 1. userspace governor control — the only mechanism verified to work on a
    //    qcom-cpufreq-hw kernel with locked scaling_max_freq.
    let gov = format!("{dir}/scaling_governor");
    let setspeed = format!("{dir}/scaling_setspeed");
    if fs.exists(&gov) && fs.is_writable(&gov) {
        // `scaling_setspeed` only appears once the policy is in userspace mode,
        // so accept the pair on the strength of the governor being writable.
        let _ = setspeed;
        return FreqTarget::Userspace { policy_dir: dir };
    }

    // 2. a per-policy scaling_max_freq knob declared in the config.
    let needle = format!("/policy{first_cpu}/");
    for (knob, path) in knob_table {
        if path.contains(&needle) && path.ends_with("scaling_max_freq") {
            return FreqTarget::PolicyMax {
                knob: knob.clone(),
                path: path.clone(),
            };
        }
    }

    // 3. the global Qualcomm knob.
    for (knob, path) in knob_table {
        if path.contains("/msm_performance/parameters/cpu_max_freq") {
            return FreqTarget::MsmPerformance {
                knob: knob.clone(),
                path: path.clone(),
            };
        }
    }

    FreqTarget::None
}

/// Convenience for parity tooling: `{knob: freq}` for one governor output.
///
/// A global (msm_performance) knob may be requested by several clusters; keep
/// the highest request, since the knob caps the whole CPU.
pub fn freq_writes(targets: &[FreqTarget], freqs_khz: &[f64]) -> Vec<(String, String)> {
    let mut best: BTreeMap<String, i64> = BTreeMap::new();
    for (t, f) in targets.iter().zip(freqs_khz.iter()) {
        let Some(knob) = t.knob() else { continue };
        let n = *f as i64;
        best.entry(knob.to_string())
            .and_modify(|e| {
                if n > *e {
                    *e = n;
                }
            })
            .or_insert(n);
    }
    best.into_iter().map(|(k, v)| (k, v.to_string())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        files: Vec<(&'static str, bool)>,
    }
    impl FsProbe for Fake {
        fn exists(&self, path: &str) -> bool {
            self.files.iter().any(|(p, _)| *p == path)
        }
        fn is_writable(&self, path: &str) -> bool {
            self.files.iter().any(|(p, w)| *p == path && *w)
        }
    }

    fn table() -> BTreeMap<String, String> {
        [
            ("cpusetTa", "/dev/cpuset/top-app/cpus"),
            ("CPU4max", "/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq"),
            ("CPU7max", "/sys/devices/system/cpu/cpufreq/policy7/scaling_max_freq"),
            ("UFSmax", "/sys/class/devfreq/1d84000.ufshc/max_freq"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn userspace_wins_when_the_governor_is_writable() {
        let fs = Fake {
            files: vec![
                ("/sys/devices/system/cpu/cpufreq/policy4/scaling_governor", true),
                ("/sys/devices/system/cpu/cpufreq/policy4/scaling_setspeed", false),
            ],
        };
        let t = freq_target_for_cluster_with(&table(), 4, &fs);
        assert_eq!(
            t,
            FreqTarget::Userspace { policy_dir: "/sys/devices/system/cpu/cpufreq/policy4".into() }
        );
        assert!(t.is_usable());
        assert_eq!(t.knob(), None, "userspace mode writes setspeed, not a config knob");
    }

    #[test]
    fn falls_back_to_the_policy_max_knob_when_governor_is_locked() {
        // The alioth situation: governor file absent/locked, config knob exists.
        let fs = Fake { files: vec![] };
        let t = freq_target_for_cluster_with(&table(), 4, &fs);
        assert_eq!(t.knob(), Some("CPU4max"));
        assert!(matches!(t, FreqTarget::PolicyMax { .. }));
    }

    #[test]
    fn msm_performance_is_the_last_resort() {
        let mut tbl = table();
        tbl.insert(
            "cpuMax".into(),
            "/sys/module/msm_performance/parameters/cpu_max_freq".into(),
        );
        let fs = Fake { files: vec![] };
        let t = freq_target_for_cluster_with(&tbl, 0, &fs);
        assert!(matches!(t, FreqTarget::MsmPerformance { .. }));
        assert!(t.is_global());
        // A cluster with its own policy knob still prefers it.
        assert_eq!(freq_target_for_cluster_with(&tbl, 4, &fs).knob(), Some("CPU4max"));
    }

    #[test]
    fn nothing_available_is_none() {
        let fs = Fake { files: vec![] };
        assert_eq!(freq_target_for_cluster_with(&table(), 1, &fs), FreqTarget::None);
    }

    #[test]
    fn devfreq_knobs_are_not_mistaken_for_cpu_freq() {
        let fs = Fake { files: vec![] };
        assert_eq!(freq_target_for_cluster_with(&table(), 1, &fs), FreqTarget::None);
    }

    #[test]
    fn global_knob_collapses_to_the_highest_request() {
        let tbl: BTreeMap<String, String> = [(
            "cpuMax",
            "/sys/module/msm_performance/parameters/cpu_max_freq",
        )]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let fs = Fake { files: vec![] };
        let targets: Vec<FreqTarget> = (0..3)
            .map(|i| freq_target_for_cluster_with(&tbl, i, &fs))
            .collect();
        assert!(targets.iter().all(|t| t.is_global()));
        let w = freq_writes(&targets, &[1_500_000.0, 1_700_000.0, 1_600_000.0]);
        assert_eq!(w, vec![("cpuMax".to_string(), "1700000".to_string())]);
    }

    #[test]
    fn userspace_targets_are_per_cluster_not_global() {
        let targets = vec![
            FreqTarget::Userspace { policy_dir: "/p0".into() },
            FreqTarget::Userspace { policy_dir: "/p4".into() },
        ];
        assert!(targets.iter().all(|t| !t.is_global()));
        // No config knob -> nothing for the knob-write path; the userspace
        // writer handles these itself.
        assert!(freq_writes(&targets, &[1_000_000.0, 2_000_000.0]).is_empty());
    }
}
