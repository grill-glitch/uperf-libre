//! Context-aware task scheduler (`modules.sched`).
//!
//! Spec: `config/README.md` §"sched/识别上下文的任务调度器" (lines 158-262, 323-327).
//!
//! The config defines four tables plus a rule list:
//!
//! ```jsonc
//! "sched": {
//!   "enable": true,
//!   "cpumask":  { "all": [0,1,2,3,4,5,6,7], "c0": [0,1,2,3], ... },   // group name -> cpu ids
//!   "affinity": { "ui": { "bg": "", "fg": "all", "idle": "all", "touch": "c1", "boost": "all" }, ... },
//!   "prio":     { "ui": { "bg": -3, "fg": 120, "idle": 110, "touch": 98, "boost": 116 }, ... },
//!   "rules": [ { "name": "Launcher", "regex": "/HOME_PACKAGE/", "pinned": true,
//!                "rules": [ { "k": "/MAIN_THREAD/", "ac": "crit", "pc": "rtusr" }, ... ] } ]
//! }
//! ```
//!
//! Resolution: process name → first matching `rules[i].regex` → scene → first
//! matching `rules[i].rules[j].k` for the thread name → (`ac`, `pc`) →
//! `affinity[ac][scene]` (a cpumask name, empty = leave alone) and
//! `prio[pc][scene]` (a code, 0 = leave alone).
//!
//! `pinned` (README line 248) means the rule is *always* applied as if the
//! process were the top-visible one, so the scene comes from the hint FSM
//! (`idle`/`touch`/`boost`) rather than from bg/fg detection.
//!
//! Substitutions happen on the *pattern text* before compiling, unescaped:
//! `/HOME_PACKAGE/` → the launcher package, `/MAIN_THREAD/` → that process's main
//! thread name. Unescaped matters: configs rely on `.` in a package name acting
//! as a wildcard (e.g. bare `com.android.systemui`, `^com.tencent.mobileqq|...`).

#![allow(dead_code)]

use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// The scheduler's dynamic scene (README line 327: `idle`, `touch`, `boost`),
/// plus `bg`/`fg` which are derived per process rather than from the FSM.
pub const SCENES: [&str; 5] = ["bg", "fg", "idle", "touch", "boost"];

/// A decoded `prio` code (README lines 217-224).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedPolicy {
    /// `0` — leave the SCHED class alone.
    Skip,
    /// `1..=98` — `SCHED_FIFO` with that static real-time priority.
    Fifo(i32),
    /// `100..=139` — `SCHED_NORMAL` with `nice = code - 120`.
    Normal { nice: i32 },
    /// `-1` — `SCHED_NORMAL` (nice 0).
    NormalDefault,
    /// `-2` — `SCHED_BATCH`.
    Batch,
    /// `-3` — `SCHED_IDLE`.
    Idle,
}

