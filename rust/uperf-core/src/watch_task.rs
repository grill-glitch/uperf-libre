//! The file watcher: `cur_powermode.txt`, `perapp_powermode.txt`, and
//! `sfanalysis.hint`.
//!
//! Three user/vendor-editable files feed the daemon out of band, none of them
//! through the event bus:
//!
//! * `cur_powermode.txt` — the preset the user picked. Written by the module's
//!   `powercfg_main.sh`; upstream logs the value as `Preset inode -> '<value>'`.
//! * `perapp_powermode.txt` — `<package> <preset>` rules, `-` for offscreen and
//!   `*` for default.
//! * `sfanalysis.hint` — a **single byte** written by the vendor
//!   `libsfanalysis.so` injected into surfaceflinger; `0..5` are the hint enum
//!   values (`hint::SfHint::from_byte`), `>= 6` is unknown.
//!
//! Design note: inotify is used **only as a wakeup signal**. On any event the
//! watched files are re-read from scratch. Attributing an event to a path looks
//! simple and is not: one `echo x > file` produces `IN_MODIFY` *and*
//! `IN_CLOSE_WRITE`, a rename-into-place arrives on the directory, and a file
//! that does not exist yet cannot be watched at all. Re-reading three small files
//! is cheaper than getting that wrong.
//!
//! Applying a mode reuses `topic_dispatch::apply_pending`, so a preset switched
//! from a file produces the same sysfs writes as one switched by an event.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex as PMutex;
use uperf_config::{InodeMode, PerappRules, SwitcherConfig};

use crate::hint::SfHint;
use crate::inotify::Inotify;
use crate::orchestrator::Orchestrator;

/// Name of the hint file inside the config's directory.
pub const SF_HINT_FILE: &str = "sfanalysis.hint";

/// Why a preset was chosen — mirrors upstream's three log shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Why {
    /// The value in `cur_powermode.txt` names a preset directly.
    Explicit,
    /// A per-app rule matched: `Perapp '<package>' -> '<preset>'`.
    App(String),
    /// The `*` rule: `Perapp '*' -> '<preset>'` + `Preset '<cur>' -> default`.
    Default,
    /// The `-` rule: `Perapp '-' -> '<preset>'` + `Preset '<cur>' -> offscreen`.
    Offscreen,
}

/// The pure decision: given the inode's contents and the per-app rules, which
/// preset should be in effect?
///
/// Returns `(mode, log_lines)`, and **only ever describes a change**: when the
/// resolved mode equals `current` it returns `(None, [])`, so the log describes
/// transitions rather than being re-emitted on every wakeup.
///
/// `auto` hands control to the per-app rules **[I]** — see `uperf_config::switcher`.
pub fn effective_mode(
    inode: Option<&InodeMode>,
    perapp: &PerappRules,
    top_app: Option<&str>,
    offscreen: bool,
    current: &str,
) -> (Option<String>, Vec<String>) {
    let (chosen, why) = match inode {
        // No inode file at all: keep whatever the caller already has.
        None => return (None, Vec::new()),
        Some(InodeMode::Preset(p)) => (p.clone(), Why::Explicit),
        // `auto` defers to the per-app table, and so does an undefined value: a
        // typo must not strand the daemon on a preset the config never defines.
        // The caller logs `Failed to switch to undefined preset '<v>'` for that
        // case, so the mistake is still visible.
        Some(InodeMode::Auto) | Some(InodeMode::Undefined(_)) => {
            match perapp.resolve(top_app, offscreen) {
                uperf_config::PerappChoice::Offscreen(p) => (p, Why::Offscreen),
                uperf_config::PerappChoice::App { package, preset } => (preset, Why::App(package)),
                uperf_config::PerappChoice::Default(p) => (p, Why::Default),
                uperf_config::PerappChoice::None => return (None, Vec::new()),
            }
        }
    };
    if chosen == current {
        return (None, Vec::new()); // already in effect: nothing to do or to log
    }
    let mut logs = Vec::new();
    match &why {
        Why::Explicit => {}
        Why::App(package) => logs.push(format!("Perapp '{package}' -> '{chosen}'")),
        Why::Default => logs.push(format!("Perapp '*' -> '{chosen}'")),
        Why::Offscreen => logs.push(format!("Perapp '-' -> '{chosen}'")),
    }
    match &why {
        Why::Default => logs.push(format!("Preset '{current}' -> default")),
        Why::Offscreen => logs.push(format!("Preset '{current}' -> offscreen")),
        _ => logs.push(format!("Preset '{current}' -> '{chosen}'")),
    }
    (Some(chosen), logs)
}

