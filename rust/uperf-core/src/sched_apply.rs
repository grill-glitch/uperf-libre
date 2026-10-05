//! Applying scheduler decisions to real threads.
//!
//! Two syscalls do the work: `sched_setaffinity(2)` for the CPU set and
//! `sched_setscheduler(2)` (+ `setpriority(2)` for the nice value) for the SCHED
//! class. Decoding of the config's `prio` code into a class is
//! [`SchedPolicy`]'s job; this module is only the kernel side.
//!
//! Every function here is safe to call on the *current* thread, which is how the
//! host unit tests exercise them for real rather than mocking the syscalls.

#![allow(dead_code)]

use std::io;
use std::path::PathBuf;

use uperf_config::SchedPolicy;

/// Bind a thread to a CPU set.
pub fn set_affinity(tid: i32, cpus: &[usize]) -> io::Result<()> {
    if cpus.is_empty() {
        return Ok(());
    }
    // SAFETY: `cpu_set_t` is a plain bitset; we zero it, set bits inside its
    // bounds (CPU_SETSIZE is checked below), and pass its real size.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        for c in cpus {
            if *c >= libc::CPU_SETSIZE as usize {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("cpu id {c} exceeds CPU_SETSIZE"),
                ));
            }
            libc::CPU_SET(*c, &mut set);
        }
        if libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Apply a SCHED class (and the nice value for the NORMAL-ish classes).