impl SchedPolicy {
    /// Decode a config code. Returns `None` for values outside the documented
    /// table so a bad config is loud rather than silently ignored.
    pub fn decode(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::Skip,
            1..=98 => Self::Fifo(code as i32),
            100..=139 => Self::Normal { nice: (code - 120) as i32 },
            -1 => Self::NormalDefault,
            -2 => Self::Batch,
            -3 => Self::Idle,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SchedRuleItem {
    /// Thread-name pattern (abbr. keyword).
    pub k: String,
    /// Affinity class — must exist in `affinity`.
    pub ac: String,
    /// Priority class — must exist in `prio`.
    pub pc: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SchedRule {
    pub name: String,
    pub regex: String,
    pub pinned: bool,
    pub rules: Vec<SchedRuleItem>,
}

/// The parsed tables, before any regex is compiled.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchedConfig {
    pub enable: bool,
    pub cpumask: BTreeMap<String, Vec<usize>>,
    pub affinity: BTreeMap<String, BTreeMap<String, String>>,
    pub prio: BTreeMap<String, BTreeMap<String, i64>>,
    pub rules: Vec<SchedRule>,
}

impl SchedConfig {
    pub fn from_value(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        let mut c = SchedConfig {
            enable: o.get("enable").and_then(|x| x.as_bool()).unwrap_or(false),
            ..Default::default()
        };
        if let Some(m) = o.get("cpumask").and_then(|x| x.as_object()) {
            for (k, list) in m {
                let cpus: Vec<usize> = list
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as usize)).collect())
                    .unwrap_or_default();
                c.cpumask.insert(k.clone(), cpus);
            }
        }
        if let Some(a) = o.get("affinity").and_then(|x| x.as_object()) {
            for (cls, scenes) in a {
                let mut inner = BTreeMap::new();
                if let Some(sm) = scenes.as_object() {
                    for (scene, mask) in sm {
                        inner.insert(
                            scene.clone(),
                            mask.as_str().unwrap_or_default().to_string(),
                        );
                    }
                }
                c.affinity.insert(cls.clone(), inner);
            }
        }
        if let Some(p) = o.get("prio").and_then(|x| x.as_object()) {
            for (cls, scenes) in p {
                let mut inner = BTreeMap::new();
                if let Some(sm) = scenes.as_object() {
                    for (scene, code) in sm {
                        inner.insert(scene.clone(), code.as_i64().unwrap_or(0));
                    }
                }
                c.prio.insert(cls.clone(), inner);
            }
        }
        if let Some(rs) = o.get("rules").and_then(|x| x.as_array()) {
            for r in rs {
                let Some(ro) = r.as_object() else { continue };
                let items = ro
                    .get("rules")
                    .and_then(|x| x.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|it| {
                                let io = it.as_object()?;
                                Some(SchedRuleItem {
                                    k: io.get("k")?.as_str()?.to_string(),
                                    ac: io.get("ac")?.as_str()?.to_string(),
                                    pc: io.get("pc")?.as_str()?.to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                c.rules.push(SchedRule {
                    name: ro.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    regex: ro.get("regex").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    pinned: ro.get("pinned").and_then(|x| x.as_bool()).unwrap_or(false),
                    rules: items,
                });
            }
        }
        Some(c)
    }

    pub fn from_modules(modules: &serde_json::Map<String, Value>) -> Option<Self> {
        Self::from_value(modules.get("sched")?)
    }
}

/// A thread rule. `k` is compiled lazily: it may contain `/MAIN_THREAD/`, which
/// is only resolvable for a concrete process (see [`SchedPlanner`]).
#[derive(Debug, Clone)]
pub struct CompiledItem {
    pub k: String,
    pub ac: String,
    pub pc: String,
}

/// A compiled process rule.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub name: String,
    pub regex: String,
    pub regex_re: Regex,
    pub pinned: bool,
    pub rules: Vec<CompiledItem>,
}

/// What to apply to one thread.
#[derive(Debug, Clone, PartialEq)]
pub struct SchedDecision {
    /// CPU set to bind to; `None` = leave the affinity alone (mask name empty).
    pub cpus: Option<Vec<usize>>,
    /// SCHED class / priority to apply.
    pub policy: SchedPolicy,
    /// Which process rule produced this (for diagnostics).
    pub rule: String,
    /// The scene that was used (`bg`/`fg`/`idle`/`touch`/`boost`).
    pub scene: String,
    /// Affinity / priority class names that matched.
    pub ac: String,
    pub pc: String,
}

/// The only unrecoverable config defect: a pattern that cannot be compiled.
/// Everything else is an *anomaly* — recorded and tolerated, because shipped
/// configs contain them (see [`Anomaly`]).
#[derive(Debug, thiserror::Error)]
pub enum SchedError {
    #[error("rule '{rule}' regex {pattern:?} failed to compile: {source}")]
    Regex {
        rule: String,
        pattern: String,
        #[source]
        source: regex::Error,
    },
    /// An empty process pattern matches *everything*, which is never intended:
    /// the shipped configs use the literal `"."` when they mean that. This guard
    /// exists because a failed `/HOME_PACKAGE/` substitution once produced an
    /// empty pattern and silently matched every process on the device.
    #[error("rule '{rule}' has an empty process pattern after substitution")]
    EmptyProcessPattern { rule: String },
}

/// A defect that the shipped configs actually contain, so it must be tolerated
/// the way upstream tolerates it, but which is worth surfacing.
///
/// Evidence (see the `all_sched_configs` gate):
///   * `sdm8g1+.json` is the reference config and defines `affinity.fuck` +
///     `prio.fuck`; `sdm8g2.json` and `sdm8g3.json` define `prio.fuck` but
///     forgot `affinity.fuck`; `sdm7g1.json` defines neither, yet all three
///     reference `ac: "fuck"` / `pc: "fuck"` in their bloatware rule.
///   * Therefore a class that does not exist must be a no-op for that aspect,
///     not a startup failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anomaly {
    /// A rule references an `ac` that is absent from `affinity` -> no affinity change.
    UndefinedAffinityClass { rule: String, class: String },
    /// A rule references a `pc` that is absent from `prio` -> no priority change.
    UndefinedPrioClass { rule: String, class: String },
    /// An `affinity` value names a cpumask group (or one comma-separated part of
    /// one) that is not defined in `cpumask` -> the whole mask is unusable.
    UnknownCpumaskName { class: String, scene: String, name: String },
    /// A `prio` code outside the documented table -> treated as `skip`.
    BadPrioCode { class: String, scene: String, code: i64 },
}

impl std::fmt::Display for Anomaly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UndefinedAffinityClass { rule, class } => write!(
                f,
                "rule {rule:?} references affinity class {class:?}, which the config does not define (no affinity change)"
            ),
            Self::UndefinedPrioClass { rule, class } => write!(
                f,
                "rule {rule:?} references prio class {class:?}, which the config does not define (no priority change)"
            ),
            Self::UnknownCpumaskName { class, scene, name } => write!(
                f,
                "affinity[{class}][{scene}] names cpumask {name:?}, which the config does not define"
            ),
            Self::BadPrioCode { class, scene, code } => write!(
                f,
                "prio[{class}][{scene}] = {code} is outside the documented table"
            ),
        }
    }
}

