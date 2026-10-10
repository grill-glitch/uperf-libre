//! Direct-binder SurfaceFlinger frame source (AGENT.md §11 queue item ⑤).
//!
//! The M8 frame source is the injected `libsfanalysis_rs.so` writing
//! `sfanalysis.hint`. This is the degraded leg: when that library cannot be
//! injected, the daemon asks SurfaceFlinger for the frame timestamps itself, over
//! binder, from inside the process — no `dumpsys` fork (measured ~20 ms each) and
//! no Java.
//!
//! The transport is the one proven by `tools/binder-probe/` (see its README for the
//! nine device-verified traps). In short:
//!
//! * target the **legacy** `SurfaceFlinger` (the AIDL `SurfaceFlingerAIDL` does not
//!   fall back to `BBinder::onTransact`, so `dump` goes nowhere);
//! * `code = DUMP_TRANSACTION` (0x5f444d50, a base-`IBinder` code);
//! * parcel = `[fd object][String16[] args]` — fd first, no interface token;
//! * `dump` sends no reply, so it is sent write-only and the pipe is read on a thread.
//!
//! Everything above the transport is pure and host-tested: the `--latency` table
//! parse, the FPS window, and the layer-name pick.

use crate::hint::SfHint;
use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------- _IOC macro
const IOC_NRBITS: u32 = 8;
const IOC_TYPEBITS: u32 = 8;
const IOC_SIZEBITS: u32 = 14;
const IOC_TYPESHIFT: u32 = IOC_NRBITS;
const IOC_NRSHIFT: u32 = 0;
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> u64 {
    ((dir << IOC_DIRSHIFT) | (size << IOC_SIZESHIFT) | (ty << IOC_TYPESHIFT) | (nr << IOC_NRSHIFT)) as u64
}
const fn io(ty: u32, nr: u32) -> u64 {
    ioc(IOC_NONE, ty, nr, 0)
}
const fn ior(ty: u32, nr: u32, size: u32) -> u64 {
    ioc(IOC_READ, ty, nr, size)
}
const fn iow(ty: u32, nr: u32, size: u32) -> u64 {
    ioc(IOC_WRITE, ty, nr, size)
}
const fn iowr(ty: u32, nr: u32, size: u32) -> u64 {
    ioc(IOC_READ | IOC_WRITE, ty, nr, size)
}

const BINDER_WRITE_READ: u64 = iowr(b'b' as u32, 1, 48);
const BINDER_VERSION: u64 = iowr(b'b' as u32, 9, 4);
const BC_TRANSACTION: u64 = iow(b'c' as u32, 0, 64);
const BC_FREE_BUFFER: u64 = iow(b'c' as u32, 3, 8);
const BC_ACQUIRE: u64 = iow(b'c' as u32, 5, 4);

const BR_ERROR: u64 = ior(b'r' as u32, 0, 4);
const BR_TRANSACTION: u64 = ior(b'r' as u32, 2, 64);
const BR_REPLY: u64 = ior(b'r' as u32, 3, 64);
const BR_DEAD_REPLY: u64 = io(b'r' as u32, 5);
const BR_TRANSACTION_COMPLETE: u64 = io(b'r' as u32, 6);
const BR_INCREFS: u64 = ior(b'r' as u32, 7, 16);
const BR_ACQUIRE: u64 = ior(b'r' as u32, 8, 16);
const BR_RELEASE: u64 = ior(b'r' as u32, 9, 16);
const BR_DECREFS: u64 = ior(b'r' as u32, 10, 16);
const BR_NOOP: u64 = io(b'r' as u32, 12);
const BR_SPAWN_LOOPER: u64 = io(b'r' as u32, 13);
const BR_DEAD_BINDER: u64 = ior(b'r' as u32, 15, 8);
const BR_CLEAR_DEATH_NOTIFICATION_DONE: u64 = ior(b'r' as u32, 16, 8);
const BR_FAILED_REPLY: u64 = io(b'r' as u32, 17);

