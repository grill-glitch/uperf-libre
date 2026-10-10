//! Device-side governor task.
//!
//! Reads the real OPP tables + `/proc/stat`, ticks the governor, and turns each
//! cluster's target into a knob write. Writes go through the same
//! [`crate::orchestrator::Sink`] the scene writes use, so `UPERF_FAKE_ROOT`
//! redirects them for offline validation and the real sysfs is untouched until
//! a later milestone wires the fd-cached writer.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
#[cfg(test)]
use std::time::Duration;

use uperf_config::{
    cpu_slices, freq_targets, freq_writes, governor_from_config, parse_stat, Config, FreqTarget,
    Governor,
};

/// Root under which sysfs writes are redirected for offline validation.
fn fake_root() -> Option<String> {
    std::env::var("UPERF_FAKE_ROOT").ok().filter(|s| !s.is_empty())
}

/// Rewrite a sysfs path under `UPERF_FAKE_ROOT`, if set.
fn apply_root(path: &str, root: &Option<String>) -> String {
    match root {
        Some(r) => format!("{}{}", r.trim_end_matches('/'), path),
        None => path.to_string(),
    }
}

/// The `userspace` governor writer: arms `scaling_governor = userspace` for each
/// cluster, drives `scaling_setspeed` every cycle, and restores the original
/// governors on stop.
///
/// This is the only frequency-control mechanism verified to work on alioth
/// (`qcom-cpufreq-hw` registers `scaling_max_freq` read-only). It is a *full
/// takeover*: once a policy runs `userspace`, the kernel no longer scales it, so
/// the governor must publish a target every cycle — which it does.
#[derive(Debug, Default)]
pub struct UserspaceWriter {
    /// (policy_dir, original governor)
    armed: Vec<(String, String)>,
    /// Optional file where the originals are recorded for the stop script.
    state_file: Option<std::path::PathBuf>,
    /// Optional machine-readable status file (`uperf.state`, M9) and the config it
    /// describes. Written on arm/disarm only, never per tick.
    status: Option<crate::status::Target>,
}

impl UserspaceWriter {
    /// Where to record the governors we replaced, so the module's stop script can
    /// restore the truth even if this process is killed outright. Set from the
    /// config's directory; `None` disables the record.
    pub fn state_file(&mut self, path: Option<std::path::PathBuf>) {
        self.state_file = path;
    }

    /// Where to report the armed state, and the config it belongs to.
    pub fn status(&mut self, target: Option<crate::status::Target>) {
        self.status = target;
    }

    /// Report the takeover state only if the file is still ours (stop path).
    fn write_status_if_owner(&self, state: &str) {
        let Some(target) = self.status.as_ref() else {
            return;
        };
        crate::status::write_if_owner(
            &target.path,
            &crate::status::Snapshot {
                state,
                takeover: true,
                armed: &[],
                config: &target.config,
            },
        );
    }

    /// Report the takeover state. Best effort — see [`crate::status::write`].
    fn write_status(&self, state: &str) {
        let Some(target) = self.status.as_ref() else {
            return;
        };
        let policies: Vec<String> = self
            .armed
            .iter()
            .map(|(dir, _)| {
                dir.rsplit('/')
                    .next()
                    .unwrap_or(dir.as_str())
                    .to_string()
            })
            .collect();
        crate::status::write(
            &target.path,
            &crate::status::Snapshot {
                state,
                takeover: true,
                armed: &policies,
                config: &target.config,
            },
        );
    }

    /// Switch every userspace cluster to the `userspace` governor, remembering
    /// what it was. Idempotent.
    pub fn arm(&mut self, targets: &[FreqTarget]) {
        let root = fake_root();
        for t in targets {
            let FreqTarget::Userspace { policy_dir } = t else { continue };
            if self.armed.iter().any(|(d, _)| d == policy_dir) {
                continue;
            }
            let gov = apply_root(&format!("{policy_dir}/scaling_governor"), &root);
            let prev = std::fs::read_to_string(&gov)
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            if std::fs::write(&gov, "userspace").is_ok() {
                self.armed.push((policy_dir.clone(), prev));
            }
        }
        self.record_state();
    }

