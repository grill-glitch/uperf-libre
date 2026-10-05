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

use uperf_config::Config;

/// Emit the cascade + sysfs path expansion for `<mode> <scene>`.
pub fn emit(cfg: &Config, mode: &str, scene: &str) {
    println!("# uperf-cli plan for mode={} scene={}", mode, scene);

    if !cfg.presets.contains_key(mode) {
        println!("CfgMgr: Failed to switch to undefined preset '{}'", mode);
        return;
    }
    let preset = &cfg.presets[mode];
    let overrides = preset.scenes.get(scene);
    let wild = preset.scenes.get("*");

    // Non-sysfs knobs first (cpu.*, sched.*), then the sysfs path expansion.
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

    for k in &keys {
        if k.starts_with("sysfs.") {
            continue; // handled below via the dispatcher
        }
        if let Some(v) = cfg.resolve(mode, scene, k) {
            println!("{} = {}   ::  {}", k, val_to_str(v), source(overrides, wild, k));
        }
    }

    println!("# --- sysfs writes ---");
    let resolved: Vec<(String, serde_json::Value)> = cfg
        .all_sysfs_keys()
        .into_iter()
        .filter_map(|k| cfg.resolve(mode, scene, &k).map(|v| (k, v.clone())))
        .collect();
    let writes = uperf_config::plan_scene(
        resolved.iter().map(|(k, v)| (k.as_str(), v)),
        &cfg.sysfs_knob_table(),
    );
    for w in &writes {
        let leaf = w
            .path
            .rsplit('/')
            .next()
            .unwrap_or("");
        println!("{} = {}   ::  ({:?})  [{}]", w.path, w.value, w.kind, leaf);
    }
    println!("# {} sysfs write(s)", writes.len());
}

fn source(
    overrides: Option<&serde_json::Map<String, serde_json::Value>>,
    wild: Option<&serde_json::Map<String, serde_json::Value>>,
    k: &str,
) -> &'static str {
    if overrides.map(|s| s.contains_key(k)).unwrap_or(false) {
        "preset[scene]"
    } else if wild.map(|m| m.contains_key(k)).unwrap_or(false) {
        "preset[*]"
    } else {
        "init"
    }
}

fn val_to_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => format!("\"{}\"", s),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Array(a) => format!(
            "[{}]",
            a.iter().map(val_to_str).collect::<Vec<_>>().join(", ")
        ),
        serde_json::Value::Object(o) => format!("{{{} entries}}", o.len()),
        serde_json::Value::Null => "null".into(),
    }
}