const TXN_SIZE: usize = 64;
const K_HEADER_BINDER: u32 = 0x5359_5354;
const BINDER_TYPE_HANDLE: u32 = 0x7368_2a85;
const BINDER_TYPE_FD: u32 = 0x6664_2a85;
const DUMP_TRANSACTION: u32 = 0x5f44_4d50;
const READ_AREA: usize = 64 * 1024;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct BinderWriteRead {
    write_size: u64,
    write_consumed: u64,
    write_buffer: u64,
    read_size: u64,
    read_consumed: u64,
    read_buffer: u64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct BinderTransactionData {
    handle: u32,
    _pad: u32,
    cookie: u64,
    code: u32,
    flags: u32,
    sender_pid: i32,
    sender_euid: u32,
    data_size: u64,
    offsets_size: u64,
    data_buffer: u64,
    data_offsets: u64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct FlatBinderObject {
    kind: u32,
    flags: u32,
    binder: u64,
    cookie: u64,
}

/// Reinterpret a `#[repr(C)]` plain-old-data struct as its bytes.
///
/// Used only with `BinderTransactionData` and `FlatBinderObject`: both are
/// `#[repr(C)]` structs of 32/64-bit integers whose field layout leaves no interior
/// padding (so all `size_of::<T>()` bytes are initialised) and both derive
/// `Default`. That is the whole precondition, and it is pinned by the
/// `size_of` assertions below — this is a private helper with two fixed types, not
/// a general `bytemuck`-style cast.
fn as_bytes<T: Sized>(t: &T) -> &[u8] {
    // SAFETY: `t` is a live shared reference, so its address is valid for
    // `size_of::<T>()` readable bytes, and the returned slice borrows from `t`
    // (same lifetime), so it cannot outlive the value it views.
    unsafe { std::slice::from_raw_parts((t as *const T) as *const u8, std::mem::size_of::<T>()) }
}

// The invariants `as_bytes`, `parse` and `get_service` rest on. If a field is ever
// added or reordered, the byte view would silently no longer match what the kernel
// writes into the command stream (and what `parse` advances past) — fail here
// instead.
const _: () = assert!(std::mem::size_of::<BinderTransactionData>() == TXN_SIZE);
const _: () = assert!(std::mem::size_of::<FlatBinderObject>() == 24);
const _: () = assert!(std::mem::size_of::<BinderWriteRead>() == 48);
fn push_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_ne_bytes());
}
fn push_string16(v: &mut Vec<u8>, s: &str) {
    let u: Vec<u16> = s.encode_utf16().collect();
    push_u32(v, u.len() as u32);
    for c in &u {
        v.extend_from_slice(&c.to_ne_bytes());
    }
    v.extend_from_slice(&0u16.to_ne_bytes());
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

// ---------------------------------------------------------------- transport

/// A connection to `/dev/binder`. `Drop` closes it.
struct Binder {
    fd: i32,
    map: *mut u8,
    map_len: usize,
    /// Command-stream buffer: a writable **heap** buffer, never the mmap (the mmap
    /// is `PROT_READ`; using it as the read buffer is a guaranteed EFAULT).
    rd: Vec<u8>,
}

impl Drop for Binder {
    fn drop(&mut self) {
        // SAFETY: both handles were created by this struct (`open`/`mmap`) and are
        // owned here; nothing else holds or uses them after `drop`.
        unsafe {
            libc::munmap(self.map as *mut c_void, self.map_len);
            libc::close(self.fd);
        }
    }
}

impl Binder {
    fn open(path: &str) -> Result<Binder, String> {
        let cpath = std::ffi::CString::new(path).map_err(|_| "bad path".to_string())?;
        // SAFETY: `cpath` is a NUL-terminated path that outlives the call; the
        // result is a fresh fd we own (negative is handled below).
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(format!("open {path}: {}", std::io::Error::last_os_error()));
        }
        let size = 256 * 1024;
        // SAFETY: a read-only private mapping of the binder fd, which is what the
        // protocol requires (binder data is read through the mapping); MAP_FAILED is
        // checked and the fd closed on failure.
        let map = unsafe { libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ, libc::MAP_PRIVATE, fd, 0) };
        if map == libc::MAP_FAILED {
            let e = std::io::Error::last_os_error();
            // SAFETY: `fd` was opened above and is not stored anywhere yet.
            unsafe { libc::close(fd) };
            return Err(format!("mmap: {e}"));
        }
        Ok(Binder { fd, map: map as *mut u8, map_len: size, rd: vec![0u8; READ_AREA] })
    }

    fn bwr(&mut self, write: &[u8], read: bool) -> Result<u64, String> {
        let mut bwr = BinderWriteRead {
            write_size: write.len() as u64,
            write_buffer: if write.is_empty() { 0 } else { write.as_ptr() as u64 },
            read_size: if read { self.rd.len() as u64 } else { 0 },
            read_buffer: if read { self.rd.as_mut_ptr() as u64 } else { 0 },
            ..Default::default()
        };
        // SAFETY: `bwr` is a live, correctly-sized `binder_write_read` whose
        // `write_buffer`/`read_buffer` point into the caller's `write` slice and
        // `self.rd` (both alive for the call) with matching sizes; the kernel reads
        // and writes exactly those ranges. `self.fd` is a live binder fd.
        let rc = unsafe { libc::ioctl(self.fd, BINDER_WRITE_READ as _, &mut bwr as *mut _) };
        if rc < 0 {
            return Err(format!("{}", std::io::Error::last_os_error()));
        }
        Ok(bwr.read_consumed)
    }

    fn transact(&mut self, handle: u32, code: u32, data: &[u8], offsets: &[u64]) -> Result<Vec<u8>, String> {
        let mut wb: Vec<u8> = Vec::new();
        push_u32(&mut wb, BC_TRANSACTION as u32);
        let txn = BinderTransactionData {
            handle,
            code,
            data_size: data.len() as u64,
            data_buffer: data.as_ptr() as u64,
            offsets_size: (offsets.len() * 8) as u64,
            data_offsets: if offsets.is_empty() { 0 } else { offsets.as_ptr() as u64 },
            ..Default::default()
        };
        wb.extend_from_slice(as_bytes(&txn));

        let mut n = self.bwr(&wb, true)?;
        for _ in 0..4 {
            let buf: Vec<u8> = self.rd[..n as usize].to_vec();
            if let Some(r) = self.parse(&buf)? {
                return Ok(r);
            }
            n = self.bwr(&[], true)?;
        }
        Err("no reply after 4 reads".into())
    }

    fn parse(&mut self, buf: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let mut off = 0usize;
        let mut reply: Option<Vec<u8>> = None;
        while off + 4 <= buf.len() {
            let code = u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap()) as u64;
            off += 4;
            if code == BR_NOOP || code == BR_TRANSACTION_COMPLETE || code == BR_SPAWN_LOOPER {
                continue;
            }
            if code == BR_REPLY || code == BR_TRANSACTION {
                if off + TXN_SIZE > buf.len() {
                    return Err("short transaction in read buffer".into());
                }
                // SAFETY: `off + TXN_SIZE <= buf.len()` was just checked, and
                // `read_unaligned` does not require alignment (the kernel packs the
                // command stream at 4-byte granularity).
                let t: BinderTransactionData =
                    unsafe { std::ptr::read_unaligned(buf[off..].as_ptr() as *const BinderTransactionData) };
                off += TXN_SIZE;
                if t.data_size > 0 && t.data_buffer != 0 {
                    // SAFETY: `data_buffer` was written by the kernel as an address
                    // inside this process' binder mapping (`Binder::open` maps 256 KiB
                    // read-only, and the kernel never reports a transaction buffer
                    // outside it) with `data_size` valid bytes; non-null is checked
                    // just above. The copy is taken immediately, so the slice does not
                    // outlive the kernel's ownership of the buffer (which is released
                    // by BC_FREE_BUFFER below).
                    let data = unsafe {
                        std::slice::from_raw_parts(t.data_buffer as *const u8, t.data_size as usize).to_vec()
                    };
                    // A handle in a reply is only held until the buffer is freed; acquire
                    // it or every later transaction to it is "invalid handle".
                    if t.offsets_size > 0 && t.data_offsets != 0 {
                        let nn = (t.offsets_size / 8) as usize;
                        // SAFETY: same kernel contract as `data_buffer` — the offsets
                        // array lives in the same mapped buffer, is `offsets_size` bytes
                        // (a multiple of 8), and the kernel filled it during the ioctl
                        // that just returned.
                        let offs = unsafe { std::slice::from_raw_parts(t.data_offsets as *const u64, nn) };
                        for &o in offs {
                            let o = o as usize;
                            if o + 24 <= data.len() {
                                // SAFETY: the offset's object was copied into `data`
                                // above, and `o + 24 <= data.len()` keeps this read
                                // inside it; `FlatBinderObject` is 24 bytes (asserted
                                // above).
                                let obj: FlatBinderObject = unsafe {
                                    std::ptr::read_unaligned(data[o..].as_ptr() as *const FlatBinderObject)
                                };
                                if obj.kind == BINDER_TYPE_HANDLE {
                                    let mut ab: Vec<u8> = Vec::new();
                                    push_u32(&mut ab, BC_ACQUIRE as u32);
                                    ab.extend_from_slice(&(obj.binder as u32).to_ne_bytes());
                                    let _ = self.bwr(&ab, false);
                                }
                            }
                        }
                    }
                    if code == BR_REPLY {
                        reply = Some(data);
                    }
                }
                if t.data_buffer != 0 {
                    let mut fw: Vec<u8> = Vec::new();
                    push_u32(&mut fw, BC_FREE_BUFFER as u32);
                    fw.extend_from_slice(&t.data_buffer.to_ne_bytes());
                    let _ = self.bwr(&fw, false);
                }
                continue;
            }
            if code == BR_DEAD_REPLY {
                return Err("BR_DEAD_REPLY".into());
            }
            if code == BR_FAILED_REPLY {
                return Err("BR_FAILED_REPLY".into());
            }
            if code == BR_INCREFS || code == BR_ACQUIRE || code == BR_RELEASE || code == BR_DECREFS {
                off += 16;
                continue;
            }
            if code == BR_DEAD_BINDER || code == BR_CLEAR_DEATH_NOTIFICATION_DONE {
                off += 8;
                continue;
            }
            if code == BR_ERROR {
                let e = if off + 4 <= buf.len() {
                    u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap())
                } else {
                    0
                };
                return Err(format!("BR_ERROR {e}"));
            }
            return Err(format!("unexpected read code 0x{code:08x}"));
        }
        Ok(reply)
    }

    fn get_service(&mut self, name: &str) -> Result<u32, String> {
        let mut data = Vec::new();
        // writeInterfaceToken preamble (strict, workSource, kHeader) + descriptor
        push_u32(&mut data, 0);
        push_u32(&mut data, 0);
        push_u32(&mut data, K_HEADER_BINDER);
        push_string16(&mut data, "android.os.IServiceManager");
        push_string16(&mut data, name);
        let reply = self.transact(0, 1, &data, &[])?;
        if reply.len() < 28 {
            return Err(format!("short reply ({} bytes)", reply.len()));
        }
        // SAFETY: `reply` is at least 28 bytes (checked above), the object is 24
        // bytes (asserted), and `read_unaligned` skips the alignment requirement; the
        // read stays inside `reply`.
        let obj: FlatBinderObject =
            unsafe { std::ptr::read_unaligned(reply[4..].as_ptr() as *const FlatBinderObject) };
        if obj.kind != BINDER_TYPE_HANDLE {
            return Err(format!("no handle for '{name}' (kind=0x{:08x})", obj.kind));
        }
        Ok(obj.binder as u32)
    }

    /// `dump(handle, args)`: parcel = [fd object][String16[] args], no interface token.
    /// Write-only (SF sends no reply), pipe read concurrently, ended by an idle timeout.
    fn dump(&mut self, handle: u32, args: &[&str]) -> Result<String, String> {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a live 2-element array, which is what `pipe(2)` writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(format!("pipe: {}", std::io::Error::last_os_error()));
        }
        let (rfd, wfd) = (fds[0], fds[1]);
        let reader = std::thread::spawn(move || {
            let mut out: Vec<u8> = Vec::new();
            let mut buf = [0u8; 65536];
            loop {
                let mut pfd = libc::pollfd { fd: rfd, events: libc::POLLIN, revents: 0 };
                // SAFETY: one valid `pollfd`, count 1.
                let pr = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, 1000) };
                if pr > 0 && (pfd.revents & libc::POLLIN) != 0 {
                    // SAFETY: `buf` is a live 65536-byte array and the length passed
                    // is its own; `poll` above said this fd is readable.
                    let n = unsafe { libc::read(rfd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
                    if n <= 0 {
                        break;
                    }
                    out.extend_from_slice(&buf[..n as usize]);
                } else {
                    break; // 1 s of silence (or EOF) ends the dump
                }
            }
            // SAFETY: this thread owns the read end after the split above; the write
            // end is closed by the sender, so the reader never needs it.
            unsafe { libc::close(rfd) };
            out
        });

        let mut data = Vec::new();
        while data.len() % 8 != 0 {
            data.push(0);
        }
        let offsets = vec![data.len() as u64];
        let obj = FlatBinderObject { kind: BINDER_TYPE_FD, flags: 0, binder: wfd as u64, cookie: 0 };
        data.extend_from_slice(as_bytes(&obj));
        push_u32(&mut data, args.len() as u32);
        for a in args {
            push_string16(&mut data, a);
        }

        let mut wb: Vec<u8> = Vec::new();
        push_u32(&mut wb, BC_TRANSACTION as u32);
        let txn = BinderTransactionData {
            handle,
            code: DUMP_TRANSACTION,
            data_size: data.len() as u64,
            data_buffer: data.as_ptr() as u64,
            offsets_size: (offsets.len() * 8) as u64,
            data_offsets: offsets.as_ptr() as u64,
            ..Default::default()
        };
        wb.extend_from_slice(as_bytes(&txn));
        let sent = self.bwr(&wb, false);
        // SAFETY: the write end is owned by this thread (the reader owns the read
        // end); closing it makes the reader see EOF after the peer's last write.
        unsafe { libc::close(wfd) };
        let out = reader.join().map_err(|_| "reader panicked".to_string())?;
        sent?;
        Ok(String::from_utf8_lossy(&out).to_string())
    }
}

