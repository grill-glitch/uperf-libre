//! `uperf-cli warn` — emit unknown-module / unknown-key lines.
//!
//! Upstream's `CfgMgr` logs lines like (extracted from binary, see
//! `docs/m1-static-reverse.md`):
//!   * `CfgMgr: Unknown module '<m>' defined in base hint of preset '<p>'`
//!   * `CfgMgr: Unknown key '<k>' of module '<m>' defined in base hint of preset '<p>'`
//!   * `CfgMgr: Base hint of preset '<p>' not defined`
//!   * `CfgMgr: Duration of hint '<h>' not specified`
//!
//! AGENT.md §10.2: we must reproduce the same lines so log parity matches.

use crate::config::{Config, HINT_SCENES};

const KNOWN_MODULES: &[&str] = &[
    "switcher", "atrace", "sfanalysis", "sysfs", "sched", "cpu", "anim", "input", "log",
];

pub fn collect(cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();

    // 1. Unknown top-level modules.
    for (name, _body) in &cfg.modules.extras {
        out.push(format!(
            "CfgMgr: Unknown module '{name}' in config (not in {KNOWN_MODULES:?})"
        ));
    }

    // 2. Per-preset: warn about unknown scene names (upstream `CfgMgr: Base hint
    // of preset '<p>' not defined` is only emitted when the user actually binds a
    // missing one; we approximate by reporting missing default `*`).
    for (pname, preset) in &cfg.presets {
        if !preset.scenes.contains_key("*") {
            out.push(format!(
                "CfgMgr: Base hint of preset '{pname}' not defined (no '*' defaults)"
            ));
        }
        for scene in preset.scenes.keys() {
            if !HINT_SCENES.contains(&scene.as_str()) {
                out.push(format!(
                    "CfgMgr: Unknown key '{scene}' in base hint of preset '{pname}'"
                ));
            }
        }
        // Knobs per scene: flag unknown module prefix.
        for (scene, body) in &preset.scenes {
            for key in body.keys() {
                let prefix = key.split('.').next().unwrap_or("");
                if !prefix.is_empty() && !KNOWN_MODULES.contains(&prefix) {
                    out.push(format!(
                        "CfgMgr: Unknown key '{key}' of module '{prefix}' defined in base hint of preset '{pname}' (scene '{scene}')"
                    ));
                }
            }
        }
    }

    // 3. Initials with unknown module prefix.
    for key in cfg.initials.keys() {
        let prefix = key.split('.').next().unwrap_or("");
        if !prefix.is_empty() && !KNOWN_MODULES.contains(&prefix) {
            out.push(format!(
                "CfgMgr: Ignored initials '{key}' [unknown module '{prefix}']"
            ));
        }
    }

    out.sort();
    out
}