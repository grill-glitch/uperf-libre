//! Config types for uperf v3 (dev-22.09.04) — `meta` / `modules` / `initials` / `presets`.
//!
//! Loaded by `uperf-core` (running on the device) and by `uperf-cli` (host parity tool).
//!
//! **Real schema** (reverse-verified across 35 v3 configs):
//!   * `initials` — flat map of `<mod>.<param>` → `<value>` (e.g. `cpu.margin`, `sysfs.*`, `sched.scene`)
//!   * `presets.<preset>` — flat map of `<scene>` → `{<mod>.<param>: <value>}`
//!     where `<scene>` is `*` (default for this preset) or one of `idle / touch /
//!     trigger / gesture / switch / junk` (the 6 SfHint values from
//!     `docs/m1-static-reverse.md` §1.3). Cascade:
//!
//!     1. `presets[<mode>][<scene>]`
//!     2. `presets[<mode>][*]`
//!     3. `initials[<key>]`
//!
//! AGENT.md §8.2 said `presets.<mode>.base_hint.<scene>` — **none of the 35
//! configs use that nesting**; the flat `presets[preset][scene]` form is what
//! upstream's `uperf.cpp` reads.
//!
//! Anything that doesn't match a known knob is **kept** so the `warn` subcommand
//! can flag it.

#![allow(dead_code)]

use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use thiserror::Error;

/// Hint scene names that may appear as direct keys under `presets.<preset>`.
/// `*` is the wildcard (default for that preset, AGENT.md §8.2).
pub const HINT_SCENES: &[&str] = &["*", "idle", "touch", "trigger", "gesture", "switch", "junk"];

#[derive(Debug, Error)]
pub enum CfgError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid: {0}")]
    Invalid(String),
}

/// Top-level structure. We deliberately do NOT use `serde(deny_unknown_fields)` —
/// the upstream `uperf.cpp` warns about unknown module keys but accepts them.
#[derive(Debug, Default, Clone)]
pub struct Config {
    pub meta: Meta,
    pub modules: Modules,
    pub initials: Map<String, Value>, // dot-keyed (`cpu.margin` → value)
    pub presets: BTreeMap<String, Preset>,
    /// Raw  object (kept for hintDuration + future knob lookups).
    pub modules_raw: Option<Map<String, Value>>,
}

/// `meta`: { name, author }
#[derive(Debug, Default, Clone)]
pub struct Meta {
    pub name: String,
    pub author: String,
}

/// `modules`: { switcher, atrace, sfanalysis, sysfs, sched, cpu, anim, input, log, … }
/// Plus a free-form `extras` list for unknown module blocks so the warn tool
/// can flag them.
#[derive(Debug, Default, Clone)]
pub struct Modules {
    pub switcher: Option<Map<String, Value>>,
    pub atrace: Option<Map<String, Value>>,
    pub sfanalysis: Option<Map<String, Value>>,
    pub sysfs: Option<Map<String, Value>>,
    pub sched: Option<Map<String, Value>>,
    pub cpu: Option<Map<String, Value>>,
    pub anim: Option<Map<String, Value>>,
    pub input: Option<Map<String, Value>>,
    pub log: Option<Map<String, Value>>,
    pub extras: Vec<(String, Map<String, Value>)>,
}

/// `presets.<name>`: flat map of `<scene>` → `{<mod>.<param>: <value>}`. `*` is
/// the wildcard scene (preset default).
#[derive(Debug, Default, Clone)]
pub struct Preset {
    pub scenes: BTreeMap<String, Map<String, Value>>, // scene name → overrides
}

impl Config {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, CfgError> {
        let v: Value = serde_json::from_slice(bytes)?;
        Self::from_value(v)
    }

