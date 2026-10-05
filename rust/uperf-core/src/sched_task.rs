//! The scan loop that turns [`SchedPlanner`] decisions into kernel changes.
//!
//! Enumerates `/proc`, resolves each process through the planner, and applies the
//! resulting affinity / SCHED class to each thread — but only when the decision
//! differs from what was last applied to that tid, so a steady state costs
//! nothing (this is the "高度优化" claim in `config/README.md` line 160).
//!
//! Two test hooks, both explicitly *not* config semantics:
//!   * `UPERF_SCHED_DRY_RUN=1` — log decisions without touching the kernel.
//!     **Use this in any device harness**: `UPERF_FAKE_ROOT` redirects the
//!     *sysfs* writer, but `sched_setaffinity`/`sched_setscheduler` are syscalls
//!     with no path to redirect, so a scheduler run against the shipped config
//!     really does retune every process on the device;
//!   * `UPERF_SCHED_ONLY=<substr>` — restrict to processes whose name contains the
//!     substring, so a live check can be confined to a process we own.

#![allow(dead_code)]

use std::collections::HashMap;


use uperf_config::{SchedPlanner, SchedPolicy};

use crate::sched_apply::{self, ProcInfo};

/// What was last written to a tid, so an unchanged decision is skipped.
#[derive(Debug, Clone, PartialEq)]
struct Applied {
    cpus: Option<Vec<usize>>,
    policy: SchedPolicy,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ScanReport {
    /// Every process walked in `/proc` (whether or not a rule matched it).
    pub procs_seen: usize,
    /// Processes with at least one matching rule.
    pub procs_matched: usize,
    /// Threads examined — only counted for matched processes, so this is *not*
    /// bounded below by `procs_seen`.
    pub threads_seen: usize,
    pub affinity_changes: usize,
    pub policy_changes: usize,
    pub errors: usize,
    /// Threads whose decision was already in effect.
    pub unchanged: usize,
    /// Affinity writes the kernel accepted but which did not take effect
    /// (cpuset constraint or an external controller re-applying its mask).
    pub affinity_ineffective: usize,
}

impl ScanReport {
    fn merge(&mut self, o: &ScanReport) {
        self.procs_seen += o.procs_seen;
        self.procs_matched += o.procs_matched;
        self.threads_seen += o.threads_seen;
        self.affinity_changes += o.affinity_changes;
        self.policy_changes += o.policy_changes;
        self.errors += o.errors;
        self.unchanged += o.unchanged;
        self.affinity_ineffective += o.affinity_ineffective;
    }
}

pub struct SchedApplier {
    planner: SchedPlanner,
    /// Package name of the current top app (from `topapp.pkgName` events).
    top_app: Option<String>,
    /// Scheduler scene from the hint FSM: `idle` / `touch` / `boost`.
    scene: String,
    applied: HashMap<i32, Applied>,
    pub dry_run: bool,
    /// Verification-only process filter (`UPERF_SCHED_ONLY`).
    only: Option<String>,
}

impl SchedApplier {
    pub fn new(planner: SchedPlanner) -> Self {
        Self {
            planner,
            top_app: None,
            scene: "idle".to_string(),
            applied: HashMap::new(),
            dry_run: std::env::var("UPERF_SCHED_DRY_RUN").map(|v| v == "1").unwrap_or(false),
            only: std::env::var("UPERF_SCHED_ONLY").ok().filter(|s| !s.is_empty()),
        }
    }

    pub fn planner(&self) -> &SchedPlanner {
        &self.planner
    }
    pub fn planner_mut(&mut self) -> &mut SchedPlanner {
        &mut self.planner
    }
    pub fn top_app(&self) -> Option<&str> {
        self.top_app.as_deref()
    }
    pub fn scene(&self) -> &str {
        &self.scene
    }

    /// The top app changed (`topapp.pkgName`). A change also means the previous
    /// top app must be re-evaluated, which the next scan does naturally.
    pub fn set_top_app(&mut self, pkg: Option<String>) {
        if self.top_app != pkg {
            self.top_app = pkg;
            self.applied.clear();
        }
    }

