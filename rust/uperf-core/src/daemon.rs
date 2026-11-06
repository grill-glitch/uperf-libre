//! The daemon: which subsystems exist, in what order they start, and in what order
//! they stop.
//!
//! `uperf_rs_start` used to be this, inline: ~340 lines of `if enabled { slot.lock()
//! .take()… stop; spawn }`. Every subsystem repeated the same five steps (derive the
//! config directory, grab the orchestrator, build a state closure, stop the previous
//! instance, spawn), and the stop path was a second hand-written list that had to be
//! kept in sync by hand. That list is the dangerous one: a task missing from it keeps
//! running while the daemon reports `state=stopped`, and the CPU governor staying
//! armed in `userspace` is the one state this project cannot recover from in-process.
//!
//! So ownership is explicit now. `Daemon` holds the settings, the orchestrator, the
//! dfps scheduler, the dispatcher and a [`Registry`] of tasks; start order is the
//! order of `push`, and the stop order is derived (reverse), not remembered. The only
//! deliberate exception is the governor, which is stopped first by name — an armed
//! frequency takeover must not wait behind six other tasks on the way out.
//!
//! Everything here is startup wiring. If you are looking for *policy*, it lives in
//! `uperf-config` (governor, scheduler, sysfs planner) and in the task modules.

use std::sync::Arc;

use parking_lot::Mutex as PMutex;

use crate::ffi::{BRIDGE, TOPICS};
use crate::orchestrator::Orchestrator;
use crate::settings::Settings;
use crate::task::Registry;
use crate::topic_dispatch::{self, Dispatcher};

/// How long one task gets to notice its stop flag. `shutdown::STOP_TIMEOUT` is ~8x the
/// longest poll interval in the tasks; the dispatcher gets the same budget.
use crate::shutdown::STOP_TIMEOUT;

pub struct Daemon {
    settings: Settings,
    /// The parsed config, read **once**. Every stage used to re-read and re-parse the
    /// JSON (five times on the old code path, because each subsystem needed it).
    config: Option<uperf_config::Config>,
    /// The preset `cur_powermode.txt` named at startup.
    mode: String,
    /// Shared with every task that needs scene / top-app / generation. Owned here
    /// rather than in a `OnceLock`: on a re-start the old `OnceLock::set` silently
    /// failed and every task kept reading the *first* orchestrator.
    orchestrator: Option<Arc<PMutex<Orchestrator>>>,
    /// The dfps subtree's timer. Stopped last: it is the least entangled.
    dfps: Option<crate::dfps_rs::DfpsScheduler>,
    /// The event loop. Stopped first — it is the only thread that drains orchestrator
    /// plans into the sink, so it must not write while the rest is being torn down.
    dispatcher: Option<Dispatcher>,
    tasks: Registry,
}

impl Daemon {
    /// Bring the whole thing up. Log lines here are the daemon's startup contract
    /// (`webui.sh`, the M9 harness and the device scripts grep for them).
    pub fn start(settings: Settings) -> Daemon {
        let (config, mode) = load_config_and_mode(&settings.config_path);
        let mut d = Daemon {
            settings,
            config,
            mode,
            orchestrator: None,
            dfps: None,
            dispatcher: None,
            tasks: Registry::new(),
        };
        // dfps is its own crate and has no opinion about the log sink; install ours
        // before its first line could be emitted.
        crate::dfps_rs::set_logger(crate::log_msg);
        d.start_dfps();
        d.start_dispatcher();
        d.start_bridge_consumers();
        d.start_governor();
        d.start_scheduler();
        d.start_frame_legs();
        d.start_control_plane();
        d.start_recorder();
        d.start_watcher();

        crate::log_msg(&format!(
            "Rust: daemon started ({} tasks: {})",
            d.tasks.len(),
            d.tasks.names().join(", ")
        ));
        d
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Tear down in the documented order. Idempotent: every step takes its field, so a
    /// second call is a no-op (the C++ supervisor can deliver the signal twice).
    pub fn stop(&mut self) {
        if let Some(mut dispatch) = self.dispatcher.take() {
            dispatch.join_timeout(STOP_TIMEOUT);
        }
        // By name, not by position: the frequency takeover is the state that must not
        // survive a slow shutdown.
        if self.tasks.take_and_stop("cpu") && !self.settings.cpu_governor {
            crate::log_msg("Rust: cpu governor stopped (was idle, nothing to disarm)");
        }
        self.tasks.stop_all();
        if let Some(dfps) = self.dfps.take() {
            dfps.stop();
        }
        crate::log_msg("Rust: daemon stopped");
    }

    // ------------------------------------------------------------------ stages

    /// dfps-rs (mounted from grill-glitch/dfps-rewrite as a subtree): built **before**
    /// the dispatcher so the dispatcher can route the five dfps topics from the first
    /// event. A missing or unparsable `dfps.txt` disables dfps and logs once — uperf
    /// itself still starts.
    fn start_dfps(&mut self) {
        let dfps_path = self.settings.config_dir.as_ref().map(|d| d.join("dfps.txt"));
        let table = dfps_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|txt| crate::dfps_rs::config::RuleTable::parse(&txt).ok());
        match table {
            Some(t) => {
                // Accessors, not fields: `RuleTable`'s internals stay inside `uperf-dfps`
                // now that the boundary is a crate boundary.
                let u = t.universal_rule();
                let minus = t.offscreen_rule();
                let n = t.rule_count();
                let sched = crate::dfps_rs::DfpsScheduler::new(t);
                crate::log_msg(&format!(
                    "Rust: dfps loaded ({} rules, universal={}/{}, offscreen={}/{})",
                    n, u.idle, u.active, minus.idle, minus.active
                ));
                sched.spawn();
                crate::log_msg("Rust: dfps timer thread started");
                self.dfps = Some(sched);
            }
            None => {
                crate::log_msg(&format!(
                    "Rust: dfps disabled (no usable '{}')",
                    dfps_path
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(no config dir)".into())
                ));
            }
        }
    }

