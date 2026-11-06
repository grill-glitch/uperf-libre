//! The sink that writes to the **live** sysfs/procfs tree.
//!
//! Until now `apply_pending` had two ways to consume a plan: `UnderRootSink` (a fake root,
//! offline validation) and "log it as `would-write`". The module's knob table ships
//! disabled, so nothing was lost in practice — but it also meant `WriterKind` had no
//! executor and the ledger/restore scripts had no producer outside the harnesses.
//!
//! This is that executor, and it carries the four things the upstream writer classes do
//! that a bare `fs::write` does not:
//!
//! * **a per-path fd cache** — the same cpuset knob is rewritten on every scene change,
//!   and on this device a sysfs `open` costs more than the write (the file is a kernfs
//!   proxy, not a regular file);
//! * **dedup** — a write whose value equals the last one *we* wrote to that path is
//!   skipped, which is upstream's "diff against the previous action" rule;
//! * **record-then-write** — the value the node held before our first write goes to the
//!   ledger, so the stop script can put it back after a `SIGKILL`;
//! * **read-back** — several knobs accept a write and keep their old value (a clamped
//!   cpufreq, a cpuset constrained by its cgroup). Those are counted and logged as
//!   ineffective rather than reported as applied.
//!
//! What it deliberately does not do: retry, poll, or "fix" a value the kernel refused.
//! Reporting what happened is the contract; guessing is how a device gets stuck.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::orchestrator::{Sink, SysfsWrite};
use crate::sysfs_ledger::SysfsLedger;

/// The live-tree sink. One instance per daemon, behind the same `Sink` trait as the
/// offline one, so `apply_pending` does not care which it got.
#[derive(Default)]
pub struct RealSink {
    /// path -> open fd, kept across writes.
    fds: BTreeMap<String, File>,
    /// path -> the value this sink last wrote successfully.
    last: BTreeMap<String, String>,
    ledger: Option<SysfsLedger>,
    pub written: usize,
    /// Value already in effect (dedup).
    pub skipped: usize,
    /// (path, error) — the open or the write failed.
    pub failed: Vec<(String, String)>,
    /// (path, wrote, read back) — the kernel kept something else.
    pub ineffective: Vec<(String, String, String)>,
}

impl RealSink {
    pub fn new(ledger: Option<SysfsLedger>) -> Self {
        Self {
            ledger,
            ..Self::default()
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "ok={} skipped={} failed={} ineffective={}",
            self.written,
            self.skipped,
            self.failed.len(),
            self.ineffective.len()
        )
    }

    fn write_path(&mut self, path: &str, payload: &str) -> std::io::Result<()> {
        let fd = match self.fds.get_mut(path) {
            Some(fd) => fd,
            None => {
                let fd = OpenOptions::new().write(true).open(path)?;
                self.fds.entry(path.to_string()).or_insert(fd)
            }
        };
        // A sysfs store runs per write(2) regardless of offset, but /proc entries are
        // seq_files: without the rewind a second write lands at the old offset and is
        // silently short.
        fd.seek(SeekFrom::Start(0))?;
        fd.write_all(payload.as_bytes())?;
        fd.flush()
    }
}

impl Sink for RealSink {
    fn write(&mut self, w: &SysfsWrite) {
        if self.last.get(&w.path).map(String::as_str) == Some(w.value.as_str()) {
            self.skipped += 1;
            return;
        }
        let payload = w.payload();
        if payload.is_empty() {
            // An empty pid list is "nothing to write", not "write a blank line".
            self.skipped += 1;
            return;
        }

        // Record the original before the first write, the same way the fake-root sink
        // does — a write that lands and then loses its recorder still leaves the original
        // on disk.
        crate::sysfs_ledger::record_before_write(&mut self.ledger, &w.path, Path::new(&w.path));

        match self.write_path(&w.path, &payload) {
            Ok(()) => {
                self.written += 1;
                self.last.insert(w.path.clone(), w.value.clone());
                if let Ok(now) = std::fs::read_to_string(&w.path) {
                    if !readback_matches(&payload, &now) {
                        self.ineffective
                            .push((w.path.clone(), w.value.clone(), now.trim().to_string()));
                    }
                }
            }
            Err(e) => {
                // The node may be gone (or was recreated): drop the cached fd so the next
                // attempt reopens instead of retrying a stale handle.
                self.fds.remove(&w.path);
                self.failed.push((w.path.clone(), e.to_string()));
            }
        }
    }
}

