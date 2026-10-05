//! Build a [`Governor`] from a parsed `Config`.
//!
//! Everything the governor needs is in the config except two runtime facts:
//!   * which CPUs belong to which cluster — taken from `powerModel[].nr` in
//!     cluster order (cpu0.., then the next cluster, …), which matches how the
//!     upstream binary enumerates clusters on every SoC we have configs for;
//!   * the available OPP list per cluster — read from
//!     `/sys/devices/system/cpu/cpufreq/policy<N>/scaling_available_frequencies`
//!     on the device and passed in here.
//!
//! The tunables come from `presets[<mode>][<scene>]` → `presets[<mode>]["*"]` →
//! `initials.cpu.*` (the normal cascade), so a scene switch re-derives them.

#![allow(dead_code)]

use crate::config::Config;
use crate::cpu::PowerModel;
use crate::freq_target::FreqTarget;
use crate::governor::{ClusterState, Governor, GovernorTunables};
use std::collections::BTreeMap;

/// Cluster CPU-id slices derived from `powerModel[].nr`, in order.
pub fn cpu_slices(models: &[PowerModel]) -> Vec<Vec<usize>> {
    let mut out = Vec::with_capacity(models.len());
    let mut next = 0usize;
    for m in models {
        let n = m.nr.max(1) as usize;
        out.push((next..next + n).collect());
        next += n;
    }
    out
}

fn f64_of(cfg: &Config, mode: &str, scene: &str, key: &str, dflt: f64) -> f64 {
    cfg.resolve(mode, scene, key)
        .and_then(|v| v.as_f64())
        .unwrap_or(dflt)
}

fn bool_of(cfg: &Config, mode: &str, scene: &str, key: &str, dflt: bool) -> bool {
    cfg.resolve(mode, scene, key)
        .and_then(|v| v.as_bool())
        .unwrap_or(dflt)
}

/// Tunables for a (mode, scene) pair, with the documented defaults as fallback.
pub fn tunables_from(cfg: &Config, mode: &str, scene: &str) -> GovernorTunables {
    let d = GovernorTunables::default();
    GovernorTunables {
        base_sample_time: f64_of(cfg, mode, scene, "cpu.baseSampleTime", d.base_sample_time),
        base_slack_time: f64_of(cfg, mode, scene, "cpu.baseSlackTime", d.base_slack_time),
        latency_time: f64_of(cfg, mode, scene, "cpu.latencyTime", d.latency_time),
        slow_limit_power: f64_of(cfg, mode, scene, "cpu.slowLimitPower", d.slow_limit_power),
        fast_limit_power: f64_of(cfg, mode, scene, "cpu.fastLimitPower", d.fast_limit_power),
        fast_limit_capacity: f64_of(cfg, mode, scene, "cpu.fastLimitCapacity", d.fast_limit_capacity),
        fast_limit_recover_scale: f64_of(cfg, mode, scene, "cpu.fastLimitRecoverScale", d.fast_limit_recover_scale),
        predict_thd: f64_of(cfg, mode, scene, "cpu.predictThd", d.predict_thd),
        margin: f64_of(cfg, mode, scene, "cpu.margin", d.margin),
        burst: f64_of(cfg, mode, scene, "cpu.burst", d.burst),
        guide_cap: bool_of(cfg, mode, scene, "cpu.guideCap", d.guide_cap),
        limit_efficiency: bool_of(cfg, mode, scene, "cpu.limitEfficiency", d.limit_efficiency),
    }
}

/// Build the governor. `opps_per_cluster` must line up with `powerModel` order.
pub fn governor_from_config(
    cfg: &Config,
    mode: &str,
    scene: &str,
    opps_per_cluster: &[Vec<f64>],
) -> Option<Governor> {
    let modules = cfg.modules_map()?;
    let models = PowerModel::list_from_modules(modules);
    if models.is_empty() {
        return None;
    }
    let slices = cpu_slices(&models);
    let clusters: Vec<ClusterState> = models
        .iter()
        .enumerate()
        .map(|(i, m)| {
            ClusterState::new(
                m.clone(),
                slices[i].clone(),
                opps_per_cluster.get(i).cloned().unwrap_or_default(),
            )
        })
        .collect();
    Some(Governor::new(tunables_from(cfg, mode, scene), clusters))
}