    /// Write `<policy-name> <original-governor>` lines. Only entries whose original
    /// is known and is not `userspace` are recorded — recording `userspace` as an
    /// "original" is how a device ends up stuck.
    ///
    /// The machine-readable status file is written here too, but *not* gated on the
    /// governor record: the two have different readers and either may be disabled.
    fn record_state(&self) {
        if let Some(path) = self.state_file.as_ref() {
            let mut out = String::new();
            for (dir, prev) in &self.armed {
                if prev.is_empty() || prev == "userspace" {
                    continue;
                }
                let name = dir.rsplit('/').next().unwrap_or(dir);
                out.push_str(&format!("{name} {prev}\n"));
            }
            if !out.is_empty() {
                let _ = std::fs::write(path, out);
            }
        }
        self.write_status("running");
    }

    /// Write one cycle's targets. Returns the number of frequencies applied.
    pub fn apply(&self, targets: &[FreqTarget], freqs_khz: &[f64]) -> usize {
        let root = fake_root();
        let mut n = 0;
        for (t, f) in targets.iter().zip(freqs_khz.iter()) {
            let FreqTarget::Userspace { policy_dir } = t else { continue };
            let path = apply_root(&format!("{policy_dir}/scaling_setspeed"), &root);
            if std::fs::write(&path, format!("{}", *f as i64)).is_ok() {
                n += 1;
            }
        }
        n
    }

    /// Restore each policy's original governor (and thus kernel-driven scaling).
    pub fn disarm(&mut self) {
        let root = fake_root();
        for (dir, prev) in self.armed.drain(..) {
            if prev.is_empty() {
                continue;
            }
            let gov = apply_root(&format!("{dir}/scaling_governor"), &root);
            let _ = std::fs::write(&gov, prev);
        }
        // `armed` is empty now, so the status reports `armed=0`: the daemon may
        // still be running (a clean stop writes `state=stopped` as well), but the
        // takeover is over. A reader that sees `state=running armed=0` is looking
        // at a run started without `UPERF_CPU_GOVERNOR=1`.
        //
        // Ownership-checked: a disarm that lands after a restart belongs to the
        // previous worker and must not overwrite the new one's status.
        self.write_status_if_owner("stopped");
    }

    pub fn is_armed(&self) -> bool {
        !self.armed.is_empty()
    }
}

use crate::orchestrator::{CollectingSink, Sink, SysfsWrite};

/// `<config dir>/orig_governor.txt` — where the governors we replace are recorded.
///
/// The module's stop script reads the same path, so a SIGKILL (which no in-process
/// handler can survive) still leaves enough information for something else to put
/// the device back.
fn state_file_for(_cfg: &Config) -> Option<std::path::PathBuf> {
    std::env::var("UPERF_STATE_FILE").ok().filter(|s| !s.is_empty()).map(std::path::PathBuf::from)
}