///
/// `SchedPolicy::Skip` is a no-op, matching the config's `0` code.
pub fn set_sched(tid: i32, policy: SchedPolicy) -> io::Result<()> {
    let (class, prio) = match policy {
        SchedPolicy::Skip => return Ok(()),
        // SCHED_FIFO/SCHED_RR carry the static priority in sched_priority.
        SchedPolicy::Fifo(p) => (libc::SCHED_FIFO, p),
        // Everything else is a normal-ish class: sched_priority must be 0 and
        // the "priority" is the nice value, applied separately.
        SchedPolicy::Normal { .. } | SchedPolicy::NormalDefault | SchedPolicy::Batch
        | SchedPolicy::Idle => (class_of(policy), 0),
    };
    let param = libc::sched_param { sched_priority: prio };
    // SAFETY: `param` outlives the call; the kernel copies it.
    unsafe {
        if libc::sched_setscheduler(tid, class, &param) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if let Some(nice) = nice_of(policy) {
        // SAFETY: plain integer argument, no pointers.
        let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, nice) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn class_of(p: SchedPolicy) -> libc::c_int {
    match p {
        SchedPolicy::Fifo(_) => libc::SCHED_FIFO,
        SchedPolicy::Normal { .. } | SchedPolicy::NormalDefault => libc::SCHED_NORMAL,
        SchedPolicy::Batch => libc::SCHED_BATCH,
        SchedPolicy::Idle => libc::SCHED_IDLE,
        SchedPolicy::Skip => libc::SCHED_NORMAL,
    }
}

/// The nice value a policy carries, if any (`None` = leave nice alone).
fn nice_of(p: SchedPolicy) -> Option<libc::c_int> {
    match p {
        SchedPolicy::Normal { nice } => Some(nice),
        SchedPolicy::NormalDefault => Some(0),
        // SCHED_IDLE/BATCH in Linux still carry a nice value, but upstream's
        // `-2`/`-3` codes say nothing about it, so leave it untouched rather
        // than inventing one.
        _ => None,
    }
}

/// Read back a thread's effective CPU set from `/proc/<tid>/status`
/// (`Cpus_allowed_list`, e.g. `0-3` or `0,4-5`).
///
/// Used to *verify* an affinity write instead of trusting its return code: a
/// write can be accepted and still be constrained by the thread's cpuset cgroup,
/// or be re-applied by Android's task-profile controller. Reporting the
/// difference is what turns a silent no-op into a visible one.
pub fn cpus_allowed(tid: i32) -> Option<Vec<usize>> {
    let status = std::fs::read_to_string(format!("/proc/{tid}/status")).ok()?;
    let line = status
        .lines()
        .find(|l| l.starts_with("Cpus_allowed_list:"))?
        .split(':')
        .nth(1)?
        .trim();
    parse_cpu_list(line)
}

/// Parse the kernel's CPU-list syntax: `0-3`, `0,4-5`, `7`.
pub fn parse_cpu_list(s: &str) -> Option<Vec<usize>> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('-') {
            Some((a, b)) => {
                let a: usize = a.trim().parse().ok()?;
                let b: usize = b.trim().parse().ok()?;
                if b < a || b - a > 1024 {
                    return None;
                }
                out.extend(a..=b);
            }
            None => out.push(part.parse().ok()?),
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// The calling thread's real id.
///
/// The syscalls accept `0` as "the calling thread", but `/proc/<tid>/stat` needs
/// the actual id — which is how the first version of the tests managed to fail
/// with `No such file or directory` on `/proc/0/stat`.
pub fn current_tid() -> i32 {
    // SAFETY: gettid takes no arguments and cannot fail on Linux.
    unsafe { libc::gettid() }
}

/// Read back a thread's `(SCHED class, nice)` from `/proc/<tid>/stat`.
///
/// Used by the tests and by device verification; `fields` are 1-based in the
/// kernel's numbering (see `proc(5)`), and `stat`'s second field is the comm in
/// parentheses, so parsing must start *after* the last `)`.
///
/// Field map after the `)`: index 0 is field 3 (`state`), so field N is index
/// N-3. The two we want are `nice` = field **19** (index 16) and `policy` =
/// field **41** (index 38). Field 18 is `priority`, which is `20 + nice` — an
/// earlier version read index 15 and confidently returned `25` for nice `5`,
/// which is exactly how that off-by-one shows up.
pub fn thread_sched(tid: i32) -> io::Result<(i32, i32)> {
    let stat = std::fs::read_to_string(format!("/proc/{tid}/stat"))?;
    let after = stat.rfind(')').ok_or_else(|| io::Error::other("malformed stat"))?;
    let rest = stat[after + 1..].trim();
    let f: Vec<&str> = rest.split_whitespace().collect();
    let nice = f.get(16).and_then(|x| x.parse().ok()).unwrap_or(0);
    let policy = f.get(38).and_then(|x| x.parse().ok()).unwrap_or(0);
    Ok((policy, nice))
}

/// A process and the threads under it, as seen through `/proc`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
    pub pid: i32,
    /// `cmdline`'s first argument (a package name for apps, a binary path for
    /// native processes) — what the config's process regexes match against.
    pub name: String,
    pub threads: Vec<ThreadInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadInfo {
    pub tid: i32,
    /// `/proc/<pid>/task/<tid>/comm`.
    pub comm: String,
    pub is_main: bool,
}

/// Read a process's cmdline-derived name and its threads.
pub fn read_proc(pid: i32) -> Option<ProcInfo> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    // cmdline is NUL-separated; the first field is the executable/package.
    let name = String::from_utf8_lossy(&raw)
        .split('\0')
        .next()
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        return None; // kernel thread
    }
    let mut threads = Vec::new();
    let task_dir = PathBuf::from(format!("/proc/{pid}/task"));
    for e in std::fs::read_dir(task_dir).ok()?.flatten() {
        let Ok(tid) = e.file_name().to_string_lossy().parse::<i32>() else { continue };
        let comm = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/comm"))
            .map(|s| s.trim_end_matches('\n').to_string())
            .unwrap_or_default();
        threads.push(ThreadInfo { tid, comm, is_main: tid == pid });
    }
    threads.sort_by_key(|t| t.tid);
    Some(ProcInfo { pid, name, threads })
}

/// Enumerate every process that has a non-empty cmdline.
pub fn list_procs() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else { return out };
    for e in entries.flatten() {
        let Some(pid) = e.file_name().to_string_lossy().parse::<i32>().ok() else { continue };
        if let Some(p) = read_proc(pid) {
            out.push(p);
        }
    }
    out.sort_by_key(|p| p.pid);
    out
}

