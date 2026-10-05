//! Orchestrator — event → hint FSM → scene → sysfs write sequence.
//!
//! AGENT.md §3 "switcher/profile/sysfs" pipeline. This is the M4 wiring:
//! the hint FSM decides the scene, `uperf-config` resolves the preset's knobs
//! for that scene, and the result becomes a concrete `path=value` list.
//!
//! The write target depends on the [`Sink`]:
//!   * `CollectingSink` — tests / parity runs (keeps the sequence in memory)
//!   * `UnderRootSink` — offline validation: every path is rewritten under a
//!     fake root (`<root>/sys/...`) so nothing on the device is touched
//!   * real device writes land in a later step (they need fd caching plus the
//!     read-modify-write retry semantics of the upstream writer classes)

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use uperf_config::{Config, SysfsWrite as PlanWrite};

use crate::hint::{HintDurations, HintState, SfHint};

/// One planned sysfs write (concrete path + final value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysfsWrite {
    pub path: String,
    pub value: String,
}

/// Where writes go.
pub trait Sink {
    fn write(&mut self, w: &SysfsWrite);
}

/// In-memory sink (tests, parity runs).
#[derive(Default, Debug)]
pub struct CollectingSink {
    pub writes: Vec<SysfsWrite>,
}
impl Sink for CollectingSink {
    fn write(&mut self, w: &SysfsWrite) {
        self.writes.push(w.clone());
    }
}

/// Rewrite every path under a fake root, then write to the real fs.
pub struct UnderRootSink {
    pub root: PathBuf,
    pub written: Vec<SysfsWrite>,
    pub failed: Vec<(String, String)>,
}
impl UnderRootSink {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            written: Vec::new(),
            failed: Vec::new(),
        }
    }
    fn mapped(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }
}
impl Sink for UnderRootSink {
    fn write(&mut self, w: &SysfsWrite) {
        let target = self.mapped(&w.path);
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&target, format!("{}\n", w.value)) {
            Ok(()) => self.written.push(w.clone()),
            Err(e) => self.failed.push((w.path.clone(), e.to_string())),
        }
    }
}

/// Hint FSM + (optional) config → sysfs write sequences.
pub struct Orchestrator {
    hint: HintState,
    cfg: Option<Config>,
    mode: String,
    scene: String,
    pending: Vec<SysfsWrite>,
    /// Package of the process `topapp.pkgName` last reported.
    top_app: Option<String>,
    /// Raw screen state from `offscreen.state` (not the FSM's interpretation).
    /// The per-app switcher needs the flag itself: `offscreen.state` maps to the
    /// `Switch` hint, but `Switch` also covers other transitions.
    offscreen: bool,
    /// Bumped whenever `scene` or `top_app` changes, so the context scheduler
    /// can tell "something a rule depends on moved" from "a tick went by"
    /// without polling strings.
    generation: u64,
}

impl Orchestrator {
    /// Without a config the orchestrator only tracks hint transitions and emits
    /// a synthetic marker per transition (used by the early smoke tests).
    pub fn new(durations: HintDurations, mode: impl Into<String>) -> Self {
        Self {
            hint: HintState::new(durations),
            cfg: None,
            mode: mode.into(),
            scene: "idle".into(),
            pending: Vec::new(),
            top_app: None,
            offscreen: false,
            generation: 0,
        }
    }

    /// Full constructor: hint durations come from the config itself.
    pub fn with_config(cfg: Config, mode: impl Into<String>) -> Self {
        let durations = cfg
            .modules_map()
            .map(HintDurations::from_modules)
            .unwrap_or_default();
        Self {
            hint: HintState::new(durations),
            cfg: Some(cfg),
            mode: mode.into(),
            scene: "idle".into(),
            pending: Vec::new(),
            top_app: None,
            offscreen: false,
            generation: 0,
        }
    }

    /// Package of the current top app, from `topapp.pkgName` events.
    ///
    /// This is what `/HOME_PACKAGE/` does NOT mean: that token is the launcher
    /// package (upstream logs `Current home is '<pkg>'`), whereas this drives the
    /// `fg`/`bg` half of a rule's scene. The context scheduler needs both.
    pub fn top_app(&self) -> Option<&str> {
        self.top_app.as_deref()
    }

    /// Monotonic counter for "scene or top app changed".
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the screen is off, straight from `offscreen.state`.
    pub fn offscreen(&self) -> bool {
        self.offscreen
    }

    /// Feed a hint that arrived out of band — the `sfanalysis.hint` byte written
    /// by the vendor injection library, which is not an event-bus topic.
    pub fn on_sf_hint(&mut self, h: crate::hint::SfHint) -> bool {
        match self.hint.process(h) {
            Some(t) => {
                let new_scene = Self::scene_for_hint(t.to);
                if new_scene != self.scene {
                    self.scene = new_scene.to_string();
                    self.generation += 1;
                    self.plan_current_scene();
                }
                true
            }
            None => false,
        }
    }