/// OPP list for a cpufreq policy, in kHz.
///
/// Preference order matches what the upstream binary reads:
/// `scaling_available_frequencies` → `scaling_boost_frequencies` →
/// `cpuinfo_max_freq` (+`cpuinfo_min_freq` as a two-point fallback).
pub fn read_opps_for_policy(policy_dir: &str) -> Vec<f64> {
    let read_list = |f: &str| -> Vec<f64> {
        std::fs::read_to_string(format!("{policy_dir}/{f}"))
            .ok()
            .map(|s| {
                s.split_whitespace()
                    .filter_map(|t| t.parse::<f64>().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut opps = read_list("scaling_available_frequencies");
    if opps.is_empty() {
        opps = read_list("scaling_boost_frequencies");
    }
    if opps.is_empty() {
        let mut two = read_list("cpuinfo_min_freq");
        two.extend(read_list("cpuinfo_max_freq"));
        two.sort_by(|a, b| a.partial_cmp(b).unwrap());
        two.dedup();
        opps = two;
    }
    opps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    opps
}

/// OPPs for every cluster, in `powerModel` order.
pub fn read_opps_for_config(cfg: &Config) -> Vec<Vec<f64>> {
    let Some(modules) = cfg.modules_map() else {
        return Vec::new();
    };
    let models = uperf_config::PowerModel::list_from_modules(modules);
    cpu_slices(&models)
        .iter()
        .map(|cores| {
            let first = cores.first().copied().unwrap_or(0);
            read_opps_for_policy(&format!("/sys/devices/system/cpu/cpufreq/policy{first}"))
        })
        .collect()
}

/// Handle to the running governor thread.
pub struct CpuTask {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The `userspace` governor arming, shared with the task thread so the stop
    /// path can restore the kernel's original governors even if the task thread
    /// is stuck and never reaches its own `disarm()`.
    writer: Arc<std::sync::Mutex<UserspaceWriter>>,
}

impl CpuTask {
    /// Spawn with an injected governor + stat reader (unit-testable path).
    pub fn spawn_with<F>(
        mut gov: Governor,
        targets: Vec<FreqTarget>,
        state_file: Option<std::path::PathBuf>,
        status: Option<crate::status::Target>,
        mut read_stat: F,
    ) -> Self
    where
        F: FnMut() -> uperf_config::CpuJiffies + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_child = stop.clone();
        let writer = Arc::new(std::sync::Mutex::new(UserspaceWriter::default()));
        let writer_child = writer.clone();
        let thread = std::thread::Builder::new()
            .name("uperf-cpu".into())
            .spawn(move || {
                let mut writer = writer_child.lock().expect("writer lock");
                writer.state_file(state_file);
                writer.status(status);
                writer.arm(&targets);
                if writer.is_armed() {
                    log_line(&format!(
                        "Rust: cpu governor armed {} userspace cluster(s)",
                        writer.armed.len()
                    ));
                }
                // Prime the sampler and throw away the first span: it covers the
                // interval from boot to process start, which reads as a full-load
                // blip and would kick every cluster to max once. Sample, wait one
                // period, then start from the *second* reading.
                std::thread::sleep(gov.sample_period());
                let mut prev = read_stat();
                let mut tick = 0u64;
                // ⑥ thermal feedback (fas-rs's `core_temp_thresh`): when the SoC runs
                // hot, ease the sustained power budget (PL1) — the OPP *points* are
                // never chosen by temperature. Sampled at 1 Hz; the governor ticks far
                // faster than any thermal zone moves.
                let tpolicy = uperf_config::thermal::ThermalPolicy::from_env();
                let troot = std::env::var("UPERF_THERMAL_ROOT")
                    .unwrap_or_else(|_| "/sys/class/thermal".into());
                let tzones = uperf_config::thermal::discover_cpu_zones(std::path::Path::new(&troot));
                match uperf_config::thermal::read_max_temp_c(&tzones) {
                    Some(t) => log_line(&format!(
                        "Rust: thermal {} zone(s) under {troot}, now {t:.1} C (thresh {:.0} C, floor x{:.2})",
                        tzones.len(),
                        tpolicy.thresh_c,
                        tpolicy.floor
                    )),
                    None => log_line(&format!(
                        "Rust: thermal no readable cpu zone under {troot}; PL1 left alone"
                    )),
                }
                let mut next_thermal = Instant::now();
                let mut last_scale = 1.0f64;
                while !stop_child.load(Ordering::Relaxed) {
                    let period = gov.sample_period();
                    std::thread::sleep(period);
                    if stop_child.load(Ordering::Relaxed) {
                        break;
                    }
                    if Instant::now() >= next_thermal {
                        next_thermal = Instant::now() + std::time::Duration::from_secs(1);
                        if let Some(t) = uperf_config::thermal::read_max_temp_c(&tzones) {
                            let s = uperf_config::thermal::pl1_scale(t, tpolicy);
                            gov.set_thermal_scale(s);
                            if (s - last_scale).abs() > 0.02 {
                                last_scale = s;
                                log_line(&format!(
                                    "Rust: thermal {t:.1} C -> PL1 x{s:.2} (of {:.2} W)",
                                    gov.tunables.slow_limit_power
                                ));
                            }
                        }
                    }
                    let cur = read_stat();
                    let freqs = gov.tick(&prev, &cur, Instant::now());
                    prev = cur;
                    tick += 1;
                    // Log every 8th tick (~0.1-0.4 s) to keep the file readable.
                    if tick % 8 == 1 {
                        log_cpu_tick(&gov, &freqs);
                    }
                    // Userspace clusters first: they need a target every cycle.
                    writer.apply(&targets, &freqs);

                    // Config-declared knobs (scaling_max_freq / msm_performance)
                    // as a secondary path, so a device that supports them gets
                    // them too. No-ops when the list is empty.
                    let writes: Vec<SysfsWrite> = freq_writes(&targets, &freqs)
                        .into_iter()
                        .map(|(knob, value)| {
                            let path = targets
                                .iter()
                                .find(|t| t.knob() == Some(knob.as_str()))
                                .and_then(|t| t.path())
                                .unwrap_or("")
                                .to_string();
                            SysfsWrite { path, value }
                        })
                        .filter(|w| !w.path.is_empty())
                        .collect();
                    if !writes.is_empty() {
                        let root = fake_root();
                        match root.as_deref() {
                            Some(root) => {
                                let mut sink =
                                    match crate::sysfs_ledger::SysfsLedger::for_status() {
                                        Some(l) => {
                                            crate::orchestrator::UnderRootSink::with_ledger(root, l)
                                        }
                                        None => crate::orchestrator::UnderRootSink::new(root),
                                    };
                                for w in &writes {
                                    sink.write(w);
                                }
                            }
                            None => {
                                let mut sink = CollectingSink::default();
                                for w in &writes {
                                    sink.write(w);
                                }
                            }
                        }
                    }
                }
                // Drop the lock before disarming so a stuck *caller* cannot
                // deadlock us; disarm is idempotent.
                drop(writer);
                writer_child.lock().expect("writer lock").disarm();
                log_line("Rust: cpu governor disarmed (original governor restored)");
            })
            .expect("spawn uperf-cpu governor");
        Self {
            stop,
            thread: Some(thread),
            writer,
        }
    }

    /// Whether the userspace frequency takeover is wanted.
    ///
    /// **Off unless `UPERF_CPU_GOVERNOR=1`.** It is a takeover: the kernel stops
    /// scaling the policy and the last published frequency sticks if the daemon
    /// dies without disarming. Measured on alioth against the stock `schedutil`:
    ///
    /// ```text
    ///                        idle            under 4 busy threads
    ///   schedutil  policy0   1804800 (max)   1804800
    ///   ours       policy0   883200-1420800  691200 (min)
    /// ```
    ///
    /// Because `config/sdm888.json` caps the whole CPU at PL1 = 1.0 W and allocates
    /// it by marginal cost, the little cluster (efficiency 115 vs 320/400) gets
    /// almost nothing — so interactive work that runs there drops to the minimum
    /// frequency. Upstream's own governor cannot run on this kernel at all (all
    /// eight CpufreqWriter strategies need a min-freq knob that the driver locks),
    /// so taking over preserves no upstream behaviour. The default is therefore to
    /// leave the platform's governor alone.
    pub fn takeover_wanted() -> bool {
        std::env::var("UPERF_CPU_GOVERNOR").map(|v| v == "1").unwrap_or(false)
    }

    /// Production constructor: real OPPs + real `/proc/stat`.
    pub fn spawn(cfg: &Config, mode: &str, scene: &str) -> Option<Self> {
        if !Self::takeover_wanted() {
            log_line(
                "Rust: cpu governor idle (set UPERF_CPU_GOVERNOR=1 to take over frequency control)",
            );
            return None;
        }
        let opps = read_opps_for_config(cfg);
        if opps.iter().all(|o| o.is_empty()) {
            log_line("Rust: cpu governor skipped (no OPP tables readable)");
            return None;
        }
        let gov = governor_from_config(cfg, mode, scene, &opps)?;
        let targets = freq_targets(cfg);
        let n_cpu = gov.clusters.iter().map(|c| c.cores.len()).sum::<usize>().max(1);
        Some(Self::spawn_with(
            gov,
            targets,
            state_file_for(cfg),
            crate::status::target(),
            move || {
                std::fs::read_to_string("/proc/stat")
                    .map(|t| parse_stat(&t, n_cpu))
                    .unwrap_or_default()
            },
        ))
    }

    pub fn stop(&mut self) {
        let Some(t) = self.thread.take() else { return };
        let writer = self.writer.clone();
        let ok = crate::shutdown::stop_and_join(&self.stop, t, crate::shutdown::STOP_TIMEOUT, || {
            // The task thread never finished, so it never disarmed. Restore the
            // kernel's governors from here: leaving every policy pinned in
            // `userspace` at the last published frequency is the one failure this
            // module cannot recover from on its own.
            if writer.lock().map(|mut w| w.disarm()).is_ok() {
                log_line("Rust: cpu governor disarmed by the stop path (task did not exit)");
            }
        });
        if !ok {
            log_line("Rust: cpu task did not stop in time, detached");
        }
    }

    pub fn is_running(&self) -> bool {
        self.thread.is_some()
    }
}

fn log_cpu_tick(gov: &Governor, freqs: &[f64]) {
    let mut s = String::with_capacity(160);
    let _ = write!(s, "Rust: cpu");
    for ((i, cl), f) in gov.clusters.iter().enumerate().zip(freqs.iter()) {
        let _ = write!(
            s,
            " c{i}={:.0}kHz(load {:.2}{})",
            f,
            cl.load,
            if cl.predicted { ",pred" } else { "" }
        );
    }
    let _ = write!(s, " pool={:.2}", gov.pool);
    log_line(&s);
}

fn log_line(s: &str) {
    use std::sync::{Mutex, OnceLock};
    static BUF: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
    let buf = BUF.get_or_init(|| Mutex::new(Vec::with_capacity(192)));
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

/// Host-side stub for the C++ log sink.
///
/// `uperf-core` is a `staticlib` linked into the Android binary, where
/// `uperf_bridge_write_log` comes from `cpp/uperf/bridge.cpp`. Host unit tests
/// have no C++ side, so provide a no-op with the same symbol. Without it the
/// test binary fails to link as soon as any test reaches the logging path.
#[cfg(test)]
#[no_mangle]
extern "C" fn uperf_bridge_write_log(
    _tag: *const libc::c_char,
    _msg: *const libc::c_char,
    _len: usize,
) {
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `UPERF_FAKE_ROOT` is process-wide, and these tests run in parallel: two of
    /// them pointing it at different fake roots is a race, and the loser's writes
    /// land in the other test's tree (observed: `original governor must be
    /// restored` failing with `userspace`). Serialise every test that touches it.
    static FAKE_ROOT_LOCK: Mutex<()> = Mutex::new(());

    fn fake_root_guard() -> std::sync::MutexGuard<'static, ()> {
        // A poisoned lock only means another test failed; the guard is still valid.
        FAKE_ROOT_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn missing_policy_dir_yields_no_opps() {
        assert!(read_opps_for_policy("/definitely/not/here").is_empty());
    }

    #[test]
    fn userspace_writer_arms_applies_and_restores() {
        let _guard = fake_root_guard();
        let tmp = std::env::temp_dir().join(format!("uperf_fake_{}", std::process::id()));
        let dir = tmp.join("sys/devices/system/cpu/cpufreq/policy0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scaling_governor"), "schedutil\n").unwrap();

        std::env::set_var("UPERF_FAKE_ROOT", tmp.to_str().unwrap());
        let targets = vec![FreqTarget::Userspace {
            policy_dir: "/sys/devices/system/cpu/cpufreq/policy0".into(),
        }];
        let mut w = UserspaceWriter::default();
        w.arm(&targets);
        assert!(w.is_armed());
        assert_eq!(
            std::fs::read_to_string(dir.join("scaling_governor")).unwrap().trim(),
            "userspace"
        );
        assert_eq!(w.apply(&targets, &[1_478_400.0]), 1);
        assert_eq!(
            std::fs::read_to_string(dir.join("scaling_setspeed")).unwrap().trim(),
            "1478400"
        );
        w.disarm();
        assert_eq!(
            std::fs::read_to_string(dir.join("scaling_governor")).unwrap().trim(),
            "schedutil",
            "original governor must be restored"
        );
        std::env::remove_var("UPERF_FAKE_ROOT");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn userspace_writer_reports_status_on_arm_and_disarm() {
        let _guard = fake_root_guard();
        let tmp = std::env::temp_dir().join(format!("uperf_status_arm_{}", std::process::id()));
        let dir = tmp.join("sys/devices/system/cpu/cpufreq/policy4");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scaling_governor"), "schedutil\n").unwrap();
        let status_path = tmp.join("uperf.state");

        std::env::set_var("UPERF_FAKE_ROOT", tmp.to_str().unwrap());
        let targets = vec![FreqTarget::Userspace {
            policy_dir: "/sys/devices/system/cpu/cpufreq/policy4".into(),
        }];
        let mut w = UserspaceWriter::default();
        w.status(Some(crate::status::Target {
            path: status_path.clone(),
            config: "/sdcard/Android/yc/uperf/uperf.json".into(),
        }));
        w.arm(&targets);

        let armed = std::fs::read_to_string(&status_path).unwrap();
        assert!(armed.contains("state=running\n"), "{armed}");
        assert!(armed.contains("armed=1\n"), "{armed}");
        assert!(
            armed.contains("policies=policy4\n"),
            "the policy name, not the whole path: {armed}"
        );

        w.disarm();
        let disarmed = std::fs::read_to_string(&status_path).unwrap();
        assert!(disarmed.contains("state=stopped\n"), "{disarmed}");
        assert!(disarmed.contains("armed=0\n"), "{disarmed}");
        assert!(disarmed.contains("policies=\n"), "{disarmed}");

        std::env::remove_var("UPERF_FAKE_ROOT");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn governor_task_runs_and_stops() {
        let mut cfg_json = serde_json::json!({
            "meta": {"name":"t","author":"t"},
            "modules": {
                "cpu": {"enable": true, "powerModel": [
                    {"efficiency":115,"nr":2,"typicalPower":0.3,"typicalFreq":1.8,
                     "sweetFreq":1.4,"plainFreq":1.2,"freeFreq":0.6}
                ]},
                "sysfs": {"enable": true, "knob": {
                    "cpuMax": "/sys/module/msm_performance/parameters/cpu_max_freq"
                }}
            },
            "initials": {"cpu": {"baseSampleTime": 0.005, "baseSlackTime": 0.005, "margin": 0.2}},
            "presets": {"balance": {"*": {}}}
        });
        let cfg = uperf_config::Config::from_value(cfg_json.clone()).unwrap();
        let opps = vec![vec![600000.0, 1200000.0, 1800000.0]];
        let gov = governor_from_config(&cfg, "balance", "*", &opps).unwrap();
        // Explicit non-userspace targets: this test runs on the build host, and
        // a `Userspace` target would switch the *host's* CPU governor.
        let _ = freq_targets(&cfg);
        let targets = vec![FreqTarget::None];
        let mut task = CpuTask::spawn_with(gov, targets, None, None, || {
            // escalating load so the governor has to move
            let mut j = uperf_config::CpuJiffies::default();
            j.busy = vec![50, 50];
            j.total = vec![100, 100];
            j
        });
        std::thread::sleep(Duration::from_millis(60));
        task.stop();
        assert!(!task.is_running());

        // silence the unused-mut warning on the json value above
        cfg_json["meta"]["name"] = serde_json::json!("t");
    }
}
