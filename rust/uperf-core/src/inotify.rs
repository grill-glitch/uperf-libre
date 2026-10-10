//! Minimal inotify wrapper for the two user-editable text files.
//!
//! Upstream names the field `switchInode`, and the vendored platform layer already
//! proves inotify works on this path: the daemon watches `<USER_PATH>/uperf.json`
//! — the same `/sdcard/Android/yc/uperf/` directory — and reloads on
//! `IN_CLOSE_WRITE` (verified on device in M0). So the same mechanism is used for
//! `cur_powermode.txt` and `perapp_powermode.txt` rather than polling.
//!
//! Only `inotify_add_watch`/`poll`/`read` are wrapped, with no external crates.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What happened to a watched file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// The file was written and closed — what `echo x > file` produces.
    Written(PathBuf),
    /// The file disappeared (deleted, or replaced by a rename).
    Gone(PathBuf),
    /// The watch was dropped by the kernel (file moved/replaced); the caller must
    /// re-add it by path for further events.
    WatchLost(PathBuf),
}

pub struct Inotify {
    fd: RawFd,
    /// watch descriptor -> path
    paths: HashMap<i32, PathBuf>,
}

impl Inotify {
    pub fn new() -> io::Result<Self> {
        // CLOEXEC: never leak the fd into the processes the applier spawns.
        // NONBLOCK: combined with poll(2) so a stop flag can interrupt the wait.
        // SAFETY: `inotify_init1` takes only flag bits and returns a new fd (owned
        // by this struct from here on) or -1.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, paths: HashMap::new() })
    }

    /// Watch a file for writes and for being replaced/removed.
    ///
    /// **Always succeeds for a well-formed path**, even when the file does not
    /// exist yet: the parent directory is watched instead, so a file created
    /// later still produces an event. That matters because `sfanalysis.hint` is
    /// written by another process and may not exist when the daemon starts — an
    /// earlier version returned `ENOENT`, the watch was never armed, and the hint
    /// byte was silently ignored.
    ///
    /// The caller should treat any event as a wakeup and re-read its files rather
    /// than trying to attribute the event to a path: one `echo x > file` produces
    /// both `IN_MODIFY` and `IN_CLOSE_WRITE`, and a rename-into-place arrives on
    /// the *directory*.
    pub fn watch(&mut self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            if let Ok(dc) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) {
                // STRUCTURAL changes only: a file appearing, being renamed into
                // place, or being removed. `IN_CLOSE_WRITE`/`IN_MODIFY` must NOT be
                // here.
                //
                // The directory we watch is the one holding the preset files — which
                // is also where the daemon writes its log (`USER_PATH/uperf_log.txt`
                // sits beside `cur_powermode.txt`). Including content-write events on
                // the directory made the daemon's own log writes wake the watcher,
                // which then re-read and logged again: a self-sustaining loop at full
                // speed. Measured: 1065 ticks/10 s with the log in the watched
                // directory, **61** with the directory watch restricted to structural
                // events — a 17x difference, and on a phone that is a core spinning
                // plus continuous storage writes. Content changes to the files we
                // actually care about are covered by their own watches.
                let dmask = libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_DELETE;
                // SAFETY: `dc` is a NUL-terminated directory path that outlives the
                // call and `self.fd` is the live inotify fd owned by this struct.
                let dwd = unsafe { libc::inotify_add_watch(self.fd, dc.as_ptr(), dmask) };
                if dwd >= 0 {
                    self.paths.insert(dwd, dir.to_path_buf());
                }
            }
        }
        self.arm_file(path)
    }

    /// Add the watch on the file itself, if it exists right now.
    fn arm_file(&mut self, path: &Path) -> io::Result<()> {
        let mask = libc::IN_CLOSE_WRITE
            | libc::IN_MODIFY
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF;
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        // SAFETY: `c` is a NUL-terminated path alive for the call; `self.fd` is the
        // live inotify fd. A missing file (ENOENT) is a normal outcome here and is
        // handled below, not treated as a failure.
        let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), mask) };
        if wd < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::NotFound {
                return Ok(()); // parent-dir watch covers it
            }
            return Err(e);
        }
        self.paths.insert(wd, path.to_path_buf());
        Ok(())
    }

    /// Wait up to `timeout` for events. `Ok(vec![])` means the timeout expired.
    pub fn poll(&mut self, timeout: Duration) -> io::Result<Vec<WatchEvent>> {
        let mut pfd = libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: one valid pollfd, count 1.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(Vec::new());
            }
            return Err(e);
        }
        if rc == 0 || pfd.revents & libc::POLLIN == 0 {
            return Ok(Vec::new());
        }
        self.read_available()
    }

    fn read_available(&mut self) -> io::Result<Vec<WatchEvent>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: buf is a valid writable slice of that length.
            let n = unsafe {
                libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len())
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::WouldBlock {
                    return Ok(out); // drained
                }
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if n == 0 {
                return Ok(out);
            }
            let mut off = 0usize;
            let n = n as usize;
            while off + std::mem::size_of::<libc::inotify_event>() <= n {
                // SAFETY: reading a copy of the header out of the byte buffer.
                let ev: libc::inotify_event = unsafe {
                    std::ptr::read_unaligned(buf[off..].as_ptr().cast::<libc::inotify_event>())
                };
                off += std::mem::size_of::<libc::inotify_event>() + ev.len as usize;
                let Some(path) = self.paths.get(&ev.wd).cloned() else { continue };
                let m = ev.mask;
                if m & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_IGNORED) != 0 {
                    out.push(WatchEvent::WatchLost(path));
                } else if m & (libc::IN_CLOSE_WRITE | libc::IN_MODIFY | libc::IN_MOVED_TO | libc::IN_CREATE) != 0 {
                    out.push(WatchEvent::Written(path));
                }
            }
        }
    }

    /// Re-arm a watch whose inode was replaced (or which could not be armed
    /// because the file did not exist yet), so the next write is seen.
    pub fn rearm(&mut self, path: &Path) {
        let _ = self.arm_file(path);
    }
}