    /// The hint FSM moved to another scene.
    pub fn set_scene(&mut self, scene: &str) {
        if self.scene != scene {
            self.scene = scene.to_string();
            self.applied.clear();
        }
    }

    /// One pass over `/proc`.
    pub fn scan(&mut self) -> ScanReport {
        let mut report = ScanReport::default();
        for p in sched_apply::list_procs() {
            if let Some(only) = &self.only {
                if !p.name.contains(only.as_str()) {
                    continue;
                }
            }
            report.merge(&self.apply_proc(&p));
        }
        // Forget tids that no longer exist so the map cannot grow forever.
        let live: std::collections::HashSet<i32> = sched_apply::list_procs()
            .into_iter()
            .flat_map(|p| p.threads.into_iter().map(|t| t.tid))
            .collect();
        self.applied.retain(|tid, _| live.contains(tid));
        report
    }

    fn apply_proc(&mut self, p: &ProcInfo) -> ScanReport {
        let mut r = ScanReport { procs_seen: 1, ..Default::default() };
        let is_top = self.top_app.as_deref() == Some(p.name.as_str());
        // The main thread's `comm` is what `/MAIN_THREAD/` substitutes to.
        let main_comm = p
            .threads
            .iter()
            .find(|t| t.is_main)
            .map(|t| t.comm.clone())
            .unwrap_or_else(|| p.name.clone());

        // Does any rule match this process? (A `"."` catch-all rule means yes for
        // almost everything, but be explicit.)
        let matches = self.planner.match_process(&p.name).is_some();
        if !matches {
            return r;
        }
        r.procs_matched = 1;

        // Resolve the scene once per process: it only depends on the rule's
        // `pinned` flag, the top-app state and the FSM scene.
        let scene = self.scene.clone();
        for t in &p.threads {
            r.threads_seen += 1;
            let Some(d) = self.planner.decide(
                &p.name,
                is_top,
                &scene,
                &main_comm,
                &t.comm,
            ) else {
                continue;
            };
            let want = Applied { cpus: d.cpus.clone(), policy: d.policy };
            if self.applied.get(&t.tid) == Some(&want) {
                r.unchanged += 1;
                continue;
            }
            if self.dry_run {
                if want.cpus.is_some() || want.policy != SchedPolicy::Skip {
                    log_line(&format!(
                        "Rust: sched[DRY] pid={} {:?} tid={} {:?} rule={:?} scene={} ac={} pc={} -> cpus={:?} policy={:?}",
                        p.pid, p.name, t.tid, t.comm, d.rule, d.scene, d.ac, d.pc, d.cpus, d.policy
                    ));
                }
                r.unchanged += 1;
                continue;
            }
            let mut changed = false;
            if let Some(cpus) = &d.cpus {
                match sched_apply::set_affinity(t.tid, cpus) {
                    Ok(()) => {
                        r.affinity_changes += 1;
                        changed = true;
                        // The syscall can be accepted and still not be what the
                        // thread ends up with: the cpuset cgroup can constrain it
                        // (a target outside the cpuset comes back EINVAL), and
                        // Android's task-profile controller re-applies its own
                        // mask. Report the discrepancy rather than claiming
                        // success — measured on device with a probe pinned to a
                        // cpuset that excluded the requested CPU.
                        match sched_apply::cpus_allowed(t.tid) {
                            Some(now) if now != *cpus => {
                                r.affinity_ineffective += 1;
                                log_line(&format!(
                                    "Rust: sched pid={} tid={} affinity={cpus:?} accepted but effective mask is {now:?} (cpuset or task-profile override)",
                                    p.pid, t.tid
                                ));
                            }
                            Some(_) => {}
                            None => {}
                        }
                    }
                    Err(e) => {
                        r.errors += 1;
                        log_line(&format!(
                            "Rust: sched pid={} tid={} setaffinity {cpus:?} failed: {e}",
                            p.pid, t.tid
                        ));
                    }
                }
            }
            if d.policy != SchedPolicy::Skip {
                match sched_apply::set_sched(t.tid, d.policy) {
                    Ok(()) => {
                        r.policy_changes += 1;
                        changed = true;
                    }
                    Err(e) => {
                        r.errors += 1;
                        log_line(&format!(
                            "Rust: sched pid={} tid={} setscheduler {:?} failed: {e}",
                            p.pid, t.tid, d.policy
                        ));
                    }
                }
            }
            if changed {
                log_line(&format!(
                    "Rust: sched pid={} {:?} tid={} {:?} rule={:?} scene={} ac={} pc={} -> cpus={:?} policy={:?}",
                    p.pid, p.name, t.tid, t.comm, d.rule, d.scene, d.ac, d.pc, d.cpus, d.policy
                ));
                self.applied.insert(t.tid, want);
            }
        }
        r
    }
}

fn log_line(s: &str) {
    use std::sync::{Mutex, OnceLock};
    static BUF: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
    let buf = BUF.get_or_init(|| Mutex::new(Vec::with_capacity(256)));
    // `try_lock`, not `lock`: this buffer is written from the shutdown path too,
    // and a signal handler can re-enter that path (SIGTERM + the supervisor's
    // SIGUSR1). A plain `lock()` there deadlocks on itself and the process never
    // exits. A dropped log line is always better than a hung daemon.
    let Ok(mut b) = buf.try_lock() else { return };
    b.clear();
    b.extend_from_slice(s.as_bytes());
    b.push(b'\n');
    // SAFETY: the C++ sink copies before returning.
    unsafe {
        crate::ffi::uperf_bridge_write_log(std::ptr::null(), b.as_ptr().cast(), b.len());
    }
}

/// Handle to the running context-scheduler thread.
pub struct SchedTask {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SchedTask {
    /// Build the planner for `cfg`, resolving `/HOME_PACKAGE/` from the device.
    pub fn planner_for(
        cfg: &uperf_config::Config,
        log: &mut dyn FnMut(&str),
    ) -> Option<SchedPlanner> {
        let modules = cfg.modules_map()?;
        let sc = uperf_config::SchedConfig::from_modules(modules)?;
        if !sc.enable {
            log("Rust: context scheduler disabled by config");
            return None;
        }
        // On failure keep the *literal* token: substituting an empty package
        // would compile the launcher rule's pattern to "" and match every
        // process on the system (observed in the device dry run).
        let home = sched_apply::resolve_home_package_with(log)
            .unwrap_or_else(|| "/HOME_PACKAGE/".to_string());
        log(&format!("Rust: current home is '{home}'"));
        match SchedPlanner::new(sc, &home) {
            Ok(p) => {
                let n = p.anomalies().len();
                if n > 0 {
                    for a in p.anomalies() {
                        log(&format!("Rust: sched config anomaly: {a}"));
                    }
                }
                Some(p)
            }
            Err(e) => {
                log(&format!("Rust: context scheduler disabled: {e}"));
                None
            }
        }
    }

