//! The AppOpt FPS **primary** leg: a uprobe on the target app's `libgui.so`
//! `queueBuffer`, counted per thread, so only *that* app's frames are counted
//! (AGENT.md §11 queue item ⑪).
//!
//! The measurements behind this design are in `docs/m11-11-ebpf-uprobe-frame-leg.md`:
//!
//! * this kernel has `CONFIG_UPROBES`/`UPROBE_EVENTS` and the `uprobe` perf PMU (type 6),
//!   and no kprobes;
//! * a tracefs uprobe fires but its tracepoint **does not honour a task binding** (0 hits)
//!   and its per-CPU count is global — the contamination AppOpt exists to avoid. Attaching
//!   the uprobe through the **PMU** *does* honour a task binding;
//! * the calls happen on **RenderThread**, so the unit is the **thread**: one event per
//!   `/proc/<pid>/task/*` tid, summed, is the app's frame count. Measured
//!   `600 hits / 5 s = 120.0/s` on RenderThread, 0 on the main thread;
//! * `sample_period` must be 0 (non-zero makes it a sampling event and `read()` blocks),
//!   and every syscall must retry `EINTR`;
//! * Android system libraries are stripped, so the offset must be read out of `.dynsym`:
//!   tracefs rejects `file:symbol`.
//!
//! No BPF program is loaded. eBPF would only be needed to aggregate inside the kernel;
//! the PMU gives the same per-app count.

use std::ffi::c_void;
use std::io::Read as _;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The library the probes live in, and the candidate symbols in the order they are tried.
pub const LIBGUI: &str = "/system/lib64/libgui.so";
pub const CANDIDATE_SYMBOLS: &[&str] = &[
    // android::Surface::queueBuffer(sp<GraphicBuffer>&&, int, SurfaceQueueBufferOutput*)
    "_ZN7android7Surface11queueBufferEONS_2spINS_13GraphicBufferEEEiPNS_24SurfaceQueueBufferOutputE",
    // android::Surface::queueBufferInternal(ANativeWindow*, ANativeWindowBuffer*, int)
    "_ZN7android7Surface19queueBufferInternalEP13ANativeWindowP19ANativeWindowBufferi",
];

// ---------------------------------------------------------------- ELF (.dynsym)

fn u16at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// A symbol's value mapped to a **file offset**, which is what a uprobe wants.
///
/// `st_value` in a shared library is an address in the loadable image; the file offset is
/// `st_value - p_vaddr + p_offset` for the `PT_LOAD` that contains it. For most Android
/// libraries the first segments have `p_vaddr == p_offset`, but mapping through the program
/// headers is the honest way and costs nothing.
pub fn sym_file_offset(elf: &[u8], symbol: &str) -> Result<u64, String> {
    if elf.len() < 0x40 || &elf[0..4] != b"\x7fELF" {
        return Err("not an ELF".into());
    }
    if elf.get(4) != Some(&2) {
        return Err("not ELF64".into());
    }
    let e_phoff = u64at(elf, 0x20).ok_or("truncated header")? as usize;
    let e_phentsize = u16at(elf, 0x36).ok_or("truncated header")? as usize;
    let e_phnum = u16at(elf, 0x38).ok_or("truncated header")? as usize;
    let e_shoff = u64at(elf, 0x28).ok_or("truncated header")? as usize;
    let e_shentsize = u16at(elf, 0x3a).ok_or("truncated header")? as usize;
    let e_shnum = u16at(elf, 0x3c).ok_or("truncated header")? as usize;

    // the loadable segments, for the address -> offset mapping
    let mut loads: Vec<(u64, u64, u64)> = Vec::new(); // (vaddr, offset, filesz)
    for i in 0..e_phnum {
        let p = e_phoff + i * e_phentsize;
        if u32at(elf, p) == Some(1) {
            loads.push((
                u64at(elf, p + 16).ok_or("bad phdr")?,
                u64at(elf, p + 8).ok_or("bad phdr")?,
                u64at(elf, p + 32).ok_or("bad phdr")?,
            ));
        }
    }

    // SHT_DYNSYM = 11
    let mut found: Option<(usize, usize, usize, usize, u64)> = None; // (off, size, entsize, strtab_off, strtab_size)
    for i in 0..e_shnum {
        let sh = e_shoff + i * e_shentsize;
        if u32at(elf, sh + 4) != Some(11) {
            continue;
        }
        let off = u64at(elf, sh + 24).ok_or("bad shdr")? as usize;
        let size = u64at(elf, sh + 32).ok_or("bad shdr")? as usize;
        let entsize = u64at(elf, sh + 56).unwrap_or(24) as usize;
        let link = u32at(elf, sh + 40).ok_or("bad shdr")? as usize;
        let str_sh = e_shoff + link * e_shentsize;
        let str_off = u64at(elf, str_sh + 24).ok_or("bad shdr")? as usize;
        let str_size = u64at(elf, str_sh + 32).ok_or("bad shdr")? as usize;
        found = Some((off, size, entsize.max(24), str_off, str_size as u64));
        break;
    }
    let (off, size, entsize, str_off, str_size) = found.ok_or("no .dynsym")?;

    let mut n = 0usize;
    while n * entsize + 24 <= size {
        let s = off + n * entsize;
        let name_off = u32at(elf, s).ok_or("bad sym")? as usize;
        let value = u64at(elf, s + 8).ok_or("bad sym")?;
        let shndx = u16at(elf, s + 6).ok_or("bad sym")?;
        n += 1;
        if shndx == 0 {
            continue; // undefined (imported) symbols have no address to probe
        }
        let start = str_off + name_off;
        if (name_off as u64) >= str_size || start >= elf.len() {
            continue;
        }
        let rest = &elf[start..];
        let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
        if std::str::from_utf8(&rest[..end]).ok() == Some(symbol) {
            return map_addr(&loads, value);
        }
    }
    Err(format!("symbol not found: {symbol}"))
}

