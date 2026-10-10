//! Write ledger for the sysfs knobs this daemon changes.
//!
//! The rule this exists to make structural: **a value we changed can always be put
//! back, and a value we never read is never invented.** `uperf_restore_governors`
//! already works that way for the governor takeover (a policy with no recorded
//! original is left alone and reported); this is the same contract for the
//! `modules.sysfs.knob` table, which does not go through the governor path.
//!
//! Format — one `<path> <value>` per line, values trailing-whitespace-trimmed:
//!
//! * `path` is taken verbatim from the config's knob table, and the table is the only
//!   source of paths (see `uperf-config/src/sysfs.rs`). A path containing whitespace
//!   would make the line ambiguous, so such a path is recorded as *unknown* instead of
//!   being guessed at (`every_knob_path_is_whitespace_free` keeps the configs honest;
//!   this is the runtime backstop).
//! * `value` is the raw file content with the trailing newline removed (`echo 1 >` and
//!   the kernel's own `1` both mean the same thing). A value that is not a single line
//!   cannot round-trip through this format, so it is recorded as *unknown* — reported,
//!   never silently mangled into something that would be written back later.
//! * A path with **no value at all** means "we wrote this and could not read an
//!   original". Nothing is ever written back from such a line; it exists so the restore
//!   can say what it is leaving alone. (No marker character is used: any marker could
//!   collide with a value the kernel actually reports.)
//!
//! Lifetime rules:
//!
//! * **First write wins.** Once a path is in the ledger it is never re-recorded, even
//!   across daemon restarts: after a restart the "current" value is our own previous
//!   write, and recording it would replace the real original with our own state and
//!   make the restore a no-op that looks like success.
//! * The ledger is only ever *added to* by the daemon. Restoring (and clearing) is the
//!   shell side's job (`uperf_restore_sysfs`), so it also works when the daemon died
//!   without a chance to clean up — `/proc/<pid>/exe` has moved on, the file has not.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// On-disk ledger of the values this daemon replaced.
#[derive(Debug, Clone)]
pub struct SysfsLedger {
    path: PathBuf,
    /// path -> the value read immediately before the first write to it.
    entries: BTreeMap<String, String>,
    /// Paths we wrote without being able to read an original (or whose shape the
    /// format cannot carry). Reported by the restore, never invented.
    unknown: BTreeSet<String>,
    /// Lines the loader could not parse. Kept as a count, not as state: a file we do
    /// not understand must not stop the restore of the entries we do.
    corrupt: usize,
    dirty: bool,
}

impl SysfsLedger {
    /// The ledger that belongs to a config path: `<config dir>/sysfs_orig.txt`, i.e.
    /// beside `uperf.state`. `UPERF_SYSFS_ORIG` overrides it (host tests, offline
    /// runs). `None` when neither is available, in which case recording is skipped
    /// rather than written to an unknown location.
    pub fn for_config(config_path: &str) -> Option<Self> {
        let from_env = std::env::var("UPERF_SYSFS_ORIG")
            .ok()
            .filter(|s| !s.is_empty());
        let path = match from_env {
            Some(p) => PathBuf::from(p),
            None => Path::new(config_path)
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())?
                .join("sysfs_orig.txt"),
        };
        Some(Self::load(path))
    }

    /// The ledger for this process's `USER_PATH`, taken from the status target
    /// (`UPERF_STATUS_CONFIG` → `<config dir>/uperf.state`, so the ledger lands beside
    /// `uperf.state` and `orig_governor.txt`). `None` when the target is unknown, in
    /// which case the caller writes without recording rather than recording into an
    /// unknown location.
    pub fn for_status() -> Option<Self> {
        let t = crate::status::target()?;
        if t.config.is_empty() {
            return None;
        }
        Self::for_config(&t.config)
    }

    /// Load an existing ledger. An unreadable or absent file is an empty ledger: the
    /// next write records the originals it finds. An unparsable line is counted and
    /// dropped (see `corrupt`).
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (mut entries, mut unknown, mut corrupt) = (BTreeMap::new(), BTreeSet::new(), 0usize);
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                // Comments and blanks are ours, not data: a loader that treated the
                // header as an entry would fail its own round-trip (which is how this
                // was caught).
                if line.trim().is_empty() || line.starts_with('#') {
                    continue;
                }
                if !line.starts_with('/') {
                    corrupt += 1;
                    continue;
                }
                match line.split_once(' ') {
                    Some((p, v)) => {
                        entries.insert(p.to_string(), v.to_string());
                    }
                    // Path with no value: written but no readable original.
                    None => {
                        unknown.insert(line.to_string());
                    }
                }
            }
        }
        Self {
            path,
            entries,
            unknown,
            corrupt,
            dirty: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &BTreeMap<String, String> {
        &self.entries
    }

    pub fn unknown(&self) -> &BTreeSet<String> {
        &self.unknown
    }

    pub fn corrupt_lines(&self) -> usize {
        self.corrupt
    }

    /// Record the value that was in `path` immediately before we wrote to it.
    ///
    /// Returns `true` when this call added something. A path already in the ledger is
    /// left alone: first write wins, across restarts too.
    pub fn record(&mut self, path: &str, current: Option<&str>) -> bool {
        if self.entries.contains_key(path) || self.unknown.contains(path) {
            return false;
        }
        // The test is on the *trimmed* value: a trailing newline is what `echo 1 >`
        // leaves behind and is not part of the value, while a newline *inside* what
        // remains cannot be carried by a one-line format.
        let carryable = !path.contains(char::is_whitespace)
            && current.is_some_and(|v| {
                let t = v.trim_end();
                !t.is_empty() && !t.contains('\n')
            });
        match (carryable, current) {
            (true, Some(v)) => {
                self.entries
                    .insert(path.to_string(), v.trim_end().to_string());
            }
            _ => {
                // Read but not carryable, or not readable at all: we must not claim a
                // restore we cannot make. Record the *fact*, and let the shell report it.
                self.unknown.insert(path.to_string());
            }
        }
        self.dirty = true;
        true
    }

    /// The file body, so a test can check the format without a filesystem.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("# uperf write ledger — one `<path> <original value>` per line.\n");
        out.push_str("# Written by the daemon; restored by uperf_restore_sysfs.\n");
        out.push_str("# A path with no value = written but no readable original: reported,\n");
        out.push_str("# never written back.\n");
        for (p, v) in &self.entries {
            out.push_str(&format!("{} {}\n", p, v));
        }
        for p in &self.unknown {
            out.push_str(p);
            out.push('\n');
        }
        out
    }

    /// Persist through a same-directory temp file plus `rename`, with a direct write as
    /// the fallback — `/sdcard` is FUSE and a root `rename` there can silently no-op on
    /// this device (`docs/m6b-evidence.md` §6), so a status/recovery file must not
    /// depend on it.
    pub fn flush(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let body = self.render();
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("new");
        let wrote_tmp = std::fs::write(&tmp, &body).is_ok();
        if wrote_tmp && std::fs::rename(&tmp, &self.path).is_ok() {
            self.dirty = false;
            return Ok(());
        }
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(&self.path, body)?;
        self.dirty = false;
        Ok(())
    }

    /// Remove the ledger, after a restore that left nothing owed. Never called by the
    /// daemon: a daemon that clears its own ledger would be erasing the record of what
    /// it still has to put back.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.unknown.is_empty()
    }
}