    /// Spawn the scan loop. `state` supplies `(scene, top_app, generation)` from
    /// the orchestrator, so the loop scans promptly when either moves and
    /// otherwise backs off.
    pub fn spawn<S>(planner: SchedPlanner, state: S) -> Self
    where
        S: Fn() -> (String, Option<String>, u64) + Send + 'static,
    {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_child = stop.clone();
        let thread = std::thread::Builder::new()
            .name("uperf-sched".into())
            .spawn(move || {
                let mut applier = SchedApplier::new(planner);
                let mut seen_gen = u64::MAX; // first pass always scans
                let mut idle_ticks: u32 = 0;
                while !stop_child.load(std::sync::atomic::Ordering::Relaxed) {
                    let (scene, top_app, generation) = state();
                    let changed = generation != seen_gen;
                    if changed {
                        seen_gen = generation;
                        applier.set_scene(&scene);
                        applier.set_top_app(top_app);
                        idle_ticks = 0;
                    } else {
                        idle_ticks += 1;
                    }
                    // Scan on a state change; otherwise every 4th tick (~1 s), so
                    // a steady state does not walk /proc at full rate.
                    if changed || idle_ticks % 4 == 0 {
                        let r = applier.scan();
                        if r.affinity_changes + r.policy_changes + r.errors + r.affinity_ineffective
                            > 0
                        {
                            log_line(&format!(
                                "Rust: sched scene={} top={:?} procs={}/{} threads={} aff={} ({} ineffective) prio={} err={}",
                                applier.scene(),
                                applier.top_app(),
                                r.procs_matched,
                                r.procs_seen,
                                r.threads_seen,
                                r.affinity_changes,
                                r.affinity_ineffective,
                                r.policy_changes,
                                r.errors
                            ));
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                log_line("Rust: context scheduler stopped");
            })
            .expect("spawn uperf-sched");
        Self { stop, thread: Some(thread) }
    }

    pub fn stop(&mut self) {
        let Some(t) = self.thread.take() else { return };
        if !crate::shutdown::stop_and_join(
            &self.stop,
            t,
            crate::shutdown::STOP_TIMEOUT,
            || {},
        ) {
            log_line("Rust: sched task did not stop in time, detached");
        }
    }

    pub fn is_running(&self) -> bool {
        self.thread.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uperf_config::SchedConfig;
    use serde_json::json;

    /// A synthetic config whose rules point at *this* test process, so the scan
    /// can be exercised for real without touching unrelated processes.
    fn self_targeting_cfg(me: &str) -> SchedConfig {
        SchedConfig::from_value(&json!({
            "enable": true,
            "cpumask": { "all": [0, 1, 2, 3] },
            "affinity": { "ui": { "bg": "all", "idle": "all", "touch": "all", "boost": "all" } },
            "prio": { "ui": { "bg": 0, "idle": 0, "touch": 0, "boost": 0 } },
            "rules": [{
                "name": "self",
                "regex": me,
                "pinned": false,
                "rules": [ { "k": ".", "ac": "ui", "pc": "ui" } ]
            }]
        }))
        .unwrap()
    }

    fn self_name() -> String {
        let pid = std::process::id() as i32;
        sched_apply::read_proc(pid).unwrap().name
    }

    fn applier_for_self() -> SchedApplier {
        let name = self_name();
        let p = SchedPlanner::new(self_targeting_cfg(&name), "com.miui.home").unwrap();
        SchedApplier::new(p)
    }

    #[test]
    fn dry_run_changes_nothing() {
        let mut a = applier_for_self();
        a.dry_run = true;
        a.only = Some(self_name());
        let me = sched_apply::current_tid();
        let before = sched_apply::thread_sched(me).unwrap().0;
        let r = a.scan();
        assert!(r.procs_matched >= 1, "{r:?}");
        assert_eq!(r.affinity_changes, 0);
        assert_eq!(r.policy_changes, 0);
        assert_eq!(sched_apply::thread_sched(me).unwrap().0, before);
    }

    #[test]
    fn unchanged_decision_is_skipped_on_the_second_pass() {
        // Same scene + no top-app change -> the second scan must find nothing to
        // do, which is the whole point of the change cache.
        let mut a = applier_for_self();
        a.only = Some(self_name());
        // Everything is a no-op by construction (prio 0, affinity "all"), so use
        // a real policy to make changes observable.
        let cfg = SchedConfig::from_value(&json!({
            "enable": true,
            "cpumask": { "all": [0, 1] },
            "affinity": { "ui": { "bg": "all" } },
            "prio": { "ui": { "bg": -1 } },
            "rules": [{ "name": "self", "regex": self_name(), "pinned": false,
                        "rules": [ { "k": ".", "ac": "ui", "pc": "ui" } ] }]
        })).unwrap();
        a = SchedApplier::new(SchedPlanner::new(cfg, "com.miui.home").unwrap());
        a.only = Some(self_name());
        if a.planner().anomalies().len() > 0 {
            // affinity[ui] has no "idle"/"touch"/"boost" in this synthetic cfg.
        }
        let first = a.scan();
        let second = a.scan();
        assert!(
            first.policy_changes + first.affinity_changes >= 1,
            "first pass should have work to do: {first:?}"
        );
        assert_eq!(
            second.policy_changes + second.affinity_changes,
            0,
            "second pass must be a no-op: {second:?}"
        );
        assert!(second.unchanged >= first.unchanged);
        // Leave the harness thread tidy.
        let _ = sched_apply::set_sched(0, SchedPolicy::Normal { nice: 0 });
    }

    #[test]
    fn scene_change_invalidates_the_cache() {
        let mut a = applier_for_self();
        a.only = Some(self_name());
        let _ = a.scan();
        let before = a.applied.len();
        a.set_scene("touch");
        assert!(a.applied.is_empty(), "a scene change must drop the cache (was {before})");
    }

    #[test]
    fn top_app_change_invalidates_the_cache() {
        let mut a = applier_for_self();
        a.set_top_app(Some("com.foo".into()));
        a.applied.insert(1, Applied { cpus: None, policy: SchedPolicy::Skip });
        a.set_top_app(Some("com.bar".into()));
        assert!(a.applied.is_empty());
        // Setting the same value again must not clear anything needlessly.
        a.applied.insert(1, Applied { cpus: None, policy: SchedPolicy::Skip });
        a.set_top_app(Some("com.bar".into()));
        assert_eq!(a.applied.len(), 1);
    }

    #[test]
    fn planner_for_returns_none_when_sched_is_disabled() {
        let mut cfg = uperf_config::Config::from_value(serde_json::json!({
            "meta": {"name":"t","author":"t"},
            "modules": {"sched": {"enable": false, "cpumask": {}, "affinity": {}, "prio": {}, "rules": []}},
            "initials": {}, "presets": {"balance": {"*": {}}}
        })).unwrap();
        let mut log = |_: &str| {};
        assert!(SchedTask::planner_for(&cfg, &mut log).is_none());
        // ...and Some(..) once enabled.
        cfg = uperf_config::Config::from_value(serde_json::json!({
            "meta": {"name":"t","author":"t"},
            "modules": {"sched": {"enable": true,
                "cpumask": {"all": [0,1]},
                "affinity": {"ui": {"bg": "all"}},
                "prio": {"ui": {"bg": -1}},
                "rules": [{"name":"r","regex":".","pinned":false,
                           "rules":[{"k":".","ac":"ui","pc":"ui"}]}]}},
            "initials": {}, "presets": {"balance": {"*": {}}}
        })).unwrap();
        std::env::set_var("UPERF_HOME_PACKAGE", "com.miui.home");
        assert!(SchedTask::planner_for(&cfg, &mut log).is_some());
    }

    #[test]
    fn task_thread_scans_and_stops() {
        let name = self_name();
        let p = SchedPlanner::new(self_targeting_cfg(&name), "com.miui.home").unwrap();
        let gen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let g2 = gen.clone();
        let mut task = SchedTask::spawn(p, move || {
            ("idle".to_string(), None, g2.load(std::sync::atomic::Ordering::Relaxed))
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(task.is_running());
        task.stop();
        assert!(!task.is_running());
    }

    #[test]
    fn only_filter_excludes_everything_else() {
        let mut a = applier_for_self();
        a.only = Some("definitely-not-a-real-process-name".to_string());
        a.dry_run = true;
        let r = a.scan();
        assert_eq!(r.procs_seen, 0);
        assert_eq!(r.procs_matched, 0);
    }

    #[test]
    fn scan_walks_the_real_proc_tree_in_dry_run() {
        // No filter: this walks every process on the host, but dry_run means no
        // kernel state changes at all.
        let name = self_name();
        let p = SchedPlanner::new(self_targeting_cfg(&name), "com.miui.home").unwrap();
        let mut a = SchedApplier::new(p);
        a.dry_run = true;
        let r = a.scan();
        assert!(r.procs_seen > 5, "expected a populated /proc: {r:?}");
        assert!(
            r.procs_matched <= r.procs_seen,
            "matched cannot exceed walked: {r:?}"
        );
        // Only our own process matches this self-targeting config, so the
        // thread count is small even though every process was walked.
        assert!(r.threads_seen >= 1, "our own threads must be examined: {r:?}");
        assert_eq!(r.affinity_changes, 0, "dry run must not touch the kernel");
        assert_eq!(r.policy_changes, 0, "dry run must not touch the kernel");
    }
}
