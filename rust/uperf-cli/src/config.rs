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
#[derive(Debug, Default)]
pub struct Config {
    pub meta: Meta,
    pub modules: Modules,
    pub initials: Map<String, Value>, // dot-keyed (`cpu.margin` → value)
    pub presets: BTreeMap<String, Preset>,
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
#[derive(Debug, Default)]
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
#[derive(Debug, Default)]
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

        // initials: flat map of dot-key → value.
        if let Some(init) = obj.get("initials").and_then(|m| m.as_object()) {
            for (k, v) in init {
                c.initials.insert(k.clone(), v.clone());
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
    pub fn scenes_in_preset(&self, mode: &str) -> Vec<String> {
        let Some(p) = self.presets.get(mode) else { return vec![]; };
        p.scenes
            .keys()
            .filter(|k| *k != "*")
            .cloned()
            .collect()
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