/// One SF query: connect (or reuse), ask, return the text.
pub struct SfClient {
    binder: Binder,
    sf_handle: u32,
}

impl SfClient {
    pub fn connect() -> Result<SfClient, String> {
        let mut binder = Binder::open("/dev/binder")?;
        let mut ver: i32 = 0;
        // SAFETY: `ver` is a live i32 the kernel writes BINDER_VERSION into; the fd
        // is a live binder fd. The version is not used — it is read to match the
        // probe's syscall shape.
        unsafe { libc::ioctl(binder.fd, BINDER_VERSION as _, &mut ver as *mut i32) };
        // The legacy registration answers `dump`; SurfaceFlingerAIDL does not.
        let sf_handle = binder.get_service("SurfaceFlinger")?;
        Ok(SfClient { binder, sf_handle })
    }

    /// `dumpsys SurfaceFlinger <args...>` over binder.
    pub fn dump(&mut self, args: &[&str]) -> Result<String, String> {
        self.binder.dump(self.sf_handle, args)
    }

    /// The layer list (`--list`).
    pub fn layer_list(&mut self) -> Result<String, String> {
        self.dump(&["--list"])
    }

    /// Frame timestamps for one layer (`--latency <layer>`).
    pub fn latency(&mut self, layer: &str) -> Result<String, String> {
        self.dump(&["--latency", layer])
    }
}

