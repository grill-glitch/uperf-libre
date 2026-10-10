//! Machine-readable daemon status (`<config dir>/uperf.state`) — M9.
//!
//! Why this file exists: the daemon is the only component that knows whether the
//! `userspace` frequency takeover is armed, and the process table cannot be used
//! to tell its processes apart. dfps rewrites the cmdline of *both* the supervisor
//! and its worker to plain `uperf`, `pidof` does not match a zombie, and a
//! `killall`-based teardown that matches the binary's filename kills only the
//! supervisor and orphans the worker that owns the governor (see
//! `docs/m6b-evidence.md` §9-§11 and `docs/m7-evidence.md` §1).
//!
//! The module's watchdog (`magisk/script/uperf_watchdog.sh`) therefore decides
//! liveness from `/proc` itself — it resolves each candidate's `exe`, which
//! survives the cmdline rewrite — and only *reads* this file for reporting. This
//! file is the machine-readable view for tooling and for the user; it is never
//! the authority on whether the takeover is armed (that is the live
//! `scaling_governor` value, which the watchdog reads directly).
//!
//! Format: `key=value`, one per line. Readers ignore unknown keys on purpose, so
//! adding a field never breaks an older reader. A stale `state=running` with no
//! live process is exactly the signature of a SIGKILL (or a panic — the release
//! profile sets `panic = "abort"`, so no destructor runs), which is the condition
//! the watchdog reacts to.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Status format version. Bump only for an incompatible change.
pub const VERSION: u32 = 1;

/// What the daemon reports about itself.
pub struct Snapshot<'a> {
    /// `running` while the daemon is up, `stopped` on the clean stop path.
    pub state: &'a str,
    /// Whether the `userspace` frequency takeover is enabled for this run.
    pub takeover: bool,
    /// Policy directory names currently in `userspace` (empty when disarmed).
    pub armed: &'a [String],
    /// The config file this daemon was started with.
    pub config: &'a str,
}

/// The status file for a config path: `<config dir>/uperf.state`.
///
/// `UPERF_STATUS_FILE` overrides it (host tests, offline parity). `None` when
/// neither is available, in which case status reporting is simply skipped.
pub fn path_for_config(config_path: &str) -> Option<PathBuf> {
    // Empty means "not set": an env var present with a blank value must not
    // produce a relative path or overwrite the real file.
    let from_env = std::env::var("UPERF_STATUS_FILE")
        .ok()
        .filter(|s| !s.is_empty());
    if let Some(p) = from_env {
        return Some(PathBuf::from(p));
    }
    Path::new(config_path)
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join("uperf.state"))
}

/// Where to report status, and which config the report belongs to.
#[derive(Debug, Clone)]
pub struct Target {
    pub path: PathBuf,
    pub config: String,
}

/// The [`Target`] for this process, from the environment.
///
/// `uperf_rs_start` seeds both (`UPERF_STATUS_FILE`, `UPERF_STATUS_CONFIG`) from
/// the config path it was called with, which is what makes the status file land
/// next to `orig_governor.txt` without threading a path through every caller.
pub fn target() -> Option<Target> {
    let config = std::env::var("UPERF_STATUS_CONFIG").unwrap_or_default();
    let path = path_for_config(&config)?;
    Some(Target { path, config })
}

/// Write the snapshot. Best effort: status reporting must never be a reason for
/// the daemon to fail, so an unwritable directory is silent (and therefore also
/// cannot spam the log from a per-tick caller — this is called on arm/disarm and
/// on the two lifecycle edges only).
pub fn write(path: &Path, snap: &Snapshot<'_>) {
    let mut out = String::with_capacity(384);
    let _ = writeln!(
        out,
        "# uperf daemon status (M9). Written by the daemon; read by tooling."
    );
    let _ = writeln!(
        out,
        "# Liveness is NOT established from this file: the watchdog scans /proc for"
    );
    let _ = writeln!(
        out,
        "# the module binary, because both our processes are renamed to `uperf`."
    );
    let _ = writeln!(out, "version={VERSION}");
    let _ = writeln!(out, "state={}", snap.state);
    let _ = writeln!(out, "takeover={}", if snap.takeover { "on" } else { "off" });
    let _ = writeln!(out, "armed={}", snap.armed.len());
    let _ = writeln!(out, "policies={}", snap.armed.join(" "));
    let _ = writeln!(out, "pid={}", std::process::id());
    if let Some((ppid, start_ticks)) = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .as_deref()
        .and_then(stat_fields)
    {
        let _ = writeln!(out, "ppid={ppid}");
        let _ = writeln!(out, "start_ticks={start_ticks}");
    }
    let _ = writeln!(
        out,
        "boot_id={}",
        read_trim("/proc/sys/kernel/random/boot_id")
    );
    let _ = writeln!(out, "uptime_ms={}", uptime_ms());
    let _ = writeln!(out, "config={}", snap.config);
    write_atomic(path, out.as_bytes());
}