    /// The event loop and the orchestrator it feeds.
    fn start_dispatcher(&mut self) {
        // Queue item ④: the top-app source. The helper's file lives beside the config
        // (the module's USER_PATH); when it is fresh it drives `topapp.pkgName`, and
        // the vendored C++ monitor stays the fallback.
        let foreground = self.settings.foreground_source();
        match foreground.as_ref() {
            Some(f) => crate::log_msg(&format!(
                "Rust: foreground helper file source = {} (max_age {} ms)",
                f.path().display(),
                f.max_age_ms()
            )),
            None => crate::log_msg(
                "Rust: foreground helper file source disabled; C++ topapp monitor only",
            ),
        }

        let orch = match self.config.clone() {
            Some(c) => Orchestrator::with_config(c, &self.mode),
            None => Orchestrator::new(crate::hint::HintDurations::default(), &self.mode),
        };
        let orch = Arc::new(PMutex::new(orch));
        self.dispatcher = Some(topic_dispatch::spawn(
            orch.clone(),
            self.dfps.clone(),
            foreground,
            self.settings.clone(),
        ));
        self.orchestrator = Some(orch);

        // Subscribe only after the dispatcher exists: a topic with no subscriber is
        // not published at all by the C++ side, and losing the first events of a
        // session is exactly what a startup does.
        match BRIDGE.get() {
            Some(bridge) => {
                for topic in TOPICS.iter() {
                    bridge.subscribe(topic);
                }
            }
            None => crate::log_msg("Rust: bridge not installed; topics not subscribed"),
        }
    }

    /// Config-driven settings that the vendored platform owns: input thresholds and
    /// the atrace markers. Both are applied to C++ listeners that are already running,
    /// because the platform layer starts them before the config is parsed.
    fn start_bridge_consumers(&mut self) {
        let Some(cfg) = self.config.clone() else { return };
        if cfg.input_enabled() == Some(false) {
            crate::log_msg(
                "Rust: modules.input.enable=false is not honoured (the listener is started by the \
                 platform layer before the config is parsed)",
            );
        }
        if let Some((swipe, gx, gy)) = cfg.input_thresholds() {
            // SAFETY: plain scalars across the C ABI.
            unsafe { crate::ffi::uperf_bridge_set_input_thresholds(swipe, gx, gy) };
        }
        if let Some(on) = cfg.atrace_enabled() {
            // SAFETY: plain scalar across the C ABI.
            unsafe { crate::ffi::uperf_bridge_set_atrace(on) };
        }
    }

    /// The userspace CPU governor (`UPERF_CPU_GOVERNOR=1`; off by default — see the
    /// note on `CpuTask::spawn`).
    fn start_governor(&mut self) {
        let Some(cfg) = self.config.clone() else { return };
        match crate::cpu_task::CpuTask::spawn(&cfg, &self.mode, "idle", &self.settings) {
            Some(task) => {
                self.tasks.push(task);
                crate::log_msg("Rust: cpu governor started");
            }
            None => crate::log_msg("Rust: cpu governor not started"),
        }
    }