// ---------------------------------------------------------------- pure parsing

/// One frame sample: the vsync timestamps SF reports, in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// vsync when the frame was due (0 = not yet posted)
    pub desired_present: u64,
    /// vsync when the frame was actually latched — the one FPS is counted from
    pub actual_present: u64,
    /// when the buffer was ready
    pub frame_ready: u64,
}

/// Parse a `--latency` table. The first non-empty line is the display's refresh
/// period; each following line is `desired \t actual \t ready`. Rows whose
/// `actual_present` is 0 or all-ones are SF's padding for unposted slots and are
/// dropped (they are not frames).
pub fn parse_latency(text: &str) -> (Option<u64>, Vec<Frame>) {
    let mut refresh: Option<u64> = None;
    let mut frames = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 3 {
            if refresh.is_none() {
                refresh = line.parse::<u64>().ok().filter(|v| *v > 0);
            }
            continue;
        }
        let f = |s: &str| s.trim().parse::<u64>().unwrap_or(0);
        let frame = Frame {
            desired_present: f(cols[0]),
            actual_present: f(cols[1]),
            frame_ready: f(cols[2]),
        };
        if frame.actual_present != 0 && frame.actual_present != u64::MAX {
            frames.push(frame);
        }
    }
    (refresh, frames)
}

