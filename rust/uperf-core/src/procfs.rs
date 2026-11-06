//! Read-only `/proc` access: one implementation of the things this crate keeps needing.
//!
//! Two modules had grown their own: the context scheduler walked `/proc` for processes,
//! threads and `comm` (`read_proc` / `comm_of` / `list_procs`, i32 throughout, `comm`
//! read lazily because an eager read cost ~3000 file opens per scan and measured at more
//! than a full CPU core), and the uprobe frame leg re-implemented the same walks in u32
//! (`thread_ids` / `pids_for_package`), including its own cmdline parsing. The third
//! consumer would have been the dfps subtree.
//!
//! Conventions, deliberately:
//!
//! * ids are `i32` — that is what `/proc/<tid>/stat`, `sched_setaffinity` and the
//!   `cgroup` topic payloads use, and what the scheduler needs. Callers that speak u32
//!   (the uprobe PMU) convert at their own boundary;
//! * `comm` is read **per thread, on demand** ([`comm_of`]) — never as part of a walk;
//! * nothing here writes, and every function reports "gone" as `None`/empty rather than
//!   an error: a process can exit between two reads, and that is normal.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
    pub pid: i32,
    /// `cmdline`'s first argument (a package name for apps, a binary path for native
    /// processes) — what the config's process regexes match against.
    pub name: String,
    pub threads: Vec<ThreadInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadInfo {
    pub tid: i32,
    pub is_main: bool,
}

/// `cmdline`'s first field, NUL-truncated. `None` for a kernel thread (empty cmdline)
/// and for a process that is gone.
pub fn cmdline_argv0(pid: i32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    // cmdline is NUL-separated; the first field is the executable/package.
    let name = String::from_utf8_lossy(&raw)
        .split('\0')
        .next()
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// The tids of a process, ascending. Empty when the process is gone.
pub fn thread_ids(pid: i32) -> Vec<i32> {
    let mut out = Vec::new();
    let task_dir = PathBuf::from(format!("/proc/{pid}/task"));
    let Ok(rd) = std::fs::read_dir(task_dir) else {
        return out;
    };
    for e in rd.flatten() {
        if let Some(tid) = e.file_name().to_string_lossy().parse::<i32>().ok() {
            out.push(tid);
        }
    }
    out.sort_unstable();
    out
}

/// Read a process's cmdline-derived name and its thread ids.
///
/// Thread ids only: reading every thread's `comm` here costs one file read per thread
/// per scan (~3000 a second on this device, measured at more than one full CPU core).
/// Callers read [`comm_of`] lazily, and only for threads they have not already decided.
pub fn read_proc(pid: i32) -> Option<ProcInfo> {
    let name = cmdline_argv0(pid)?;
    let threads = thread_ids(pid)
        .into_iter()
        .map(|tid| ThreadInfo {
            tid,
            is_main: tid == pid,
        })
        .collect();
    Some(ProcInfo { pid, name, threads })
}

/// A thread's `comm` (`/proc/<pid>/task/<tid>/comm`), trimmed.
pub fn comm_of(pid: i32, tid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/comm"))
        .map(|s| s.trim_end_matches('\n').to_string())
        .unwrap_or_default()
}

/// A thread's `comm` when only the tid is known (`/proc/<tid>/comm` resolves for
/// threads too). Used for diagnostics, where the owning pid is not at hand.
pub fn comm_by_tid(tid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{tid}/comm"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "?".into())
}

/// Enumerate every process that has a non-empty cmdline, by pid.
pub fn list_procs() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in entries.flatten() {
        let Some(pid) = e.file_name().to_string_lossy().parse::<i32>().ok() else {
            continue;
        };
        if let Some(p) = read_proc(pid) {
            out.push(p);
        }
    }
    out.sort_by_key(|p| p.pid);
    out
}

/// Pids whose `argv[0]` is the package itself or one of its `:remote` processes.
pub fn pids_for_package(pkg: &str) -> Vec<i32> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    let remote_prefix = format!("{pkg}:");
    for e in entries.flatten() {
        let Some(pid) = e.file_name().to_string_lossy().parse::<i32>().ok() else {
            continue;
        };
        let Some(argv0) = cmdline_argv0(pid) else { continue };
        if argv0 == pkg || argv0.starts_with(&remote_prefix) {
            out.push(pid);
        }
    }
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_is_readable_and_has_this_thread() {
        let me = std::process::id() as i32;
        let p = read_proc(me).expect("our own /proc entry must be readable");
        assert_eq!(p.pid, me);
        assert!(!p.name.is_empty(), "argv[0] must not be empty");
        assert!(
            p.threads.iter().any(|t| t.is_main && t.tid == me),
            "the main thread must be flagged: {:?}",
            p.threads
        );
        // Lazy `comm`: the main thread's name is the process name, minus the 15-char cap.
        let comm = comm_of(me, me);
        assert!(!comm.is_empty(), "comm_of must work for the main thread");
    }

    #[test]
    fn a_process_that_cannot_exist_reads_as_gone() {
        // The contract every caller relies on: "gone" is None/empty, never an error and
        // never a fabricated entry.
        let impossible = 0x7fff_ffffi32;
        assert!(read_proc(impossible).is_none());
        assert!(thread_ids(impossible).is_empty());
        assert!(comm_of(impossible, impossible).is_empty());
        assert!(pids_for_package("com.example.definitely.not.running").is_empty());
    }

    #[test]
    fn a_package_name_matches_its_remote_processes_too() {
        // The rule this function exists for: `com.foo:push` is the same app as `com.foo`.
        // Asserted on the string rule rather than on a live process, because the test
        // cannot start one on every host.
        let me = std::process::id() as i32;
        let argv0 = cmdline_argv0(me).expect("argv[0]");
        // Our own binary's argv[0] is the test binary path, so a package query must not
        // match it; what must match is the exact string.
        let hits = pids_for_package(&argv0);
        assert!(hits.contains(&me), "an exact argv[0] match must find this process: {hits:?}");
    }
}
