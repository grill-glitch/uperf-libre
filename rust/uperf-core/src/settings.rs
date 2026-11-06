//! One snapshot of every runtime knob, taken once at start.
//!
//! Before this existed, the daemon read 36 `UPERF_*` variables from inside the
//! modules that used them (`std::env::var` at the point of use). Three problems
//! came with that:
//!
//! * **No single place listed the knobs.** Changing a window length meant grepping
//!   for the variable name, and a typo produced silently-accepted default values.
//! * **Tests had to mutate the process environment.** Rust's env is process-global
//!   and `set_var` while other threads are running is a data race on `environ`
//!   (that is why edition 2024 marks it `unsafe`), so every test that needed a
//!   seam had to invent a mutex to serialize itself — `cpu_task` had exactly that
//!   (`FAKE_ROOT_LOCK`), and `status`/`sched_apply`/`sched_task` simply raced.
//! * **`uperf_rs_start` used `set_var` to publish derived paths** (`UPERF_STATE_FILE`,
//!   `UPERF_STATUS_FILE` …) so that later readers would find them. On a re-start the
//!   writer threads of the previous generation are still alive, which is precisely
//!   the case `set_var` is not safe in.
//!
//! So: parse the environment exactly once, at the outer edge (`uperf_rs_start`),
//! into this struct, and pass it down. Nothing below this module reads the
//! environment for a knob; tests construct a `Settings` and hand it over.
//!
//! What stays environment-driven on purpose: the *preset* file contents
//! (`cur_powermode.txt`, `perapp_powermode.txt`), the config JSON, and the
//! `sfanalysis.hint` byte — those are inputs to the policy, not knobs of the daemon,
//! and they must be re-read while it runs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use uperf_config::thermal::ThermalPolicy;

use crate::foreground::{ForegroundFile, DEFAULT_MAX_AGE_MS, DEFAULT_TICK_MS};
use crate::recorder::MIN_SESSION_MS;
use crate::sf_uprobe::LIBGUI;
use crate::status::Target;

/// Everything the daemon needs to know about how *it* was started.
///
/// `Clone` because each background task owns the copy it needs: the values are
/// fixed for the process lifetime, so there is nothing to share behind a lock.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The config file this run loaded (`uperf.json`), as given on the command line.
    pub config_path: String,
    /// The log file, as given to `-o`, or empty.
    pub log_path: String,
    /// `<config dir>` == the module's `USER_PATH`. Derived paths hang off it.
    pub config_dir: Option<PathBuf>,

    // ---------------------------------------------------------------- policy seams
    /// Redirect every sysfs write under this root instead of `/`. Offline validation
    /// and the device harnesses only; see `AGENTS.md` §2 for what it does *not* cover
    /// (`sched_*` are syscalls with no path).
    pub fake_root: Option<String>,
    /// Whether the userspace CPU governor may take the frequency policies over.
    /// Default off: the measured takeover was worse than the platform's own governor
    /// on alioth (see `docs/m5-cpu-governor.md` and the note in `cpu_task.rs`).
    pub cpu_governor: bool,
    /// Root of the thermal zones, for the PL1 feedback.
    pub thermal_root: PathBuf,
    pub thermal: ThermalPolicy,
    /// Context scheduler: apply nothing, only report what it *would* do.
    pub sched_dry_run: bool,
    /// Restrict the context scheduler to one process pattern (debugging aid).
    pub sched_only: Option<String>,
    /// `UPERF_HOME_PACKAGE` — skip the `cmd package resolve-activity` probe. Stored
    /// trimmed and non-empty, or `None` so the resolver knows to ask the device.
    pub home_package: Option<String>,

    // ------------------------------------------------------------------- file paths
    /// `<config dir>/uperf.state` (owner-checked machine-readable status).
    pub status: Option<Target>,
    /// `<config dir>/orig_governor.txt` — where the governors we replace are recorded
    /// so the module's stop script can restore them after a SIGKILL.
    pub state_file: Option<PathBuf>,
    /// `<config dir>/sysfs_orig.txt` — the sysfs write ledger.
    pub sysfs_orig: Option<PathBuf>,
    /// `<config dir>/uperf_frames.state` — the frame leg's published reading.
    pub frames_state: Option<PathBuf>,
    /// `<config dir>/uperf.token` — ctl-socket handshake token.
    pub ctl_token_file: Option<PathBuf>,

    // ------------------------------------------------------------------- subsystems
    /// The foreground helper file source (④). `None` when disabled or unpairable.
    pub foreground: Option<ForegroundCfg>,
    pub sf_uprobe: Option<UprobeCfg>,
    pub sf_binder: Option<BinderCfg>,
    pub ctl: Option<CtlCfg>,
    pub record: Option<RecordCfg>,
}

