//! `uperf-cli plan` — emit the sysfs write sequence for `<mode> <scene>`.
//!
//! Output format (one line per write, mirrors what the M3 sysfs writer will emit
//! when applying the same preset):
//!   `<path> = <expanded_value>  ::  <source>`
//!
//! where `<source>` is one of `init`, `preset[*]`, `preset[scene]`. The sysfs
//! knobs additionally get a `<dotted> -> <path>` expansion from the local mirror
//! of `uperf_core::sysfs::dispatch` (kept in sync here — both crates are
//! independently tested).

use crate::config::Config;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterKind {
    String,
    PerCluster,
    PerCpu,
    Cpufreq,
    CgroupProcs,
    CpusetCpus,
}

#[derive(Debug, Clone)]
pub struct SysfsWrite {
    pub knob: String,
    pub path: String,
    pub value: String,
    pub kind: WriterKind,
}

/// Mirror of `uperf_core::sysfs::dispatch` (Rust M3) — see `rust/uperf-core/src/sysfs.rs`.
pub fn dispatch(knob: &str, raw: &Value) -> Vec<SysfsWrite> {
    let v = serialize_value(raw);
    let lower = knob.to_ascii_lowercase();
    let k = WriterKind::String;

    if lower.starts_with("cpuset")
        && (lower.ends_with("ta")
            || lower.ends_with("fg")
            || lower.ends_with("bg")
            || lower.ends_with("re")
            || lower.ends_with("sysbg"))
    {
        // strip the "cpuset" prefix (6 chars) before matching the group suffix.
        let group = match &lower[6..] {
            "ta" => "top-app",
            "fg" => "foreground",
            "bg" => "background",
            "re" => "restricted",
            "sysbg" => "system-background",
            _ => return vec![],
        };
        return vec![SysfsWrite {
            knob: knob.to_string(),
            path: format!("/dev/cpuset/{group}/cpus"),
            value: v,
            kind: WriterKind::CpusetCpus,
        }];
    }

    if matches!(knob, "cpuMax" | "cpuMin" | "cpumax" | "cpumin") {
        let path = if knob.to_ascii_lowercase().contains("max") {
            "/sys/module/msm_performance/parameters/cpu_max_freq"
        } else {
            "/sys/module/msm_performance/parameters/cpu_min_freq"
        };
        return vec![SysfsWrite {
            knob: knob.to_string(),
            path: path.into(),
            value: v,
            kind: k,
        }];
    }
    if matches!(knob, "cciboost" | "ddrboost") {
        return vec![SysfsWrite {
            knob: knob.to_string(),
            path: "/proc/boost".into(),
            value: v,
            kind: k,
        }];
    }
    // cpu4max / cpu7max / cpu4min / cpu7min / CPU4max etc. (case-insensitive)
    let lstrip = knob.to_ascii_lowercase();
    if let Some(rest) = lstrip.strip_prefix("cpu") {
        if let Some(idx) = rest.find(|c: char| !c.is_ascii_digit()) {
            let suffix = &rest[idx..];
            if matches!(suffix, "max" | "min" | "maxfreq" | "minfreq") {
                let path = if suffix.contains("max") {
                    "/sys/module/msm_performance/parameters/cpu_max_freq"
                } else {
                    "/sys/module/msm_performance/parameters/cpu_min_freq"
                };
                return vec![SysfsWrite {
                    knob: knob.to_string(),
                    path: path.into(),
                    value: v,
                    kind: k,
                }];
            }
        }
    }
    // cpuFreqMin<n> (literal template-style)
    if let Some(rest) = knob.strip_prefix("cpuFreqMin") {
        if let Ok(_cpu_id) = rest.parse::<usize>() {
            return vec![SysfsWrite {
                knob: knob.to_string(),
                path: format!("/sys/devices/system/cpu/cpu{rest}/cpufreq/scaling_min_freq"),
                value: v,
                kind: k,
            }];
        }
    }
    // cpuX / cpu{n} -> /sys/devices/system/cpu/cpuN/online (returns 1)
    if let Some(rest) = knob.strip_prefix("cpu") {
        if rest.parse::<usize>().is_ok() {
            return vec![SysfsWrite {
                knob: knob.to_string(),
                path: format!("/sys/devices/system/cpu/cpu{rest}/online"),
                value: v,
                kind: k,
            }];
        }
    }
    // SoC-specific per-cluster devfreq paths (real device paths observed via
    // /proc/<pid>/fd on alioth, recorded in docs/m3-evidence.md §3). These come
    // from upstream binary's device enumeration; the orchestrator picks the
    // matching cluster based on the chip's address layout.
    let soc_devfreq = |cluster: &str, suffix: &str, file: &str| -> String {
        format!("/sys/devices/platform/soc/soc:qcom,cpu{cluster}-{suffix}/devfreq/soc:qcom,cpu{cluster}-{suffix}/{file}")
    };
    if (lower.starts_with("cpu") || lower.starts_with("llccddr")) {
        for cluster in ["0", "4", "7"] {
            let pfx = if lower.starts_with("llccddr") { format!("llccddr{cluster}") } else { format!("cpu{cluster}llcc") };
            if lower.starts_with(&pfx) {
                let file = if lower.contains("min") { "min_freq" } else { "max_freq" };
                return vec![SysfsWrite {
                    knob: knob.to_string(),
                    path: soc_devfreq(cluster, "llcc-ddr-lat", file),
                    value: v,
                    kind: WriterKind::String,
                }];
            }
        }
    }
    // CPUllccmax / CPUllccmin / cpullcc*: /sys/.../soc:qcom,cpu-llcc-ddr-bw/...
    // (Note the bizarre path: 'cpu-llcc-ddr-bw' has cpu-llcc prefix not cpu0llcc.)
    if lower.starts_with("cpullcc") || lower.starts_with("cpu-llcc") {
        return vec![SysfsWrite {
            knob: knob.to_string(),
            path: "/sys/devices/platform/soc/soc:qcom,cpu-llcc-ddr-bw/devfreq/soc:qcom,cpu-llcc-ddr-bw/".to_string()
                + if lower.contains("min") { "min_freq" } else { "max_freq" },
            value: v,
            kind: WriterKind::String,
        }];
    }
    // CPU<N>ddrmax / CPU<N>ddrmin: /sys/.../soc:qcom,cpu-cpu-llcc-bw/...
    // Knob name is "CPU4ddrmax" so lower starts with "cpu4ddr" not "cpuddr".
    if (lower.starts_with("cpu0ddr") || lower.starts_with("cpu4ddr") || lower.starts_with("cpu7ddr"))
        || lower == "cpuddrmax" || lower == "cpuddrmin"
    {
        return vec![SysfsWrite {
            knob: knob.to_string(),
            path: "/sys/devices/platform/soc/soc:qcom,cpu-cpu-llcc-bw/devfreq/soc:qcom,cpu-cpu-llcc-bw/".to_string()
                + if lower.contains("min") { "min_freq" } else { "max_freq" },
            value: v,
            kind: WriterKind::String,
        }];
    }
    // CPUl3max / CPUl3min: cluster-specific L3 latency path
    if lower.starts_with("cpul3") {
        let cluster = if lower.starts_with("cpul30") { "0" } else if lower.starts_with("cpul34") { "4" } else if lower.starts_with("cpul37") { "7" } else { "" };
        return vec![SysfsWrite {
            knob: knob.to_string(),
            path: soc_devfreq(cluster, "cpu4-cpu-l3-lat", if lower.contains("min") { "min_freq" } else { "max_freq" }),
            value: v,
            kind: WriterKind::String,
        }];
    }
    if let Some(rest) = lower.strip_prefix("cpufreqmin") {
        if let Ok(cpu_id) = rest.parse::<usize>() {
            return vec![SysfsWrite {
                knob: knob.to_string(),
                path: format!("/sys/devices/system/cpu/cpu{cpu_id}/cpufreq/scaling_min_freq"),
                value: v,
                kind: WriterKind::String,
            }];
        }
    }

    // Unknown knob. Emit to a sentinel path so parity runs surface the
    // attempt; the orchestrator ignores unrecognized writes (no real device
    // write happens).
    vec![SysfsWrite {
        knob: knob.to_string(),
        path: format!("/dev/null/unknown/sysfs/{knob}"),
        value: v,
        kind: WriterKind::String,
    }]
}

