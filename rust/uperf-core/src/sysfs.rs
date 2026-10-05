//! Sysfs writer dispatch.
//!
//! AGENT.md §8.3 lists six writer kinds (string / percluster / percpu / cpufreq /
//! cgroup_procs / cpuset_cpus). The first five follow the upstream taxonomy; the
//! sixth (`cpuset_cpus`) was added in `docs/m1-static-reverse.md` §1.4 after we
//! observed uperf v3 writing to `/dev/cpuset/<group>/cpus` (cpu mask) instead of
//! `tasks`.
//!
//! `SysfsDispatcher::plan` takes the resolved config knobs and emits a list of
//! `(path, value)` writes. The Rust orchestrator (the Switcher/ProfileSwitcher
//! equivalents) hands those to the C++ side via the FFI bridge.

#![allow(dead_code)]

use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriterKind {
    /// `echo val > path` (and read-then-write on failure).
    String,
    /// Template string containing `{}` for cluster index; value is comma-separated.
    PerCluster,
    /// One write per CPU id; value is comma-separated.
    PerCpu,
    /// Scaling freq (value × 100000, retried against current max).
    Cpufreq,
    /// PID list to `cgroup.procs`.
    CgroupProcs,
    /// CPU mask string to `cpuset/<g>/cpus`.
    /// (`docs/m1-static-reverse.md` §3-§4 — **added after the upstream 5-class
    /// taxonomy** was found insufficient on alioth: uperf v3 opens
    /// `/dev/cpuset/{bg,fg,re,ta,sys-bg}/cpus` for write, fd 15-19.)
    CpusetCpus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysfsWrite {
    pub path: String,
    pub value: String,
    pub kind: WriterKind,
}

/// Compute the sysfs write sequence for a single resolved knob.
///
/// `knob` is the leaf name after `sysfs.` (e.g. `cpusetTa`, `cpuMax`, `CPU4max`).
/// `clusters` is the list of cluster indices (0..N). `online_cpus` is the full
/// set of online CPU ids (for percpu writers).
pub fn dispatch(knob: &str, raw: &Value, clusters: &[usize], online_cpus: &[usize]) -> Vec<SysfsWrite> {
    let v = serialize_value(raw);
    let lower = knob.to_ascii_lowercase();

    // ----- cpuset.*  -- cpuset/X/cpus (cpu mask) -----
    if lower.starts_with("cpuset") && (lower.ends_with("ta") || lower.ends_with("fg")
        || lower.ends_with("bg") || lower.ends_with("re") || lower.ends_with("sysbg"))
    {
        let group = match &lower[3..] {
            "ta" => "top-app",
            "fg" => "foreground",
            "bg" => "background",
            "re" => "restricted",
            "sysbg" => "system-background",
            _ => return vec![],
        };
        // Map `clusters` (cluster idx list) → cpu mask string.
        let mask = cluster_mask(online_cpus, clusters);
        return vec![SysfsWrite {
            path: format!("/dev/cpuset/{group}/cpus"),
            value: mask,
            kind: WriterKind::CpusetCpus,
        }];
    }

    // ----- cpu{N}max  -- cluster-specific freq via msm_performance -----
    if let Some(rest) = knob.strip_prefix("cpuN").or_else(|| knob.strip_prefix("cpun")) {
        // "cpu4max" → rest = "4max"; "cpuNmax" → "Nmax"
        // Treat any cpu + some digit prefix as cluster-specific.
        if let Some(pos) = rest.find(|c: char| !c.is_ascii_digit()) {
            let cluster_str = &rest[..pos];
            let suffix = &rest[pos..];
            let cluster: usize = cluster_str.parse().unwrap_or(0);
            match suffix {
                "max" | "max_freq" | "maxfreq" => {
                    return vec![SysfsWrite {
                        path: format!("/sys/module/msm_performance/parameters/cpu_max_freq"),
                        value: v.clone(),
                        kind: WriterKind::String,
                    }];
                    let _ = cluster;
                }
                "min" | "min_freq" | "minfreq" => {
                    return vec![SysfsWrite {
                        path: format!("/sys/module/msm_performance/parameters/cpu_min_freq"),
                        value: v.clone(),
                        kind: WriterKind::String,
                    }];
                }
                _ => {}
            }
        }
    }

    // ----- cpuMax / cpuMin  -- global msm_performance knobs -----
    if knob == "cpuMax" || lower == "cpumax" {
        return vec![SysfsWrite {
            path: "/sys/module/msm_performance/parameters/cpu_max_freq".into(),
            value: v.clone(),
            kind: WriterKind::String,
        }];
    }
    if knob == "cpuMin" || lower == "cpumin" {
        return vec![SysfsWrite {
            path: "/sys/module/msm_performance/parameters/cpu_min_freq".into(),
            value: v.clone(),
            kind: WriterKind::String,
        }];
    }
    if knob == "cciboost" {
        // MTK path or generic CCI: defer.
        return vec![SysfsWrite {
            path: "/proc/cciboost".into(),
            value: v.clone(),
            kind: WriterKind::String,
        }];
    }

    // ----- llccddr / ddr / sca / cpu4max / cpu7max / cpuset* (additional clusters) -----
    // The cpuset_* writer above already handled the most common case; this catch-all
    // is for less common knobs that we don't yet support.
    vec![]
}

/// Render the knob value as a string. Numbers are written as decimal (cpufreq
/// expects kHz without separators).
pub fn serialize_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(true) => "1".into(),
        Value::Bool(false) => "0".into(),
        Value::Array(a) => a.iter().map(serialize_value).collect::<Vec<_>>().join(","),
        Value::Object(o) => format!("{}={}", o.keys().next().map(|k| k.as_str()).unwrap_or(""),
            o.values().next().map(serialize_value).unwrap_or_default()),
        Value::Null => "".into(),
    }
}