/// Compiles the config and resolves decisions.
///
/// `/HOME_PACKAGE/` is substituted at construction time. `/MAIN_THREAD/` is
/// per-process (README line 255: it is *that process's* main thread name), so
/// those patterns are compiled lazily and cached by their substituted text.
#[derive(Debug)]
pub struct SchedPlanner {
    cfg: SchedConfig,
    rules: Vec<CompiledRule>,
    home_package: String,
    /// Substituted-pattern -> compiled regex, so a per-process recompile of a
    /// `/MAIN_THREAD/` pattern happens once per distinct main-thread name.
    thread_cache: HashMap<String, Regex>,
    /// Tolerated config defects, collected at construction.
    anomalies: Vec<Anomaly>,
}

impl SchedPlanner {
    /// `home_package` is the current launcher package (`/HOME_PACKAGE/`).
    pub fn new(cfg: SchedConfig, home_package: &str) -> Result<Self, SchedError> {
        let mut rules = Vec::with_capacity(cfg.rules.len());
        for r in &cfg.rules {
            let pattern = substitute_home(&r.regex, home_package);
            if pattern.is_empty() {
                return Err(SchedError::EmptyProcessPattern { rule: r.name.clone() });
            }
            let regex_re = Regex::new(&pattern).map_err(|source| SchedError::Regex {
                rule: r.name.clone(),
                pattern: pattern.clone(),
                source,
            })?;
            let mut items = Vec::with_capacity(r.rules.len());
            for it in &r.rules {
                // `/MAIN_THREAD/` cannot be resolved yet (it is per process).
                // Compile only what is resolvable now; the rest is verified at
                // construction too so a bad pattern fails at startup rather than
                // in the scheduler hot path.
                let probe = substitute_home(&it.k, home_package);
                if !probe.contains("/MAIN_THREAD/") {
                    Regex::new(&probe).map_err(|source| SchedError::Regex {
                        rule: r.name.clone(),
                        pattern: probe.clone(),
                        source,
                    })?;
                }
                items.push(CompiledItem {
                    k: it.k.clone(),
                    ac: it.ac.clone(),
                    pc: it.pc.clone(),
                });
            }
            rules.push(CompiledRule {
                name: r.name.clone(),
                regex: r.regex.clone(),
                regex_re,
                pinned: r.pinned,
                rules: items,
            });
        }
        let mut me = Self {
            cfg: cfg.clone(),
            rules,
            home_package: home_package.to_string(),
            thread_cache: HashMap::new(),
            anomalies: Vec::new(),
        };
        me.collect_anomalies();
        Ok(me)
    }

    /// Record (do not reject) every dangling class / cpumask / out-of-range prio
    /// code. Shipped configs contain these, so rejecting would refuse to run
    /// configs upstream runs happily; recording them keeps the behaviour visible
    /// and lets the acceptance gate whitelist exactly the known set.
    fn collect_anomalies(&mut self) {
        let mut out = Vec::new();
        for r in &self.cfg.rules {
            for it in &r.rules {
                if !self.cfg.affinity.contains_key(&it.ac) {
                    out.push(Anomaly::UndefinedAffinityClass {
                        rule: r.name.clone(),
                        class: it.ac.clone(),
                    });
                }
                if !self.cfg.prio.contains_key(&it.pc) {
                    out.push(Anomaly::UndefinedPrioClass {
                        rule: r.name.clone(),
                        class: it.pc.clone(),
                    });
                }
            }
        }
        for (class, scenes) in &self.cfg.affinity {
            for (scene, mask) in scenes {
                for part in mask.split(',').filter(|p| !p.is_empty()) {
                    if !self.cfg.cpumask.contains_key(part) {
                        out.push(Anomaly::UnknownCpumaskName {
                            class: class.clone(),
                            scene: scene.clone(),
                            name: part.to_string(),
                        });
                    }
                }
            }
        }
        for (class, scenes) in &self.cfg.prio {
            for (scene, code) in scenes {
                if SchedPolicy::decode(*code).is_none() {
                    out.push(Anomaly::BadPrioCode {
                        class: class.clone(),
                        scene: scene.clone(),
                        code: *code,
                    });
                }
            }
        }
        self.anomalies = out;
    }

    /// Config defects that were tolerated, in config order.
    pub fn anomalies(&self) -> &[Anomaly] {
        &self.anomalies
    }