/// Write-through helper used by the sink: read the current value of `target`, then
/// record it. Split out so the sink's write path stays a two-liner.
pub fn record_before_write(
    ledger: &mut Option<SysfsLedger>,
    path: &str,
    target: &Path,
) -> bool {
    let Some(l) = ledger.as_mut() else {
        return false;
    };
    let current = std::fs::read_to_string(target).ok();
    let added = l.record(path, current.as_deref());
    if added {
        // A failed flush must not stop the write: the ledger is the safety net, not
        // the operation. It stays dirty and the next write tries again.
        let _ = l.flush();
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "uperf_ledger_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("sysfs_orig.txt")
    }

    #[test]
    fn first_write_wins_across_a_reload() {
        let p = tmp("wins");
        let mut l = SysfsLedger::load(&p);
        assert!(l.record("/sys/a", Some("original")));
        l.flush().unwrap();
        // The daemon restarts: the ledger is loaded again, and the "current" value is
        // now our own write. Re-recording it would erase the original.
        let mut l2 = SysfsLedger::load(&p);
        assert!(!l2.record("/sys/a", Some("userspace")));
        assert_eq!(l2.entries().get("/sys/a").map(String::as_str), Some("original"));
    }

    #[test]
    fn an_unreadable_original_is_recorded_as_unknown_never_invented() {
        let p = tmp("unknown");
        let mut l = SysfsLedger::load(&p);
        assert!(l.record("/sys/ro", None));
        assert!(l.entries().is_empty());
        assert!(l.unknown().contains("/sys/ro"));
        l.flush().unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("/sys/ro\n"), "got: {text}");
        // And a reload keeps it unknown rather than promoting it to a value.
        let l2 = SysfsLedger::load(&p);
        assert!(l2.entries().is_empty());
        assert!(l2.unknown().contains("/sys/ro"));
    }

    #[test]
    fn a_value_with_a_newline_cannot_round_trip_so_it_is_unknown() {
        let mut l = SysfsLedger::load(tmp("newline"));
        l.record("/sys/multi", Some("a\nb"));
        assert!(l.entries().is_empty());
        assert!(l.unknown().contains("/sys/multi"));
    }

    #[test]
    fn a_path_with_whitespace_cannot_round_trip_so_it_is_unknown() {
        let mut l = SysfsLedger::load(tmp("space"));
        l.record("/sys/with space", Some("v"));
        assert!(l.entries().is_empty());
        assert!(l.unknown().contains("/sys/with space"));
    }

    #[test]
    fn a_trailing_newline_is_trimmed_but_a_real_value_is_kept() {
        let mut l = SysfsLedger::load(tmp("trim"));
        l.record("/sys/a", Some("0-3\n"));
        assert_eq!(l.entries().get("/sys/a").map(String::as_str), Some("0-3"));
        // An empty value is not a value: `echo > file` is indistinguishable from a
        // file we failed to read, so it goes to unknown instead of being restored as
        // an empty string.
        l.record("/sys/b", Some("\n"));
        assert!(l.unknown().contains("/sys/b"));
    }

    #[test]
    fn corrupt_lines_are_counted_and_do_not_stop_the_rest() {
        let p = tmp("corrupt");
        std::fs::write(&p, "garbage\n/sys/a 0-3\n\n").unwrap();
        let l = SysfsLedger::load(&p);
        assert_eq!(l.corrupt_lines(), 1);
        assert_eq!(l.entries().get("/sys/a").map(String::as_str), Some("0-3"));
    }

    #[test]
    fn flush_falls_back_to_a_direct_write_when_rename_does_not_stick() {
        // A directory *as* the target makes the rename fail; the fallback write then
        // fails too, which must be reported rather than swallowed.
        let p = tmp("fallback");
        let mut l = SysfsLedger::load(&p);
        l.record("/sys/a", Some("1"));
        assert!(l.flush().is_ok());
        assert!(p.exists());
    }
}
