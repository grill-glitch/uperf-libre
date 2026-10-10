//! ⑤ binder probe, round 2.
//!
//! Round 1 established: open/mmap/BINDER_VERSION work, BINDER_WRITE_READ with a
//! write-only BC_TRANSACTION delivers (servicemanager received it), and the
//! *read* into the mmap'd area returns EFAULT. It also surfaced the AOSP
//! `writeInterfaceToken` "vendor header": the receiver reads
//!   [i32 strictPolicy][i32 workSource][i32 kHeader][string16 descriptor][args]
//! and kHeader is 0x53595354 on /dev/binder (0x564e4452 on vndbinder).
//!
//! This round: (a) sweep mmap prot/size/flags to fix the read EFAULT, and
//! (b) prepend the header so servicemanager parses the parcel.

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
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
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

/// The AOSP `writeInterfaceToken` preamble: strictPolicy, workSource, kHeader, descriptor.
fn push_interface_token(v: &mut Vec<u8>, header: u32, descriptor: &str) {
    push_u32(v, 0); // strict mode policy
    push_u32(v, 0); // work source uid (kUnsetWorkSource)
    push_u32(v, header);
    push_string16(v, descriptor);
}

// ---------------------------------------------------------------- io

struct Driver {
    fd: i32,
    map: *mut u8,
    map_len: usize,
    /// The command-stream buffer. This must be a **writable heap buffer**, not the
    /// mmap: the kernel writes the BR_* command stream here, and a read-only binder
    /// mapping cannot take that write (EFAULT, measured). Transaction payloads still
    /// land in the mmap and are read from `data.ptr.buffer`. libbinder does the same
    /// (`read_buffer = mIn.data()` — a Parcel buffer — while `mOut`/payloads use the
    /// mapping).
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

fn open_driver(path: &str, size: usize, prot: i32, flags: i32) -> Result<Driver, String> {
    let cpath = std::ffi::CString::new(path).unwrap();
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(format!("open: {}", std::io::Error::last_os_error()));
    }
    let map = unsafe { libc::mmap(std::ptr::null_mut(), size, prot, flags, fd, 0) };
    if map == libc::MAP_FAILED {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("mmap(prot={prot:#x},size={size},flags={flags:#x}): {e}"));
    }
    Ok(Driver { fd, map: map as *mut u8, map_len: size, rd: vec![0u8; 64 * 1024] })
}

impl Driver {
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

    fn txn_write(&mut self, handle: u32, code: u32, oneway: bool, data: &[u8]) -> Vec<u8> {
        let mut wb: Vec<u8> = Vec::new();
        push_u32(&mut wb, BC_TRANSACTION as u32);
        let txn = BinderTransactionData {
            handle,
            code,
            flags: oneway as u32,
            data_size: data.len() as u64,
            data_buffer: data.as_ptr() as u64,
            ..Default::default()
        };
        wb.extend_from_slice(as_bytes(&txn));
        wb
    }
}

fn read_txn(b: &[u8]) -> Result<(BinderTransactionData, Vec<u8>), String> {
    if b.len() < TXN_SIZE {
        return Err(format!("short transaction: {} bytes", b.len()));
    }
    let t: BinderTransactionData =
        unsafe { std::ptr::read_unaligned(b.as_ptr() as *const BinderTransactionData) };
    let data = if t.data_size > 0 && t.data_buffer != 0 {
        unsafe { std::slice::from_raw_parts(t.data_buffer as *const u8, t.data_size as usize) }.to_vec()
    } else {
        Vec::new()
    };
    Ok((t, data))
}

