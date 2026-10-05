//! Sysfs write planner.
//!
//! **Key finding (docs/m4-evidence.md §1)**: the paths are NOT hardcoded in the
//! binary. `modules.sysfs.knob` is a `{knob_name: absolute_path}` table in the
//! config:
//!
//! ```jsonc
//! "modules": { "sysfs": { "enable": true, "knob": {
//!     "cpusetTa":  "/dev/cpuset/top-app/cpus",
//!     "CPU4ddrmax":"/sys/class/devfreq/soc:qcom,cpu4-llcc-ddr-lat/max_freq",
//!     "CPU4max":   "/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq",
//!     "UFSmax":    "/sys/class/devfreq/1d84000.ufshc/max_freq"
//! } } }
//! ```
//!
//! (`strings` on the upstream binary finds no `devfreq`/`llcc`/`ufshc` at all.
//! `/proc/<pid>/fd` shows the *resolved* path because `/sys/class/devfreq/<name>`
//! is a symlink into `/sys/devices/platform/soc/<name>/devfreq/<name>`.)
//!
//! So the planner is: look the path up in the knob table, then infer the write
//! *kind* from the path's shape. Nothing SoC-specific is hardcoded on our side.

#![allow(dead_code)]

use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriterKind {
    /// Plain `echo value > path`.
    String,
    /// `cpufreq/policy*/scaling_{min,max}_freq`.
    Cpufreq,
    /// `cpuset/<group>/cpus` — CPU mask ("0-3,4-5").
    CpusetCpus,
    /// `cgroup.procs` / `tasks` — PID list.
    CgroupProcs,
    /// `cpu<N>/online` — per-CPU enable.
    PerCpu,
}

impl WriterKind {
    /// Infer the kind from the resolved path (upstream does the same at
    /// config-load time; the schema has no per-knob `type` field — the `type`
    /// column in `config/README.md` is v1/v2-era).
    pub fn from_path(path: &str) -> Self {
        if path.starts_with("/dev/cpuset/") && path.ends_with("/cpus") {
            return Self::CpusetCpus;
        }
        if path.ends_with("/cgroup.procs") || path.ends_with("/tasks") {
            return Self::CgroupProcs;
        }
        if path.contains("/cpufreq/") && path.ends_with("_freq") {
            return Self::Cpufreq;
        }
        if path.ends_with("/online") {
            return Self::PerCpu;
        }
        // devfreq `*/min_freq|max_freq`, `/sys/module/msm_performance/...`,
        // `/proc/ppm/...` all take the value verbatim.
        Self::String
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysfsWrite {
    pub knob: String,
    pub path: String,
    pub value: String,
    pub kind: WriterKind,
}

impl SysfsWrite {
    /// Rewrite the path under a fake root, so offline validation never touches
    /// the real filesystem.
    pub fn under_root(&self, root: &str) -> String {
        format!("{}{}", root.trim_end_matches('/'), self.path)
    }
}

/// Resolve a single knob against the config's knob table.
///
/// `None` when the knob isn't declared — upstream logs
/// `Knob '<k>' not defined in '<module>'` and skips it.
pub fn dispatch(
    knob: &str,
    raw: &Value,
    knob_table: &BTreeMap<String, String>,
) -> Option<SysfsWrite> {
    let path = knob_table.get(knob)?;
    Some(SysfsWrite {
        knob: knob.to_string(),
        path: path.clone(),
        value: serialize_value(raw),
        kind: WriterKind::from_path(path),
    })
}

pub fn serialize_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(true) => "1".into(),
        Value::Bool(false) => "0".into(),
        Value::Array(a) => a
            .iter()
            .map(serialize_value)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(o) => format!(
            "{}={}",
            o.keys().next().map(|k| k.as_str()).unwrap_or(""),
            o.values().next().map(serialize_value).unwrap_or_default()
        ),
        Value::Null => "".into(),
    }
}

/// Plan every write for a resolved scene.
pub fn plan_scene<'a, I>(resolved: I, knob_table: &BTreeMap<String, String>) -> Vec<SysfsWrite>
where
    I: IntoIterator<Item = (&'a str, &'a Value)>,
{
    let mut out = Vec::new();
    for (k, v) in resolved {
        let leaf = k.strip_prefix("sysfs.").unwrap_or(k);
        if let Some(w) = dispatch(leaf, v, knob_table) {
            out.push(w);
        }
    }
    // Two knobs can resolve to the same node (two cluster aliases -> one
    // devfreq). Upstream diffs against the previous action and skips identical
    // values (AGENT.md 8.3), so collapse (path, value) pairs.
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path && a.value == b.value);
    out
}