/// FPS over the last `window_ns`, counted from `actual_present` timestamps. The
/// clock is CLOCK_MONOTONIC, the same one SF's vsync timestamps use.
pub fn frames_in_window(frames: &[Frame], now_ns: u64, window_ns: u64) -> usize {
    if window_ns == 0 {
        return 0;
    }
    let start = now_ns.saturating_sub(window_ns);
    frames
        .iter()
        .filter(|f| f.actual_present > start && f.actual_present <= now_ns)
        .count()
}

pub fn fps_in_window(frames: &[Frame], now_ns: u64, window_ns: u64) -> f64 {
    if window_ns == 0 {
        return 0.0;
    }
    frames_in_window(frames, now_ns, window_ns) as f64 * 1_000_000_000.0 / window_ns as f64
}

/// What a frame sample can **honestly** assert about UI activity.
///
/// Frames prove one thing the input path cannot: that the UI is *not drawing*. So a
/// window with no frames derives `Idle`, and nothing else is derived — touch, gesture
/// and switch are facts about input, and this degraded leg must not dress a frame
/// measurement up as the injected `sfanalysis.hint`. (Upstream parity this leans on:
/// `HintState::expired()` is never polled in this daemon, so without an input event
/// nothing would ever move the scene back to idle — the frame leg is what does.)
pub fn frame_hint(frames_in_window: usize) -> Option<SfHint> {
    if frames_in_window == 0 {
        Some(SfHint::Idle)
    } else {
        None
    }
}

/// The last fps the frame leg measured, ×100 (0 = none / not running). Process-wide so
/// the recorder (⑧) can read it without a channel between the two tasks.
static LAST_FPS_X100: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Last measured fps, or 0.0 when the frame leg is not the active source.
pub fn last_fps() -> f64 {
    LAST_FPS_X100.load(std::sync::atomic::Ordering::Relaxed) as f64 / 100.0
}

fn set_last_fps(fps: f64) {
    LAST_FPS_X100.store((fps * 100.0) as u64, std::sync::atomic::Ordering::Relaxed);
}

/// `CLOCK_MONOTONIC` in nanoseconds — the clock SF's timestamps are on.
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid writable out-parameter on the stack; a failure is
    // reported as 0 rather than assumed away.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return 0;
    }
    (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
}

/// Pick the layer to measure for `package` from a `--list` output.
///
/// The list lines look like `RequestedLayerState{<name>#<id> ...}`; the name is what
/// `--latency` wants. `ActivityRecordInputSink …` layers are excluded outright — they
/// carry no frames (measured: `--latency` on one returns an empty table). Preference,
/// most specific first: the package's `SurfaceView` buffer (games render there), then
/// any buffer layer whose name starts with the package, then any layer containing a
/// `/`, then anything left.
pub fn pick_layer(list_text: &str, package: &str) -> Option<String> {
    let mut names: Vec<String> = Vec::new();
    for line in list_text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // `RequestedLayerState{NAME#id ...}` -> NAME#id ; otherwise take the line.
        let inner = if let Some(rest) = line.strip_prefix("RequestedLayerState{") {
            rest.split_once(' ').map(|(n, _)| n).unwrap_or(rest)
        } else {
            line
        };
        let inner = inner.trim_end_matches('}');
        if inner.contains(package) && !inner.contains("InputSink") {
            names.push(inner.to_string());
        }
    }
    let prefix = format!("{package}/");
    // A SurfaceView layer is named `SurfaceView[pkg/act](…)#N` for games, so it does
    // NOT start with `pkg/` — check it before the prefix rule.
    if let Some(sv) = names.iter().find(|n| n.contains("SurfaceView")) {
        return Some(sv.clone());
    }
    if let Some(a) = names.iter().find(|n| n.starts_with(&prefix)) {
        return Some(a.clone());
    }
    if let Some(a) = names.iter().find(|n| n.contains('/')) {
        return Some(a.clone());
    }
    names.into_iter().next()
}

// ---------------------------------------------------------------- source priority