    /// Resolve an `affinity` value into a CPU list.
    ///
    /// The value is a cpumask group name, or **several comma-separated group
    /// names** whose CPU sets are unioned — `sdm8g3.json` ships
    /// `"affinity": {"bg": {"fg": "c1,c2", ...}}` (8 occurrences across the
    /// tree), and upstream clearly tolerates them. An empty value means "leave
    /// the affinity alone".
    pub fn resolve_mask(&self, mask: &str) -> Option<Vec<usize>> {
        let mut cpus: Vec<usize> = Vec::new();
        let mut any = false;
        for part in mask.split(',').filter(|p| !p.is_empty()) {
            let Some(list) = self.cfg.cpumask.get(part) else {
                // An unknown part makes the whole value unusable rather than
                // silently binding to a subset.
                return None;
            };
            any = true;
            for c in list {
                if !cpus.contains(c) {
                    cpus.push(*c);
                }
            }
        }
        if !any {
            return None;
        }
        cpus.sort_unstable();
        Some(cpus)
    }

    pub fn config(&self) -> &SchedConfig {
        &self.cfg
    }
    pub fn enabled(&self) -> bool {
        self.cfg.enable
    }
    pub fn home_package(&self) -> &str {
        &self.home_package
    }

    /// Which process rule (if any) matches `proc_name`.
    pub fn match_process(&self, proc_name: &str) -> Option<&CompiledRule> {
        self.rules.iter().find(|r| r.regex_re.is_match(proc_name))
    }

    /// The scene a process runs in.
    ///
    /// `pinned` rules are always treated as top-visible (README line 248), so
    /// they take the FSM scene. Otherwise a top app takes the FSM scene and
    /// anything else is `bg`.
    pub fn scene_for<'a>(
        &self,
        rule: Option<&CompiledRule>,
        is_top_app: bool,
        fsm_scene: &'a str,
    ) -> &'a str {
        let pinned = rule.map(|r| r.pinned).unwrap_or(false);
        if pinned || is_top_app {
            fsm_scene
        } else {
            "bg"
        }
    }

    /// Full resolution for one thread of one process.
    ///
    /// `fsm_scene` is the scheduler scene from the hint FSM (`idle`/`touch`/
    /// `boost`, via `initials.sched.scene` / `presets.<mode>.<scene>.sched.scene`).
    pub fn decide(
        &mut self,
        proc_name: &str,
        is_top_app: bool,
        fsm_scene: &str,
        main_thread: &str,
        thread_name: &str,
    ) -> Option<SchedDecision> {
        if !self.cfg.enable {
            return None;
        }
        let rule_idx = self.rules.iter().position(|r| r.regex_re.is_match(proc_name))?;
        let scene = {
            let pinned = self.rules[rule_idx].pinned;
            if pinned || is_top_app {
                fsm_scene
            } else {
                "bg"
            }
        }
        .to_string();

        // Gather the (k, ac, pc) triples first so the borrow on self.rules ends
        // before we touch the thread cache.
        let triples: Vec<(String, String, String)> = self.rules[rule_idx]
            .rules
            .iter()
            .map(|it| (it.k.clone(), it.ac.clone(), it.pc.clone()))
            .collect();

        for (k, ac, pc) in triples {
            let matched = {
                let text = substitute_main_thread(&k, main_thread);
                match self.thread_cache.get(&text) {
                    Some(re) => re.is_match(thread_name),
                    None => match Regex::new(&text) {
                        Ok(re) => {
                            let m = re.is_match(thread_name);
                            self.thread_cache.insert(text, re);
                            m
                        }
                        Err(_) => false,
                    },
                }
            };
            if !matched {
                continue;
            }
            // Affinity: the mask name must resolve to a cpumask; an empty name
            // means "leave it alone".
            let mask_name = self
                .cfg
                .affinity
                .get(&ac)
                .and_then(|s| s.get(&scene))
                .cloned()
                .unwrap_or_default();
            let cpus = self.resolve_mask(&mask_name);
            let code = self
                .cfg
                .prio
                .get(&pc)
                .and_then(|s| s.get(&scene))
                .copied()
                .unwrap_or(0);
            let policy = SchedPolicy::decode(code).unwrap_or(SchedPolicy::Skip);
            return Some(SchedDecision {
                cpus,
                policy,
                rule: self.rules[rule_idx].name.clone(),
                scene,
                ac,
                pc,
            });
        }
        None
    }
}

/// `/HOME_PACKAGE/` → launcher package, unescaped (see module docs).
fn substitute_home(pattern: &str, home_package: &str) -> String {
    pattern.replace("/HOME_PACKAGE/", home_package)
}