fn map_addr(loads: &[(u64, u64, u64)], value: u64) -> Result<u64, String> {
    for (vaddr, offset, filesz) in loads {
        if value >= *vaddr && value < vaddr + filesz {
            return Ok(value - vaddr + offset);
        }
    }
    Err(format!("address {value:#x} is not inside any PT_LOAD"))
}

/// Resolve the first candidate symbol present in `path`.
pub fn resolve_candidate(path: &Path) -> Result<(String, u64), String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut last = String::from("no candidate symbol found");
    for sym in CANDIDATE_SYMBOLS {
        match sym_file_offset(&buf, sym) {
            Ok(off) => return Ok(((*sym).to_string(), off)),
            Err(e) => last = e,
        }
    }
    Err(last)
}

// ---------------------------------------------------------------- perf (uprobe PMU)

// `struct perf_event_attr`, only what this leg sets.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct PerfEventAttr {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
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

const fn ioc(dir: u32, ty: u32, nr: u32) -> u64 {
    ((dir << 30) | (ty << 8) | nr) as u64
}
const PERF_EVENT_IOC_ENABLE: u64 = ioc(0, b'$' as u32, 0);
const PERF_EVENT_IOC_DISABLE: u64 = ioc(0, b'$' as u32, 1);
const PERF_EVENT_IOC_RESET: u64 = ioc(0, b'$' as u32, 3);

/// Retry on `EINTR`: any backgrounded child's SIGCHLD lands mid-syscall and would
/// otherwise read as a refusal (measured on device).
fn retry_int(mut f: impl FnMut() -> i64) -> i64 {
    loop {
        let r = f();
        if r != -1 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return r;
        }
    }
}

