//! Top-app source: the foreground helper's file (AGENT.md §11 queue item ④).
//!
//! `cpp/dfps/source/modules/topapp_monitor.cpp` learns the top app from a spawned
//! `dumpsys` and so only looks when the top-app cgroup's pid count moves by more
//! than `TOP_TASK_NR_DIFF_MIN` (10) — a switch between two apps whose process
//! counts differ by fewer than ten pids is invisible. That gate is a cost
//! workaround, not a property of the problem.
//!
//! The helper (see `helper/`) replaces the poll with an `ITaskStackListener`
//! notification and publishes the answer to a one-line file, rewritten atomically
//! on every callback *and* every poll:
//!
//! ```text
//! <package> <uptime_ms>
//! ```
//!
//! The timestamp is `SystemClock.uptimeMillis()` (CLOCK_MONOTONIC), not wall
//! clock, so freshness cannot be fooled by a clock jump. `-` in the package slot
//! means "could not resolve the top task".
//!
//! This module is the daemon half: a periodic read of that file, used as the
//! `topapp.pkgName` source. When the file is absent, unreadable, or its timestamp
//! is stale, the helper is gone — and the vendored C++ monitor, which still
//! publishes `topapp.pkgName` over the topic bridge, remains the fallback.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A line whose timestamp is older than this is "no news" — the helper stopped
/// writing (or died). Must comfortably exceed the helper's own poll interval.
pub const DEFAULT_MAX_AGE_MS: u64 = 10_000;
/// How often the dispatcher wakes to re-read the file. Well under the helper's
/// poll interval, so a switch is picked up within one poll at most.
pub const DEFAULT_TICK_MS: u64 = 500;

/// The foreground helper's output file, read as the top-app source.
#[derive(Debug)]
pub struct ForegroundFile {
    path: PathBuf,
    max_age_ms: u64,
    /// Last package reported, for dedup: the file is rewritten every poll even
    /// when nothing changed, so without this the log would re-emit on every tick.
    last: Option<String>,
}

impl ForegroundFile {
    pub fn new(path: impl Into<PathBuf>, max_age_ms: u64) -> Self {
        Self {
            path: path.into(),
            max_age_ms,
            last: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn max_age_ms(&self) -> u64 {
        self.max_age_ms
    }

    /// Parse one line: `<package> <uptime_ms>`. Returns `None` for `-`, an empty
    /// line, a missing timestamp, or a non-numeric one — every one of which means
    /// "the helper has nothing to say", never a package to invent.
    pub fn parse_line(line: &str) -> Option<(String, u64)> {
        let mut it = line.trim().split_whitespace();
        let pkg = it.next()?;
        if pkg == "-" {
            return None;
        }
        let ts = it.next()?.parse::<u64>().ok()?;
        Some((pkg.to_string(), ts))
    }

    /// Decide what to report, given the file's text (`None` = absent/unreadable)
    /// and the current monotonic millisecond clock.
    ///
    /// * fresh line, new package  -> `Some(pkg)` (and remembered)
    /// * fresh line, same package  -> `None` (dedup; the file rewrites every poll)
    /// * stale timestamp           -> `None`, and the memory is cleared so the same
    ///   package is reported again after the helper restarts
    /// * absent / `-` / malformed   -> `None`, memory cleared
    pub fn decide(&mut self, contents: Option<&str>, now_ms: u64) -> Option<String> {
        let Some(text) = contents else {
            self.last = None;
            return None;
        };
        let Some((pkg, ts)) = Self::parse_line(text) else {
            self.last = None;
            return None;
        };
        if now_ms.saturating_sub(ts) > self.max_age_ms {
            self.last = None;
            return None;
        }
        if self.last.as_deref() == Some(pkg.as_str()) {
            return None;
        }
        self.last = Some(pkg.clone());
        Some(pkg)
    }

    /// Read the file and decide. Any filesystem error reads as "no news".
    pub fn poll(&mut self) -> Option<String> {
        let contents = std::fs::read_to_string(&self.path).ok();
        self.decide(contents.as_deref(), monotonic_ms())
    }
}

/// `CLOCK_MONOTONIC` in milliseconds — the same clock Java's
/// `SystemClock.uptimeMillis()` (and therefore the helper) timestamps with, so the
/// age comparison is between two readings of the same clock.
pub fn monotonic_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable out-parameter.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return 0;
    }
    (ts.tv_sec as u64) * 1000 + (ts.tv_nsec as u64) / 1_000_000
}

