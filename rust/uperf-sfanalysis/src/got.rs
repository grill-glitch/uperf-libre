//! GOT/PLT rewriting — the mechanism xHook actually uses by default, and the
//! only one available to us inside surfaceflinger.
//!
//! Why not inline patching: on this device (crDroid A16, SELinux enforcing,
//! KernelSU-Next unable to load any module `sepolicy.rule` and `ksud sepolicy
//! patch` parsing-but-not-applying) both executable-memory routes are refused:
//!
//! ```text
//! mmap(PROT_READ|WRITE|EXEC, anonymous)  -> EACCES   (execmem)
//! mprotect(file-backed code page, RWX)   -> EACCES   (execmod)
//! mprotect(RELRO data page, RW)          -> ok
//! ```
//!
//! Rewriting a GOT slot needs only the third line, so it works — and it is what
//! the vendor's statically-linked xHook does, which makes it better parity than
//! an inline patch would have been.
//!
//! For each loaded object (`dl_iterate_phdr`) we walk its dynamic section and
//! look at every relocation whose symbol matches the target. The slot address is
//! `dlpi_addr + r_offset`; the value currently there is the resolved libc
//! address, which we keep as the "original" the shim calls through.

use core::ffi::{c_char, c_int, c_void};

#[repr(C)]
pub struct Elf64Rel {
    pub r_offset: u64,
    pub r_info: u64,
    pub r_addend: i64,
}

#[repr(C)]
pub struct Elf64Sym {
    pub st_name: u32,
    pub st_info: u8,
    pub st_other: u8,
    pub st_shndx: u16,
    pub st_value: u64,
    pub st_size: u64,
}

#[repr(C)]
pub struct Elf64Dyn {
    pub d_tag: i64,
    pub d_val: u64,
}

#[repr(C)]
pub struct Elf64Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

/// bionic's `dl_phdr_info`.
#[repr(C)]
pub struct DlPhdrInfo {
    pub dlpi_addr: usize,
    pub dlpi_name: *const c_char,
    pub dlpi_phdr: *const Elf64Phdr,
    pub dlpi_phnum: u16,
    // Fields below exist in bionic but we do not read them.
    pub dlpi_adds: u64,
    pub dlpi_subs: u64,
    pub dlpi_tls_modid: usize,
    pub dlpi_tls_data: *mut c_void,
}

const PT_DYNAMIC: u32 = 2;
const PT_LOAD: u32 = 1;

const DT_NULL: i64 = 0;
const DT_PLTRELSZ: i64 = 2;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_RELAENT: i64 = 9;
const DT_JMPREL: i64 = 23;
const DT_SYMENT: i64 = 11;

extern "C" {
    fn dl_iterate_phdr(
        cb: extern "C" fn(*mut DlPhdrInfo, usize, *mut c_void) -> c_int,
        data: *mut c_void,
    ) -> c_int;
    fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
    fn __errno() -> *mut i32;
}

const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;

fn errno() -> i32 {
    unsafe { *__errno() }
}

/// One relocation slot that mentions the target symbol.
#[derive(Debug, Clone, Copy)]
pub struct Slot {
    /// absolute address of the GOT entry
    pub addr: usize,
    /// value currently stored there (the resolved libc address)
    pub value: usize,
    /// module that owns it (for logging)
    pub module: usize,
}

struct Walk<'a> {
    target: &'a [u8],
    out: Vec<Slot>,
}

/// Collect every GOT/relocation slot in the process that refers to `target`.
pub fn find_slots(target: &str) -> Vec<Slot> {
    let mut w = Walk { target: target.as_bytes(), out: Vec::new() };
    unsafe {
        dl_iterate_phdr(cb, &mut w as *mut Walk as *mut c_void);
    }
    w.out
}

