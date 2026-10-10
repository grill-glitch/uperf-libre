//! uprobe-count — per-PID counting of a tracefs uprobe event via `perf_event_open`.
//!
//! This is the primitive AppOpt's eBPF leg is built on: attach a probe to the target
//! process's `libgui.so` `queueBuffer` and count only *that* task's calls, so another
//! app's frames are never mixed in. The probe itself is registered through tracefs (see
//! the caller); this tool proves the counting half — `PERF_TYPE_TRACEPOINT` on the
//! uprobe event's id, attached to one pid.
//!
//! usage: uprobe-count <tracepoint-id> <pid|0=all> <seconds>

use std::os::unix::io::FromRawFd;

/// `struct perf_event_attr`, only the fields this probe needs; the rest stay zero.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct PerfEventAttr {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64, // bitfields: disabled, inherit, pinned, ...
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
    config2: u64,
    branch_sample_type: u64,
    sample_regs_user: u64,
    sample_stack_user: u32,
    clockid: i32,
    sample_regs_intr: u64,
    aux_watermark: u32,
    sample_max_stack: u16,
    reserved_2: u16,
    aux_sample_size: u32,
    reserved_3: u32,
    sig_data: u64,
}

const PERF_TYPE_TRACEPOINT: u32 = 2;

// _IOC('$', dir, nr); the request type differs per libc flavour, so callers cast.
const fn ioc(dir: u32, ty: u32, nr: u32) -> u64 {
    ((dir << 30) | (0 << 16) | (ty << 8) | nr) as u64
}
const PERF_EVENT_IOC_ENABLE: u64 = ioc(0, b'$' as u32, 0);
const PERF_EVENT_IOC_DISABLE: u64 = ioc(0, b'$' as u32, 1);
const PERF_EVENT_IOC_RESET: u64 = ioc(0, b'$' as u32, 3);

/// One counting event. `pid > 0, cpu = -1` is per-task; `pid = -1, cpu >= 0` is per-CPU
/// (the only legal way to ask for "everything").
/// Retry on EINTR: a signal arriving during the syscall must not read as a refusal.
fn retry_int(mut f: impl FnMut() -> i64) -> i64 {
    loop {
        let r = f();
        if r != -1 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return r;
        }
    }
}