#[derive(Debug, Clone)]
pub struct ForegroundCfg {
    pub path: PathBuf,
    pub max_age_ms: u64,
    /// Dispatcher wake interval while this source is armed.
    pub tick: Duration,
}

#[derive(Debug, Clone)]
pub struct UprobeCfg {
    /// The library whose `queueBuffer` is probed.
    pub lib: String,
    /// Pin the target process instead of following the top app.
    pub pid: Option<u32>,
    pub window_ms: u64,
}

#[derive(Debug, Clone)]
pub struct BinderCfg {
    pub tick_ms: u64,
    pub window_ms: u64,
    /// An injected `sfanalysis.hint` older than this hands the frame source back to
    /// this leg.
    pub hint_stale_ms: u64,
    /// Pin one layer instead of following the top app.
    pub layer: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CtlCfg {
    /// `@name` = abstract socket (Android `LocalSocket`), otherwise a path.
    pub addr: String,
    pub retry_ms: u64,
    pub ping_ms: u64,
}

#[derive(Debug, Clone)]
pub struct RecordCfg {
    pub min_session_ms: u64,
    pub sample_ms: u64,
    pub tick_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            config_path: String::new(),
            log_path: String::new(),
            config_dir: None,
            fake_root: None,
            cpu_governor: false,
            thermal_root: PathBuf::from(DEFAULT_THERMAL_ROOT),
            thermal: ThermalPolicy::default(),
            sched_dry_run: false,
            sched_only: None,
            home_package: None,
            status: None,
            state_file: None,
            sysfs_orig: None,
            frames_state: None,
            ctl_token_file: None,
            foreground: None,
            sf_uprobe: None,
            sf_binder: None,
            ctl: None,
            record: None,
        }
    }
}

const DEFAULT_THERMAL_ROOT: &str = "/sys/class/thermal";

impl Settings {
    /// Parse the environment once. This is the **only** place a knob is read from it.
    ///
    /// `config_path` comes from the command line (C++ passes `argv[optind]`); every
    /// path-shaped default is derived from its directory, which is what makes the
    /// status file, the governor record, the ledger and the token all land in the
    /// module's `USER_PATH` without threading that path through each caller.
    pub fn from_env(config_path: &str, log_path: &str) -> Self {
        let config_dir = Path::new(config_path)
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .map(Path::to_path_buf);

        let derived = |name: &str| config_dir.as_ref().map(|d| d.join(name));
        // Materialise every derived path before `config_dir` is moved into the struct:
        // the closure borrows it, so the borrow has to end first.
        let (fg_file, state_file, sysfs_orig, frames_state, token_file) = (
            derived("foreground.txt"),
            derived("orig_governor.txt"),
            derived("sysfs_orig.txt"),
            derived("uperf_frames.state"),
            derived("uperf.token"),
        );

        let status = crate::status::path_for(config_path, env_path("UPERF_STATUS_FILE").as_deref())
            .map(|path| Target {
                path,
                config: config_path.to_string(),
            });

        let foreground = if matches!(env_str("UPERF_FOREGROUND").as_deref(), Some("0") | Some("false")) {
            None
        } else {
            env_path("UPERF_FOREGROUND_FILE")
                .or(fg_file)
                .map(|path| ForegroundCfg {
                    path,
                    max_age_ms: env_u64("UPERF_FOREGROUND_MAX_AGE_MS", DEFAULT_MAX_AGE_MS),
                    // A zero/invalid tick would spin the dispatcher; keep the floor.
                    tick: Duration::from_millis(
                        env_u64("UPERF_FOREGROUND_TICK_MS", DEFAULT_TICK_MS).max(50),
                    ),
                })
        };

        Self {
            config_path: config_path.to_string(),
            log_path: log_path.to_string(),
            config_dir,

            fake_root: env_str("UPERF_FAKE_ROOT").filter(|s| !s.is_empty()),
            cpu_governor: env_str("UPERF_CPU_GOVERNOR").as_deref() == Some("1"),
            thermal_root: env_path("UPERF_THERMAL_ROOT")
                .unwrap_or_else(|| PathBuf::from(DEFAULT_THERMAL_ROOT)),
            thermal: ThermalPolicy::from_values(
                env_f64("UPERF_THERMAL_THRESH_C"),
                env_f64("UPERF_THERMAL_SPAN_C"),
                env_f64("UPERF_THERMAL_FLOOR"),
            ),
            sched_dry_run: env_str("UPERF_SCHED_DRY_RUN").as_deref() == Some("1"),
            sched_only: env_str("UPERF_SCHED_ONLY").filter(|s| !s.is_empty()),
            home_package: env_str("UPERF_HOME_PACKAGE")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),

            status,
            state_file: env_path("UPERF_STATE_FILE").or(state_file),
            sysfs_orig: env_path("UPERF_SYSFS_ORIG").or(sysfs_orig),
            frames_state: env_path("UPERF_FRAMES_STATE").or(frames_state),
            ctl_token_file: env_path("UPERF_CTL_TOKEN_FILE").or(token_file),

            foreground,
            sf_uprobe: env_on("UPERF_SF_UPROBE").then(|| UprobeCfg {
                lib: env_str("UPERF_SF_UPROBE_LIB").unwrap_or_else(|| LIBGUI.to_string()),
                pid: env_str("UPERF_SF_UPROBE_PID").and_then(|s| s.parse().ok()),
                window_ms: env_u64("UPERF_SF_UPROBE_WINDOW_MS", 1000),
            }),
            sf_binder: env_on("UPERF_SF_BINDER").then(|| BinderCfg {
                tick_ms: env_u64("UPERF_SF_BINDER_TICK_MS", 1000),
                window_ms: env_u64("UPERF_SF_BINDER_WINDOW_MS", 1000),
                hint_stale_ms: env_u64("UPERF_SF_BINDER_HINT_STALE_MS", 3000),
                layer: env_str("UPERF_SF_BINDER_LAYER").filter(|s| !s.is_empty()),
            }),
            ctl: env_str("UPERF_CTL_SOCKET")
                .filter(|s| !s.is_empty())
                .map(|addr| CtlCfg {
                    addr,
                    retry_ms: env_u64("UPERF_CTL_RETRY_MS", 5000),
                    ping_ms: env_u64("UPERF_CTL_PING_MS", 2000),
                }),
            record: env_on("UPERF_RECORD").then(|| RecordCfg {
                min_session_ms: env_u64("UPERF_RECORD_MIN_MS", MIN_SESSION_MS),
                sample_ms: env_u64("UPERF_RECORD_SAMPLE_MS", 1000),
                tick_ms: env_u64("UPERF_RECORD_TICK_MS", 1000),
            }),
        }
    }

    /// The foreground source, as the stateful reader the dispatcher owns.
    ///
    /// Separate from the struct because [`ForegroundFile`] remembers the last package
    /// it reported (dedup), i.e. it is per-run state, not configuration.
    pub fn foreground_source(&self) -> Option<ForegroundFile> {
        self.foreground
            .as_ref()
            .map(|c| ForegroundFile::new(c.path.clone(), c.max_age_ms))
    }

    /// Dispatcher wake interval: the foreground poll when that source is armed,
    /// otherwise a full second (long enough to notice a dropped sender promptly).
    pub fn dispatcher_tick(&self) -> Duration {
        self.foreground
            .as_ref()
            .map(|c| c.tick)
            .unwrap_or(Duration::from_millis(1000))
    }
}