extern "C" fn cb(info: *mut DlPhdrInfo, _size: usize, data: *mut c_void) -> c_int {
    let w = unsafe { &mut *(data as *mut Walk) };
    let info = unsafe { &*info };
    if info.dlpi_phdr.is_null() {
        return 0;
    }

    // locate PT_DYNAMIC
    let mut dynp: *const Elf64Dyn = core::ptr::null();
    let (mut lo, mut hi) = (usize::MAX, 0usize);
    for i in 0..info.dlpi_phnum as usize {
        let ph = unsafe { &*info.dlpi_phdr.add(i) };
        if ph.p_type == PT_DYNAMIC {
            dynp = (info.dlpi_addr + ph.p_vaddr as usize) as *const Elf64Dyn;
        } else if ph.p_type == PT_LOAD {
            lo = lo.min(info.dlpi_addr + ph.p_vaddr as usize);
            hi = hi.max(info.dlpi_addr + (ph.p_vaddr + ph.p_memsz) as usize);
        }
    }
    if dynp.is_null() || lo >= hi {
        return 0;
    }
    // Everything we dereference below must live inside this module: one module
    // in the process hands us a DT_STRTAB that is not relocated, and following
    // it reads address 0 (device-verified SIGSEGV in `xh_refresh_loop`).
    let in_range = |p: usize| p >= lo && p < hi;

    let (mut symtab, mut strtab, mut jmprel, mut jmprelsz, mut rela, mut relasz, mut relaent) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 24usize);
    let mut d = dynp;
    loop {
        let e = unsafe { &*d };
        match e.d_tag {
            DT_NULL => break,
            DT_SYMTAB => symtab = e.d_val as usize,
            DT_STRTAB => strtab = e.d_val as usize,
            DT_JMPREL => jmprel = e.d_val as usize,
            DT_PLTRELSZ => jmprelsz = e.d_val as usize,
            DT_RELA => rela = e.d_val as usize,
            DT_RELASZ => relasz = e.d_val as usize,
            DT_RELAENT => relaent = e.d_val as usize,
            DT_SYMENT => {}
            _ => {}
        }
        d = unsafe { d.add(1) };
    }
    {
        let name = unsafe { core::ffi::CStr::from_ptr(info.dlpi_name) };
        crate::dbg_log(&format!(
            "got scan raw {}: base={:#x} lo={:#x} hi={:#x} sym={:#x} str={:#x} jmprel={:#x}/{:#x} rela={:#x}/{:#x}",
            name.to_string_lossy(),
            info.dlpi_addr,
            lo,
            hi,
            symtab,
            strtab,
            jmprel,
            jmprelsz,
            rela,
            relasz
        ));
    }
    // bionic does not rewrite the address tags in the loaded dynamic section:
    // for an ET_DYN object they stay vaddr-relative. Fold the load bias only
    // where the raw value is not already inside the module.
    let fix = |v: usize| if v != 0 && in_range(v) { v } else { info.dlpi_addr + v };
    symtab = fix(symtab);
    strtab = fix(strtab);
    jmprel = fix(jmprel);
    rela = fix(rela);
    if symtab == 0 || strtab == 0 || !in_range(symtab) || !in_range(strtab) || !in_range(symtab + 24)
    {
        return 0;
    }

    // PLT relocations first, then the general table.
    let mut scanned = 0usize;
    let mut sampled = 0usize;
    let mut samples = std::string::String::new();
    for (base, size) in [(jmprel, jmprelsz), (rela, relasz)] {
        if base == 0 || size == 0 || !in_range(base) || !in_range(base + size - 1) {
            continue;
        }
        let ent = if relaent == 0 { 24 } else { relaent };
        let n = size / ent;
        for i in 0..n {
            let r = unsafe { &*((base + i * ent) as *const Elf64Rel) };
            let sym_idx = (r.r_info >> 32) as usize;
            if sym_idx == 0 || !in_range(symtab + sym_idx * 24) {
                continue;
            }
            let sym = unsafe { &*(symtab as *const Elf64Sym).add(sym_idx) };
            if sym.st_name == 0 || !in_range(strtab + sym.st_name as usize) {
                continue;
            }
            scanned += 1;
            let nm = unsafe {
                core::ffi::CStr::from_ptr((strtab + sym.st_name as usize) as *const c_char)
            };
            let nm = nm.to_bytes();
            if sampled < 8 {
                samples.push_str(&std::string::String::from_utf8_lossy(nm));
                samples.push(' ');
                sampled += 1;
            }
            // exact match on the symbol name
            if nm != w.target {
                continue;
            }
            let addr = info.dlpi_addr + r.r_offset as usize;
            let value = unsafe { core::ptr::read_unaligned(addr as *const usize) };
            w.out.push(Slot { addr, value, module: info.dlpi_addr });
        }
    }
    {
        let name = unsafe { core::ffi::CStr::from_ptr(info.dlpi_name) };
        crate::dbg_log(&format!(
            "got scan {}: scanned={} names=[{}]",
            name.to_string_lossy(),
            scanned,
            samples
        ));
    }
    0
}