/// Which frame source is primary.
///
/// The M8-injected `sfanalysis.hint` wins whenever it is fresh — it is the real
/// thing, a per-vsync event from inside SurfaceFlinger. The direct-binder FPS leg is
/// the **degraded fallback** for when that library is not injected. While the hint is
/// live the FPS leg is not even polled: each poll is a binder round trip plus a pipe
/// read, so the priority has a real cost consequence, not just a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameSource {
    Hint,
    /// ⑪'s primary leg: a uprobe on the app's own `libgui.so` `queueBuffer`, counted per
    /// thread, so only that app's frames are counted.
    Uprobe,
    Fps,
}

impl FrameSource {
    pub fn as_str(self) -> &'static str {
        match self {
            FrameSource::Hint => "hint",
            FrameSource::Uprobe => "uprobe",
            FrameSource::Fps => "fps",
        }
    }
}

/// Pick the primary source from the hint file's age. `None` (absent/unreadable) means
/// the hint leg is not there at all.
pub fn choose_source(hint_age_ms: Option<u64>, stale_after_ms: u64) -> FrameSource {
    match hint_age_ms {
        Some(age) if age <= stale_after_ms => FrameSource::Hint,
        _ => FrameSource::Fps,
    }
}

/// Age of the hint file in milliseconds, from its mtime. The hint carries no timestamp
/// of its own (one raw byte, spec §2.2), so mtime is the only freshness signal — and
/// this is a wall-clock comparison, which a clock jump could fool; the injected library
/// writing on every refresh is the real proof of life.
pub fn hint_age_ms(path: &Path) -> Option<u64> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let age = std::time::SystemTime::now().duration_since(mtime).ok()?;
    Some(age.as_millis() as u64)
}

// ---------------------------------------------------------------- daemon task

/// Periodic frame sampling, as a daemon task. Opt-in: `UPERF_SF_BINDER=1`.
///
/// `UPERF_SF_BINDER_LAYER` pins a layer; otherwise the top app's layer is re-resolved
/// when the top app changes. `UPERF_SF_BINDER_HINT_STALE_MS` (default 3000) is the age
/// past which the injected hint stops counting as live. State is published to
/// `<USER_PATH>/uperf_frames.state` (or `UPERF_FRAMES_STATE`) for `webui.sh`/the WebUI.
pub struct FrameTask {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// Best-effort atomic write of the frame-source state file. Reporting must never take
/// the sampler down, so every failure is silent.
fn write_state(path: &Path, lines: &str) {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".new");
    let tmp = std::path::PathBuf::from(tmp);
    if std::fs::write(&tmp, lines).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::write(path, lines);
}

fn env_ms(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(default)
}

impl FrameTask {
    pub fn enabled() -> bool {
        matches!(std::env::var("UPERF_SF_BINDER").ok().as_deref(), Some("1") | Some("true"))
    }