    pub fn mode(&self) -> &str {
        &self.mode
    }
    pub fn current_scene(&self) -> &str {
        &self.scene
    }
    pub fn current_hint(&self) -> SfHint {
        self.hint.current()
    }

    /// Switch preset (upstream watches `cur_powermode.txt` for this). Plans the
    /// new preset for the scene we're currently in.
    pub fn set_mode(&mut self, mode: impl Into<String>) {
        self.mode = mode.into();
        self.generation += 1;
        self.plan_current_scene();
    }

    /// Apply an inbound event. A scene change re-resolves the preset and queues
    /// the corresponding sysfs writes.
    pub fn on_event(&mut self, ev: &crate::topic_dispatch::Event) {
        if let crate::topic_dispatch::Event::Offscreen(b) = ev {
            if self.offscreen != *b {
                self.offscreen = *b;
                self.generation += 1;
            }
        }
        if let crate::topic_dispatch::Event::Topapp(pkg) = ev {
            if self.top_app.as_deref() != Some(pkg.as_str()) {
                self.top_app = Some(pkg.clone());
                self.generation += 1;
            }
        }
        let incoming = event_to_hint(ev);
        if let Some(transition) = self.hint.process(incoming) {
            let new_scene = Self::scene_for_hint(transition.to);
            if new_scene != self.scene {
                self.scene = new_scene.to_string();
                self.generation += 1;
                self.plan_current_scene();
            }
        }
    }

    fn plan_current_scene(&mut self) {
        let Some(cfg) = self.cfg.as_ref() else {
            self.pending.push(SysfsWrite {
                path: format!("__hint_transition__{}", self.scene),
                value: self.scene.clone(),
            });
            return;
        };
        let planned: Vec<PlanWrite> = uperf_config::plan_for_config(cfg, &self.mode, &self.scene);
        for w in planned {
            self.pending.push(SysfsWrite {
                path: w.path,
                value: w.value,
            });
        }
    }

    pub fn drain<S: Sink>(&mut self, sink: &mut S) {
        for w in std::mem::take(&mut self.pending) {
            sink.write(&w);
        }
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Map a SfHint to a scene name (AGENT.md §8.1).
    pub fn scene_for_hint(h: SfHint) -> &'static str {
        match h {
            SfHint::Idle => "idle",
            SfHint::Touch => "touch",
            SfHint::Trigger => "trigger",
            SfHint::Gesture => "gesture",
            SfHint::Switch => "switch",
            SfHint::Junk => "junk",
            SfHint::Unknown => "idle",
        }
    }

    /// Scenes present in the current preset.
    pub fn scenes(&self) -> Vec<String> {
        self.cfg
            .as_ref()
            .map(|c| c.scenes_in_preset(&self.mode))
            .unwrap_or_default()
    }

    /// Effective `sysfs.*` knobs for the current (mode, scene).
    pub fn resolved_knobs(&self) -> BTreeMap<String, String> {
        let Some(cfg) = self.cfg.as_ref() else {
            return BTreeMap::new();
        };
        cfg.all_sysfs_keys()
            .into_iter()
            .filter_map(|k| {
                cfg.resolve(&self.mode, &self.scene, &k)
                    .map(|v| (k, uperf_config::serialize_value(v)))
            })
            .collect()
    }
}

