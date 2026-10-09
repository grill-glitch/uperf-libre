//! Inline-hook engine for the four libc functions the vendor library observes.
//!
//! Behaviour being replicated (docs/m8-sfanalysis-reverse.md §5): the vendor
//! registers replacements for `ioctl`, `epoll_wait`, `pthread_cond_wait` and
//! `pthread_cond_timedwait`; each replacement calls the original first, keeps
//! its return value, runs an observer, then returns the original's value.
//! `ioctl` additionally matches `BINDER_WRITE_READ` (0xc0306201).
//!
//! Mechanics:
//!  1. resolve with `dlsym(RTLD_DEFAULT, name)` (the vendor walks ELF dynsym by
//!     hand; dlsym is equivalent and shorter);
//!  2. follow unconditional-branch stubs — bionic implements `epoll_wait` as
//!     `mov x4,xzr; mov w5,#8; b __epoll_pwait`;
//!  3. copy the first four instructions into an `mmap`'d RWX trampoline,
//!     followed by `LDR x17,#8; BR x17; .quad entry+16`;
//!  4. `mprotect` the entry page RWX and write the same jump at the entry,
//!     targeting the asm shim.
//!
//! The copied prologue must be relocation-safe. Any PC-relative instruction is
//! refused rather than corrupted. All four prologues on this device's bionic
//! pass (asserted in tests).

use core::sync::atomic::{AtomicUsize, Ordering};
use std::ffi::{c_char, c_void, CString};

/// The four symbols, in the order the vendor registers them.
pub const TARGETS: [&str; 4] = [
    "ioctl",
    "pthread_cond_timedwait",
    "pthread_cond_wait",
    "epoll_wait",
];

/// One installed hook.
#[derive(Debug)]
pub struct Hook {
    pub name: &'static str,
    pub entry: usize,
    pub tramp: usize,
    pub entry_page_len: usize,
}

#[derive(Debug)]
pub enum HookError {
    NoSymbol(String),
    NoExecMapping(String),
    RelocationUnsafe { name: String, insn: u32 },
    Syscall(&'static str, i32),
    Unsupported(&'static str),
}

impl core::fmt::Display for HookError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HookError::NoSymbol(s) => write!(f, "symbol not resolvable: {}", s),
            HookError::NoExecMapping(s) => write!(f, "no r-x mapping for {}", s),
            HookError::RelocationUnsafe { name, insn } => {
                write!(f, "{}: pc-relative insn {:#010x} in prologue", name, insn)
            }
            HookError::Syscall(w, e) => write!(f, "{} errno={}", w, e),
            HookError::Unsupported(a) => write!(f, "inline hooks unsupported on {}", a),
        }
    }
}

// --- per-symbol original pointers, read by the asm shims -------------------

/// Per-symbol original-function pointers, read by the asm shims.
///
/// Deliberately NOT `#[no_mangle]`: a preemptible (exported) symbol cannot be
/// addressed with `adrp`+`:lo12:` inside a shared object. The assembly refers
/// to them through `sym`, which resolves local items.
pub static SFH_ORIG_IOCTL: AtomicUsize = AtomicUsize::new(0);
pub static SFH_ORIG_EPOLL_WAIT: AtomicUsize = AtomicUsize::new(0);
pub static SFH_ORIG_COND_WAIT: AtomicUsize = AtomicUsize::new(0);
pub static SFH_ORIG_COND_TIMEDWAIT: AtomicUsize = AtomicUsize::new(0);

fn slot_for(name: &str) -> Option<&'static AtomicUsize> {
    Some(match name {
        "ioctl" => &SFH_ORIG_IOCTL,
        "epoll_wait" => &SFH_ORIG_EPOLL_WAIT,
        "pthread_cond_wait" => &SFH_ORIG_COND_WAIT,
        "pthread_cond_timedwait" => &SFH_ORIG_COND_TIMEDWAIT,
        _ => return None,
    })
}

// --- pure helpers (unit-tested on the host) --------------------------------