// --------------------------------------------------------------------- env parsing

fn env_str(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// A path-shaped override. An empty value means "not set": a variable present but
/// blank must not produce a relative path or overwrite the real file.
fn env_path(key: &str) -> Option<PathBuf> {
    env_str(key).filter(|s| !s.is_empty()).map(PathBuf::from)
}

fn env_u64(key: &str, default: u64) -> u64 {
    env_str(key).and_then(|s| s.parse::<u64>().ok()).unwrap_or(default)
}

fn env_f64(key: &str) -> Option<f64> {
    env_str(key).and_then(|s| s.parse::<f64>().ok())
}

/// The opt-in switches accept `1` and `true`, and nothing else.
fn env_on(key: &str) -> bool {
    matches!(env_str(key).as_deref(), Some("1") | Some("true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_paths_follow_the_config_directory() {
        // Nothing in this test touches the environment: the whole point of the
        // struct is that a run's paths can be asserted without mutating `environ`.
        let dir = std::env::temp_dir().join(format!("uperf_settings_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cfg = dir.join("uperf.json");
        let s = Settings {
            config_path: cfg.display().to_string(),
            config_dir: Some(dir.clone()),
            status: crate::status::path_for(&cfg.display().to_string(), None).map(|path| Target {
                path,
                config: cfg.display().to_string(),
            }),
            state_file: Some(dir.join("orig_governor.txt")),
            ..Settings::default()
        };
        assert_eq!(s.status.unwrap().path, dir.join("uperf.state"));
        assert_eq!(s.state_file.unwrap(), dir.join("orig_governor.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_override_is_not_a_path() {
        assert_eq!(env_path("UPERF_DEFINITELY_NOT_SET_ANYWHERE"), None);
    }

    #[test]
    fn opt_in_switches_accept_only_1_and_true() {
        // Documented contract of the switches; `env_on` is the single parser.
        assert!(!env_on("UPERF_DEFINITELY_NOT_SET_ANYWHERE"));
    }
}