    pub fn spawn<F, L, H>(
        cfg_dir: Option<std::path::PathBuf>,
        top_app: F,
        on_hint: H,
        log: L,
    ) -> Option<FrameTask>
    where
        F: Fn() -> Option<String> + Send + 'static,
        H: Fn(SfHint) + Send + 'static,
        L: Fn(&str) + Send + 'static,
    {
        let tick_ms = env_ms("UPERF_SF_BINDER_TICK_MS", 1000);
        let window_ms = env_ms("UPERF_SF_BINDER_WINDOW_MS", 1000);
        let stale_ms = env_ms("UPERF_SF_BINDER_HINT_STALE_MS", 3000);
        let pinned = std::env::var("UPERF_SF_BINDER_LAYER").ok().filter(|s| !s.is_empty());

        let hint_path = cfg_dir.as_ref().map(|d| d.join("sfanalysis.hint"));
        let state_path = std::env::var("UPERF_FRAMES_STATE")
            .ok()
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| cfg_dir.as_ref().map(|d| d.join("uperf_frames.state")));

        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::Builder::new()
            .name("uperf-sf-bind".into())
            .spawn(move || {
                let publish = |source: FrameSource, age: Option<u64>, fps: Option<f64>, frames: usize, layer: Option<&str>, refresh: Option<u64>| {
                    if let Some(p) = state_path.as_ref() {
                        let mut out = String::with_capacity(256);
                        out.push_str(&format!("source={}\n", source.as_str()));
                        out.push_str(&format!(
                            "hint_age_ms={}\n",
                            age.map(|a| a.to_string()).unwrap_or_else(|| "-".into())
                        ));
                        out.push_str(&format!(
                            "fps={}\n",
                            fps.map(|f| format!("{f:.1}")).unwrap_or_else(|| "-".into())
                        ));
                        out.push_str(&format!("frames={frames}\n"));
                        out.push_str(&format!(
                            "refresh_ns={}\n",
                            refresh.map(|r| r.to_string()).unwrap_or_else(|| "-".into())
                        ));
                        out.push_str(&format!("layer={}\n", layer.unwrap_or("-")));
                        out.push_str(&format!("ts_ms={}\n", monotonic_ns() / 1_000_000));
                        write_state(p, &out);
                    }
                };

                let mut client = match SfClient::connect() {
                    Ok(c) => c,
                    Err(e) => {
                        log(&format!("Rust: sf-binder connect failed: {e}"));
                        publish(FrameSource::Fps, None, None, 0, None, None);
                        return;
                    }
                };
                log("Rust: sf-binder connected (direct binder, no dumpsys)");

                let mut layer: Option<String> = pinned.clone();
                let mut layer_for: Option<String> = None;
                let mut last_src: Option<FrameSource> = None;
                let mut last_derived: Option<SfHint> = None;
                let mut fails = 0u32;

                while !stop_thread.load(Ordering::SeqCst) {
                    let age = hint_path.as_deref().and_then(hint_age_ms);
                    let src = choose_source(age, stale_ms);
                    if last_src != Some(src) {
                        log(&format!(
                            "Rust: sf-binder frame source -> {} (hint_age_ms={:?})",
                            src.as_str(),
                            age
                        ));
                        last_src = Some(src);
                    }

                    match src {
                        FrameSource::Hint => {
                            // The injected leg is live; do not touch binder at all.
                            last_derived = None;
                            set_last_fps(0.0);
                            publish(FrameSource::Hint, age, None, 0, None, None);
                        }
                        // `choose_source` only ever returns Hint or Fps; Uprobe is the label
                        // this branch publishes once ⑪'s probe has been found healthy.
                        FrameSource::Fps | FrameSource::Uprobe => {
                            // ⑪ sits between the injected hint and the binder fallback:
                            // while its probe is healthy it is the fps source and binder is
                            // not touched at all (each `--latency` poll is a round trip plus
                            // a pipe read, so this is a cost decision, not a label).
                            if crate::sf_uprobe::healthy() {
                                let fps = crate::sf_uprobe::last_fps();
                                let frames = crate::sf_uprobe::last_frames();
                                set_last_fps(fps);
                                publish(FrameSource::Uprobe, age, Some(fps), frames as usize, None, None);
                                if last_derived.take().is_some() {
                                    log("Rust: sf frame source -> uprobe (binder --latency stands down)");
                                }
                                std::thread::sleep(std::time::Duration::from_millis(window_ms));
                                continue;
                            }
                            let pkg = top_app();
                            if layer.is_none() || (pinned.is_none() && pkg != layer_for) {
                                if let Some(p) = pkg.as_deref() {
                                    match client.layer_list().and_then(|l| {
                                        pick_layer(&l, p).ok_or_else(|| format!("no layer for {p}"))
                                    }) {
                                        Ok(l) => {
                                            log(&format!("Rust: sf-binder layer '{l}' for {p}"));
                                            layer = Some(l);
                                            layer_for = Some(p.to_string());
                                        }
                                        Err(e) => log(&format!("Rust: sf-binder layer resolve: {e}")),
                                    }
                                }
                            }
                            match layer.clone() {
                                Some(l) => match client.latency(&l) {
                                    Ok(text) => {
                                        let (refresh, frames) = parse_latency(&text);
                                        let fps = fps_in_window(&frames, monotonic_ns(), window_ms * 1_000_000);
                                        let in_win = frames_in_window(&frames, monotonic_ns(), window_ms * 1_000_000);
                                        log(&format!(
                                            "Rust: sf-binder layer={l} refresh_ns={} frames={} in_window={in_win} fps={fps:.1}",
                                            refresh.unwrap_or(0),
                                            frames.len()
                                        ));
                                        set_last_fps(fps);
                                        publish(FrameSource::Fps, age, Some(fps), frames.len(), Some(&l), refresh);
                                        if let Some(h) = frame_hint(in_win) {
                                            if last_derived != Some(h) {
                                                last_derived = Some(h);
                                                log(&format!(
                                                    "Rust: sf-binder frame hint -> {} (nothing drawn in {} ms)",
                                                    h.as_str(),
                                                    window_ms
                                                ));
                                                on_hint(h);
                                            }
                                        }
                                        fails = 0;
                                    }
                                    Err(e) => {
                                        fails += 1;
                                        log(&format!("Rust: sf-binder latency: {e}"));
                                        publish(FrameSource::Fps, age, None, 0, Some(&l), None);
                                        if fails >= 5 {
                                            layer = pinned.clone();
                                            layer_for = None;
                                        }
                                    }
                                },
                                None => publish(FrameSource::Fps, age, None, 0, None, None),
                            }
                        }
                    }

                    let mut slept = 0u64;
                    let step = 50.min(tick_ms).max(1);
                    while slept < tick_ms && !stop_thread.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(step));
                        slept += step;
                    }
                }
                log("Rust: sf-binder stopped");
            })
            .ok()?;
        Some(FrameTask { stop, handle: Some(handle) })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "8333333\n\
        1000000000\t1000500000\t1000100000\n\
        1001000000\t1001500000\t1001100000\n\
        0\t0\t0\n\
        1002000000\t18446744073709551615\t1002100000\n\
        1003000000\t1003500000\t1003100000\n";

    #[test]
    fn parses_refresh_and_drops_padding() {
        let (refresh, frames) = parse_latency(TABLE);
        assert_eq!(refresh, Some(8_333_333));
        // the 0/0/0 row and the all-ones row are SF padding, not frames
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].actual_present, 1_000_500_000);
        assert_eq!(frames[2].actual_present, 1_003_500_000);
    }

    #[test]
    fn fps_counts_the_window() {
        let (_, frames) = parse_latency(TABLE);
        // window (1.0s, 1.5s] holds actual=1.0005e9, 1.0015e9, 1.0035e9 -> 3 frames
        let fps = fps_in_window(&frames, 1_500_000_000, 1_000_000_000);
        assert!((fps - 3.0).abs() < 1e-9, "got {fps}");
        // a 120 Hz cadence counts its frames: place all 120 strictly inside (1.0s, 2.0s]
        let mut tv = Vec::new();
        for i in 1..=120u64 {
            tv.push(Frame {
                desired_present: 0,
                actual_present: 1_000_000_000 + i * 8_333_333,
                frame_ready: 0,
            });
        }
        let fps = fps_in_window(&tv, 2_000_000_000, 1_000_000_000);
        assert!((fps - 120.0).abs() < 1e-6, "got {fps}");
    }

    #[test]
    fn empty_and_garbage_are_safe() {
        assert_eq!(parse_latency(""), (None, vec![]));
        assert_eq!(parse_latency("not a number\n"), (None, vec![]));
        assert_eq!(fps_in_window(&[], 100, 0), 0.0);
    }

    #[test]
    fn picks_the_surfaceview_then_the_activity() {
        let list = "RequestedLayerState{com.foo/com.foo.MainActivity#7 parentId=3}\n\
                    RequestedLayerState{SurfaceView[com.foo/com.foo.MainActivity](BLAST)#9 parentId=7}\n\
                    RequestedLayerState{com.other/com.other.Thing#11 parentId=3}\n";
        assert_eq!(
            pick_layer(list, "com.foo").as_deref(),
            Some("SurfaceView[com.foo/com.foo.MainActivity](BLAST)#9")
        );
        // no SurfaceView -> the package's buffer layer
        let list2 = "RequestedLayerState{com.bar/com.bar.Main#1 parentId=3}\n";
        assert_eq!(pick_layer(list2, "com.bar").as_deref(), Some("com.bar/com.bar.Main#1"));
        // absent package
        assert_eq!(pick_layer(list, "com.nope"), None);
    }

    /// The real device list is full of `ActivityRecordInputSink …` layers for the same
    /// package; those carry no frames, so they must never win.
    #[test]
    fn an_inputsink_layer_is_never_chosen() {
        let list = "RequestedLayerState{dc7055b ActivityRecordInputSink com.android.settings/.homepage.SettingsHomepageActivity#285 parentId=280 !handle z=-2147483648}\n\
                    RequestedLayerState{ActivityRecord{166295764 u0 com.android.settings/.Settings#100 parentId=99}\n\
                    RequestedLayerState{com.android.settings/com.android.settings.Settings#286 parentId=103}\n";
        assert_eq!(
            pick_layer(list, "com.android.settings").as_deref(),
            Some("com.android.settings/com.android.settings.Settings#286")
        );
        // and with nothing but InputSink layers there is no answer, not a bad one
        let only_sink = "RequestedLayerState{dc7055b ActivityRecordInputSink com.android.settings/.homepage.SettingsHomepageActivity#285}\n";
        assert_eq!(pick_layer(only_sink, "com.android.settings"), None);
    }

    #[test]
    fn monotonic_clock_reads() {
        let a = monotonic_ns();
        assert!(a > 0);
        assert!(monotonic_ns() >= a);
    }

    /// The injected hint is primary while it is fresh; the binder leg takes over when
    /// it is gone. `None` (no hint file) is the degraded case, never a reason to wait.
    #[test]
    fn the_hint_wins_only_while_it_is_fresh() {
        assert_eq!(choose_source(Some(0), 3000), FrameSource::Hint);
        assert_eq!(choose_source(Some(3000), 3000), FrameSource::Hint);
        assert_eq!(choose_source(Some(3001), 3000), FrameSource::Fps);
        assert_eq!(choose_source(None, 3000), FrameSource::Fps);
        assert_eq!(FrameSource::Hint.as_str(), "hint");
        assert_eq!(FrameSource::Fps.as_str(), "fps");
    }

    #[test]
    fn a_missing_hint_file_has_no_age() {
        assert_eq!(hint_age_ms(Path::new("/nonexistent/sfanalysis.hint")), None);
    }

    /// Frames may only ever assert idle. Anything else would be a frame measurement
    /// dressed up as an input fact.
    #[test]
    fn only_the_idle_claim_is_derived_from_frames() {
        assert_eq!(frame_hint(0), Some(SfHint::Idle));
        assert_eq!(frame_hint(1), None);
        assert_eq!(frame_hint(120), None);
    }

    #[test]
    fn frames_in_window_matches_the_rate() {
        let (_, frames) = parse_latency(TABLE);
        assert_eq!(frames_in_window(&frames, 1_500_000_000, 1_000_000_000), 3);
        assert_eq!(frames_in_window(&frames, 1_500_000_000, 0), 0);
        // the rate and the count agree
        let fps = fps_in_window(&frames, 1_500_000_000, 1_000_000_000);
        assert!((fps - 3.0).abs() < 1e-9);
    }
}