    pub fn from_value(v: Value) -> Result<Self, CfgError> {
        let mut c = Config::default();
        let obj = v
            .as_object()
            .ok_or_else(|| CfgError::Invalid("root not object".into()))?;

        // meta
        if let Some(meta) = obj.get("meta").and_then(|m| m.as_object()) {
            c.meta.name = meta.get("name").and_then(|v| v.as_str()).unwrap_or("").into();
            c.meta.author = meta.get("author").and_then(|v| v.as_str()).unwrap_or("").into();
        }

        // modules
        if let Some(mods) = obj.get("modules").and_then(|m| m.as_object()) {
            for (name, body) in mods {
                let body = body
                    .as_object()
                    .ok_or_else(|| CfgError::Invalid(format!("modules.{name} not object")))?
                    .clone();
                match name.as_str() {
                    "switcher" => c.modules.switcher = Some(body),
                    "atrace" => c.modules.atrace = Some(body),
                    "sfanalysis" => c.modules.sfanalysis = Some(body),
                    "sysfs" => c.modules.sysfs = Some(body),
                    "sched" => c.modules.sched = Some(body),
                    "cpu" => c.modules.cpu = Some(body),
                    "anim" => c.modules.anim = Some(body),
                    "input" => c.modules.input = Some(body),
                    "log" => c.modules.log = Some(body),
                    other => c.modules.extras.push((other.to_string(), body)),
                }
            }
        }

        // Keep the raw modules map for hintDuration and other module-level knobs.
        c.modules_raw = obj.get("modules").and_then(|m| m.as_object()).cloned();

        // initials: per-module block (e.g. `{ cpu: { margin: 0.2, ... }, sysfs: {...} }`)
        // OR a flat dotted map (`{ "cpu.margin": 0.2, ... }`). Both shapes appear
        // across upstream release (some configs use flat, some nested). We always
        // flatten to dotted keys so `Config::resolve` and the sysfs writer work
        // against one shape.
        if let Some(init) = obj.get("initials").and_then(|m| m.as_object()) {
            for (top, body) in init {
                let key_prefix = top.clone();
                if let Some(inner) = body.as_object() {
                    // Heuristic: if every inner key already starts with
                    // `top.` or doesn't contain `.`, treat this as an already-flat
                    // map at this top-level only. Otherwise treat as nested per-mod.
                    let looks_flat = inner.keys().all(|k| !k.contains('.'));
                    if looks_flat {
                        // Flat: keys are top-subkey (treat as dotted in their own right).
                        for (k, v) in inner {
                            let dotted = if k.contains('.') {
                                k.clone()
                            } else {
                                format!("{key_prefix}.{k}")
                            };
                            c.initials.insert(dotted, v.clone());
                        }
                    } else {
                        for (k, v) in inner {
                            c.initials.insert(format!("{key_prefix}.{k}"), v.clone());
                        }
                    }
                } else {
                    // Scalar at top-level (rare) — treat as `top.<value>`.
                    c.initials.insert(key_prefix, body.clone());
                }
            }
        }

        // presets.<name>: flat map of scene → {dotted-key: value}.
        if let Some(presets) = obj.get("presets").and_then(|m| m.as_object()) {
            for (name, body) in presets {
                let mut p = Preset::default();
                if let Some(scenes) = body.as_object() {
                    for (scene, body2) in scenes {
                        let m = body2
                            .as_object()
                            .ok_or_else(|| {
                                CfgError::Invalid(format!(
                                    "presets.{name}.{scene} not object"
                                ))
                            })?
                            .clone();
                        p.scenes.insert(scene.clone(), m);
                    }
                }
                c.presets.insert(name.clone(), p);
            }
        }

        Ok(c)
    }