/// Build the source from the environment and the config directory.
///
/// * `UPERF_FOREGROUND=0` disables it entirely (the C++ monitor is then the only
///   source, exactly as before this change).
/// * `UPERF_FOREGROUND_FILE` overrides the path.
/// * Otherwise it is `<config dir>/foreground.txt`, i.e. the module's `USER_PATH`.
pub fn from_env(cfg_dir: Option<&Path>) -> Option<ForegroundFile> {
    if matches!(
        std::env::var("UPERF_FOREGROUND").ok().as_deref(),
        Some("0") | Some("false")
    ) {
        return None;
    }
    let path = std::env::var_os("UPERF_FOREGROUND_FILE")
        .map(PathBuf::from)
        .or_else(|| cfg_dir.map(|d| d.join("foreground.txt")))?;
    let max_age_ms = std::env::var("UPERF_FOREGROUND_MAX_AGE_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_AGE_MS);
    Some(ForegroundFile::new(path, max_age_ms))
}

/// The dispatcher's wake interval, from `UPERF_FOREGROUND_TICK_MS` (default 500).
pub fn tick_from_env() -> Duration {
    let ms = std::env::var("UPERF_FOREGROUND_TICK_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TICK_MS);
    Duration::from_millis(ms.max(50))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fg(max_age_ms: u64) -> ForegroundFile {
        ForegroundFile::new("/nonexistent/foreground.txt", max_age_ms)
    }

    #[test]
    fn parses_a_line() {
        assert_eq!(
            ForegroundFile::parse_line("com.android.settings 12345"),
            Some(("com.android.settings".into(), 12345))
        );
        // trailing newline is normal
        assert_eq!(
            ForegroundFile::parse_line("com.foo 7\n"),
            Some(("com.foo".into(), 7))
        );
    }

    #[test]
    fn rejects_unknown_and_malformed() {
        assert_eq!(ForegroundFile::parse_line("- 100"), None);
        assert_eq!(ForegroundFile::parse_line(""), None);
        assert_eq!(ForegroundFile::parse_line("com.foo"), None); // no timestamp
        assert_eq!(ForegroundFile::parse_line("com.foo notanumber"), None);
    }

    #[test]
    fn fresh_new_package_is_reported_once() {
        let mut f = fg(10_000);
        assert_eq!(f.decide(Some("com.a 1000"), 1500), Some("com.a".into()));
        // same package, still fresh, next poll -> deduped
        assert_eq!(f.decide(Some("com.a 2000"), 2600), None);
        // a switch -> reported
        assert_eq!(f.decide(Some("com.b 3000"), 3100), Some("com.b".into()));
    }

    #[test]
    fn stale_timestamp_is_no_news_and_allows_a_re_report() {
        let mut f = fg(1_000);
        assert_eq!(f.decide(Some("com.a 1000"), 1500), Some("com.a".into()));
        // 5 s later the helper has clearly stopped writing
        assert_eq!(f.decide(Some("com.a 1000"), 6_000), None);
        // helper restarted and reports the same package: must be visible again
        assert_eq!(f.decide(Some("com.a 9000"), 9_100), Some("com.a".into()));
    }

    #[test]
    fn absent_or_unknown_is_none() {
        let mut f = fg(10_000);
        assert_eq!(f.decide(None, 1000), None);
        assert_eq!(f.decide(Some("- 1000"), 1000), None);
    }

    #[test]
    fn poll_reads_a_real_file() {
        let path = std::env::temp_dir().join(format!("uperf_fg_{}.txt", std::process::id()));
        std::fs::write(&path, format!("com.poll.test {}\n", monotonic_ms())).unwrap();
        let mut f = ForegroundFile::new(&path, DEFAULT_MAX_AGE_MS);
        assert_eq!(f.poll(), Some("com.poll.test".into()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn monotonic_clock_advances() {
        let a = monotonic_ms();
        assert!(a > 0, "CLOCK_MONOTONIC should be readable");
        let b = monotonic_ms();
        assert!(b >= a);
    }
}