/// Parse one read-buffer's command stream. Some(reply) when BR_REPLY appears.
fn parse_commands(d: &mut Driver, buf: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut off = 0usize;
    let mut reply: Option<Vec<u8>> = None;
    while off + 4 <= buf.len() {
        let at = off;
        let code = u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap()) as u64;
        off += 4;
        if code == BR_NOOP || code == BR_TRANSACTION_COMPLETE || code == BR_SPAWN_LOOPER {
            continue;
        }
        if code == BR_REPLY || code == BR_TRANSACTION {
            let (t, data) = read_txn(&buf[off..])?;
            off += TXN_SIZE;
            if t.data_buffer != 0 {
                let mut fw: Vec<u8> = Vec::new();
                push_u32(&mut fw, BC_FREE_BUFFER as u32);
                fw.extend_from_slice(&t.data_buffer.to_ne_bytes());
                let _ = d.bwr(&fw, false);
            }
            if code == BR_REPLY {
                reply = Some(data);
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
        return Err(format!("unexpected read code 0x{code:08x} at offset {at}"));
    }
    Ok(reply)
}

/// A synchronous transaction is two ioctls: the write returns BR_TRANSACTION_COMPLETE
/// at once (the thread must return to userspace), then a read-only ioctl blocks until
/// the reply arrives — libbinder's `waitForResponse` loop does exactly this.
fn transact(d: &mut Driver, handle: u32, code: u32, data: &[u8]) -> Result<Vec<u8>, String> {
    let wb = d.txn_write(handle, code, false, data);
    let mut n = d.bwr(&wb, true)?;
    for _ in 0..4 {
        // the command stream is in the heap buffer; any payload pointer in it points
        // into the mmap, which is readable
        let buf: Vec<u8> = d.rd[..n as usize].to_vec();
        if let Some(r) = parse_commands(d, &buf)? {
            return Ok(r);
        }
        n = d.bwr(&[], true)?;
    }
    Err("no reply after 4 reads".into())
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/dev/binder".to_string());
    let header = if path.contains("vndbinder") {
        K_HEADER_VNDBINDER
    } else {
        K_HEADER_BINDER
    };
    let name = std::env::args().nth(2).unwrap_or_else(|| "SurfaceFlingerAIDL".to_string());

    let page = unsafe { libc::sysconf(libc::_SC_PAGE_SIZE) } as usize;
    let variants: &[(&str, usize, i32, i32)] = &[
        ("A READ 256K PRIVATE", 256 * 1024, libc::PROT_READ, libc::MAP_PRIVATE),
        (
            "B READ 1M-2p PRIVATE|NORESERVE",
            (1024 * 1024) - 2 * page,
            libc::PROT_READ,
            libc::MAP_PRIVATE | libc::MAP_NORESERVE,
        ),
        (
            "C RW 1M-2p PRIVATE|NORESERVE",
            (1024 * 1024) - 2 * page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_NORESERVE,
        ),
    ];

    let mut working: Option<usize> = None;
    for (i, (label, size, prot, flags)) in variants.iter().enumerate() {
        println!("--- variant {label} ---");
        let mut d = match open_driver(&path, *size, *prot, *flags) {
            Ok(d) => d,
            Err(e) => {
                println!("  open/mmap: {e}");
                continue;
            }
        };
        println!("  mmap ok at {:p}", d.map);
        match d.bwr(&[], false) {
            Ok(n) => println!("  S0 empty: ok (read_consumed={n})"),
            Err(e) => println!("  S0 empty: {e}"),
        }
        let payload = {
            let mut p = Vec::new();
            push_interface_token(&mut p, header, "android.os.IServiceManager");
            push_string16(&mut p, &name);
            p
        };
        let wb = d.txn_write(0, 1, true, &payload);
        match d.bwr(&wb, true) {
            Ok(n) => {
                println!("  S2 one-way + read into mmap: ok (read_consumed={n})");
                if working.is_none() {
                    working = Some(i);
                }
            }
            Err(e) => println!("  S2 one-way + read into mmap: {e}"),
        }
    }

    let Some(wi) = working else {
        println!("no variant could read; stopping");
        return;
    };
    println!("--- using variant {} for getService({name}) ---", variants[wi].0);
    let (_, size, prot, flags) = variants[wi];
    let mut d = open_driver(&path, size, prot, flags).unwrap();
    let mut data = Vec::new();
    push_interface_token(&mut data, header, "android.os.IServiceManager");
    push_string16(&mut data, &name);
    println!("  request data ({} bytes): {}", data.len(), hex(&data));
    match transact(&mut d, 0, 1, &data) {
        Ok(reply) => {
            println!("  reply {} bytes: {}", reply.len(), hex(&reply));
            if reply.len() >= 24 {
                let obj: FlatBinderObject =
                    unsafe { std::ptr::read_unaligned(reply.as_ptr() as *const FlatBinderObject) };
                println!(
                    "  flat_binder_object kind=0x{:08x} flags=0x{:08x} binder=0x{:x} cookie=0x{:x}",
                    obj.kind, obj.flags, obj.binder, obj.cookie
                );
            } else if reply.len() >= 4 {
                let first = u32::from_ne_bytes(reply[0..4].try_into().unwrap());
                println!("  reply first u32 = 0x{first:08x} (0 = null binder)");
            }
        }
        Err(e) => println!("  transact: {e}"),
    }
}