    /// The context scheduler (`modules.sched`).
    fn start_scheduler(&mut self) {
        let Some(cfg) = self.config.clone() else { return };
        if cfg.modules_map().is_none() {
            return;
        }
        let mut log_fn = |m: &str| crate::log_msg(m);
        let Some(planner) = crate::sched_task::SchedTask::planner_for(&cfg, &self.settings, &mut log_fn)
        else {
            return;
        };
        let Some(orch) = self.orchestrator.clone() else {
            // Unreachable: `start_dispatcher` runs first and always sets it. Kept
            // because the alternative is an `unwrap`, and this is the shutdown path.
            crate::log_msg("Rust: context scheduler skipped (no orchestrator)");
            return;
        };
        let state = move || {
            let g = orch.lock();
            (
                g.current_scene().to_string(),
                g.top_app().map(str::to_string),
                g.generation(),
            )
        };
        self.tasks
            .push(crate::sched_task::SchedTask::spawn(planner, &self.settings, state));
        crate::log_msg("Rust: context scheduler started");
    }

    /// The two frame legs. They are independent tasks; which one *wins* is decided in
    /// `frame_state` (the ladder), not here.
    fn start_frame_legs(&mut self) {
        // ⑪ (user-appended): the AppOpt FPS **primary** leg — a uprobe on the target
        // app's own `libgui.so` `queueBuffer`, one probe per thread, so only *that*
        // app's frames are counted (opt-in `UPERF_SF_UPROBE=1`). While its probe is
        // healthy the ⑤ binder leg stands down; see `sf_uprobe` and
        // `docs/m11-11-ebpf-uprobe-frame-leg.md`.
        if let Some(cfg_uprobe) = self.settings.sf_uprobe.clone() {
            let orch = self.orchestrator.clone();
            let top = move || orch.as_ref().and_then(|o| o.lock().top_app().map(str::to_string));
            match crate::sf_uprobe::UprobeTask::spawn(&cfg_uprobe, top, |m: &str| crate::log_msg(m)) {
                Some(t) => {
                    self.tasks.push(t);
                    crate::log_msg("Rust: sf-uprobe frame source started");
                }
                None => crate::log_msg("Rust: sf-uprobe frame source could not start"),
            }
        }

        // ⑤: the direct-binder SurfaceFlinger frame source (opt-in
        // `UPERF_SF_BINDER=1`). The degraded leg for when the M8 injection library is
        // not there: read SF's `--latency` over binder instead of spawning dumpsys.
        if let Some(cfg_binder) = self.settings.sf_binder.clone() {
            let orch = self.orchestrator.clone();
            let state = move || orch.as_ref().and_then(|o| o.lock().top_app().map(str::to_string));
            // The degraded leg's one honest assertion: nothing drawn in the window means
            // the UI is idle. It goes through the same entry the injected hint uses
            // (`Orchestrator::on_sf_hint`), and the queued scene writes are drained here
            // because no event-bus message carries this one.
            let hint_orch = self.orchestrator.clone();
            let hint_settings = self.settings.clone();
            let on_hint = move |h: crate::hint::SfHint| {
                let Some(o) = hint_orch.as_ref() else { return };
                if o.lock().on_sf_hint(h) {
                    crate::topic_dispatch::apply_pending(o, &hint_settings);
                }
            };
            let hint_path = self.settings.config_dir.as_ref().map(|d| d.join("sfanalysis.hint"));
            match crate::sf_binder::FrameTask::spawn(
                &cfg_binder,
                hint_path,
                self.settings.frames_state.clone(),
                state,
                on_hint,
                |m: &str| crate::log_msg(m),
            ) {
                Some(t) => {
                    self.tasks.push(t);
                    crate::log_msg("Rust: sf-binder frame source started");
                }
                None => crate::log_msg("Rust: sf-binder frame source could not start"),
            }
        }
    }

    /// ⑦ control-plane socket (opt-in `UPERF_CTL_SOCKET`, e.g. `@uperf-ctl`). The
    /// daemon connects out and performs the token/version/pid handshake.
    fn start_control_plane(&mut self) {
        let Some(cfg_ctl) = self.settings.ctl.clone() else { return };
        match crate::ctl_socket::CtlTask::spawn(
            &cfg_ctl,
            self.settings.ctl_token_file.clone(),
            |m: &str| crate::log_msg(m),
        ) {
            Some(t) => self.tasks.push(t),
            None => crate::log_msg("Rust: ctl-socket task could not start"),
        }
    }