/// Frequency write targets per cluster, derived from the knob table
/// (real filesystem probe).
pub fn freq_targets(cfg: &Config) -> Vec<FreqTarget> {
    freq_targets_with(cfg, &crate::freq_target::RealFs)
}

/// Same, with an injected probe so the resolution is unit-testable.
pub fn freq_targets_with(
    cfg: &Config,
    fs: &dyn crate::freq_target::FsProbe,
) -> Vec<FreqTarget> {
    let table = cfg.sysfs_knob_table();
    let models = PowerModel::list_from_modules(cfg.modules_map().unwrap_or(&Default::default()));
    cpu_slices(&models)
        .iter()
        .map(|cores| {
            let first = cores.first().copied().unwrap_or(0);
            crate::freq_target::freq_target_for_cluster_with(&table, first, fs)
        })
        .collect()
}

/// Convenience for parity tooling: `{knob: freq}` for one governor output.
pub fn freq_writes(
    targets: &[FreqTarget],
    freqs_khz: &[f64],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (t, f) in targets.iter().zip(freqs_khz.iter()) {
        if let Some(knob) = t.knob() {
            out.push((knob.to_string(), format!("{}", *f as i64)));
        }
    }
    // A global (msm_performance) knob can be hit by several clusters — keep the
    // highest request, matching "the whole CPU can't exceed this" semantics.
    let mut best: BTreeMap<String, i64> = BTreeMap::new();
    for (k, v) in out {
        let n: i64 = v.parse().unwrap_or(0);
        best.entry(k)
            .and_modify(|e| {
                if n > *e {
                    *e = n;
                }
            })
            .or_insert(n);
    }
    best.into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg() -> Config {
        Config::from_value(json!({
            "meta": {"name":"t","author":"t"},
            "modules": {
                "cpu": {"enable": true, "powerModel": [
                    {"efficiency":115,"nr":4,"typicalPower":0.3,"typicalFreq":1.8,
                     "sweetFreq":1.4,"plainFreq":1.2,"freeFreq":0.6},
                    {"efficiency":320,"nr":3,"typicalPower":1.6,"typicalFreq":2.4,
                     "sweetFreq":1.7,"plainFreq":1.0,"freeFreq":0.7},
                    {"efficiency":400,"nr":1,"typicalPower":3,"typicalFreq":2.9,
                     "sweetFreq":1.8,"plainFreq":1.6,"freeFreq":0.8}
                ]},
                "sysfs": {"enable": true, "knob": {
                    "CPU4max": "/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq",
                    "CPU7max": "/sys/devices/system/cpu/cpufreq/policy7/scaling_max_freq",
                    "cpuMax": "/sys/module/msm_performance/parameters/cpu_max_freq"
                }},
                "switcher": {"hintDuration": {"touch": 4.0}}
            },
            "initials": {"cpu": {
                "baseSampleTime": 0.01, "baseSlackTime": 0.01, "latencyTime": 0.6,
                "slowLimitPower": 1.0, "fastLimitPower": 2.0, "fastLimitCapacity": 6.0,
                "fastLimitRecoverScale": 0.2, "predictThd": 0.6, "margin": 0.2,
                "burst": 0.0, "guideCap": true, "limitEfficiency": true
            }},
            "presets": {"balance": {"*": {"cpu.margin": 0.22}, "touch": {"cpu.margin": 0.25}}}
        }))
        .unwrap()
    }

    #[test]
    fn slices_follow_nr_in_order() {
        let c = cfg();
        let models = PowerModel::list_from_modules(c.modules_map().unwrap());
        let s = cpu_slices(&models);
        assert_eq!(s, vec![vec![0, 1, 2, 3], vec![4, 5, 6], vec![7]]);
    }

    #[test]
    fn tunables_come_from_initials_then_preset_overrides() {
        let c = cfg();
        let t = tunables_from(&c, "balance", "*");
        assert_eq!(t.base_sample_time, 0.01);
        assert_eq!(t.slow_limit_power, 1.0);
        assert_eq!(t.margin, 0.22, "preset[*] must override initials");
        let t2 = tunables_from(&c, "balance", "touch");
        assert_eq!(t2.margin, 0.25, "preset[scene] must win");
        assert_eq!(t2.burst, 0.0, "unset keys fall back to initials");
        assert!(t2.guide_cap && t2.limit_efficiency);
    }

    #[test]
    fn governor_builds_with_cluster_cores() {
        let c = cfg();
        let opps = vec![
            vec![614400.0, 1804800.0],
            vec![710400.0, 2419200.0],
            vec![844800.0, 3187200.0],
        ];
        let g = governor_from_config(&c, "balance", "*", &opps).unwrap();
        assert_eq!(g.clusters.len(), 3);
        assert_eq!(g.clusters[0].cores, vec![0, 1, 2, 3]);
        assert_eq!(g.clusters[2].cores, vec![7]);
        assert_eq!(g.clusters[0].max_opp(), 1804800.0);
        assert_eq!(g.pool, 6.0, "pool starts at fastLimitCapacity");
    }

    /// No sysfs at all -> pure config-knob resolution (the host has a real
    /// `/sys/devices/system/cpu/cpufreq/`, so the probe must be injected).
    struct NoFs;
    impl crate::freq_target::FsProbe for NoFs {
        fn exists(&self, _p: &str) -> bool {
            false
        }
        fn is_writable(&self, _p: &str) -> bool {
            false
        }
    }

    #[test]
    fn freq_targets_and_global_dedupe() {
        let c = cfg();
        let t = freq_targets_with(&c, &NoFs);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].knob(), Some("cpuMax"), "cluster0 falls back to the global knob");
        assert_eq!(t[1].knob(), Some("CPU4max"));
        assert_eq!(t[2].knob(), Some("CPU7max"));

        // cluster0 has no policy knob -> it maps to the global one, which must
        // collapse to a single write holding the highest request.
        let writes = crate::freq_target::freq_writes(&t, &[1_500_000.0, 1_600_000.0, 1_700_000.0]);
        let m: std::collections::BTreeMap<_, _> = writes.into_iter().collect();
        assert_eq!(m.get("cpuMax").map(String::as_str), Some("1500000"));
        assert_eq!(m.get("CPU4max").map(String::as_str), Some("1600000"));
        assert_eq!(m.get("CPU7max").map(String::as_str), Some("1700000"));
    }

    #[test]
    fn userspace_control_is_preferred_per_cluster() {
        struct AllGovernors;
        impl crate::freq_target::FsProbe for AllGovernors {
            fn exists(&self, p: &str) -> bool {
                p.ends_with("/scaling_governor")
            }
            fn is_writable(&self, p: &str) -> bool {
                p.ends_with("/scaling_governor")
            }
        }
        let c = cfg();
        let t = freq_targets_with(&c, &AllGovernors);
        assert_eq!(t.len(), 3);
        assert!(t.iter().all(|x| matches!(x, FreqTarget::Userspace { .. })));
        // Different policy dirs, one per cluster.
        let dirs: Vec<_> = t.iter().filter_map(|x| x.path()).collect();
        assert_eq!(
            dirs,
            vec![
                "/sys/devices/system/cpu/cpufreq/policy0",
                "/sys/devices/system/cpu/cpufreq/policy4",
                "/sys/devices/system/cpu/cpufreq/policy7"
            ]
        );
    }
}