/// Convenience: plan for (mode, scene) straight from a parsed config.
pub fn plan_for_config(cfg: &crate::config::Config, mode: &str, scene: &str) -> Vec<SysfsWrite> {
    let table = cfg.sysfs_knob_table();
    let resolved: Vec<(String, Value)> = cfg
        .all_sysfs_keys()
        .into_iter()
        .filter_map(|k| cfg.resolve(mode, scene, &k).map(|v| (k, v.clone())))
        .collect();
    plan_scene(resolved.iter().map(|(k, v)| (k.as_str(), v)), &table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The knob table exactly as shipped in UGT sdm888.json.
    fn table() -> BTreeMap<String, String> {
        [
            ("cpusetTa", "/dev/cpuset/top-app/cpus"),
            ("cpusetFg", "/dev/cpuset/foreground/cpus"),
            ("cpusetBg", "/dev/cpuset/background/cpus"),
            ("cpusetSysBg", "/dev/cpuset/system-background/cpus"),
            ("cpusetRe", "/dev/cpuset/restricted/cpus"),
            ("CPUl3max", "/sys/class/devfreq/18590100.qcom,cpu4-cpu-l3-lat/max_freq"),
            ("CPUl3min", "/sys/class/devfreq/18590100.qcom,cpu4-cpu-l3-lat/min_freq"),
            ("CPU4ddrmax", "/sys/class/devfreq/soc:qcom,cpu4-llcc-ddr-lat/max_freq"),
            ("CPU4ddrmin", "/sys/class/devfreq/soc:qcom,cpu4-llcc-ddr-lat/min_freq"),
            ("CPUllccmin", "/sys/class/devfreq/soc:qcom,cpu-cpu-llcc-bw/min_freq"),
            ("CPUllccmax", "/sys/class/devfreq/soc:qcom,cpu-cpu-llcc-bw/max_freq"),
            ("CPU7ddrmax", "/sys/class/devfreq/soc:qcom,cpu-llcc-ddr-bw/max_freq"),
            ("CPU7ddrmin", "/sys/class/devfreq/soc:qcom,cpu-llcc-ddr-bw/min_freq"),
            ("CPU7max", "/sys/devices/system/cpu/cpufreq/policy7/scaling_max_freq"),
            ("CPU4max", "/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq"),
            ("UFSmax", "/sys/class/devfreq/1d84000.ufshc/max_freq"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn knob_table_drives_path_selection() {
        let t = table();
        let w = dispatch("CPU4ddrmax", &json!("5931"), &t).unwrap();
        assert_eq!(w.path, "/sys/class/devfreq/soc:qcom,cpu4-llcc-ddr-lat/max_freq");
        assert_eq!(w.value, "5931");
        assert_eq!(w.kind, WriterKind::String);
    }

    #[test]
    fn undeclared_knob_is_skipped() {
        let t = table();
        assert!(dispatch("NoSuchKnob", &json!("1"), &t).is_none());
    }

    #[test]
    fn kind_inference() {
        use WriterKind::{Cpufreq, CgroupProcs, CpusetCpus, PerCpu, String as KString};
        assert_eq!(WriterKind::from_path("/dev/cpuset/top-app/cpus"), CpusetCpus);
        assert_eq!(
            WriterKind::from_path("/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq"),
            Cpufreq
        );
        assert_eq!(
            WriterKind::from_path("/sys/class/devfreq/soc:qcom,cpu-cpu-llcc-bw/max_freq"),
            KString
        );
        assert_eq!(WriterKind::from_path("/dev/cpuset/background/cgroup.procs"), CgroupProcs);
        assert_eq!(WriterKind::from_path("/sys/devices/system/cpu/cpu7/online"), PerCpu);
    }

    #[test]
    fn under_root_rewrites() {
        let t = table();
        let w = dispatch("cpusetTa", &json!("0-7"), &t).unwrap();
        assert_eq!(w.under_root("/tmp/fake"), "/tmp/fake/dev/cpuset/top-app/cpus");
    }

    #[test]
    fn bool_serializes_to_1_0() {
        assert_eq!(serialize_value(&json!(true)), "1");
        assert_eq!(serialize_value(&json!(false)), "0");
    }

    #[test]
    fn plan_covers_upstream_fd_trace_paths() {
        let t = table();
        let resolved = [
            ("sysfs.CPU4max", json!("2227200")),
            ("sysfs.CPU7max", json!("2496000")),
            ("sysfs.CPUllccmax", json!("9155")),
            ("sysfs.CPUllccmin", json!("2288")),
            ("sysfs.CPU4ddrmax", json!("5931")),
            ("sysfs.CPU4ddrmin", json!("762")),
            ("sysfs.CPU7ddrmax", json!("5931")),
            ("sysfs.CPU7ddrmin", json!("762")),
            ("sysfs.CPUl3max", json!("614400000")),
            ("sysfs.CPUl3min", json!("300000000")),
            ("sysfs.UFSmax", json!("300000000")),
            ("sysfs.cpusetTa", json!("0-7")),
            ("sysfs.cpusetFg", json!("0-2,4-7")),
            ("sysfs.cpusetBg", json!("0-3")),
            ("sysfs.cpusetSysBg", json!("0-3")),
            ("sysfs.cpusetRe", json!("0-6")),
        ];
        let ws = plan_scene(resolved.iter().map(|(k, v)| (*k, v)), &t);
        let paths: Vec<&str> = ws.iter().map(|w| w.path.as_str()).collect();
        for want in [
            "/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq",
            "/sys/devices/system/cpu/cpufreq/policy7/scaling_max_freq",
            "/sys/class/devfreq/soc:qcom,cpu4-llcc-ddr-lat/max_freq",
            "/sys/class/devfreq/soc:qcom,cpu4-llcc-ddr-lat/min_freq",
            "/sys/class/devfreq/soc:qcom,cpu-llcc-ddr-bw/max_freq",
            "/sys/class/devfreq/soc:qcom,cpu-llcc-ddr-bw/min_freq",
            "/sys/class/devfreq/soc:qcom,cpu-cpu-llcc-bw/max_freq",
            "/sys/class/devfreq/soc:qcom,cpu-cpu-llcc-bw/min_freq",
            "/dev/cpuset/top-app/cpus",
            "/dev/cpuset/foreground/cpus",
            "/dev/cpuset/background/cpus",
            "/dev/cpuset/system-background/cpus",
            "/dev/cpuset/restricted/cpus",
        ] {
            assert!(paths.contains(&want), "missing {want} in {paths:?}");
        }
        assert_eq!(ws.len(), 16, "expected 16 writes: {paths:?}");
    }
}