/// `(ppid, start_ticks)` from a `/proc/<pid>/stat` line.
///
/// Field 4 is the parent pid, field 22 the process start time in clock ticks.
/// Both are counted *after* the `(comm)` field, which can itself contain spaces
/// and parentheses, so the fields are located from the last `)` rather than by
/// naive whitespace splitting.
fn stat_fields(stat: &str) -> Option<(i32, u64)> {
    let close = stat.rfind(')')?;
    let rest = stat.get(close + 1..)?;
    let mut it = rest.split_whitespace();
    it.next()?; // state
    let ppid = it.next()?.parse().ok()?;
    // After the state and ppid the iterator sits on field 5 (`pgrp`);
    // starttime is field 22, i.e. 17 further.
    let start_ticks = it.nth(17)?.parse().ok()?;
    Some((ppid, start_ticks))
}

/// Milliseconds since boot, from `/proc/uptime`.
///
/// Monotonic, so it cannot be fooled by a clock change — the same reason the
/// watchdog's owner lock stores it instead of a wall-clock timestamp.
fn uptime_ms() -> u64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|t| t.split_whitespace().next().map(str::to_string))
        .and_then(|s| s.parse::<f64>().ok())
        .map(|secs| (secs * 1000.0) as u64)
        .unwrap_or(0)
}

fn read_trim(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Write through a same-directory temp file so a reader never sees a half-written
/// status, falling back to a direct write when the rename does not stick.
///
/// `/sdcard` is FUSE and a root `unlink` there can silently no-op on this device
/// (docs/m6b-evidence.md §6), so `rename` is not assumed to work — but reporting
/// the state must not depend on it either.
fn write_atomic(path: &Path, data: &[u8]) {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".new");
    let tmp = PathBuf::from(tmp);

    if std::fs::write(&tmp, data).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::write(path, data);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uperf_status_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stat_fields_survive_spaces_and_parens_in_comm() {
        // Same shape as /proc/self/stat, with a hostile comm: a real case for
        // threads whose name contains a space (dfps renames its threads).
        let stat = "1234 (sh (weird) name) S 1 1234 1234 0 -1 4194560 100 0 0 0 5 6 7 8 \
                    20 0 3 0 987654 4096 500 18446744073709551615 1 2 3 4 5 6 7 8 9 10 11 12 \
                    13 14 15 16 17 18 19 20 21 22 23 24 25";
        let (ppid, start_ticks) = stat_fields(stat).expect("fields");
        assert_eq!(ppid, 1);
        // field 22 overall == 20th token after "(comm) "
        assert_eq!(start_ticks, 987654, "starttime is field 22, not field 21");
    }

    #[test]
    fn stat_fields_returns_none_on_garbage() {
        assert!(stat_fields("").is_none());
        assert!(stat_fields("no parens here").is_none());
    }

    #[test]
    fn write_replaces_the_previous_snapshot() {
        let dir = tmp_dir("replace");
        let path = dir.join("uperf.state");

        let armed = vec!["policy0".to_string(), "policy4".to_string()];
        write(
            &path,
            &Snapshot {
                state: "running",
                takeover: true,
                armed: &armed,
                config: "/sdcard/Android/yc/uperf/uperf.json",
            },
        );
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("state=running\n"), "{first}");
        assert!(first.contains("takeover=on\n"), "{first}");
        assert!(first.contains("armed=2\n"), "{first}");
        assert!(first.contains("policies=policy0 policy4\n"), "{first}");
        assert!(
            first.contains(&format!("pid={}\n", std::process::id())),
            "{first}"
        );
        assert!(first.contains("boot_id="), "{first}");
        assert!(first.contains("start_ticks="), "{first}");
        assert!(first.contains("uptime_ms="), "{first}");
        assert!(
            first.contains("config=/sdcard/Android/yc/uperf/uperf.json\n"),
            "{first}"
        );
        assert!(
            !std::fs::read_dir(&dir)
                .unwrap()
                .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".new")),
            "the temp file must not be left behind"
        );

        write(
            &path,
            &Snapshot {
                state: "stopped",
                takeover: true,
                armed: &[],
                config: "/sdcard/Android/yc/uperf/uperf.json",
            },
        );
        let second = std::fs::read_to_string(&path).unwrap();
        assert!(second.contains("state=stopped\n"), "{second}");
        assert!(second.contains("armed=0\n"), "{second}");
        assert!(second.contains("policies=\n"), "{second}");
        assert!(
            !second.contains("state=running"),
            "the old snapshot must be gone, not appended: {second}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_is_silent_when_the_directory_is_missing() {
        // Status reporting must never take the daemon down; a missing directory
        // (user dir deleted under us) is not an error.
        write(
            Path::new("/definitely/not/here/uperf.state"),
            &Snapshot {
                state: "running",
                takeover: false,
                armed: &[],
                config: "",
            },
        );
    }

    #[test]
    fn target_falls_back_to_the_config_directory() {
        let dir = tmp_dir("target");
        let cfg = dir.join("uperf.json");
        std::fs::write(&cfg, "{}").unwrap();
        std::env::remove_var("UPERF_STATUS_FILE");
        let got = path_for_config(cfg.to_str().unwrap()).unwrap();
        assert_eq!(got, dir.join("uperf.state"));
        std::env::set_var("UPERF_STATUS_FILE", "/tmp/explicit.state");
        assert_eq!(
            path_for_config(cfg.to_str().unwrap()).unwrap(),
            PathBuf::from("/tmp/explicit.state")
        );
        std::env::remove_var("UPERF_STATUS_FILE");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
