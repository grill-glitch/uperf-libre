//! binder-probe — ⑤ 直连 binder 帧源（真机验证的最小实现）。
//!
//! 从零实现一个 root binder 客户端，不 spawn `dumpsys`、不依赖 Java/NDK binder：
//! open /dev/binder → mmap → servicemanager 取 SurfaceFlinger handle → 对 SF 发
//! `dump` 事务（`DUMP_TRANSACTION`，base `IBinder` 的码，与任何 AIDL 方法码无关），
//! 参数经一个 **pipe fd** 递进去，SF 把 dump 文本写进 pipe，我们在另一个线程读回。
//! 这就是 AppOpt `--latency` 降级腿的同构形态，只是去掉了 fork+exec。
//!
//! 用法：`binder-probe /dev/binder [--latency] [layer]`
//!
//! 真机验证（alioth / crDroid A16 / Enforcing）：`binder-probe /dev/binder --latency`
//! → pipe 收到 `8333333`（1e9/120，120 Hz 的刷新周期），与
//! `dumpsys SurfaceFlinger --latency` 逐字相同。
//!
//! 踩出来的坑（都真机实测过，见 README）：
//!   1. 读缓冲必须是**可写堆缓冲**，不是 mmap（mmap 是 PROT_READ，用它读必 EFAULT）。
//!   2. 事务 data 必须带 `writeInterfaceToken` 的 vendor 头（`kHeader=0x53595354`）。
//!   3. 同步调用是**两次 ioctl**（第二次只读阻塞等 BR_REPLY）。
//!   4. 回包里的 handle 必须 **BC_ACQUIRE**，否则下一次用它就 "invalid handle"。
//!   5. `BINDER_TYPE_FD = B_PACK_CHARS('f','d','*',B_TYPE_LARGE) = 0x66642a85`
//!      —— 写错类型内核 `binder_validate_object` 返回 0，报
//!      "invalid offset ... or object" + BR_FAILED_REPLY。
//!   6. **`dump` 的 fd 必须在 parcel 最前、且不能带 interface token**（带 token 时
//!      服务端 `readFileDescriptor` 读到 token 首字节 → fd 无效 → 异常）。
//!   7. **要对 legacy `SurfaceFlinger` 发，不要对 `SurfaceFlingerAIDL`**：AIDL 那个
//!      的 `onTransact` 不回落 `BBinder::onTransact`，`dump` 事务石沉大海（空回包、
//!      pipe 0 字节）。`service call SurfaceFlinger 1` 对 root 被拒是**另一回事**
//!      （那是 SF 自己的权限检查，只挡普通方法码，不挡 `dump`）。

use std::ffi::c_void;

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
const K_HEADER_BINDER: u32 = 0x5359_5354; // "SYST" — /dev/binder
const K_HEADER_VNDBINDER: u32 = 0x564e_4452; // "VNDR" — /dev/vndbinder
const BINDER_TYPE_HANDLE: u32 = 0x7368_2a85;
const BINDER_TYPE_FD: u32 = 0x6664_2a85;
const DUMP_TRANSACTION: u32 = 0x5f44_4d50; // B_PACK_CHARS('_','D','M','P')

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
#[derive(Default, Clone, Copy, Debug)]
struct FlatBinderObject {
    kind: u32,
    flags: u32,
    binder: u64,
    cookie: u64,
}

fn as_bytes<T: Sized>(t: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((t as *const T) as *const u8, std::mem::size_of::<T>()) }
}
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
fn pad8(v: &mut Vec<u8>) {
    while v.len() % 8 != 0 {
        v.push(0);
    }
}
/// AOSP `writeInterfaceToken`: [strict][workSource][kHeader][descriptor].
fn push_interface_token(v: &mut Vec<u8>, header: u32, descriptor: &str) {
    push_u32(v, 0);
    push_u32(v, 0);
    push_u32(v, header);
    push_string16(v, descriptor);
}

// ---------------------------------------------------------------- driver

struct Driver {
    fd: i32,
    map: *mut u8,
    map_len: usize,
    /// Command-stream buffer: a writable **heap** buffer, never the mmap.
    rd: Vec<u8>,
}

impl Drop for Driver {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.map as *mut c_void, self.map_len);
            libc::close(self.fd);
        }
    }
}