fn uprobe_pmu_type() -> Option<u32> {
    std::fs::read_to_string("/sys/bus/event_source/devices/uprobe/type")
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// One uprobe perf event bound to one thread.
struct ThreadEvent {
    file: std::fs::File,
}

impl ThreadEvent {
    fn open(path: &str, offset: u64, tid: u32) -> Result<ThreadEvent, String> {
        let ty = uprobe_pmu_type().ok_or("no uprobe PMU")?;
        let cpath = std::ffi::CString::new(path).map_err(|_| "bad path".to_string())?;
        let attr = PerfEventAttr {
            type_: ty,
            size: std::mem::size_of::<PerfEventAttr>() as u32,
            config: 0,
            // the kernel reads this string during the call, so cpath stays alive
            config1: cpath.as_ptr() as u64,
            config2: offset,
            // 0: a COUNTING event. Non-zero makes the kernel treat it as sampling and
            // read() waits for samples instead of returning a count (measured).
            sample_period: 0,
            flags: 1, // start disabled; enabled for the window
            ..Default::default()
        };
        let fd = retry_int(|| unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &attr as *const PerfEventAttr,
                tid as libc::pid_t,
                -1i32 as libc::c_int,
                -1i32 as libc::c_int,
                0u64,
            )
        });
        if fd < 0 {
            return Err(format!("perf_event_open(tid={tid}): {}", std::io::Error::last_os_error()));
        }
        Ok(ThreadEvent { file: unsafe { std::fs::File::from_raw_fd(fd as i32) } })
    }

    fn ctl(&self, req: u64) {
        retry_int(|| unsafe { libc::ioctl(self.file.as_raw_fd(), req as _, 0) as i64 });
    }

    fn count(&self) -> u64 {
        let mut c: u64 = 0;
        let n = retry_int(|| unsafe {
            libc::read(self.file.as_raw_fd(), &mut c as *mut u64 as *mut c_void, 8) as i64
        });
        if n < 0 {
            0
        } else {
            c
        }
    }
}

/// Every thread of `pid` running one probe: the app's frame counter.
pub struct FrameCounter {
    events: Vec<ThreadEvent>,
    /// Threads enumerated from `/proc/<pid>/task`.
    pub enumerated: usize,
    /// The thread the frames actually come from, when it was seen (`RenderThread`).
    pub render_tid: Option<u32>,
}

impl FrameCounter {
    /// Open one probe per thread of `pid` for `path:offset`.
    pub fn open(path: &str, offset: u64, pid: u32) -> Result<FrameCounter, String> {
        let tids = thread_ids(pid);
        if tids.is_empty() {
            return Err(format!("no threads visible for pid {pid}"));
        }
        let enumerated = tids.len();
        let mut events = Vec::with_capacity(enumerated);
        let mut err = String::new();
        let mut render_tid = None;
        for t in &tids {
            if render_tid.is_none() && thread_name(*t) == "RenderThread" {
                render_tid = Some(*t);
            }
            match ThreadEvent::open(path, offset, *t) {
                Ok(e) => events.push(e),
                Err(e) => err = e, // a thread may exit between listing and opening
            }
        }
        if events.is_empty() {
            return Err(if err.is_empty() { "no thread events".into() } else { err });
        }
        Ok(FrameCounter { events, enumerated, render_tid })
    }

    pub fn reset_and_enable(&self) {
        for e in &self.events {
            e.ctl(PERF_EVENT_IOC_RESET);
            e.ctl(PERF_EVENT_IOC_ENABLE);
        }
    }

    pub fn disable_and_sum(&self) -> u64 {
        let mut total = 0;
        for e in &self.events {
            e.ctl(PERF_EVENT_IOC_DISABLE);
            total += e.count();
        }
        total
    }
}

/// The tids of a process, from `/proc/<pid>/task`.
pub fn thread_ids(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return out;
    };
    for e in rd.flatten() {
        if let Some(t) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) {
            out.push(t);
        }
    }
    out.sort_unstable();
    out
}

/// A thread's comm, for diagnostics.
pub fn thread_name(tid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{tid}/comm"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "?".into())
}

/// Pids whose `cmdline` is the package (or one of its `:remote` processes).
pub fn pids_for_package(pkg: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(cmd) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let first = cmd.split(|&b| b == 0).next().unwrap_or(&[]);
        let Ok(argv0) = std::str::from_utf8(first) else { continue };
        if argv0 == pkg || argv0.starts_with(&format!("{pkg}:")) {
            out.push(pid);
        }
    }
    out.sort_unstable();
    out
}

// ---------------------------------------------------------------- shared state

/// The last uprobe-measured fps, ×100 (0 = the leg is not running or healthy).
static UPROBE_FPS_X100: AtomicU64 = AtomicU64::new(0);
/// Frames counted in the last window (for the state file's `frames=` field).
static UPROBE_FRAMES: AtomicU64 = AtomicU64::new(0);
/// Set while the uprobe leg has a healthy probe, so the binder `--latency` leg stands down.
static UPROBE_HEALTHY: AtomicBool = AtomicBool::new(false);