/// Whether what the node now holds is what we wrote.
///
/// Compared as *tokens*, not as strings: a `cgroup.procs` write is several lines of PIDs
/// and the value we asked for is `a,b,c`, so a string compare would call every successful
/// cgroup write ineffective. Whitespace and commas are both separators; order matters
/// (the kernel keeps the list in the order written).
pub fn readback_matches(payload: &str, read_back: &str) -> bool {
    let norm = |s: &str| -> Vec<String> {
        s.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    };
    let a = norm(payload);
    let b = norm(read_back);
    if a.is_empty() {
        return b.is_empty();
    }
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::Sink;
    use self::tempdir::TempDirShim;

    /// Small local shim: the workspace has no `tempfile`, and `uperf-core` deliberately
    /// keeps its dependency list short.
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct TempDirShim(pub PathBuf);

        impl TempDirShim {
            pub fn new(tag: &str) -> Self {
                let p = std::env::temp_dir().join(format!(
                    "uperf_real_{tag}_{}",
                    std::process::id()
                ));
                let _ = std::fs::remove_dir_all(&p);
                std::fs::create_dir_all(&p).unwrap();
                Self(p)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDirShim {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn write_knob(sink: &mut RealSink, path: &Path, value: &str) {
        sink.write(&SysfsWrite::new(path.display().to_string(), value));
    }

    #[test]
    fn a_second_identical_write_is_skipped_and_the_first_one_lands() {
        let d = TempDirShim::new("dedup");
        let knob = d.path().join("cpus");
        std::fs::write(&knob, "0-7\n").unwrap();
        let mut sink = RealSink::default();

        write_knob(&mut sink, &knob, "0-3");
        assert_eq!(std::fs::read_to_string(&knob).unwrap(), "0-3\n");
        assert_eq!((sink.written, sink.skipped), (1, 0));

        // Same value twice: upstream's diff rule — one write, one skip.
        write_knob(&mut sink, &knob, "0-3");
        assert_eq!((sink.written, sink.skipped), (1, 1));
        assert!(sink.ineffective.is_empty(), "{:?}", sink.ineffective);
    }

    #[test]
    fn the_original_is_recorded_before_the_first_write_only() {
        let d = TempDirShim::new("ledger");
        let knob = d.path().join("param");
        std::fs::write(&knob, "before\n").unwrap();
        let ledger = SysfsLedger::load(d.path().join("sysfs_orig.txt"));
        let mut sink = RealSink::new(Some(ledger));

        write_knob(&mut sink, &knob, "after");
        write_knob(&mut sink, &knob, "later");
        let text = std::fs::read_to_string(d.path().join("sysfs_orig.txt")).unwrap();
        assert!(text.contains("param before"), "ledger: {text}");
        assert_eq!(
            text.matches("param").count(),
            1,
            "the original is recorded once, not per write: {text}"
        );
    }

    #[test]
    fn a_write_the_node_ignores_is_reported_not_claimed() {
        // /dev/null accepts every write and reads back empty — the same shape as a driver
        // that clamps a value. The sink must count it as ineffective.
        let mut sink = RealSink::default();
        sink.write(&SysfsWrite::new("/dev/null", "1804800"));
        assert_eq!(sink.written, 1);
        assert_eq!(sink.ineffective.len(), 1, "{:?}", sink.ineffective);
        assert_eq!(sink.ineffective[0].0, "/dev/null");
    }

    #[test]
    fn a_missing_node_is_a_failure_not_a_panic() {
        let mut sink = RealSink::default();
        sink.write(&SysfsWrite::new("/definitely/not/here/knob", "1"));
        assert_eq!(sink.failed.len(), 1);
        assert_eq!(sink.written, 0);
    }

    #[test]
    fn a_pid_list_matches_its_readback_as_tokens() {
        // The value is `a,b,c`; the kernel holds one PID per line. A string compare would
        // report every successful cgroup write as ineffective.
        assert!(readback_matches("101\n102\n103\n", "101\n102\n103\n"));
        assert!(readback_matches("101,102,103", "101 102 103"));
        assert!(!readback_matches("101\n102\n", "101\n102\n103\n"));
        assert!(!readback_matches("101", ""));
        assert!(readback_matches("", "  \n"));
    }
}