impl Driver {
    fn open(path: &str) -> Result<Driver, String> {
        let cpath = std::ffi::CString::new(path).unwrap();
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(format!("open {path}: {}", std::io::Error::last_os_error()));
        }
        let size = 256 * 1024;
        let map = unsafe { libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ, libc::MAP_PRIVATE, fd, 0) };
        if map == libc::MAP_FAILED {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(format!("mmap: {e}"));
        }
        Ok(Driver { fd, map: map as *mut u8, map_len: size, rd: vec![0u8; 64 * 1024] })
    }

    fn bwr(&mut self, write: &[u8], read: bool) -> Result<u64, String> {
        let mut bwr = BinderWriteRead {
            write_size: write.len() as u64,
            write_buffer: if write.is_empty() { 0 } else { write.as_ptr() as u64 },
            read_size: if read { self.rd.len() as u64 } else { 0 },
            read_buffer: if read { self.rd.as_mut_ptr() as u64 } else { 0 },
            ..Default::default()
        };
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
                let t: BinderTransactionData =
                    unsafe { std::ptr::read_unaligned(buf[off..].as_ptr() as *const BinderTransactionData) };
                off += TXN_SIZE;
                if t.data_size > 0 && t.data_buffer != 0 {
                    let data = unsafe {
                        std::slice::from_raw_parts(t.data_buffer as *const u8, t.data_size as usize).to_vec()
                    };
                    // acquire handles before freeing the buffer, or they go invalid
                    if t.offsets_size > 0 && t.data_offsets != 0 {
                        let nn = (t.offsets_size / 8) as usize;
                        let offs = unsafe { std::slice::from_raw_parts(t.data_offsets as *const u64, nn) };
                        for &o in offs {
                            let o = o as usize;
                            if o + 24 <= data.len() {
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
}

/// `getService(name)` -> the service's local binder handle (BC_ACQUIRE'd by `parse`).
fn get_service(d: &mut Driver, header: u32, name: &str) -> Result<u32, String> {
    let mut data = Vec::new();
    push_interface_token(&mut data, header, "android.os.IServiceManager");
    push_string16(&mut data, name);
    let reply = d.transact(0, 1, &data, &[])?;
    if reply.len() < 28 {
        return Err(format!("short reply ({} bytes)", reply.len()));
    }
    let obj: FlatBinderObject = unsafe { std::ptr::read_unaligned(reply[4..].as_ptr() as *const FlatBinderObject) };
    if obj.kind != BINDER_TYPE_HANDLE {
        return Err(format!("no handle for '{name}' (kind=0x{:08x})", obj.kind));
    }
    Ok(obj.binder as u32)
}

/// `dump` on the handle: parcel = [fd object][String16[] args], **no interface token**.
/// The text arrives on a pipe, read on a helper thread (a >64 KiB dump would otherwise
/// deadlock on the pipe buffer).
fn dump(d: &mut Driver, header: u32, handle: u32, args: &[&str]) -> Result<String, String> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(format!("pipe: {}", std::io::Error::last_os_error()));
    }
    let (rfd, wfd) = (fds[0], fds[1]);
    // SF keeps its dup of the write end and, measured, sends NO reply to `dump`
    // (strace: the reader sees the text while the caller is still blocked in the
    // second ioctl). So this is a write-only send, and the reader ends on a pipe
    // idle timeout. Reading runs on its own thread, so a dump larger than the 64 KiB
    // pipe buffer cannot deadlock.
    let reader = std::thread::spawn(move || {
        let mut out: Vec<u8> = Vec::new();
        let mut buf = [0u8; 65536];
        loop {
            let mut pfd = libc::pollfd { fd: rfd, events: libc::POLLIN, revents: 0 };
            let pr = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, 1000) };
            if pr > 0 && (pfd.revents & libc::POLLIN) != 0 {
                let n = unsafe { libc::read(rfd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
                if n <= 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n as usize]);
            } else {
                // 1 s of silence (or EOF) ends the dump
                break;
            }
        }
        unsafe { libc::close(rfd) };
        out
    });

    let mut data = Vec::new();
    pad8(&mut data);
    let offsets = vec![data.len() as u64];
    let obj = FlatBinderObject { kind: BINDER_TYPE_FD, flags: 0, binder: wfd as u64, cookie: 0 };
    data.extend_from_slice(as_bytes(&obj));
    push_u32(&mut data, args.len() as u32);
    for a in args {
        push_string16(&mut data, a);
    }
    let _ = header;

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
    d.bwr(&wb, false)?;
    unsafe { libc::close(wfd) };
    let out = reader.join().map_err(|_| "reader panicked".to_string())?;
    Ok(String::from_utf8_lossy(&out).to_string())
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/dev/binder".to_string());
    let header = if path.contains("vndbinder") { K_HEADER_VNDBINDER } else { K_HEADER_BINDER };
    let args: Vec<String> = std::env::args().skip(2).collect();
    let args: Vec<&str> = if args.is_empty() { vec!["--latency"] } else { args.iter().map(|s| s.as_str()).collect() };

    let mut d = match Driver::open(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let mut ver: i32 = 0;
    unsafe { libc::ioctl(d.fd, BINDER_VERSION as _, &mut ver as *mut i32) };
    println!("// opened {path}, BINDER_VERSION={ver}, header=0x{header:08x}, args={args:?}");

    // The LEGACY registration answers `dump`; SurfaceFlingerAIDL does not.
    let handle = match get_service(&mut d, header, "SurfaceFlinger") {
        Ok(h) => h,
        Err(e) => {
            eprintln!("getService(SurfaceFlinger): {e}");
            std::process::exit(2);
        }
    };
    println!("// SurfaceFlinger handle={handle}");
    match dump(&mut d, header, handle, &args) {
        Ok(text) => {
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
        }
        Err(e) => {
            eprintln!("dump: {e}");
            std::process::exit(1);
        }
    }
}