fn open_event(id: u64, pid: i32, cpu: i32, inherit: bool, start_disabled: bool) -> Result<i32, String> {
    let mut attr = PerfEventAttr {
        type_: PERF_TYPE_TRACEPOINT,
        size: std::mem::size_of::<PerfEventAttr>() as u32,
        config: id,
        // 0 = a pure COUNTING event. With a non-zero period the kernel makes it a
        // sampling event and `read()` waits for samples instead of returning a count.
        sample_period: 0,
        flags: 0,
        ..Default::default()
    };
    if start_disabled {
        attr.flags |= 1; // disabled
    }
    if inherit {
        attr.flags |= 1 << 1; // inherit
    }
    let fd = retry_int(|| unsafe {
        libc::syscall(
            libc::SYS_perf_event_open,
            &attr as *const PerfEventAttr,
            pid as libc::pid_t,
            cpu as libc::c_int,
            -1i32 as libc::c_int,
            0u64,
        )
    });
    if fd < 0 {
        return Err(format!("{}", std::io::Error::last_os_error()));
    }
    Ok(fd as i32)
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    // `pmu <path> <offset-hex> <pid> <secs>`: the clean route -- the kernel's `uprobe`
    // PMU (type 6) takes the file and offset directly and HONOURS a task binding, unlike
    // a tracefs uprobe's tracepoint (measured: per-task 0, per-CPU counts).
    if a.first().map(|s| s.as_str()) == Some("pmu") {
        let path = a.get(1).cloned().unwrap_or_default();
        let off = a
            .get(2)
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        let pid: i32 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
        let secs: u64 = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(3);
        let uprobe_type: u32 = std::fs::read_to_string("/sys/bus/event_source/devices/uprobe/type")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(6);
        let cpath = std::ffi::CString::new(path.clone()).expect("path");
        let mut attr = PerfEventAttr {
            type_: uprobe_type,
            size: std::mem::size_of::<PerfEventAttr>() as u32,
            config: 0,
            config1: cpath.as_ptr() as u64, // the kernel reads this string during the call
            config2: off,
            sample_period: 0,
            flags: 1, // disabled; enabled below
            ..Default::default()
        };
        let _ = &mut attr;
        let fd = retry_int(|| unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &attr as *const PerfEventAttr,
                pid as libc::pid_t,
                -1i32 as libc::c_int,
                -1i32 as libc::c_int,
                0u64,
            )
        });
        if fd < 0 {
            eprintln!("perf_event_open(uprobe PMU type={uprobe_type}, pid={pid}): {}", std::io::Error::last_os_error());
            std::process::exit(1);
        }
        let f = fd as i32;
        retry_int(|| unsafe { libc::ioctl(f, PERF_EVENT_IOC_RESET as _, 0) as i64 });
        retry_int(|| unsafe { libc::ioctl(f, PERF_EVENT_IOC_ENABLE as _, 0) as i64 });
        std::thread::sleep(std::time::Duration::from_secs(secs));
        retry_int(|| unsafe { libc::ioctl(f, PERF_EVENT_IOC_DISABLE as _, 0) as i64 });
        let mut c: u64 = 0;
        let n = retry_int(|| unsafe { libc::read(f, &mut c as *mut u64 as *mut libc::c_void, 8) as i64 });
        if n < 0 {
            eprintln!("read: {}", std::io::Error::last_os_error());
            std::process::exit(1);
        }
        println!(
            "uprobe-pmu {path}:{off:#x} pid={pid} over {secs}s: {c} hits ({:.1}/s)",
            c as f64 / secs as f64
        );
        return;
    }

    // `pmuall <path> <offset-hex> <pid> <secs>`: exactly what the daemon does -- one PMU
    // event per thread of the process, summed. Isolates the aggregate from the daemon's code.
    if a.first().map(|s| s.as_str()) == Some("pmuall") {
        let path = a.get(1).cloned().unwrap_or_default();
        let off = a
            .get(2)
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        let pid: i32 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
        let secs: u64 = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(3);
        let mut tids: Vec<i32> = std::fs::read_dir(format!("/proc/{pid}/task"))
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().to_str().and_then(|s| s.parse().ok()))
                    .collect()
            })
            .unwrap_or_default();
        tids.sort_unstable();
        let uprobe_type: u32 = std::fs::read_to_string("/sys/bus/event_source/devices/uprobe/type")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(6);
        let cpath = std::ffi::CString::new(path.clone()).expect("path");
        let mut fds: Vec<i32> = Vec::new();
        for t in &tids {
            let attr = PerfEventAttr {
                type_: uprobe_type,
                size: std::mem::size_of::<PerfEventAttr>() as u32,
                config: 0,
                config1: cpath.as_ptr() as u64,
                config2: off,
                sample_period: 0,
                flags: 1,
                ..Default::default()
            };
            let fd = retry_int(|| unsafe {
                libc::syscall(
                    libc::SYS_perf_event_open,
                    &attr as *const PerfEventAttr,
                    *t as libc::pid_t,
                    -1i32 as libc::c_int,
                    -1i32 as libc::c_int,
                    0u64,
                )
            });
            if fd >= 0 {
                fds.push(fd as i32);
            }
        }
        println!("pmuall: {} of {} thread events opened", fds.len(), tids.len());
        for fd in &fds {
            retry_int(|| unsafe { libc::ioctl(*fd, PERF_EVENT_IOC_RESET as _, 0) as i64 });
            retry_int(|| unsafe { libc::ioctl(*fd, PERF_EVENT_IOC_ENABLE as _, 0) as i64 });
        }
        std::thread::sleep(std::time::Duration::from_secs(secs));
        let mut total = 0u64;
        let mut per: Vec<(i32, u64)> = Vec::new();
        for (i, fd) in fds.iter().enumerate() {
            retry_int(|| unsafe { libc::ioctl(*fd, PERF_EVENT_IOC_DISABLE as _, 0) as i64 });
            let mut c: u64 = 0;
            let n = retry_int(|| unsafe { libc::read(*fd, &mut c as *mut u64 as *mut libc::c_void, 8) as i64 });
            if n >= 0 {
                total += c;
                if c > 0 {
                    per.push((tids[i], c));
                }
            }
        }
        println!("pmuall over {secs}s: {total} hits ({:.1}/s)", total as f64 / secs as f64);
        println!("threads with hits: {per:?}");
        return;
    }


    let id: u64 = a.first().and_then(|s| s.parse().ok()).unwrap_or_else(|| {
        eprintln!("usage: uprobe-count <tracepoint-id> <pid|0=all> <seconds> [inherit]");
        std::process::exit(2);
    });
    let pid: i32 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let secs: u64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
    let inherit = a.iter().any(|s| s == "inherit");
    let pid = if pid == 0 { -1 } else { pid };
    if pid != -1 && unsafe { libc::kill(pid, 0) } != 0 {
        eprintln!("pid {pid} is not alive / not visible");
        std::process::exit(2);
    }

    // pids for the events: per-task when pid > 0; otherwise one per online CPU.
    let mut plan: Vec<(i32, i32)> = Vec::new();
    if pid > 0 {
        plan.push((pid, -1));
    } else {
        let online = std::fs::read_to_string("/sys/devices/system/cpu/online").unwrap_or_default();
        let (lo, hi) = match online.trim().split_once('-') {
            Some((a, b)) => (a.parse::<i32>().unwrap_or(0), b.parse::<i32>().unwrap_or(0)),
            None => (0, 0),
        };
        for c in lo..=hi {
            plan.push((-1, c));
        }
    }

    let mut fds: Vec<std::fs::File> = Vec::new();
    let mut errs = 0;
    for (p, c) in &plan {
        match open_event(id, *p, *c, inherit, true) {
            Ok(fd) => fds.push(unsafe { std::fs::File::from_raw_fd(fd) }),
            Err(e) => {
                if errs == 0 {
                    eprintln!("perf_event_open(pid={p},cpu={c}): {e}");
                }
                errs += 1;
            }
        }
    }
    if fds.is_empty() {
        eprintln!("no perf events could be opened");
        std::process::exit(1);
    }
    use std::os::unix::io::AsRawFd;
    for f in &fds {
        let fd = f.as_raw_fd();
        retry_int(|| unsafe { libc::ioctl(fd, PERF_EVENT_IOC_RESET as _, 0) as i64 });
        retry_int(|| unsafe { libc::ioctl(fd, PERF_EVENT_IOC_ENABLE as _, 0) as i64 });
    }
    std::thread::sleep(std::time::Duration::from_secs(secs));
    let mut total: u64 = 0;
    for f in &fds {
        let fd = f.as_raw_fd();
        retry_int(|| unsafe { libc::ioctl(fd, PERF_EVENT_IOC_DISABLE as _, 0) as i64 });
        let mut c: u64 = 0;
        let n = retry_int(|| unsafe { libc::read(fd, &mut c as *mut u64 as *mut libc::c_void, 8) as i64 });
        if n >= 0 {
            total += c;
        }
    }
    println!(
        "perf count id={id} pid={} events={} inherit={inherit} over {secs}s: {total} hits ({:.1}/s)",
        if pid == -1 { "all-cpus".to_string() } else { pid.to_string() },
        fds.len(),
        total as f64 / secs as f64
    );
}