/// True when the instruction cannot be relocated into a trampoline:
/// B/BL, B.cond, CBZ/CBNZ, TBZ/TBNZ, ADR/ADRP, LDR/PRFM (literal).
pub fn is_pc_relative(insn: u32) -> bool {
    insn & 0x7C00_0000 == 0x1400_0000      // B / BL
        || insn & 0xFF00_0010 == 0x5400_0000 // B.cond
        || insn & 0x7E00_0000 == 0x3400_0000 // CBZ / CBNZ
        || insn & 0x7E00_0000 == 0x3600_0000 // TBZ / TBNZ
        || insn & 0x1F00_0000 == 0x1000_0000 // ADR / ADRP
        || insn & 0x3B00_0000 == 0x1800_0000 // LDR (literal) / PRFM (literal)
}

/// Absolute target of an unconditional `B` at `at`, else `None`.
pub fn branch_target(insn: u32, at: usize) -> Option<usize> {
    if insn & 0xFC00_0000 != 0x1400_0000 {
        return None;
    }
    let imm26 = (insn & 0x03FF_FFFF) as i32;
    let imm26 = (imm26 << 6) >> 6; // sign-extend 26 bits
    Some(((at as isize) + (imm26 as isize) * 4) as usize)
}

/// Read `n` 32-bit instructions from `addr` (unaligned-safe).
///
/// # Safety
/// `addr` must point at readable code.
pub unsafe fn read_insns(addr: usize, n: usize) -> Vec<u32> {
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        v.push(core::ptr::read_unaligned((addr as *const u32).add(i)));
    }
    v
}

/// Read `n` 32-bit words from `addr` (data, not necessarily code).
///
/// # Safety
/// `addr` must point at readable memory.
pub unsafe fn read_words(addr: usize, n: usize) -> Vec<u32> {
    read_insns(addr, n)
}

extern "C" {
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn __errno() -> *mut i32;
    fn sysconf(name: i32) -> i64;
    #[cfg(target_arch = "aarch64")]
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
    #[cfg(target_arch = "aarch64")]
    fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
}

const RTLD_DEFAULT: *mut c_void = core::ptr::null_mut();
const _SC_PAGESIZE: i32 = 39;

pub fn page_size() -> usize {
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if p <= 0 { 4096 } else { p as usize }
}

fn errno() -> i32 {
    unsafe { *__errno() }
}

/// Resolve `name`, following bionic tail-call stubs.
///
/// bionic implements several calls as a few register moves followed by an
/// unconditional `b` (e.g. `epoll_wait` = `mov x4,xzr; mov w5,#8;
/// b __epoll_pwait`). We look at the first four instructions: if one of them is
/// an unconditional `B` and everything before it is relocation-safe, that is a
/// stub body — follow it and look again. Up to three hops.
pub fn resolve_entry(name: &str) -> Result<usize, HookError> {
    let cname = CString::new(name).map_err(|_| HookError::NoSymbol(name.into()))?;
    let p = unsafe { dlsym(RTLD_DEFAULT, cname.as_ptr()) };
    if p.is_null() {
        return Err(HookError::NoSymbol(name.into()));
    }
    let mut addr = p as usize;
    for _ in 0..3 {
        let insns = unsafe { read_insns(addr, 4) };
        let mut followed = false;
        for (i, &insn) in insns.iter().enumerate() {
            let at = addr + i * 4;
            if let Some(t) = branch_target(insn, at) {
                // Only follow when the preceding instructions are a plain stub
                // body; a `b` deeper in a real prologue is left alone.
                if insns[..i].iter().all(|x| !is_pc_relative(*x)) {
                    addr = t;
                    followed = true;
                }
                break;
            }
            if is_pc_relative(insn) {
                break; // B.cond/CBZ/ADR/… : not a stub, stop looking
            }
        }
        if !followed {
            break;
        }
    }
    Ok(addr)
}

/// Is `addr` inside a mapping with the `x` permission?
///
/// The vendor's scan does NOT filter on perms and therefore patches the
/// *first* matching mapping — for `libandroidfw.so` that is the r--p ELF
/// header, not the r-xp text (docs/m8-sfanalysis-reverse.md §0). Filtering is
/// a deliberate correction, recorded in the spec.
pub fn is_executable(addr: usize) -> bool {
    let Ok(txt) = std::fs::read_to_string("/proc/self/maps") else {
        return false;
    };
    for line in txt.lines() {
        let mut it = line.split_whitespace();
        let (Some(range), Some(perms)) = (it.next(), it.next()) else { continue };
        let Some((s, e)) = range.split_once('-') else { continue };
        let (Ok(s), Ok(e)) = (usize::from_str_radix(s, 16), usize::from_str_radix(e, 16)) else {
            continue;
        };
        if addr >= s && addr < e {
            return perms.as_bytes().get(2).copied() == Some(b'x');
        }
    }
    false
}