pub fn last_fps() -> f64 {
    UPROBE_FPS_X100.load(Ordering::Relaxed) as f64 / 100.0
}

pub fn last_frames() -> u64 {
    UPROBE_FRAMES.load(Ordering::Relaxed)
}

pub fn healthy() -> bool {
    UPROBE_HEALTHY.load(Ordering::Relaxed)
}

fn set_state(healthy: bool, fps: f64, frames: u64) {
    UPROBE_HEALTHY.store(healthy, Ordering::Relaxed);
    UPROBE_FPS_X100.store((fps.max(0.0) * 100.0) as u64, Ordering::Relaxed);
    UPROBE_FRAMES.store(frames, Ordering::Relaxed);
}

// ---------------------------------------------------------------- daemon task

/// Opt-in (`UPERF_SF_UPROBE=1`): the primary frame leg.
pub struct UprobeTask {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl UprobeTask {
    pub fn enabled() -> bool {
        matches!(
            std::env::var("UPERF_SF_UPROBE").ok().as_deref(),
            Some("1") | Some("true")
        )
    }

    pub fn spawn<F, L>(top_app: F, log: L) -> Option<UprobeTask>
    where
        F: Fn() -> Option<String> + Send + 'static,
        L: Fn(&str) + Send + 'static,
    {
        if !Self::enabled() {
            return None;
        }
        let lib = std::env::var("UPERF_SF_UPROBE_LIB").unwrap_or_else(|_| LIBGUI.to_string());
        // `UPERF_SF_UPROBE_PID` pins the target process (also lets the leg be tested
        // without the foreground reader); otherwise it follows the top app each tick.
        let pinned_pid: Option<u32> = std::env::var("UPERF_SF_UPROBE_PID")
            .ok()
            .and_then(|s| s.parse().ok());
        let window_ms = std::env::var("UPERF_SF_UPROBE_WINDOW_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1000);

        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::Builder::new()
            .name("uperf-sf-uprobe".into())
            .spawn(move || {
                let path = PathBuf::from(&lib);
                let (sym, off) = match resolve_candidate(&path) {
                    Ok(v) => v,
                    Err(e) => {
                        log(&format!("Rust: sf-uprobe unavailable ({e}); the --latency leg owns it"));
                        set_state(false, 0.0, 0);
                        return;
                    }
                };
                log(&format!(
                    "Rust: sf-uprobe {lib}:{off:#x} ({}), per-thread, {window_ms} ms window",
                    sym.rsplit("11queueBuffer").next().unwrap_or("")
                ));

                let mut open_for: Option<(String, u32)> = None;
                let mut counter: Option<FrameCounter> = None;
                let mut zero_windows = 0u32;
                while !stop_thread.load(Ordering::SeqCst) {
                    let pkg = top_app();
                    let target = match pinned_pid {
                        Some(pid) => Some(("pinned".to_string(), pid)),
                        None => pkg.as_deref().and_then(|p| {
                            pids_for_package(p).into_iter().next().map(|pid| (p.to_string(), pid))
                        }),
                    };
                    match (&counter, &target) {
                        (Some(_), Some((p, pid))) if open_for.as_ref() == Some(&(p.clone(), *pid)) => {}
                        _ => {
                            counter = None;
                            open_for = None;
                            match &target {
                                Some((p, pid)) => match FrameCounter::open(&lib, off, *pid) {
                                    Ok(c) => {
                                        log(&format!(
                                            "Rust: sf-uprobe attached {p} pid={pid} events={} of {} threads, RenderThread={:?}",
                                            c.events.len(),
                                            c.enumerated,
                                            c.render_tid
                                        ));
                                        // RESET starts the window: what a later
                                        // `disable_and_sum` returns is that window's count.
                                        c.reset_and_enable();
                                        counter = Some(c);
                                        open_for = Some((p.clone(), *pid));
                                    }
                                    Err(e) => {
                                        log(&format!("Rust: sf-uprobe attach failed: {e}"));
                                        set_state(false, 0.0, 0);
                                    }
                                },
                                None => set_state(false, 0.0, 0),
                            }
                        }
                    }

                    std::thread::sleep(Duration::from_millis(window_ms));
                    let window = counter.as_ref().map(|c| {
                        // `disable_and_sum` returns the count since the last RESET, i.e.
                        // exactly this window's frames.
                        let frames = c.disable_and_sum();
                        c.reset_and_enable(); // zero the counters for the next window
                        (frames, c.events.len(), open_for.clone().unwrap_or_default())
                    });
                    if let Some((frames, tids, (p, pid))) = window {
                        let fps = frames as f64 * 1000.0 / window_ms as f64;
                        set_state(true, fps, frames);
                        log(&format!(
                            "Rust: sf-uprobe {p} pid={pid} events={tids} frames={frames} fps={fps:.1}"
                        ));
                        // A process's RenderThread — and its other worker threads — can be
                        // created *after* we attached, and a probe opened for a tid that has
                        // since exited is dead. After a few silent windows, re-enumerate.
                        if frames == 0 {
                            zero_windows += 1;
                        } else {
                            zero_windows = 0;
                        }
                        if zero_windows >= 3 {
                            log(&format!(
                                "Rust: sf-uprobe {p} silent for 3 windows; re-enumerating threads"
                            ));
                            counter = None;
                            open_for = None;
                            zero_windows = 0;
                        }
                    }
                }
                set_state(false, 0.0, 0);
                log("Rust: sf-uprobe stopped");
            })
            .ok()?;
        Some(UprobeTask { stop, handle: Some(handle) })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Timestamp helper kept local so the module does not depend on the frame-source one.
#[allow(dead_code)]
fn now_ms() -> u64 {
    Instant::now().elapsed().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal ELF64 with one PT_LOAD and a `.dynsym` holding `names`.
    /// `vaddr_delta` lets a test make st_value != file offset on purpose.
    fn synthetic_elf(names: &[&str], vaddr_delta: u64) -> Vec<u8> {
        let mut strtab: Vec<u8> = vec![0];
        let mut name_offs = Vec::new();
        for n in names {
            name_offs.push(strtab.len() as u32);
            strtab.extend_from_slice(n.as_bytes());
            strtab.push(0);
        }
        // layout: [ehdr 64][phdr 56][strtab][dynsym][shdrs]
        let ehsize = 64usize;
        let phsize = 56usize;
        let str_off = ehsize + phsize;
        let sym_off = str_off + strtab.len();
        let sym_entsize = 24usize;
        let sym_size = sym_entsize * (names.len() + 1);
        let sh_off = sym_off + sym_size;
        let sh_entsize = 64usize;
        let sh_num = 3usize; // null, .dynsym, .strtab
        let total = sh_off + sh_entsize * sh_num;
        let mut b = vec![0u8; total];
        b[0..4].copy_from_slice(b"\x7fELF");
        b[4] = 2; // ELF64
        b[5] = 1; // little endian
        b[6] = 1;
        b[0x10..0x12].copy_from_slice(&2u16.to_le_bytes()); // e_type ET_EXEC
        b[0x12..0x14].copy_from_slice(&0xb7u16.to_le_bytes()); // EM_AARCH64
        b[0x20..0x28].copy_from_slice(&(ehsize as u64).to_le_bytes()); // e_phoff
        b[0x28..0x30].copy_from_slice(&(sh_off as u64).to_le_bytes()); // e_shoff
        b[0x36..0x38].copy_from_slice(&(phsize as u16).to_le_bytes());
        b[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        b[0x3a..0x3c].copy_from_slice(&(sh_entsize as u16).to_le_bytes());
        b[0x3c..0x3e].copy_from_slice(&(sh_num as u16).to_le_bytes());
        b[0x3e..0x40].copy_from_slice(&2u16.to_le_bytes()); // e_shstrndx
        // phdr[0]: PT_LOAD covering everything, vaddr = off + delta
        let p = ehsize;
        b[p..p + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        b[p + 8..p + 16].copy_from_slice(&0u64.to_le_bytes()); // p_offset
        b[p + 16..p + 24].copy_from_slice(&vaddr_delta.to_le_bytes()); // p_vaddr
        b[p + 32..p + 40].copy_from_slice(&0x2000u64.to_le_bytes()); // p_filesz: must span the
                                                                   // st_values the symbols use
        b[str_off..str_off + strtab.len()].copy_from_slice(&strtab);
        // dynsym: index 0 is the null symbol, then one per name
        for (i, no) in name_offs.iter().enumerate() {
            let s = sym_off + sym_entsize * (i + 1);
            b[s..s + 4].copy_from_slice(&no.to_le_bytes());
            b[s + 4] = 0x12; // GLOBAL FUNC
            b[s + 6..s + 8].copy_from_slice(&1u16.to_le_bytes()); // st_shndx: defined
            // st_value: an address inside the PT_LOAD
            let value = 0x1000u64 + i as u64 * 0x40 + vaddr_delta;
            b[s + 8..s + 16].copy_from_slice(&value.to_le_bytes());
            b[s + 16..s + 24].copy_from_slice(&16u64.to_le_bytes());
        }
        // section headers: [0] null, [1] .dynsym (link=2), [2] .strtab
        let d = sh_off + sh_entsize;
        b[d + 4..d + 8].copy_from_slice(&11u32.to_le_bytes()); // SHT_DYNSYM
        b[d + 24..d + 32].copy_from_slice(&(sym_off as u64).to_le_bytes());
        b[d + 32..d + 40].copy_from_slice(&(sym_size as u64).to_le_bytes());
        b[d + 40..d + 44].copy_from_slice(&2u32.to_le_bytes()); // sh_link -> .strtab
        b[d + 56..d + 64].copy_from_slice(&(sym_entsize as u64).to_le_bytes());
        let t = sh_off + sh_entsize * 2;
        b[t + 4..t + 8].copy_from_slice(&3u32.to_le_bytes()); // SHT_STRTAB
        b[t + 24..t + 32].copy_from_slice(&(str_off as u64).to_le_bytes());
        b[t + 32..t + 40].copy_from_slice(&(strtab.len() as u64).to_le_bytes());
        b
    }

    #[test]
    fn finds_a_dynsym_symbol_and_maps_it_to_a_file_offset() {
        let sym = "_ZN7android7Surface11queueBufferEONS_2spINS_13GraphicBufferEEEiPNS_24SurfaceQueueBufferOutputE";
        // vaddr == file offset (the common Android case)
        let elf = synthetic_elf(&[sym, "_ZN7android7Surface19queueBufferInternalEP13ANativeWindowP19ANativeWindowBufferi"], 0);
        assert_eq!(sym_file_offset(&elf, sym).unwrap(), 0x1000);
        assert_eq!(
            sym_file_offset(&elf, CANDIDATE_SYMBOLS[1]).unwrap(),
            0x1040,
        );
    }

    #[test]
    fn the_address_is_mapped_through_the_program_headers() {
        // when the segment's vaddr is offset from its file offset, the probe needs the
        // file offset, not st_value
        let sym = "some_symbol";
        let elf = synthetic_elf(&[sym], 0x8000);
        assert_eq!(sym_file_offset(&elf, sym).unwrap(), 0x1000);
    }

    #[test]
    fn a_missing_symbol_or_a_non_elf_is_an_error() {
        let elf = synthetic_elf(&["a"], 0);
        assert!(sym_file_offset(&elf, "nope").is_err());
        assert!(sym_file_offset(b"not an elf at all", "a").is_err());
        assert!(sym_file_offset(&[], "a").is_err());
        // truncated but with a valid magic must not panic
        assert!(sym_file_offset(b"\x7fELF\x02\x01\x01", "a").is_err());
    }

    #[test]
    fn resolve_candidate_returns_the_first_present_symbol() {
        let dir = std::env::temp_dir().join(format!("uperf_uprobe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("lib.so");
        std::fs::write(&p, synthetic_elf(&[CANDIDATE_SYMBOLS[1]], 0)).unwrap();
        let (sym, off) = resolve_candidate(&p).unwrap();
        assert_eq!(sym, CANDIDATE_SYMBOLS[1]);
        assert_eq!(off, 0x1000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thread_ids_of_this_process_include_us() {
        let tids = thread_ids(std::process::id());
        assert!(!tids.is_empty(), "at least the calling thread");
        assert!(tids.contains(&std::process::id()));
    }

    #[test]
    fn pids_for_a_package_that_cannot_exist_is_empty() {
        assert!(pids_for_package("com.example.definitely.not.running").is_empty());
    }
}