/// The launcher package, as `/HOME_PACKAGE/` resolves to it.
///
/// Upstream logs `Current home is '<pkg>'` at startup, and this is the value
/// substituted into the config's `/HOME_PACKAGE/` token. On device the package
/// comes from the resolved HOME activity
/// (`cmd package resolve-activity --brief -a android.intent.action.MAIN -c
/// android.intent.category.HOME` returns `com.android.launcher3/.uioverrides.
/// QuickstepLauncher` -> `com.android.launcher3`, matching the upstream log).
pub fn resolve_home_package() -> Option<String> {
    resolve_home_package_with(&mut |m| eprintln!("{m}"))
}

/// Same, with a log sink, and **never** returning an empty package.
///
/// The first version returned whatever came out of `Command::new("cmd")`, and on
/// device that produced `Some("")` — which substituted `/HOME_PACKAGE/` with the
/// empty string, turning the launcher rule's regex into `""` and matching *every
/// process on the system*. The dry run caught it before it touched the kernel.
/// An empty or whitespace-only result is therefore treated as a failure, and the
/// failure is reported rather than swallowed.
pub fn resolve_home_package_with(log: &mut dyn FnMut(&str)) -> Option<String> {
    if let Ok(explicit) = std::env::var("UPERF_HOME_PACKAGE") {
        let t = explicit.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    // `cmd` is at /system/bin/cmd; try the absolute path first because the
    // process may be started with a minimal PATH.
    let mut last_err = String::new();
    for bin in ["/system/bin/cmd", "cmd"] {
        match std::process::Command::new(bin)
            .args([
                "package",
                "resolve-activity",
                "--brief",
                "-a",
                "android.intent.action.MAIN",
                "-c",
                "android.intent.category.HOME",
            ])
            .output()
        {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                match parse_home_activity(&text) {
                    Some(pkg) if !pkg.trim().is_empty() => return Some(pkg.trim().to_string()),
                    _ => {
                        last_err = format!(
                            "{bin}: exit={:?} stdout={:?} stderr={:?}",
                            out.status.code(),
                            text.trim(),
                            String::from_utf8_lossy(&out.stderr).trim()
                        );
                    }
                }
            }
            Err(e) => last_err = format!("{bin}: spawn failed: {e}"),
        }
    }
    log(&format!("Rust: cannot resolve the home package ({last_err})"));
    None
}