// ===========================================================================
// aarch64 implementation
// ===========================================================================

#[cfg(target_arch = "aarch64")]
mod arch {
    use super::*;

    extern "C" {
        #[link_name = "sfh_shim_ioctl"]
        static SFH_SHIM_IOCTL: u8;
        #[link_name = "sfh_shim_epoll_wait"]
        static SFH_SHIM_EPOLL_WAIT: u8;
        #[link_name = "sfh_shim_cond_wait"]
        static SFH_SHIM_COND_WAIT: u8;
        #[link_name = "sfh_shim_cond_timedwait"]
        static SFH_SHIM_COND_TIMEDWAIT: u8;
    }

    /// Address of each asm shim. `addr_of!` is required: naming the static in a
    /// value expression loads its *contents* (a `u8`), which is the low byte of
    /// the shim's first instruction — 0xff for `sub sp, sp, #64` — not the
    /// address we need.
    pub fn shim_for(name: &str) -> Option<usize> {
        Some(match name {
            "ioctl" => core::ptr::addr_of!(SFH_SHIM_IOCTL) as usize,
            "epoll_wait" => core::ptr::addr_of!(SFH_SHIM_EPOLL_WAIT) as usize,
            "pthread_cond_wait" => core::ptr::addr_of!(SFH_SHIM_COND_WAIT) as usize,
            "pthread_cond_timedwait" => core::ptr::addr_of!(SFH_SHIM_COND_TIMEDWAIT) as usize,
            _ => return None,
        })
    }

    const PROT_READ: i32 = 1;
    const PROT_WRITE: i32 = 2;
    const PROT_EXEC: i32 = 4;
    const MAP_PRIVATE: i32 = 0x02;
    const MAP_ANONYMOUS: i32 = 0x20;