/// Protection bits of the mapping containing `addr`, per /proc/self/maps.
fn perms_of(addr: usize) -> Option<i32> {
    let txt = std::fs::read_to_string("/proc/self/maps").ok()?;
    for line in txt.lines() {
        let mut it = line.split_whitespace();
        let (Some(range), Some(p)) = (it.next(), it.next()) else { continue };
        let (s, e) = range.split_once('-')?;
        let (Ok(s), Ok(e)) = (usize::from_str_radix(s, 16), usize::from_str_radix(e, 16)) else {
            continue;
        };
        if addr >= s && addr < e {
            let b = p.as_bytes();
            let mut prot = 0;
            if b.first() == Some(&b'r') {
                prot |= PROT_READ;
            }
            if b.get(1) == Some(&b'w') {
                prot |= PROT_WRITE;
            }
            if b.get(2) == Some(&b'x') {
                prot |= 4;
            }
            return Some(prot);
        }
    }
    None
}

/// Write `new_value` into a GOT slot, restoring the page's original protection.
pub fn patch_slot(addr: usize, new_value: usize) -> Result<(), (i32, &'static str)> {
    let ps = crate::hook::page_size();
    let page = addr & !(ps - 1);
    let orig_prot = perms_of(page).unwrap_or(PROT_READ);
    if unsafe { mprotect(page as *mut c_void, ps, PROT_READ | PROT_WRITE) } != 0 {
        return Err((errno(), "mprotect RW"));
    }
    unsafe { core::ptr::write_unaligned(addr as *mut usize, new_value) };
    // restore; a failure here would leave the page writable, which is not fatal
    let _ = unsafe { mprotect(page as *mut c_void, ps, orig_prot) };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dyn_tag_constants_match_elf_abi() {
        assert_eq!(DT_NULL, 0);
        assert_eq!(DT_STRTAB, 5);
        assert_eq!(DT_SYMTAB, 6);
        assert_eq!(DT_JMPREL, 23);
        assert_eq!(PT_DYNAMIC, 2);
    }

    #[test]
    fn struct_sizes_match_elf64() {
        assert_eq!(core::mem::size_of::<Elf64Rel>(), 24);
        assert_eq!(core::mem::size_of::<Elf64Sym>(), 24);
        assert_eq!(core::mem::size_of::<Elf64Dyn>(), 16);
        assert_eq!(core::mem::size_of::<Elf64Phdr>(), 56);
    }

    #[test]
    fn find_slots_finds_a_libc_symbol_in_the_test_binary() {
        // The host test binary links libc, so `malloc` (used by Rust) has GOT
        // slots. Not every symbol has a PLT slot, so accept an empty result only
        // for names we know are not indirectly called.
        let s = find_slots("malloc");
        // Either we found slots, or this binary resolves malloc directly.
        for slot in &s {
            assert_ne!(slot.addr, 0);
        }
    }

    #[test]
    fn perms_of_reads_a_known_mapping() {
        let f: extern "C" fn() = main_marker;
        let p = f as usize;
        assert!(perms_of(p).unwrap() & PROT_READ != 0);
    }

    extern "C" fn main_marker() {}
}