impl Drop for Inotify {
    fn drop(&mut self) {
        // SAFETY: closing an fd we own; the return value is not interesting.
        unsafe {
            libc::close(self.fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("uperf_inotify_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn fires_on_an_in_place_write() {
        let d = tmpdir("inplace");
        let f = d.join("cur_powermode.txt");
        std::fs::write(&f, "balance\n").unwrap();
        let mut ino = Inotify::new().unwrap();
        ino.watch(&f).unwrap();

        // Same shape as powercfg_main.sh: `echo x > file`.
        std::fs::write(&f, "performance\n").unwrap();
        let ev = ino.poll(Duration::from_millis(1500)).unwrap();
        assert!(
            ev.iter().any(|e| matches!(e, WatchEvent::Written(p) if p == &f)),
            "expected a write event, got {ev:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn fires_on_a_rename_into_place() {
        let d = tmpdir("rename");
        let f = d.join("perapp_powermode.txt");
        std::fs::write(&f, "* balance\n").unwrap();
        let mut ino = Inotify::new().unwrap();
        ino.watch(&f).unwrap();

        let tmp = d.join(".tmp_new");
        std::fs::write(&tmp, "* performance\n").unwrap();
        std::fs::rename(&tmp, &f).unwrap();

        let ev = ino.poll(Duration::from_millis(1500)).unwrap();
        assert!(!ev.is_empty(), "a rename-into-place must be noticed: {ev:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn times_out_without_events() {
        let d = tmpdir("idle");
        let f = d.join("idle.txt");
        std::fs::write(&f, "x").unwrap();
        let mut ino = Inotify::new().unwrap();
        ino.watch(&f).unwrap();
        let ev = ino.poll(Duration::from_millis(120)).unwrap();
        assert!(ev.is_empty(), "nothing wrote to the file: {ev:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn survives_many_polls() {
        // The runtime calls poll() in a loop for the process lifetime; make sure
        // repeated timeouts do not break the fd.
        let d = tmpdir("loop");
        let f = d.join("f.txt");
        std::fs::write(&f, "1").unwrap();
        let mut ino = Inotify::new().unwrap();
        ino.watch(&f).unwrap();
        for _ in 0..5 {
            assert!(ino.poll(Duration::from_millis(20)).unwrap().is_empty());
        }
        let mut fh = std::fs::OpenOptions::new().append(true).open(&f).unwrap();
        writeln!(fh, "2").unwrap();
        drop(fh);
        let ev = ino.poll(Duration::from_millis(1500)).unwrap();
        assert!(!ev.is_empty(), "the watch must still work after idling: {ev:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn watching_a_missing_file_succeeds_and_catches_its_creation() {
        // sfanalysis.hint may not exist when the daemon starts.
        let d = tmpdir("missing");
        let f = d.join("sfanalysis.hint");
        let mut ino = Inotify::new().unwrap();
        ino.watch(&f).expect("a missing file must not be an error");
        assert!(ino.poll(Duration::from_millis(120)).unwrap().is_empty());
        std::fs::write(&f, [4u8]).unwrap();
        let ev = ino.poll(Duration::from_millis(1500)).unwrap();
        assert!(!ev.is_empty(), "creating the file must wake us up: {ev:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A write to a *sibling* file in the watched directory must not produce an
    /// event. This is the shape that spun a core on device: the daemon's own log
    /// lives beside the preset files, so content-write events on the directory
    /// closed a feedback loop with the log.
    #[test]
    fn sibling_file_writes_do_not_wake_us() {
        let d = tmpdir("sibling");
        let f = d.join("cur_powermode.txt");
        std::fs::write(&f, "balance\n").unwrap();
        let mut ino = Inotify::new().unwrap();
        ino.watch(&f).unwrap();
        // drain anything from arming
        let _ = ino.poll(Duration::from_millis(50));

        // a sibling that the daemon also writes (its log)
        let sibling = d.join("uperf_log.txt");
        std::fs::write(&sibling, "line 1\n").unwrap();
        for i in 0..5 {
            let mut fh = std::fs::OpenOptions::new()
                .append(true)
                .open(&sibling)
                .unwrap();
            use std::io::Write as _;
            writeln!(fh, "line {i}").unwrap();
            drop(fh);
        }
        let ev = ino.poll(Duration::from_millis(300)).unwrap();
        assert!(
            ev.iter().all(|e| !matches!(e, WatchEvent::Written(p) if p == &sibling)),
            "a sibling write must not be reported as a watched file: {ev:?}"
        );

        // ...but the watched file itself still works.
        std::fs::write(&f, "performance\n").unwrap();
        let ev = ino.poll(Duration::from_millis(1500)).unwrap();
        assert!(
            ev.iter().any(|e| matches!(e, WatchEvent::Written(p) if p == &f)),
            "the watched file must still wake us: {ev:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rearm_keeps_the_watch_alive_after_a_replacement() {
        let d = tmpdir("rearm");
        let f = d.join("f.txt");
        std::fs::write(&f, "1").unwrap();
        let mut ino = Inotify::new().unwrap();
        ino.watch(&f).unwrap();
        // Replace the file wholesale, then re-arm and write again.
        std::fs::remove_file(&f).unwrap();
        std::fs::write(&f, "2").unwrap();
        ino.rearm(&f);
        std::fs::write(&f, "3").unwrap();
        let ev = ino.poll(Duration::from_millis(1500)).unwrap();
        assert!(
            ev.iter().any(|e| matches!(e, WatchEvent::Written(p) if p == &f)),
            "expected a write after re-arming: {ev:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