fn event_to_hint(ev: &crate::topic_dispatch::Event) -> SfHint {
    use crate::topic_dispatch::Event::*;
    match ev {
        Touch(true) => SfHint::Touch,
        Touch(false) => SfHint::Trigger,
        Btn(_) => SfHint::Idle,
        InputState {
            hold: true,
            swipe: false,
            gesture: false,
        } => SfHint::Touch,
        InputState { swipe: true, .. } => SfHint::Trigger,
        InputState { gesture: true, .. } => SfHint::Gesture,
        InputState { .. } => SfHint::Idle,
        Topapp(_) => SfHint::Idle,
        Offscreen(true) => SfHint::Switch,
        Offscreen(false) => SfHint::Touch,
        CgroupList { .. } | CgroupUpdate(_) => SfHint::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topic_dispatch::Event;
    use serde_json::json;

    fn durations() -> HintDurations {
        let mut m = serde_json::Map::new();
        m.insert(
            "switcher".into(),
            json!({"hintDuration": {"idle": 0.0, "touch": 4.0,
                "trigger": 0.03, "gesture": 0.1, "switch": 0.4, "junk": 0.06}}),
        );
        HintDurations::from_modules(&m)
    }

    fn tiny_cfg() -> Config {
        Config::from_value(json!({
            "meta": {"name": "test", "author": "t"},
            "modules": {
                "switcher": {"hintDuration": {"idle": 0.0, "touch": 4.0,
                    "trigger": 0.03, "gesture": 0.1, "switch": 0.4, "junk": 0.06}},
                // The knob table is the ONLY source of sysfs paths (see
                // uperf-config/src/sysfs.rs).
                "sysfs": {"enable": true, "knob": {
                    "cpusetTa": "/dev/cpuset/top-app/cpus",
                    "cpusetRe": "/dev/cpuset/restricted/cpus",
                    "cpuMax": "/sys/module/msm_performance/parameters/cpu_max_freq"
                }}
            },
            "initials": {
                "sysfs": {"cpusetTa": "0-7", "cpusetRe": "0-6", "cpuMax": "2419200"}
            },
            "presets": {
                "balance": {
                    "*": {"cpu.margin": 0.2},
                    "touch": {"cpu.margin": 0.25},
                    "switch": {"sysfs.cpusetTa": "0-3"}
                }
            }
        }))
        .expect("cfg parses")
    }

    #[test]
    fn without_config_emits_marker_only() {
        let mut o = Orchestrator::new(durations(), "balance");
        o.on_event(&Event::Touch(true));
        assert_eq!(o.current_scene(), "touch");
        let mut sink = CollectingSink::default();
        o.drain(&mut sink);
        assert_eq!(sink.writes.len(), 1);
        assert!(sink.writes[0].path.starts_with("__hint_transition__"));
    }

    #[test]
    fn with_config_emits_real_paths() {
        let mut o = Orchestrator::with_config(tiny_cfg(), "balance");
        o.on_event(&Event::Touch(true));
        assert_eq!(o.current_scene(), "touch");
        let mut sink = CollectingSink::default();
        o.drain(&mut sink);
        assert!(!sink.writes.is_empty(), "expected real sysfs writes");
        assert!(
            sink.writes
                .iter()
                .any(|w| w.path == "/dev/cpuset/top-app/cpus"),
            "cpusetTa not planned: {:?}",
            sink.writes
        );
        assert!(
            sink.writes
                .iter()
                .any(|w| w.path.contains("msm_performance")),
            "cpuMax not planned: {:?}",
            sink.writes
        );
    }

    #[test]
    fn scene_override_wins_over_initials() {
        let mut o = Orchestrator::with_config(tiny_cfg(), "balance");
        o.on_event(&Event::Offscreen(true)); // -> switch scene
        assert_eq!(o.current_scene(), "switch");
        let mut sink = CollectingSink::default();
        o.drain(&mut sink);
        let ta = sink
            .writes
            .iter()
            .find(|w| w.path == "/dev/cpuset/top-app/cpus")
            .expect("cpusetTa missing in switch scene");
        assert_eq!(ta.value, "0-3", "switch scene should override initials 0-7");
    }

    #[test]
    fn under_root_sink_writes_files() {
        let dir = std::env::temp_dir().join(format!("uperf_orch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut o = Orchestrator::with_config(tiny_cfg(), "balance");
        o.on_event(&Event::Touch(true));
        let mut sink = UnderRootSink::new(&dir);
        o.drain(&mut sink);
        assert!(!sink.written.is_empty(), "nothing written: {:?}", sink.failed);
        let f = dir.join("dev/cpuset/top-app/cpus");
        assert!(f.exists(), "expected {} to exist", f.display());
        assert_eq!(std::fs::read_to_string(&f).unwrap().trim(), "0-7");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_touch_is_a_refresh_not_a_new_plan() {
        let mut o = Orchestrator::with_config(tiny_cfg(), "balance");
        o.on_event(&Event::Touch(true));
        o.drain(&mut CollectingSink::default());
        o.on_event(&Event::Touch(true));
        let mut s2 = CollectingSink::default();
        o.drain(&mut s2);
        assert!(s2.writes.is_empty(), "refresh re-planned: {:?}", s2.writes);
    }

    #[test]
    fn scene_for_hint_table() {
        assert_eq!(Orchestrator::scene_for_hint(SfHint::Idle), "idle");
        assert_eq!(Orchestrator::scene_for_hint(SfHint::Touch), "touch");
        assert_eq!(Orchestrator::scene_for_hint(SfHint::Trigger), "trigger");
        assert_eq!(Orchestrator::scene_for_hint(SfHint::Gesture), "gesture");
        assert_eq!(Orchestrator::scene_for_hint(SfHint::Switch), "switch");
        assert_eq!(Orchestrator::scene_for_hint(SfHint::Junk), "junk");
        assert_eq!(Orchestrator::scene_for_hint(SfHint::Unknown), "idle");
    }
}