    /// Install one hook. The trampoline is built before the entry is patched,
    /// so a failure leaves the target untouched.
    pub fn install(name: &str) -> Result<Hook, HookError> {
        let entry = resolve_entry(name)?;
        let shim = shim_for(name).ok_or_else(|| HookError::NoSymbol(name.into()))?;
        let slot = slot_for(name).ok_or_else(|| HookError::NoSymbol(name.into()))?;

        if !is_executable(entry) {
            return Err(HookError::NoExecMapping(name.into()));
        }

        let saved = unsafe { read_insns(entry, 4) };
        for &insn in &saved {
            if is_pc_relative(insn) {
                return Err(HookError::RelocationUnsafe { name: name.into(), insn });
            }
        }

        let ps = page_size();
        let tramp = unsafe {
            mmap(
                core::ptr::null_mut(),
                ps,
                PROT_READ | PROT_WRITE | PROT_EXEC,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if tramp as isize == -1 {
            return Err(HookError::Syscall("mmap", errno()));
        }
        let tramp = tramp as usize;

        unsafe {
            let p = tramp as *mut u32;
            for (i, insn) in saved.iter().enumerate() {
                core::ptr::write_unaligned(p.add(i), *insn);
            }
            core::ptr::write_unaligned(p.add(4), 0x5800_0051); // ldr x17, #8
            core::ptr::write_unaligned(p.add(5), 0xD61F_0220); // br  x17
            core::ptr::write_unaligned((tramp + 24) as *mut u64, (entry + 16) as u64);
        }
        slot.store(tramp, Ordering::SeqCst);

        // Both the trampoline and the patched entry are newly written code.
        // The D-cache must be cleaned and the I-cache invalidated per line, or
        // the CPU may execute the stale (zero) contents and take SIGBUS/SEGV.
        unsafe fn flush_code(start: usize, len: usize) {
            const LINE: usize = 64;
            let mut p = start & !(LINE - 1);
            let end = start + len;
            while p < end {
                core::arch::asm!(
                    "dc cvau, {0}",
                    "dsb ish",
                    "ic ivau, {0}",
                    "dsb ish",
                    in(reg) p,
                    options(nostack, preserves_flags)
                );
                p += LINE;
            }
            core::arch::asm!("isb", options(nostack, preserves_flags));
        }

        unsafe { flush_code(tramp, 32) };

        let page_start = entry & !(ps - 1);
        let page_len = ((entry - page_start) + 16 + ps - 1) & !(ps - 1);
        if unsafe {
            mprotect(page_start as *mut c_void, page_len, PROT_READ | PROT_WRITE | PROT_EXEC)
        } != 0
        {
            return Err(HookError::Syscall("mprotect", errno()));
        }
        unsafe {
            let p = entry as *mut u32;
            core::ptr::write_unaligned(p, 0x5800_0051); // ldr x17, #8
            core::ptr::write_unaligned(p.add(1), 0xD61F_0220); // br x17
            core::ptr::write_unaligned((entry + 8) as *mut u64, shim as u64);
            flush_code(entry, 16);
        }

        // Optional self-check: dump the patched entry and the trampoline so a
        // device run can prove the 16-byte jump and the continuation literal.
        if std::env::var("UPERF_SFANALYSIS_DEBUG").is_ok() {
            unsafe {
                let e = entry as *const u32;
                let t = tramp as *const u32;
                eprintln!(
                    "uperf-sfanalysis[dbg] {} entry={:#x} [{:#010x} {:#010x} {:#010x} {:#010x}] shim={:#x} tramp={:#x} [{:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}] cont={:#x}",
                    name,
                    entry,
                    core::ptr::read_unaligned(e),
                    core::ptr::read_unaligned(e.add(1)),
                    core::ptr::read_unaligned(e.add(2)),
                    core::ptr::read_unaligned(e.add(3)),
                    shim,
                    tramp,
                    core::ptr::read_unaligned(t),
                    core::ptr::read_unaligned(t.add(1)),
                    core::ptr::read_unaligned(t.add(2)),
                    core::ptr::read_unaligned(t.add(3)),
                    core::ptr::read_unaligned(t.add(4)),
                    core::ptr::read_unaligned(t.add(5)),
                    core::ptr::read_unaligned((tramp + 24) as *const u64),
                );
            }
        }

        Ok(Hook {
            name: TARGETS.iter().find(|t| **t == name).copied().unwrap_or("?"),
            entry,
            tramp,
            entry_page_len: page_len,
        })
    }

    // -----------------------------------------------------------------------
    // Assembly shims.
    //
    // Each shim saves the callee-saved registers it touches, keeps the two
    // argument registers the observer wants, calls the original through the
    // trampoline, calls `observe::note(a, b, ret, kind)`, restores, and
    // returns the original's value.
    //
    //   x19 = a, x20 = b, w21 = ret, w3 = kind
    // -----------------------------------------------------------------------
    core::arch::global_asm!(
        ".text",
        ".p2align 2",

        ".macro SFH_SHIM name, orig, kind, ra, rb",
        "\\name:",
        "sub  sp, sp, #64",
        "stp  x19, x20, [sp, #0]",
        "stp  x21, x22, [sp, #16]",
        "stp  x29, x30, [sp, #32]",
        "mov  x19, \\ra",
        "mov  x20, \\rb",
        "adrp x17, \\orig",
        "add  x17, x17, :lo12:\\orig",
        "ldr  x17, [x17]",
        "blr  x17",
        "mov  w21, w0",
        "mov  x0, x19",
        "mov  x1, x20",
        "mov  w2, w21",
        "mov  w3, #\\kind",
        "bl   {note}",
        "mov  w0, w21",
        "ldp  x19, x20, [sp, #0]",
        "ldp  x21, x22, [sp, #16]",
        "ldp  x29, x30, [sp, #32]",
        "add  sp, sp, #64",
        "ret",
        ".endm",

        ".global sfh_shim_ioctl",
        "SFH_SHIM sfh_shim_ioctl, {orig_ioctl}, 0, x1, x2",
        ".global sfh_shim_epoll_wait",
        "SFH_SHIM sfh_shim_epoll_wait, {orig_epoll}, 1, x0, x3",
        ".global sfh_shim_cond_wait",
        "SFH_SHIM sfh_shim_cond_wait, {orig_cw}, 2, x0, x1",
        ".global sfh_shim_cond_timedwait",
        "SFH_SHIM sfh_shim_cond_timedwait, {orig_ctw}, 3, x0, x2",

        orig_ioctl = sym SFH_ORIG_IOCTL,
        orig_epoll = sym SFH_ORIG_EPOLL_WAIT,
        orig_cw = sym SFH_ORIG_COND_WAIT,
        orig_ctw = sym SFH_ORIG_COND_TIMEDWAIT,
        note = sym crate::observe::note,
    );
}

#[cfg(target_arch = "aarch64")]
pub use arch::install;

#[cfg(not(target_arch = "aarch64"))]
pub fn install(_name: &str) -> Result<Hook, HookError> {
    Err(HookError::Unsupported("inline hooks are aarch64-only"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pc_relative_detection() {
        assert!(!is_pc_relative(0xA9BF_7BFD)); // stp x29, x30, [sp, #-16]!
        assert!(!is_pc_relative(0xD104_03FF)); // sub sp, sp, #0x100
        assert!(!is_pc_relative(0xAA1F_03E4)); // mov x4, xzr
        assert!(is_pc_relative(0x1400_0002)); // b
        assert!(is_pc_relative(0x9400_0002)); // bl
        assert!(is_pc_relative(0x9000_0000)); // adrp
        assert!(is_pc_relative(0x5800_0000)); // ldr <literal>
        assert!(is_pc_relative(0x5400_0000)); // b.eq
        assert!(is_pc_relative(0x3400_0000)); // cbz
        assert!(is_pc_relative(0x3600_0000)); // tbz
    }

    #[test]
    fn branch_target_forward_and_back() {
        assert_eq!(branch_target(0x1400_0002, 0x1000), Some(0x1008));
        assert_eq!(branch_target(0x17FF_FFFF, 0x1000), Some(0xFFC));
        assert_eq!(branch_target(0xA9BF_7BFD, 0x1000), None);
    }

    #[test]
    fn epoll_wait_stub_is_a_tail_call() {
        // mov x4, xzr ; mov w5, #8 ; b __epoll_pwait  (bionic libc, this device)
        let insns = [0xAA1F_03E4u32, 0x5280_0105u32, 0x1401_3D6A];
        assert!(!is_pc_relative(insns[0]));
        assert!(!is_pc_relative(insns[1]));
        assert!(is_pc_relative(insns[2]));
        assert_eq!(branch_target(insns[2], 0x96798), Some(0x96798 + 0x4F5A8));
    }

    #[test]
    fn device_prologues_are_relocation_safe() {
        // ioctl: sub sp,sp,#0x100 ; stp x29,x30,[sp,#0xe0] ;
        //        str x19,[sp,#0xf0] ; add x29,sp,#0xe0
        for insn in [0xD104_03FFu32, 0xA90E_7BFD, 0xF900_7BF3, 0x9103_83FD] {
            assert!(!is_pc_relative(insn), "{:#010x} should be safe", insn);
        }
        // pthread_cond_wait: stp x29,x30,[sp,#-0x30]! ; str x21,[sp,#0x10] ;
        //                    stp x20,x19,[sp,#0x20] ; mov x29,sp
        for insn in [0xA9BD_7BFDu32, 0xF900_0BF5, 0xA902_4FF4, 0x9100_03FD] {
            assert!(!is_pc_relative(insn), "{:#010x} should be safe", insn);
        }
        // pthread_cond_timedwait: stp x29,x30,[sp,#-0x40]! ; str x23,[sp,#0x10] ;
        //                         stp x22,x21,[sp,#0x20] ; stp x20,x19,[sp,#0x30]
        for insn in [0xA9BC_7BFDu32, 0xF900_0BF7, 0xA902_57F6, 0xA903_4FF4] {
            assert!(!is_pc_relative(insn), "{:#010x} should be safe", insn);
        }
    }

    #[test]
    fn page_size_is_sane() {
        assert!(page_size().is_power_of_two());
        assert!(page_size() >= 4096);
    }

    #[test]
    fn hook_error_is_displayable() {
        let e = HookError::RelocationUnsafe { name: "b".into(), insn: 0x1400_0000 };
        assert!(format!("{}", e).contains("pc-relative"));
        let e = HookError::Unsupported("x86_64");
        assert!(format!("{}", e).contains("unsupported"));
    }
}