/// The three paths plus the preset names a value is validated against.
pub struct WatchPlan {
    pub switch_inode: Option<PathBuf>,
    pub perapp: Option<PathBuf>,
    /// `None` when `modules.sfanalysis.enable` is false — upstream logs
    /// `SfAnalysisListener disabled by config` and does not listen at all, so the
    /// byte is not even polled.
    pub sf_hint: Option<PathBuf>,
    pub known_presets: Vec<String>,
}

impl WatchPlan {
    /// Derive the paths from the config. `config_path` is
    /// `<USER_PATH>/uperf.json`; the hint file is taken to live beside it.
    ///
    /// The hint file's own path is **[I]**: the name `sfanalysis.hint` is a
    /// string **in the uperf binary** (the consumer), while the producer — the
    /// vendor `libsfanalysis.so` injected into surfaceflinger — contains no path
    /// string at all (only `/proc/<pid>/comm`, `/proc/<pid>/stat`,
    /// `/proc/self/maps` and `/system/bin/surfaceflinger`), so it must receive or
    /// build the path some other way. Confirming it needs the real module
    /// installed, which is M7's packaging step.
    pub fn from_config(cfg: &uperf_config::Config, config_path: &Path) -> Self {
        let sw = cfg
            .modules_map()
            .and_then(SwitcherConfig::from_modules)
            .unwrap_or_default();
        let sf_enabled = cfg
            .modules_map()
            .and_then(|m| m.get("sfanalysis"))
            .and_then(|v| v.get("enable"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let sf_hint = sf_enabled.then(|| {
            config_path
                .parent()
                .map(|d| d.join(SF_HINT_FILE))
                .unwrap_or_else(|| PathBuf::from(SF_HINT_FILE))
        });
        Self {
            switch_inode: sw.switch_inode.map(PathBuf::from),
            perapp: sw.perapp.map(PathBuf::from),
            sf_hint,
            known_presets: cfg.preset_names(),
        }
    }
}

pub struct WatchTask {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WatchTask {
    /// Spawn the watcher. `log` receives the upstream-format log lines.
    pub fn spawn(
        plan: WatchPlan,
        orch: Arc<PMutex<Orchestrator>>,
        fake_root: Option<String>,
        mut log: impl FnMut(&str) + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_child = stop.clone();
        let thread = std::thread::Builder::new()
            .name("uperf-watch".into())
            .spawn(move || {
                let mut ino = match Inotify::new() {
                    Ok(i) => i,
                    Err(e) => {
                        log(&format!(
                            "Rust: inotify unavailable ({e}), preset switching disabled"
                        ));
                        return;
                    }
                };

                // Take the orchestrator state in ONE lock. Writing
                // `orch.lock().a()` and `orch.lock().b()` inside a single
                // expression deadlocks — the first guard is a temporary that lives
                // until the end of the statement and parking_lot's mutex is not
                // reentrant — and the failure is silent: no panic, no log, the
                // thread simply never runs.
                let snapshot = |o: &Arc<PMutex<Orchestrator>>| {
                    let g = o.lock();
                    (g.top_app().map(str::to_string), g.offscreen(), g.mode().to_string())
                };

                // ---- startup -------------------------------------------------
                let mut last_inode_raw: Option<String> = None;
                let mut inode_mode = load_inode(
                    plan.switch_inode.as_deref(),
                    &plan.known_presets,
                    &orch,
                    &mut last_inode_raw,
                    &mut log,
                    None,
                );
                let mut perapp = load_perapp(plan.perapp.as_deref(), &plan.known_presets, &mut log);

                let (top, off, current) = snapshot(&orch);
                let (mode, lines) =
                    effective_mode(inode_mode.as_ref(), &perapp, top.as_deref(), off, &current);
                for l in &lines {
                    log(l);
                }
                if let Some(m) = mode {
                    apply_mode(&orch, &m, fake_root.as_deref(), &mut log);
                }

                // ---- arm the watches ----------------------------------------
                // `watch` also arms a directory watch, so a file that does not
                // exist yet (sfanalysis.hint) still wakes us up when created.
                if plan.sf_hint.is_none() {
                    log("SfAnalysisListener disabled by config");
                }
                for p in [
                    plan.switch_inode.as_deref(),
                    plan.perapp.as_deref(),
                    plan.sf_hint.as_deref(),
                ]
                .into_iter()
                .flatten()
                {
                    if let Err(e) = ino.watch(p) {
                        log(&format!("Rust: cannot watch {} ({e})", p.display()));
                    }
                }

                // ---- loop ---------------------------------------------------
                let mut last_gen = orch.lock().generation();
                let mut last_sf_byte: Option<u8> = None;
                while !stop_child.load(Ordering::Relaxed) {
                    let events = ino.poll(Duration::from_millis(250)).unwrap_or_default();
                    // Re-arm unconditionally. `inotify_add_watch` on an
                    // already-watched path is idempotent (same wd, no duplicate
                    // events), and this is what arms a file created *after*
                    // startup: on /sdcard the directory create notification did
                    // not arrive, so the hint file never got a watch of its own.
                    for p in [
                        plan.switch_inode.as_deref(),
                        plan.perapp.as_deref(),
                        plan.sf_hint.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        ino.rearm(p);
                    }
                    if !events.is_empty() {
                        // Any event is a wakeup: re-read everything and re-arm
                        // whatever could not be armed earlier.
                        inode_mode = load_inode(
                            plan.switch_inode.as_deref(),
                            &plan.known_presets,
                            &orch,
                            &mut last_inode_raw,
                            &mut log,
                            inode_mode.clone(),
                        );
                        perapp = load_perapp(plan.perapp.as_deref(), &plan.known_presets, &mut log);
                    }

                    // Read the hint byte on *every* tick, not only when inotify
                    // speaks. The vendor library's first write is also the file's
                    // creation, and a watch armed after that write never sees it —
                    // measured on /sdcard. A 1-byte read every 250 ms with the
                    // `last_sf_byte` dedup is cheap and cannot miss a hint.
                    if let Some(hint_path) = plan.sf_hint.as_deref() {
                        handle_sf_hint(hint_path, &orch, &mut last_sf_byte, &mut log);
                    }

                    // The scene / top app may have moved, which changes what the
                    // per-app rules resolve to.
                    let gen = orch.lock().generation();
                    if gen == last_gen && events.is_empty() {
                        continue; // nothing changed; the hint was already polled
                    }
                    last_gen = gen;

                    let (top, off, current) = snapshot(&orch);
                    let (mode, lines) =
                        effective_mode(inode_mode.as_ref(), &perapp, top.as_deref(), off, &current);
                    for l in &lines {
                        log(l);
                    }
                    if let Some(m) = mode {
                        apply_mode(&orch, &m, fake_root.as_deref(), &mut log);
                    }
                }
            })
            .expect("spawn uperf-watch");
        Self { stop, thread: Some(thread) }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
    pub fn is_running(&self) -> bool {
        self.thread.is_some()
    }
}

/// Read `cur_powermode.txt`, creating it when absent (upstream:
/// `Preset inode not existed, create one`).
///
/// `last_raw` dedupes the `Preset inode -> '<v>'` line: one write produces two
/// inotify events and the loop also re-reads on wakeups, so without this the line
/// repeats.
fn load_inode(
    path: Option<&Path>,
    known: &[String],
    orch: &Arc<PMutex<Orchestrator>>,
    last_raw: &mut Option<String>,
    log: &mut impl FnMut(&str),
    last_classified: Option<InodeMode>,
) -> Option<InodeMode> {
    let Some(path) = path else {
        if last_raw.is_none() {
            log("Preset inode path not specified");
            *last_raw = Some(String::new()); // log once
        }
        return None;
    };
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let value = text.trim().to_string();
            if value.is_empty() {
                // `echo x > file` truncates before it writes, so a reader can
                // catch the file empty. Treat that as "no news" — reporting it
                // would log a bogus `Preset inode -> ''` and could blank the
                // preset on a writer that never fills the file in.
                return last_classified;
            }
            let classified = InodeMode::classify(&value, known);
            if last_raw.as_deref() != Some(value.as_str()) {
                log(&format!("Preset inode -> '{value}'"));
                if let Some(InodeMode::Undefined(v)) = &classified {
                    log(&format!("Failed to switch to undefined preset '{v}'"));
                }
                *last_raw = Some(value);
            }
            classified
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if last_raw.is_none() {
                log(&format!(
                    "Preset inode not existed, create one ({})",
                    path.display()
                ));
            }
            *last_raw = Some(String::new());
            let cur = orch.lock().mode().to_string();
            let _ = std::fs::write(path, format!("{cur}\n"));
            Some(InodeMode::Preset(cur))
        }
        Err(e) => {
            log(&format!("Rust: cannot read {} ({e})", path.display()));
            None
        }
    }
}

/// Parse `perapp_powermode.txt`. Logged once per change, like the inode.
fn load_perapp(
    path: Option<&Path>,
    known: &[String],
    log: &mut impl FnMut(&str),
) -> PerappRules {
    let Some(path) = path else {
        return PerappRules::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        log("Failed to load perapp rule");
        return PerappRules::default();
    };
    let rules = PerappRules::parse(&text, known);
    for a in &rules.anomalies {
        log(&format!("Rust: {a}"));
    }
    if rules.default.is_none() {
        log("Default perapp preset not specified");
    }
    if rules.offscreen.is_none() {
        log("Offscreen perapp preset not specified");
    }
    rules
}

/// Read the single hint byte and drive the FSM.
fn handle_sf_hint(
    path: &Path,
    orch: &Arc<PMutex<Orchestrator>>,
    last_byte: &mut Option<u8>,
    log: &mut impl FnMut(&str),
) {
    let Ok(bytes) = std::fs::read(path) else { return };
    let Some(b) = bytes.first().copied() else { return };
    if *last_byte == Some(b) {
        return; // same hint, nothing new
    }
    *last_byte = Some(b);
    let hint = SfHint::from_byte(b as i8);
    let transitioned = orch.lock().on_sf_hint(hint);
    log(&format!(
        "Rust: SfAnalysis hint '{}' (byte {}) transitioned={}",
        hint.as_str(),
        b as i8,
        transitioned
    ));
}

fn apply_mode(
    orch: &Arc<PMutex<Orchestrator>>,
    mode: &str,
    fake_root: Option<&str>,
    log: &mut impl FnMut(&str),
) {
    orch.lock().set_mode(mode);
    let (written, failed) = crate::topic_dispatch::apply_pending(orch, fake_root);
    log(&format!(
        "Rust: preset applied mode={mode} writes={written} failed={failed}"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        ["balance", "powersave", "performance", "fast"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    const PERAPP: &str = "com.game performance\n- powersave\n* balance\n";

    #[test]
    fn explicit_preset_wins() {
        let r = PerappRules::parse(PERAPP, &known());
        let (m, logs) = effective_mode(
            Some(&InodeMode::Preset("fast".into())),
            &r,
            Some("com.game"),
            false,
            "balance",
        );
        assert_eq!(m.as_deref(), Some("fast"));
        assert_eq!(logs, vec!["Preset 'balance' -> 'fast'"]);
    }

    #[test]
    fn auto_defers_to_the_app_rule() {
        let r = PerappRules::parse(PERAPP, &known());
        let (m, logs) =
            effective_mode(Some(&InodeMode::Auto), &r, Some("com.game"), false, "balance");
        assert_eq!(m.as_deref(), Some("performance"));
        assert_eq!(
            logs,
            vec!["Perapp 'com.game' -> 'performance'", "Preset 'balance' -> 'performance'"]
        );
    }

    #[test]
    fn auto_on_an_unknown_app_uses_the_default_rule() {
        let r = PerappRules::parse(PERAPP, &known());
        let (m, logs) =
            effective_mode(Some(&InodeMode::Auto), &r, Some("com.other"), false, "performance");
        assert_eq!(m.as_deref(), Some("balance"));
        // Upstream has three terminal shapes — `Preset 'x' -> 'y'`,
        // `Preset 'x' -> default`, `Preset 'x' -> offscreen` — so the
        // default/offscreen forms replace the plain one rather than stacking
        // with it.
        assert_eq!(
            logs,
            vec!["Perapp '*' -> 'balance'", "Preset 'performance' -> default"]
        );
    }

    #[test]
    fn auto_while_offscreen_uses_the_offscreen_rule() {
        let r = PerappRules::parse(PERAPP, &known());
        let (m, logs) =
            effective_mode(Some(&InodeMode::Auto), &r, Some("com.game"), true, "performance");
        assert_eq!(m.as_deref(), Some("powersave"), "offscreen beats the app rule");
        assert!(logs.iter().any(|l| l == "Preset 'performance' -> offscreen"), "{logs:?}");
    }

    #[test]
    fn an_undefined_inode_value_falls_back_to_the_rules() {
        let r = PerappRules::parse(PERAPP, &known());
        let (m, _) = effective_mode(
            Some(&InodeMode::Undefined("turbo".into())),
            &r,
            Some("com.game"),
            false,
            "balance",
        );
        assert_eq!(m.as_deref(), Some("performance"));
    }

    /// The re-evaluation runs on every wakeup, so "already in effect" must be
    /// free of log output — this is what keeps the log from spamming.
    #[test]
    fn a_no_op_transition_logs_nothing() {
        let r = PerappRules::parse(PERAPP, &known());
        for inode in [
            InodeMode::Preset("balance".into()),
            // `auto` + the `*` rule also resolves to balance
        ] {
            let (m, logs) = effective_mode(Some(&inode), &r, None, false, "balance");
            assert_eq!(m, None);
            assert!(logs.is_empty(), "{logs:?}");
        }
        // auto -> '*' rule is 'balance', current is 'balance': silent.
        let (m, logs) = effective_mode(Some(&InodeMode::Auto), &r, Some("com.unknown"), false, "balance");
        assert_eq!(m, None);
        assert!(logs.is_empty(), "{logs:?}");
    }

    #[test]
    fn missing_inode_file_keeps_the_current_mode() {
        let r = PerappRules::parse(PERAPP, &known());
        let (m, logs) = effective_mode(None, &r, Some("com.game"), false, "balance");
        assert_eq!(m, None);
        assert!(logs.is_empty());
    }

    #[test]
    fn auto_with_no_default_rule_changes_nothing() {
        let r = PerappRules::parse("com.game performance\n", &known());
        let (m, _) = effective_mode(Some(&InodeMode::Auto), &r, Some("com.other"), false, "balance");
        assert_eq!(m, None, "no '*' rule and no app match -> nothing to apply");
    }

    #[test]
    fn the_hint_path_is_dropped_when_sfanalysis_is_disabled() {
        // sdm888.json ships "sfanalysis": {"enable": false}, and upstream then
        // logs `SfAnalysisListener disabled by config` instead of listening.
        let cfg = uperf_config::Config::from_value(serde_json::json!({
            "meta": {"name":"t","author":"t"},
            "modules": {"sfanalysis": {"enable": false, "renderIdleSlackTime": 0.2}},
            "initials": {},
            "presets": {"balance": {"*": {}}}
        }))
        .unwrap();
        let plan = WatchPlan::from_config(&cfg, Path::new("/sdcard/Android/yc/uperf/uperf.json"));
        assert_eq!(plan.sf_hint, None);
    }

    #[test]
    fn watch_plan_derives_the_three_paths_from_a_config() {
        let cfg = uperf_config::Config::from_value(serde_json::json!({
            "meta": {"name":"t","author":"t"},
            "modules": {
                "switcher": {
                    "switchInode": "/sdcard/Android/yc/uperf/cur_powermode.txt",
                    "perapp": "/sdcard/Android/yc/uperf/perapp_powermode.txt",
                    "hintDuration": {"idle": 0.0}
                },
                "sfanalysis": {"enable": true, "renderIdleSlackTime": 0.2}
            },
            "initials": {},
            "presets": {"balance": {"*": {}}, "fast": {"*": {}}}
        }))
        .unwrap();
        let plan = WatchPlan::from_config(&cfg, Path::new("/sdcard/Android/yc/uperf/uperf.json"));
        assert_eq!(
            plan.switch_inode.as_deref(),
            Some(Path::new("/sdcard/Android/yc/uperf/cur_powermode.txt"))
        );
        assert_eq!(
            plan.perapp.as_deref(),
            Some(Path::new("/sdcard/Android/yc/uperf/perapp_powermode.txt"))
        );
        assert_eq!(
            plan.sf_hint.as_deref(),
            Some(Path::new("/sdcard/Android/yc/uperf/sfanalysis.hint"))
        );
        assert_eq!(plan.known_presets, vec!["balance".to_string(), "fast".to_string()]);
    }
}