    /// Effective value for `<dotted>` when applying preset `<mode>` and scene
    /// `<scene>`. Cascade (per `m1-static-reverse.md`):
    ///
    /// 1. `presets[<mode>][<scene>][<dotted>]`
    /// 2. `presets[<mode>][*][<dotted>]`   (wildcard)
    /// 3. `initials[<dotted>]`
    pub fn resolve<'a>(&'a self, mode: &str, scene: &str, dotted: &str) -> Option<&'a Value> {
        let p = self.presets.get(mode)?;
        if let Some(s) = p.scenes.get(scene) {
            if let Some(v) = s.get(dotted) {
                if !v.is_null() {
                    return Some(v);
                }
            }
        }
        if let Some(s) = p.scenes.get("*") {
            if let Some(v) = s.get(dotted) {
                if !v.is_null() {
                    return Some(v);
                }
            }
        }
        self.initials
            .get(dotted)
            .and_then(|v| if v.is_null() { None } else { Some(v) })
    }

    /// All scenes that have at least one knob in this preset (excludes `*`).
    /// `modules.log.level`, if present (`trace|debug|info|warn|...`).
    pub fn log_level(&self) -> Option<String> {
        self.modules
            .log
            .as_ref()
            .and_then(|m| m.get("level"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }

    /// `modules.input` thresholds the vendored `InputListener` needs.
    ///
    /// Returns `(swipeThd, gestureThdX, gestureThdY)`. `gestureDelayTime` and
    /// `holdEnterTime` are deliberately absent: `config/README.md` lines 99-100
    /// document both as 暂不使用 (unused), so the vendored listener's own values
    /// for them are already correct.
    pub fn input_thresholds(&self) -> Option<(f32, f32, f32)> {
        let m = self.modules.input.as_ref()?;
        let f = |k: &str| m.get(k).and_then(|v| v.as_f64()).map(|v| v as f32);
        Some((f("swipeThd")?, f("gestureThdX")?, f("gestureThdY")?))
    }

    /// `modules.atrace.enable`.
    ///
    /// Upstream has an `AtraceSwitcher` class; the mechanism is the vendored dfps
    /// `utils/atrace.c`, whose two trace_marker paths are exactly the strings the
    /// uperf binary contains.
    pub fn atrace_enabled(&self) -> Option<bool> {
        self.modules
            .atrace
            .as_ref()
            .and_then(|m| m.get("enable"))
            .and_then(|v| v.as_bool())
    }

    /// `modules.input.enable`. All 63 shipped configs set it true.
    pub fn input_enabled(&self) -> Option<bool> {
        self.modules
            .input
            .as_ref()
            .and_then(|m| m.get("enable"))
            .and_then(|v| v.as_bool())
    }

    /// `meta.name` / `meta.author`, as upstream prints them:
    /// `Config '<name>' by '<author>'`.
    pub fn meta_ident(&self) -> (String, String) {
        (self.meta.name.clone(), self.meta.author.clone())
    }

    /// The config's `presets` keys, **alphabetically** (they are stored in a
    /// `BTreeMap`, so this is not the JSON order). Only used to validate a
    /// referenced preset name, where order is irrelevant.
    ///
    /// Used to validate values that reference a preset by name but live outside
    /// the JSON — `cur_powermode.txt` and `perapp_powermode.txt`. Upstream's
    /// message for a bad one is `Perapp preset '{}' for app '{}' not defined in
    /// config` / `Failed to switch to undefined preset '{}'`.
    pub fn preset_names(&self) -> Vec<String> {
        self.presets.keys().cloned().collect()
    }

    pub fn scenes_in_preset(&self, mode: &str) -> Vec<String> {
        let Some(p) = self.presets.get(mode) else { return vec![]; };
        p.scenes
            .keys()
            .filter(|k| *k != "*")
            .cloned()
            .collect()
    }

    /// Every `sysfs.*` key that appears anywhere (initials or any preset scene).
    /// Used by the sysfs planner to build the write set.
    pub fn all_sysfs_keys(&self) -> Vec<String> {
        let mut out = std::collections::BTreeSet::new();
        for k in self.initials.keys() {
            if k.starts_with("sysfs.") {
                out.insert(k.clone());
            }
        }
        for p in self.presets.values() {
            for scene in p.scenes.values() {
                for k in scene.keys() {
                    if k.starts_with("sysfs.") {
                        out.insert(k.clone());
                    }
                }
            }
        }
        out.into_iter().collect()
    }
    /// `modules.sysfs.knob` — the `{knob_name: absolute_path}` table.
    ///
    /// This is the ONLY place sysfs paths come from: the upstream binary has no
    /// hardcoded paths at all (see `docs/m4-evidence.md` §1). Missing/empty
    /// table → empty map (all sysfs knobs then resolve to nothing).
    pub fn sysfs_knob_table(&self) -> std::collections::BTreeMap<String, String> {
        let mut out = std::collections::BTreeMap::new();
        let Some(m) = self.modules_raw.as_ref() else {
            return out;
        };
        let Some(knob) = m
            .get("sysfs")
            .and_then(|s| s.as_object())
            .and_then(|s| s.get("knob"))
            .and_then(|k| k.as_object())
        else {
            return out;
        };
        for (name, path) in knob {
            if let Some(p) = path.as_str() {
                out.insert(name.clone(), p.to_string());
            }
        }
        out
    }

    /// Borrow the `modules` block as a raw map (used for hintDuration lookup).
    pub fn modules_map(&self) -> Option<&Map<String, Value>> {
        self.modules_raw.as_ref()
    }
}


impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(
            f,
            "meta: name='{}' author='{}'",
            self.meta.name, self.meta.author
        )?;
        writeln!(
            f,
            "modules: {} known, {} extras",
            self.modules.count_known(),
            self.modules.extras.len()
        )?;
        writeln!(f, "initials: {} keys", self.initials.len())?;
        for k in self.initials.keys() {
            writeln!(f, "  init.{}", k)?;
        }
        for (name, preset) in &self.presets {
            let mut keys: Vec<&String> = preset.scenes.keys().collect();
            keys.sort();
            writeln!(f, "preset '{}' (scenes: {})", name, keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(","))?;
        }
        Ok(())
    }
}

impl Modules {
    pub fn count_known(&self) -> usize {
        let mut n = 0;
        if self.switcher.is_some() { n += 1; }
        if self.atrace.is_some() { n += 1; }
        if self.sfanalysis.is_some() { n += 1; }
        if self.sysfs.is_some() { n += 1; }
        if self.sched.is_some() { n += 1; }
        if self.cpu.is_some() { n += 1; }
        if self.anim.is_some() { n += 1; }
        if self.input.is_some() { n += 1; }
        if self.log.is_some() { n += 1; }
        n
    }
}

// Kept here so the `Deserialize` import stays used (silences the rustc warning
// against an unused trait import that some Cargo setups trip on).
#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct PhantomSerdeImportUser(u8);