/// `/MAIN_THREAD/` → that process's main thread name, unescaped.
fn substitute_main_thread(pattern: &str, main_thread: &str) -> String {
    pattern.replace("/MAIN_THREAD/", main_thread)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every unique pattern that appears in the shipped configs, extracted from
    /// `config/*.json` (63 files, 2528 occurrences, 28 unique). They are all
    /// plain ERE — no lookaround, backreference, atomic group, possessive
    /// quantifier or inline flag — so the `regex` crate is sufficient and no
    /// PCRE2 dependency is needed. If a future config introduces one of those
    /// constructs this test is where it surfaces.
    const SHIPPED_PATTERNS: [&str; 28] = [
        ".",
        "/HOME_PACKAGE/",
        "/MAIN_THREAD/",
        "/system/bin/surfaceflinger",
        "[.].+:",
        "^(/system|/vendor|magiskd|zygiskd)|@|-service$",
        "^(Chrome_InProc|CrRendererMain|CrGpuMain|CompositorTile)",
        "^(GPU completion|HWC release|hwui|FramePolicy|ScrollPolicy|ged-swd)",
        "^(JS|libweexjsb|WeexJsBridge|mqt_native|mqt_js|JavaScriptTh)",
        "^(Jit thread pool|HeapTaskDaemon|FinalizerDaemon|ReferenceQueueD)",
        "^(OkHttp|Ysa|Xqa|Rx|APM|TVKDL-|tp-|cgi-|ODCP-|Bugly|xlog_)",
        "^(RenderThread|GLThread)",
        "^(RenderThread|JNISurfaceText|IJK_External_Re)|[Aa]nim|([.]raster|[.]ui)$",
        "^(TaskSnapshot|Greezer|CachedApp|SystemPressure|SensorService)|[Mm]emory",
        "^(UnityMain|RenderThread |GameThread)",
        "^(Unity|Worker Thread|TaskGraph|RHIThread|GLThread|Thread-|Job.Worker)",
        "^(Viz|Chrome_|Compositor)|[Vv]sync|mali-",
        "^(app|RenderEngine)",
        "^(pool-|glide-|launcher-|Fresco)|[Dd]ownload|[Ss]chedule|[Ww]ork|[Pp]ool|[Dd]efau",
        "^(xg_vip_service|Profile|SearchDaemon|default_matrix|FrameDecoder|FrameSeq)",
        "^Async",
        "^Binder:",
        "^com.android.providers.media",
        "^com.tencent.mobileqq|tv.danmaku.bili|com.tencent.mm|com.smile.gifmaker|com.tencent.qqmusic|com.netease.cloudmusic|com.ss.android.ugc.aweme.lite|com.kuaishou.nebula",
        "com.android.phone",
        "com.android.systemui",
        "swapd|compactd",
        "system_server",
    ];

    #[test]
    fn every_shipped_pattern_compiles() {
        for p in SHIPPED_PATTERNS {
            let text = substitute_main_thread(&substitute_home(p, "com.miui.home"), "com.miui.home");
            Regex::new(&text)
                .unwrap_or_else(|e| panic!("{p:?} -> {text:?} failed: {e}"));
        }
    }

    /// The shipped set really is ERE-only. These are the constructs that would
    /// force a PCRE2 dependency; assert none appear.
    #[test]
    fn shipped_patterns_contain_no_pcre_only_constructs() {
        let pcre_only = [
            "(?=", "(?!", "(?<=", "(?<!", "(?>", "(?|", "(?(", "(?R", "\\K", "(?i)", "(?m)",
        ];
        for p in SHIPPED_PATTERNS {
            for needle in pcre_only {
                assert!(!p.contains(needle), "{p:?} contains PCRE-only {needle:?}");
            }
            // possessive quantifier (a+, a*+, a?+)
            assert!(
                !p.contains("+") || !p.contains("++"),
                "{p:?} may contain a possessive quantifier"
            );
            // numbered backreference
            for n in 1..10 {
                assert!(!p.contains(&format!("\\{n}")), "{p:?} contains a backreference");
            }
        }
    }

    #[test]
    fn prio_codes_decode_per_the_readme_table() {
        assert_eq!(SchedPolicy::decode(0), Some(SchedPolicy::Skip));
        assert_eq!(SchedPolicy::decode(1), Some(SchedPolicy::Fifo(1)));
        assert_eq!(SchedPolicy::decode(98), Some(SchedPolicy::Fifo(98)));
        assert_eq!(SchedPolicy::decode(99), None, "99 is not in any documented band");
        assert_eq!(SchedPolicy::decode(100), Some(SchedPolicy::Normal { nice: -20 }));
        assert_eq!(SchedPolicy::decode(120), Some(SchedPolicy::Normal { nice: 0 }));
        assert_eq!(SchedPolicy::decode(139), Some(SchedPolicy::Normal { nice: 19 }));
        assert_eq!(SchedPolicy::decode(140), None);
        assert_eq!(SchedPolicy::decode(-1), Some(SchedPolicy::NormalDefault));
        assert_eq!(SchedPolicy::decode(-2), Some(SchedPolicy::Batch));
        assert_eq!(SchedPolicy::decode(-3), Some(SchedPolicy::Idle));
        assert_eq!(SchedPolicy::decode(-4), None);
    }

    /// Every value that actually occurs in the 63 shipped configs must decode.
    #[test]
    fn all_codes_used_by_shipped_configs_decode() {
        for code in [-3i64, -1, 0, 96, 97, 98, 100, 110, 116, 120, 122, 124, 130, 139] {
            assert!(SchedPolicy::decode(code).is_some(), "code {code} from a shipped config");
        }
    }

    fn sdm888_sched() -> SchedConfig {
        let cfg = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("config/sdm888.json"),
        )
        .expect("config/sdm888.json");
        let v: Value = serde_json::from_str(&cfg).unwrap();
        SchedConfig::from_modules(v["modules"].as_object().unwrap()).expect("sched module")
    }

    #[test]
    fn parses_the_shipped_sched_module() {
        let s = sdm888_sched();
        assert!(s.enable);
        assert_eq!(s.cpumask.get("all"), Some(&vec![0, 1, 2, 3, 4, 5, 6, 7]));
        assert_eq!(s.cpumask.get("c1"), Some(&vec![4, 5, 6]));
        assert_eq!(s.affinity["ui"]["touch"], "c1");
        assert_eq!(s.affinity["auto"]["idle"], "", "empty mask = leave affinity alone");
        assert_eq!(s.prio["rtusr"]["idle"], 97);
        assert_eq!(s.prio["bg"]["bg"], -3);
        assert_eq!(s.rules.len(), 10);
        assert_eq!(s.rules[0].name, "Launcher");
        assert!(s.rules[0].pinned);
        assert_eq!(s.rules[0].rules[0].k, "/MAIN_THREAD/");
    }

    #[test]
    fn launcher_main_thread_is_crit_and_fifo_97_at_idle() {
        // The real first rule of sdm888.json: /HOME_PACKAGE/ -> main thread gets
        // ac=crit pc=rtusr. affinity[crit][idle] = "all", prio[rtusr][idle] = 97.
        let mut p = SchedPlanner::new(sdm888_sched(), "com.miui.home").unwrap();
        let d = p
            .decide("com.miui.home", true, "idle", "com.miui.home", "com.miui.home")
            .expect("launcher main thread should match");
        assert_eq!(d.rule, "Launcher");
        assert_eq!(d.scene, "idle");
        assert_eq!(d.ac, "crit");
        assert_eq!(d.pc, "rtusr");
        assert_eq!(d.cpus, Some(vec![0, 1, 2, 3, 4, 5, 6, 7]));
        assert_eq!(d.policy, SchedPolicy::Fifo(97));
    }

    #[test]
    fn launcher_render_thread_matches_the_second_thread_rule() {
        // Items are tried in order: `/MAIN_THREAD/` misses, then
        // `^(RenderThread|GLThread)` hits with ac=crit pc=rtusr.
        let mut p = SchedPlanner::new(sdm888_sched(), "com.miui.home").unwrap();
        let d = p
            .decide("com.miui.home", true, "idle", "com.miui.home", "RenderThread")
            .expect("RenderThread matches ^(RenderThread|GLThread)");
        assert_eq!(d.ac, "crit");
        assert_eq!(d.pc, "rtusr");
        assert_eq!(d.cpus, Some(vec![0, 1, 2, 3, 4, 5, 6, 7]), "affinity[crit][idle]=all");
    }

    #[test]
    fn launcher_hwui_thread_matches_the_background_item() {
        // `^(GPU completion|HWC release|hwui|...)` -> ac=bg; affinity[bg][idle]=c0.
        let mut p = SchedPlanner::new(sdm888_sched(), "com.miui.home").unwrap();
        let d = p
            .decide("com.miui.home", true, "idle", "com.miui.home", "hwuiTask0")
            .expect("hwui* matches the third item");
        assert_eq!(d.ac, "bg");
        assert_eq!(d.cpus, Some(vec![0, 1, 2, 3]), "affinity[bg][idle]=c0");
        assert_eq!(d.policy, SchedPolicy::Fifo(97), "prio[rtusr][idle]=97");
    }

    #[test]
    fn unlisted_thread_hits_the_dot_catch_all_and_does_nothing() {
        // Last item of the Launcher rule is k="." -> ac=auto pc=auto; both
        // affinit[auto] and prio[auto] are empty/0 in every scene, so the result
        // is "no change" rather than an error.
        let mut p = SchedPlanner::new(sdm888_sched(), "com.miui.home").unwrap();
        let d = p
            .decide("com.miui.home", true, "idle", "com.miui.home", "some-random-thread")
            .expect("'.' matches everything");
        assert_eq!(d.ac, "auto");
        assert_eq!(d.pc, "auto");
        assert_eq!(d.cpus, None, "affinity[auto][*] is the empty string");
        assert_eq!(d.policy, SchedPolicy::Skip, "prio[auto][*] is 0");
    }

    /// There is no such thing as an unmatched process: the last rule of every
    /// shipped config is `"regex": "."` ("Default rule"), so everything falls
    /// through to it. An unmatched *thread* inside a matched process, on the
    /// other hand, really does yield a no-op.
    #[test]
    fn unmatched_process_falls_through_to_the_default_rule() {
        let mut p = SchedPlanner::new(sdm888_sched(), "com.miui.home").unwrap();
        let d = p
            .decide("com.example.unknown", false, "idle", "x", "binder:1234_1")
            .expect("the '.' catch-all rule matches any process");
        assert_eq!(d.rule, "Default rule");
        assert_eq!(d.scene, "bg", "unpinned + not top app -> bg");
        assert_eq!(d.ac, "norm", "falls to the trailing '.' thread rule");
        assert_eq!(d.pc, "auto");
        assert_eq!(d.cpus, None, "affinity[norm][bg] is the empty string");
        assert_eq!(d.policy, SchedPolicy::Skip, "prio[auto][bg] is 0");
    }

    #[test]
    fn default_rule_main_thread_is_ui_class() {
        let mut p = SchedPlanner::new(sdm888_sched(), "com.miui.home").unwrap();
        let d = p
            .decide("com.example.unknown", false, "idle", "com.example.unknown", "com.example.unknown")
            .unwrap();
        assert_eq!(d.ac, "ui");
        assert_eq!(d.pc, "ui");
        assert_eq!(d.cpus, None, "affinity[ui][bg] is the empty string");
        assert_eq!(d.policy, SchedPolicy::Idle, "prio[ui][bg] = -3");
    }

    /// A rule can match the process while none of its thread rules match — rule
    /// 8 ("App co-process", `[.].+:`) has only one thread item and no `.`
    /// fallback, so a non-JIT thread inside such a process gets nothing.
    #[test]
    fn matched_process_with_no_matching_thread_rule_is_a_noop() {
        let mut p = SchedPlanner::new(sdm888_sched(), "com.miui.home").unwrap();
        let d = p.decide("com.example.app:remote", false, "idle", "com.example.app:remote", "Binder_1");
        assert!(d.is_none(), "no thread rule matched, expected None: {d:?}");
        // ...while the listed thread name does match.
        let d = p
            .decide("com.example.app:remote", false, "idle", "com.example.app:remote", "HeapTaskDaemon")
            .expect("HeapTaskDaemon is listed in the App co-process rule");
        assert_eq!(d.rule, "App co-process");
        assert_eq!(d.ac, "bg");
        assert_eq!(d.scene, "bg");
    }

    #[test]
    fn pinned_rule_uses_the_fsm_scene_even_when_not_top_app() {
        let mut p = SchedPlanner::new(sdm888_sched(), "com.miui.home").unwrap();
        // Launcher is pinned=True, so scene stays "touch" even with is_top_app=false.
        let d = p.decide("com.miui.home", false, "touch", "com.miui.home", "com.miui.home").unwrap();
        assert_eq!(d.scene, "touch");
        assert_eq!(d.cpus, Some(vec![4, 5, 6]), "affinity[crit][touch] = c1");
        assert_eq!(d.policy, SchedPolicy::Fifo(97), "prio[rtusr][touch] = 97");
    }

    #[test]
    fn unpinned_rule_on_a_background_process_uses_bg_scene() {
        let mut p = SchedPlanner::new(sdm888_sched(), "com.miui.home").unwrap();
        // Find an unpinned rule and drive it as a non-top process.
        let unpinned = p
            .config()
            .rules
            .iter()
            .find(|r| !r.pinned)
            .map(|r| r.name.clone());
        let Some(name) = unpinned else { return };
        // com.android.phone is unpinned in sdm888.json.
        if name == "Phone" || p.match_process("com.android.phone").map(|r| !r.pinned) == Some(true) {
            let d = p.decide("com.android.phone", false, "touch", "com.android.phone", "main");
            if let Some(d) = d {
                assert_eq!(d.scene, "bg", "a non-top, unpinned process runs in the bg scene");
            }
        }
    }

    #[test]
    fn home_package_substitution_is_unescaped() {
        // A package name's dots must stay regex metacharacters, as the shipped
        // configs rely on (bare `com.android.systemui`, `^com.tencent.mobileqq|...`).
        // Only `match_process` is used here, which takes &self.
        let p = SchedPlanner::new(sdm888_sched(), "com.miui.home").unwrap();
        let re = p.match_process("comXmiuiYhome").map(|r| r.name.clone());
        assert_eq!(
            re.as_deref(),
            Some("Launcher"),
            "unescaped substitution means '.' matches any char"
        );
    }

    #[test]
    fn dangling_classes_are_tolerated_and_recorded() {
        // The shipped configs do exactly this (sdm7g1.json defines neither
        // `affinity.fuck` nor `prio.fuck` but references both).
        let cfg = SchedConfig::from_value(&json!({
            "enable": true,
            "cpumask": {"all": [0,1]},
            "affinity": {"ui": {"idle": "all"}},
            "prio": {"ui": {"idle": 120}},
            "rules": [{"name":"bloat","regex":"x","pinned":true,
                       "rules":[{"k":".","ac":"fuck","pc":"fuck"}]}]
        }))
        .unwrap();
        let mut p = SchedPlanner::new(cfg, "p").expect("dangling classes must not be fatal");
        assert_eq!(p.anomalies().len(), 2, "{:?}", p.anomalies());
        assert!(matches!(
            p.anomalies()[0],
            Anomaly::UndefinedAffinityClass { .. }
        ));
        assert!(matches!(p.anomalies()[1], Anomaly::UndefinedPrioClass { .. }));
        // ...and the decision is a no-op rather than an error.
        let d = p.decide("x", true, "idle", "x", "t").unwrap();
        assert_eq!(d.cpus, None);
        assert_eq!(d.policy, SchedPolicy::Skip);
    }

    #[test]
    fn out_of_range_prio_code_is_recorded_and_skipped() {
        let cfg = SchedConfig::from_value(&json!({
            "enable": true,
            "cpumask": {"all": [0,1]},
            "affinity": {"ui": {"idle": "all"}},
            "prio": {"ui": {"idle": 99}},
            "rules": [{"name":"x","regex":"y","pinned":true,"rules":[{"k":".","ac":"ui","pc":"ui"}]}]
        }))
        .unwrap();
        let mut p = SchedPlanner::new(cfg, "p").unwrap();
        assert!(matches!(p.anomalies()[0], Anomaly::BadPrioCode { code: 99, .. }));
        assert_eq!(p.decide("y", true, "idle", "y", "t").unwrap().policy, SchedPolicy::Skip);
    }

    #[test]
    fn comma_separated_cpumask_names_are_unioned() {
        // sdm8g3.json ships affinity.bg.* = "c1,c2" with c1=[2,3], c2=[4,5,6].
        let cfg = SchedConfig::from_value(&json!({
            "enable": true,
            "cpumask": {"all": [0,1,2,3,4,5,6,7], "c1": [2,3], "c2": [4,5,6]},
            "affinity": {"bg": {"fg": "c1,c2", "idle": "", "bg": "c1"}},
            "prio": {"auto": {"fg": 0}},
            "rules": [{"name":"r","regex":"x","pinned":true,"rules":[{"k":".","ac":"bg","pc":"auto"}]}]
        }))
        .unwrap();
        let p = SchedPlanner::new(cfg, "p").unwrap();
        assert_eq!(p.resolve_mask("c1,c2"), Some(vec![2, 3, 4, 5, 6]));
        assert_eq!(p.resolve_mask("c1"), Some(vec![2, 3]));
        assert_eq!(p.resolve_mask(""), None, "empty = leave affinity alone");
        assert_eq!(p.resolve_mask("c1,nope"), None, "one unknown part poisons the value");
    }

    #[test]
    fn unknown_cpumask_name_is_recorded() {
        let cfg = SchedConfig::from_value(&json!({
            "enable": true,
            "cpumask": {"all": [0,1]},
            "affinity": {"ui": {"idle": "nope"}},
            "prio": {"ui": {"idle": 120}},
            "rules": [{"name":"x","regex":"y","pinned":true,"rules":[{"k":".","ac":"ui","pc":"ui"}]}]
        }))
        .unwrap();
        let mut p = SchedPlanner::new(cfg, "p").unwrap();
        assert!(matches!(p.anomalies()[0], Anomaly::UnknownCpumaskName { .. }));
        assert_eq!(p.decide("y", true, "idle", "y", "t").unwrap().cpus, None);
    }

    #[test]
    fn shipped_configs_only_contain_the_three_known_dangling_configs() {
        // Pins the known-quirk set: the gate test whitelists exactly this, so a
        // new config with a dangling class fails there.
        let mut with_anomalies = Vec::new();
        for dir in ["docs/ugt-configs", "docs/upstream-configs"] {
            let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(dir);
            let Ok(entries) = std::fs::read_dir(&root) else { continue };
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else { continue };
                let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { continue };
                let Some(m) = v.get("modules").and_then(|x| x.as_object()) else { continue };
                let Some(sc) = SchedConfig::from_modules(m) else { continue };
                let p = SchedPlanner::new(sc, "com.miui.home").unwrap();
                if !p.anomalies().is_empty() {
                    with_anomalies.push((
                        path.file_name().unwrap().to_string_lossy().to_string(),
                        p.anomalies().len(),
                    ));
                }
            }
        }
        with_anomalies.sort();
        assert_eq!(
            with_anomalies,
            vec![
                ("sdm7g1.json".to_string(), 2),
                ("sdm8g2.json".to_string(), 1),
                ("sdm8g3.json".to_string(), 1),
            ],
            "the set of shipped configs with tolerated defects changed"
        );
    }

    #[test]
    fn disabled_sched_yields_nothing() {
        let mut cfg = sdm888_sched();
        cfg.enable = false;
        let mut p = SchedPlanner::new(cfg, "com.miui.home").unwrap();
        assert!(p.decide("com.miui.home", true, "idle", "x", "y").is_none());
    }
}