fn serialize_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(true) => "1".into(),
        Value::Bool(false) => "0".into(),
        Value::Array(a) => a.iter().map(serialize_value).collect::<Vec<_>>().join(","),
        Value::Object(o) => format!(
            "{}={}",
            o.keys().next().map(|k| k.as_str()).unwrap_or(""),
            o.values().next().map(serialize_value).unwrap_or_default()
        ),
        Value::Null => "".into(),
    }
}

pub fn emit(cfg: &Config, mode: &str, scene: &str) {
    println!("# uperf-cli plan for mode={} scene={}", mode, scene);

    if !cfg.presets.contains_key(mode) {
        println!("CfgMgr: Failed to switch to undefined preset '{}'", mode);
        return;
    }
    let preset = &cfg.presets[mode];
    let overrides = preset.scenes.get(scene);
    let wild = preset.scenes.get("*");

    let mut keys: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
    for k in cfg.initials.keys() {
        keys.insert(k);
    }
    if let Some(w) = wild {
        for k in w.keys() {
            keys.insert(k);
        }
    }
    if let Some(s) = overrides {
        for k in s.keys() {
            keys.insert(k);
        }
    }
    for k in keys {
        if let Some(v) = cfg.resolve(mode, scene, k) {
            let src = if overrides.map(|s| s.contains_key(k)).unwrap_or(false) {
                "preset[scene]"
            } else if wild.map(|m| m.contains_key(k)).unwrap_or(false) {
                "preset[*]"
            } else {
                "init"
            };
            if let Some(stripped) = k.strip_prefix("sysfs.") {
                // Sysfs knobs go through the path-expansion dispatcher.
                for w in dispatch(stripped, v) {
                    println!(
                        "{} = {}   ::  {}   ->  ({:?})",
                        w.path, w.value, src, w.kind
                    );
                }
            } else {
                println!("{} = {}   ::  {}", k, val_to_str(v), src);
            }
        }
    }
}

fn val_to_str(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{}\"", s),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(val_to_str)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(o) => format!("{{{} entries}}", o.len()),
        Value::Null => "null".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cpuset_ta_path() {
        let ws = dispatch("cpusetTa", &json!("0-3,4-5"));
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].path, "/dev/cpuset/top-app/cpus");
        assert_eq!(ws[0].value, "0-3,4-5");
    }

    #[test]
    fn cpu_max_path() {
        let ws = dispatch("cpuMax", &json!(2419200));
        assert!(ws[0].path.contains("cpu_max_freq"));
    }

    #[test]
    fn cpu4max_path() {
        let ws = dispatch("cpu4max", &json!(2227200));
        assert!(ws[0].path.contains("cpu_max_freq"));
    }
}