/// Extract the package from the `pkg/activity` line of `resolve-activity`.
pub fn parse_home_activity(text: &str) -> Option<String> {
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() || !line.contains('/') {
            continue;
        }
        let pkg = line.split('/').next().unwrap_or("").trim();
        // Guard against the "priority=... match=..." noise lines.
        if pkg.contains('=') || pkg.is_empty() {
            continue;
        }
        return Some(pkg.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use uperf_config::SchedPolicy as P;

    /// These run against the real kernel, on the *calling* thread (tid 0 means
    /// "the current thread"), so they prove the syscalls work rather than
    /// asserting against a mock. Each one restores what it changed.
    #[test]
    fn set_affinity_is_accepted_and_reversible() {
        let tid = 0; // current thread
        // Read the original mask by round-tripping: bind to a single CPU the
        // thread will accept, then widen back out.
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        let single = vec![0usize];
        if set_affinity(tid, &single).is_err() {
            // Some sandboxes (CI containers with restricted cpusets) refuse.
            eprintln!("skipping: affinity change not permitted here");
            return;
        }
        let all: Vec<usize> = (0..n).collect();
        set_affinity(tid, &all).expect("widening back must work");
    }

    #[test]
    fn empty_cpu_set_is_a_noop() {
        assert!(set_affinity(0, &[]).is_ok());
    }

    #[test]
    fn out_of_range_cpu_is_rejected_before_the_syscall() {
        let err = set_affinity(0, &[libc::CPU_SETSIZE as usize + 1]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// Lowering one's own scheduling priority is always allowed; raising it back
    /// needs `CAP_SYS_NICE`. Verified on the dev host (CapEff = 0):
    ///
    /// ```text
    /// ->IDLE    ok
    /// ->NORMAL  EPERM
    /// ->BATCH   EPERM
    /// ->FIFO10  EPERM
    /// ```
    ///
    /// So every destructive policy assertion here is written to accept either
    /// outcome and only checks the part the kernel permits. On device uperf runs
    /// as real root with full caps, where the round trip succeeds.
    fn can_raise_priority() -> bool {
        let me = current_tid();
        let before = thread_sched(me).expect("read own stat").0;
        if set_sched(0, P::Idle).is_err() {
            return false; // cannot even lower -> nothing to learn
        }
        let raised = set_sched(0, P::Normal { nice: 0 }).is_ok();
        if !raised {
            // Leave the thread as the kernel will let us: nothing more to do,
            // it is the last thing this test thread runs.
            return false;
        }
        let _ = set_sched(0, P::Normal { nice: 0 });
        let _ = before;
        true
    }

    #[test]
    fn sched_idle_sets_policy_5() {
        let me = current_tid();
        if set_sched(0, P::Idle).is_err() {
            eprintln!("skipping: lowering the policy is not permitted here");
            return;
        }
        assert_eq!(thread_sched(me).unwrap().0, libc::SCHED_IDLE, "SCHED_IDLE is 5");
        // Only undo it if the kernel lets us; an unprivileged host refuses to
        // raise priority back, which is expected rather than a failure.
        let _ = set_sched(0, P::Normal { nice: 0 });
    }

    #[test]
    fn batch_class_is_3() {
        let me = current_tid();
        if set_sched(0, P::Batch).is_err() {
            eprintln!("skipping: SCHED_BATCH not permitted here");
            return;
        }
        assert_eq!(thread_sched(me).unwrap().0, libc::SCHED_BATCH);
        let _ = set_sched(0, P::Normal { nice: 0 });
    }

    #[test]
    fn unprivileged_can_lower_but_not_raise_priority_back() {
        // The documented asymmetry, asserted without assuming which side of it
        // this test environment is on.
        let me = current_tid();
        if set_sched(0, P::Idle).is_err() {
            eprintln!("skipping: cannot lower the policy here at all");
            return;
        }
        assert_eq!(thread_sched(me).unwrap().0, libc::SCHED_IDLE);
        if set_sched(0, P::Normal { nice: 0 }).is_ok() {
            // Privileged (root on device, or a host with CAP_SYS_NICE).
            assert_eq!(thread_sched(me).unwrap().0, libc::SCHED_NORMAL);
            // And with privilege, the nice code really lands.
            assert!(set_sched(0, P::Normal { nice: 5 }).is_ok());
            assert_eq!(thread_sched(me).unwrap().1, 5, "nice = code - 120");
        } else {
            // Unprivileged: the kernel refuses to let us raise priority back.
            assert_eq!(
                thread_sched(me).unwrap().0,
                libc::SCHED_IDLE,
                "a refused raise must leave the policy untouched"
            );
        }
    }

    #[test]
    fn fifo_needs_privilege_and_is_skipped_without_it() {
        if set_sched(0, P::Fifo(10)).is_err() {
            eprintln!("skipping: SCHED_FIFO not permitted here");
            return;
        }
        assert_eq!(thread_sched(current_tid()).unwrap().0, libc::SCHED_FIFO);
        let _ = set_sched(0, P::Normal { nice: 0 });
    }

    #[test]
    fn nice_matches_field_19_not_field_18() {
        // Regression for the off-by-one: field 18 is `priority` (= 20 + nice),
        // so reading index 15 returns e.g. 25 for a nice of 5. Assert the
        // relationship instead of a fixed value, since the harness nice varies.
        let me = current_tid();
        let raw = std::fs::read_to_string(format!("/proc/{me}/stat")).unwrap();
        let after = raw.rfind(')').unwrap();
        let f: Vec<&str> = raw[after + 1..].split_whitespace().collect();
        let priority_field: i32 = f[15].parse().unwrap(); // field 18
        let nice_field: i32 = f[16].parse().unwrap(); // field 19
        let (_, nice_from_helper) = thread_sched(me).unwrap();
        assert_eq!(nice_from_helper, nice_field);
        assert_eq!(priority_field, 20 + nice_field, "field 18 is priority");
    }

    #[test]
    fn reads_our_own_process_and_finds_the_main_thread() {
        let pid = std::process::id() as i32;
        let p = read_proc(pid).expect("read own process");
        assert_eq!(p.pid, pid);
        assert!(!p.name.is_empty());
        assert!(!p.threads.is_empty());
        assert!(
            p.threads.iter().any(|t| t.is_main && t.tid == pid),
            "the main thread must be tid == pid"
        );
    }

    #[test]
    fn list_procs_includes_ourselves_and_skips_kernel_threads() {
        let procs = list_procs();
        assert!(procs.len() > 5, "expected a populated /proc");
        let me = std::process::id() as i32;
        assert!(procs.iter().any(|p| p.pid == me));
        assert!(
            procs.iter().all(|p| !p.name.is_empty()),
            "kernel threads have an empty cmdline and must be skipped"
        );
    }

    #[test]
    fn empty_output_is_rejected_not_returned_as_empty_package() {
        // The bug the device dry run caught: a failed resolution must never look
        // like a valid empty package.
        assert_eq!(parse_home_activity(""), None);
        assert_eq!(parse_home_activity("no slashes here\n"), None);
        assert_eq!(parse_home_activity("a=b/c\n"), None, "the priority= noise line");
    }

    #[test]
    fn resolves_via_the_explicit_override_when_set() {
        std::env::set_var("UPERF_HOME_PACKAGE", "com.example.launcher");
        assert_eq!(
            resolve_home_package_with(&mut |_| {}).as_deref(),
            Some("com.example.launcher")
        );
        // A blank override must fall through to the real resolver, not be used.
        std::env::set_var("UPERF_HOME_PACKAGE", "   ");
        assert_ne!(resolve_home_package_with(&mut |_| {}).as_deref(), Some("   "));
        std::env::remove_var("UPERF_HOME_PACKAGE");
    }

    #[test]
    fn parses_cpu_lists() {
        assert_eq!(parse_cpu_list("0-7"), Some(vec![0, 1, 2, 3, 4, 5, 6, 7]));
        assert_eq!(parse_cpu_list("0-3"), Some(vec![0, 1, 2, 3]));
        assert_eq!(parse_cpu_list("0,4-5"), Some(vec![0, 4, 5]));
        assert_eq!(parse_cpu_list("7"), Some(vec![7]));
        assert_eq!(parse_cpu_list(""), None);
        assert_eq!(parse_cpu_list("5-2"), None, "inverted range");
        assert_eq!(parse_cpu_list("x"), None);
    }

    #[test]
    fn cpus_allowed_reflects_a_real_write_within_the_cpuset() {
        // Only assert a mask the process is actually allowed to hold: the cpuset
        // cgroup can veto a wider one (EINVAL), and the probe verified that a
        // write outside the cpuset is refused while one inside sticks.
        let me = current_tid();
        let Some(orig) = cpus_allowed(me) else {
            eprintln!("skipping: Cpus_allowed_list unavailable");
            return;
        };
        if orig.len() < 2 {
            eprintln!("skipping: only one CPU allowed here");
            return;
        }
        let narrow = vec![orig[0]];
        if set_affinity(0, &narrow).is_err() {
            eprintln!("skipping: affinity change not permitted");
            return;
        }
        match cpus_allowed(me) {
            Some(now) => assert_eq!(now, narrow, "the write must be observable"),
            None => eprintln!("skipping: cannot read Cpus_allowed_list back"),
        }
        // Restore is best-effort: it can be refused if the cpuset shrank.
        let _ = set_affinity(0, &orig);
    }

    #[test]
    fn parses_the_device_resolve_activity_output() {
        // Verbatim from alioth.
        let out = "priority=0 preferredOrder=0 match=0x108000 specificIndex=-1 isDefault=true\n\
                   com.android.launcher3/.uioverrides.QuickstepLauncher\n";
        assert_eq!(parse_home_activity(out).as_deref(), Some("com.android.launcher3"));
        assert_eq!(parse_home_activity(""), None);
    }
}