/// Build a cpu-mask string ("0,1-3,5-7") from cluster indices and online CPUs.
///
/// `clusters` is the list of cluster indices whose CPUs we want; `online_cpus`
/// is the global CPU id list. We don't have cluster→cpu mapping here; we use a
/// safe default (include all online CPUs) and let the orchestrator refine per
/// SoC. For alioth kona + our test stub (`m1scheduler_topology.cpp` style), the
/// mapping is:
///   cluster 0 = cpu 0-3 (little), cluster 1 = cpu 4-5 (big), cluster 2 = cpu 6-7 (prime)
pub fn cluster_mask(online_cpus: &[usize], clusters: &[usize]) -> String {
    // Default cluster ranges for sdm8xx / kona. Refined by SoC-specific helpers in
// the M3 orchestrator; this fallback covers the common case for our tests.
    const CLUSTERS: &[&str] = &["0-3", "4-5", "6-7"];
    let mut parts = Vec::new();
    for c in clusters {
        if let Some(s) = CLUSTERS.get(*c) {
            parts.push(s.to_string());
        }
    }
    if parts.is_empty() {
        // No cluster info → include everything.
        online_cpus
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(",")
    } else {
        parts.join(",")
    }
}

/// Plan the sysfs writes for a complete (mode, scene) pair.
///
/// `modules_sysfs` is the value of `modules.sysfs` from the config (typically empty —
/// 4-tuples are all under `initials.<sysfs>.<dotted>`).
/// `initials` is the already-flattened dotted-key map.
/// `knobs_for_mode` is the result of `Config::resolve_all(mode, scene)` for keys
/// matching `sysfs.*`.
pub fn plan_scene<'a>(
    resolved_sysfs: BTreeMap<&'a str, &'a Value>,
    clusters: &[usize],
    online_cpus: &[usize],
) -> Vec<SysfsWrite> {
    let mut out = Vec::new();
    for (k, v) in resolved_sysfs {
        let leaf = k.strip_prefix("sysfs.").unwrap_or(k);
        for w in dispatch(leaf, v, clusters, online_cpus) {
            out.push(w);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cpuset_ta_dispatches_to_cpuset_writer() {
        let ws = dispatch("cpusetTa", &json!("0-3,4-5"), &[], &[true]);
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].path, "/dev/cpuset/top-app/cpus");
        assert_eq!(ws[0].value, "0-3,4-5");
        assert!(matches!(ws[0].kind, WriterKind::CpusetCpus));
    }

    #[test]
    fn cpuset_fg_bg_re_sysbg_all_supported() {
        for k in ["cpusetFg", "cpusetBg", "cpusetRe", "cpusetSysBg"] {
            let ws = dispatch(k, &json!("0"), &[0, 1], &[0, 1, 2, 3]);
            assert!(matches!(ws[0].kind, WriterKind::CpusetCpus), "{}", k);
        }
    }

    #[test]
    fn cpu_max_maps_to_msm_performance() {
        let ws = dispatch("cpuMax", &json!(2419200), &[], &[true]);
        assert_eq!(ws[0].path, "/sys/module/msm_performance/parameters/cpu_max_freq");
    }

    #[test]
    fn cpu4max_maps_to_msm_performance() {
        let ws = dispatch("cpu4max", &json!(2227200), &[0], &[true]);
        assert_eq!(ws[0].path, "/sys/module/msm_performance/parameters/cpu_max_freq");
    }

    #[test]
    fn unknown_knob_is_skipped() {
        let ws = dispatch("future_knob", &json!("x"), &[], &[true]);
        assert!(ws.is_empty());
    }

    #[test]
    fn cluster_mask_default_kona() {
        let m = cluster_mask(&[0, 1, 2, 3, 4, 5, 6, 7], &[0]);
        assert_eq!(m, "0-3");
        let m2 = cluster_mask(&[0, 1, 2, 3, 4, 5, 6, 7], &[0, 1]);
        assert_eq!(m2, "0-3,4-5");
    }
}