    /// ⑧ recording channel (opt-in `UPERF_RECORD=1`): per-app sessions under
    /// `<USER_PATH>/history/`, kept only past the ≥3 min rule, DEFLATE-compressed,
    /// deduped by pkg+epoch.
    fn start_recorder(&mut self) {
        let Some(cfg_record) = self.settings.record.clone() else { return };
        let orch = self.orchestrator.clone();
        let state = move || match orch.as_ref() {
            Some(o) => {
                let g = o.lock();
                (
                    g.top_app().map(str::to_string),
                    g.current_scene().to_string(),
                )
            }
            None => (None, "idle".to_string()),
        };
        match crate::recorder::RecorderTask::spawn(
            &cfg_record,
            self.settings.config_dir.clone(),
            state,
            |m: &str| crate::log_msg(m),
        ) {
            Some(t) => self.tasks.push(t),
            None => crate::log_msg("Rust: recorder task could not start"),
        }
    }

    /// The file watcher: `cur_powermode.txt` / `perapp_powermode.txt` preset switching
    /// and the single-byte `sfanalysis.hint` feed. Last, because nothing depends on it
    /// and it is the task most likely to be idle.
    fn start_watcher(&mut self) {
        let (Some(cfg), Some(orch)) = (self.config.clone(), self.orchestrator.clone()) else {
            return;
        };
        let plan = crate::watch_task::WatchPlan::from_config(
            &cfg,
            std::path::Path::new(&self.settings.config_path),
        );
        self.tasks.push(crate::watch_task::WatchTask::spawn(
            plan,
            orch,
            self.dfps.clone(),
            &self.settings,
            |m: &str| crate::log_msg(m),
        ));
        crate::log_msg("Rust: preset/hint watcher started");
    }
}

/// Read + parse the config. `None` means the daemon runs without policy (the early
/// smoke-test path) — never a reason to abort.
fn load_config(path: &str) -> Option<uperf_config::Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => match uperf_config::Config::from_slice(text.as_bytes()) {
            Ok(c) => Some(c),
            Err(e) => {
                crate::log_msg(&format!("Rust: config parse failed: {e}"));
                None
            }
        },
        Err(e) => {
            crate::log_msg(&format!("Rust: cannot read config '{path}': {e}"));
            None
        }
    }
}

/// The preset named by `modules.switcher.switchInode` (default
/// `/sdcard/Android/yc/uperf/cur_powermode.txt`), matching upstream's
/// `Preset inode -> '<mode>'` startup behaviour.
fn current_mode(cfg: &uperf_config::Config) -> String {
    let inode = cfg
        .modules_map()
        .and_then(|m| m.get("switcher"))
        .and_then(|s| s.get("switchInode"))
        .and_then(|p| p.as_str())
        .unwrap_or("/sdcard/Android/yc/uperf/cur_powermode.txt");
    std::fs::read_to_string(inode)
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "balance".into())
}

/// Config + initial preset, plus the two startup lines upstream prints (criterion 2 in
/// AGENT.md §1): the config identity and the knob-writability warnings, verbatim and
/// **without** the `Rust:` prefix this crate's own diagnostics carry, so they grep
/// against an upstream log.
fn load_config_and_mode(path: &str) -> (Option<uperf_config::Config>, String) {
    let cfg = load_config(path);
    let mode = match cfg.as_ref() {
        Some(c) => current_mode(c),
        None => "balance".to_string(),
    };
    if let Some(c) = cfg.as_ref() {
        let (name, author) = c.meta_ident();
        crate::log_msg(&crate::startup_lines::config_line(&name, &author));
        for line in crate::startup_lines::knob_warnings(
            &c.sysfs_knob_table(),
            &crate::startup_lines::real_probe(),
        ) {
            crate::log_msg(&line);
        }
        // Apply `modules.log.level` (upstream's LogLevelSwitcher). Every shipped
        // config sets "info"; the logger used to be hardcoded to debug.
        if let Some(level) = c.log_level() {
            if let Ok(lv) = std::ffi::CString::new(level) {
                // SAFETY: NUL-terminated, read only for the duration of the call.
                unsafe { crate::ffi::uperf_bridge_set_log_level(lv.as_ptr()) };
            }
        }
    }
    (cfg, mode)
}
