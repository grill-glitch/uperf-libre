//! `uperf-cli plan` — emit the sysfs write sequence for `<mode> <scene>`.
//!
//! Output format (one line per write, mirrors what the M3 sysfs writer will emit
//! when applying the same preset):
//!   `<path> = <expanded_value>  ::  <source>`
//!
//! where `<source>` is one of `init`, `preset[*]`, `preset[scene]`.

use crate::config::Config;
use serde_json::Value;

pub fn emit(cfg: &Config, mode: &str, scene: &str) {
    println!(
        "# uperf-cli plan for mode={} scene={}",
        mode,
        scene
    );

    if !cfg.presets.contains_key(mode) {
        println!("CfgMgr: Failed to switch to undefined preset '{}'", mode);
        return;
    }
    let preset = &cfg.presets[mode];
    let overrides = preset.scenes.get(scene);
    let wild = preset.scenes.get("*");

    // Collect all dotted keys visible in any of the three sources, sorted.
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
            println!("{} = {}   ::  {}", k, val_to_str(v), src);
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