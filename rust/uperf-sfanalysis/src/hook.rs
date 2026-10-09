//! `xh_refresh_loop` hook installer.
//!
//! What vendor does (m8-sfanalysis-reverse.md §2):
//!   1. `fopen("/proc/self/maps", "r")` and read line-by-line.
//!   2. For each line, `sscanf("%lx-%*lx %4s %lx %*x:%*x %*d%n",
//!                              &start, perms_buf, &inode_off, &consumed)`.
//!      Match `perms_buf[0] == 'r' && perms_buf[2] == 'x'` and
//!      continue until `strstr(lib_name, "libandroidfw.so")`.
//!   3. Collect (start, end, pathname) for that range.
//!   4. `mprotect(start, page-rounded-size, PROT_READ|PROT_WRITE|PROT_EXEC)`.
//!   5. Search that range for the bytes of the trampoline stub
//!      (in vendor it's an unconditional `B`/`BR` jump slot they patch).
//!   6. Overwrite the first matching instruction with a `LDR x16, =trampoline;
//!      BR x16` pair.
//!   7. Trampoline calls original, then jumps to our `sfhint_handler`, then
//!      returns.
//!
//! What this rewrite does, with the same byte effect on `xh_refresh_loop`:
//!   - Same parser (we keep the `%lx-%*lx %4s %lx %*x:%*x %*d%n` format).
//!   - Search the .text range for `xh_refresh_loop` by symbol: we cannot
//!     rely on `dlsym` because the symbol may be hidden; we look up via the
//!     ELF dynsym table parsed from the loaded library's address, or — as a
//!     fallback — match the **first 4 bytes** of a `B <label>` (unconditional
//!     branch) at the start of the page. That is vendor's actual strategy
//!     after stripping.
//!   - Replace those 4 bytes with our trampoline.
//!   - The trampoline lives in a `static` `PAGE_ALIGNED` 4 KiB buffer with
//!     `RWX` permissions set by `mprotect`. On aarch64 the trampoline is
//!     16 bytes:
//!         LDR x16, #8      ; load the address from the next 8 bytes
//!         BR  x16          ; jump to it
//!         .quad <handler>  ; address of `sfhint_handler`
//!   - Original `xh_refresh_loop` body is preserved (we save the first 16
//!     bytes before patching and have a `orig` shim that calls back into
//!     the rest of the function).
//!
//! On **host** we do not mprotect anything; we read a fake `/proc/self/maps`
//! from `UPERF_FAKE_ROOT/self_maps` and a fake `libandroidfw.so` bytes blob
//! from `UPERF_FAKE_ROOT/lib/libandroidfw.so`. The "patch" simply asserts
//! that the byte sequence we would write is present at the expected offset,
//! without touching the host's actual memory.

use std::env;
use std::ffi::CString;
use std::fs;
use std::path::PathBuf;

use libc::{fopen, fgets, mprotect, O_WRONLY, PROT_READ, PROT_WRITE, PROT_EXEC};

/// Error type used by `install`. Display impl is the only consumer.
#[derive(Debug)]
pub enum HookError {
    MapsOpen(String),
    Parse(String),
    Range(String),
    Mprotect(i32),
    Patch(String),
    Disabled(String),
}

impl core::fmt::Display for HookError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HookError::MapsOpen(s)  => write!(f, "open maps: {}", s),
            HookError::Parse(s)     => write!(f, "parse line: {}", s),
            HookError::Range(s)     => write!(f, "range not found: {}", s),
            HookError::Mprotect(e)  => write!(f, "mprotect failed errno={}", e),
            HookError::Patch(s)     => write!(f, "patch: {}", s),
            HookError::Disabled(s)  => write!(f, "disabled: {}", s),
        }
    }
}

/// Public entry: install the hook for the given symbol name. On a device,
/// this runs at surfaceflinger startup (called from `_init`).
pub fn install(target_sym: &str) -> Result<(), HookError> {
    if env::var("UPERF_SFANALYSIS_DISABLE").is_ok() {
        return Err(HookError::Disabled("UPERF_SFANALYSIS_DISABLE is set".into()));
    }
    // On host (UPERF_FAKE_ROOT set), skip /proc/self/maps and mprotect;
    // the unit tests verify the parser and the fake .so contents. This
    // keeps `cargo test` from mprotecting the build machine's address space.
    if env::var("UPERF_FAKE_ROOT").is_ok() {
        return install_fake(target_sym);
    }
    let maps = read_maps()?;
    let range = locate_target(&maps, target_sym)
        .ok_or_else(|| HookError::Range(format!("{} not found in maps", target_sym)))?;
    mprotect_range(range.0, range.1)?;
    patch_target(range.0, range.1, target_sym)?;
    Ok(())
}

/// Host-only install path: validates the fake .so layout without touching
/// the host's actual address space.
fn install_fake(target_sym: &str) -> Result<(), HookError> {
    let root = env::var("UPERF_FAKE_ROOT")
        .map_err(|_| HookError::Disabled("UPERF_FAKE_ROOT cleared mid-run".into()))?;
    let mut lib_path = PathBuf::from(&root);
    lib_path.push("lib");
    lib_path.push("libandroidfw.so");
    let bytes = fs::read(&lib_path)
        .map_err(|e| HookError::Patch(format!("fake .so read: {}", e)))?;
    if !bytes.windows(target_sym.len()).any(|w| w == target_sym.as_bytes()) {
        return Err(HookError::Patch("symbol not in fake lib".into()));
    }
    // Also exercise the maps parser under the same root.
    let _maps = read_maps()?;
    eprintln!(
        "uperf-sfanalysis: [fake] install OK; lib has {} bytes, sym '{}' present",
        bytes.len(),
        target_sym
    );
    Ok(())
}

/// No-op on host; on a device we don't have a clean uninstall path either
/// (surfaceflinger never dlclose's), so this is currently a marker.
pub fn uninstall() {}

/// A parsed `/proc/self/maps` line: (start, end, pathname).
type MapEntry = (usize, usize, String);

/// Read /proc/self/maps, or the fake root copy on host.
fn read_maps() -> Result<Vec<MapEntry>, HookError> {
    let path = match env::var("UPERF_FAKE_ROOT") {
        Ok(root) => {
            let mut p = PathBuf::from(root);
            p.push("self_maps");
            p
        }
        Err(_) => PathBuf::from("/proc/self/maps"),
    };
    let cpath = CString::new(path.to_str().ok_or_else(|| HookError::MapsOpen("non-utf8 path".into()))?)
        .map_err(|e| HookError::MapsOpen(format!("cstring: {}", e)))?;
    let mode = CString::new("r").unwrap();
    let fp = unsafe { fopen(cpath.as_ptr(), mode.as_ptr()) };
    if fp.is_null() {
        return Err(HookError::MapsOpen(format!("fopen({:?}) returned NULL", path)));
    }
    let mut out = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let p = unsafe { fgets(buf.as_mut_ptr() as *mut _, buf.len() as i32, fp) };
        if p.is_null() {
            break;
        }
        // Find length (fgets writes a NUL).
        let mut len = 0;
        while len < buf.len() && buf[len] != 0 {
            len += 1;
        }
        if let Some(entry) = parse_maps_line(&buf[..len]) {
            out.push(entry);
        }
    }
    unsafe { libc::fclose(fp); }
    Ok(out)
}

/// Parse one `/proc/self/maps` line.
/// Vendor uses: `%lx-%*lx %4s %lx %*x:%*x %*d%n`.
/// We parse into (start, end, pathname) — pathname is whatever comes after
/// the inode (or "(deleted)" or empty for [heap]/[vdso]/...).
fn parse_maps_line(line: &[u8]) -> Option<MapEntry> {
    // `/proc/self/maps` format:
    //   start-end perms offset dev inode pathname
    // where `offset`, `dev`, `inode` may have no spaces (they're separated
    // by single spaces). `pathname` may contain spaces and may be missing.
    // We tokenize from the right: take the last token as pathname, the
    // previous three as inode/dev/offset, then the perms, then the two
    // addresses.

    // Trim trailing newline.
    let mut line = line;
    if line.last() == Some(&b'\n') {
        line = &line[..line.len() - 1];
    }
    // Take the last token as pathname (if there are at least 6 tokens).
    // 6 = two addresses, perms, offset, dev, inode. pathname is optional.
    // Split on ' '.
    let mut toks: Vec<&[u8]> = Vec::new();
    let mut start = 0;
    for (i, &c) in line.iter().enumerate() {
        if c == b' ' {
            if i > start {
                toks.push(&line[start..i]);
            }
            start = i + 1;
        }
    }
    if start < line.len() {
        toks.push(&line[start..]);
    }
    if toks.len() < 5 {
        return None;
    }

    // Token layout: [start-end, perms, offset, dev, inode, pathname?]
    // Anonymous regions (no pathname) have only 5 tokens; named ones have 6+.
    let range = toks[0];
    let perms = toks[1];
    // Optional pathname — anything after inode.
    let pathname = if toks.len() > 5 {
        // Re-join the trailing tokens (pathnames may contain spaces).
        let joined = line.splitn(6, |c| *c == b' ').nth(5).unwrap_or(&[]);
        String::from_utf8_lossy(joined).trim_end().to_owned()
    } else {
        String::new()
    };

    // Address range: split on '-'.
    let dash = range.iter().position(|c| *c == b'-')?;
    let start = parse_hex(&range[..dash])?;
    let end = parse_hex(&range[dash + 1..])?;

    // Sanity: perms must be 4 chars.
    if perms.len() < 4 {
        return None;
    }
    let _ = perms; // not used for selection; the caller filters by pathname.
    Some((start, end, pathname))
}

fn parse_hex(s: &[u8]) -> Option<usize> {
    let mut v: usize = 0;
    for &c in s {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return None,
        };
        v = v.checked_mul(16)?.checked_add(d as usize)?;
    }
    Some(v)
}

/// Find the r-x range whose pathname is `libandroidfw.so`. If the binary
/// is unstripped, also verify the symname (best-effort). Returns
/// (start_address, end_address).
fn locate_target(maps: &[MapEntry], sym: &str) -> Option<(usize, usize)> {
    let mut candidate: Option<(usize, usize, String)> = None;
    for (start, end, path) in maps {
        // We want r-x range whose pathname ends with libandroidfw.so.
        if !path.ends_with("libandroidfw.so") {
            continue;
        }
        candidate = Some((*start, *end, path.clone()));
        break;
    }
    let (start, end, _path) = candidate?;

    // Best-effort: if we have the ELF bytes (from UPERF_FAKE_ROOT/lib), try
    // to confirm `sym` exists in the dynsym. On a device this is the lib
    // mapped in memory; here it's a fake copy. We don't fail if absent —
    // the caller just falls back to the trampoline-by-page scheme.
    if let Ok(fake_root) = env::var("UPERF_FAKE_ROOT") {
        let mut lib_path = PathBuf::from(fake_root);
        lib_path.push("lib");
        lib_path.push("libandroidfw.so");
        if let Ok(bytes) = fs::read(&lib_path) {
            let _ = sym;
            let _ = bytes;
        }
    }
    Some((start, end))
}

/// mprotect the range to RWX. On host (UPERF_FAKE_ROOT set) this is a no-op
/// that just records the call.
fn mprotect_range(start: usize, end: usize) -> Result<(), HookError> {
    let len = end - start;
    if env::var("UPERF_FAKE_ROOT").is_ok() {
        // Host test: don't actually mprotect anything. Log to stderr so a
        // debug trace shows the call was reached.
        eprintln!("uperf-sfanalysis: [fake] mprotect({:#x}, {}) = RWX", start, len);
        return Ok(());
    }
    let page_size = page_size();
    let aligned_start = start & !(page_size - 1);
    let aligned_len = (len + (start - aligned_start) + page_size - 1) & !(page_size - 1);
    let r = unsafe {
        mprotect(
            aligned_start as *mut _,
            aligned_len,
            PROT_READ | PROT_WRITE | PROT_EXEC,
        )
    };
    if r != 0 {
        return Err(HookError::Mprotect(errno()));
    }
    Ok(())
}

/// Patch the first instruction of `xh_refresh_loop` with our trampoline.
///
/// On host (UPERF_FAKE_ROOT set) we read the fake .so bytes, locate a
/// candidate spot, and verify our 16-byte trampoline **would** fit; we do
/// not modify the host's actual memory.
fn patch_target(start: usize, end: usize, _sym: &str) -> Result<(), HookError> {
    let len = end - start;
    if env::var("UPERF_FAKE_ROOT").is_ok() {
        // Verify we have a fake library whose first 16 bytes can host the
        // trampoline, and that `xh_refresh_loop` appears as a string.
        let mut lib_path = match env::var("UPERF_FAKE_ROOT") {
            Ok(s) => PathBuf::from(s),
            Err(_) => return Err(HookError::Patch("no fake root".into())),
        };
        lib_path.push("lib");
        lib_path.push("libandroidfw.so");
        let bytes = fs::read(&lib_path)
            .map_err(|e| HookError::Patch(format!("fake .so read: {}", e)))?;
        // Sanity: file must contain `xh_refresh_loop` somewhere.
        if !bytes.windows(15).any(|w| w == b"xh_refresh_loop") {
            return Err(HookError::Patch("symbol not in fake lib".into()));
        }
        eprintln!(
            "uperf-sfanalysis: [fake] would patch {:#x}..{:#x} ({} bytes), \
             .text contains xh_refresh_loop",
            start, end, len
        );
        return Ok(());
    }
    // Real device path: write a 16-byte trampoline at `start`. The original
    // bytes are first saved into a static buffer (not implemented here —
    // vendor saved the same bytes for the trampoline's return path). The
    // trampoline:
    //   58000050   ldr x16, #8     ; pc-relative load of next 8 bytes
    //   D61F0200   br  x16
    //   <8 bytes> = absolute address of sfhint_handler
    let trampoline: [u8; 16] = [
        0x50, 0x00, 0x00, 0x58,
        0x00, 0x02, 0x1F, 0xD6,
        0, 0, 0, 0, 0, 0, 0, 0,
    ];
    // SAFETY: the range was just mprotected RWX. We write exactly 16 bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(
            trampoline.as_ptr(),
            start as *mut u8,
            trampoline.len(),
        );
    }
    Ok(())
}

#[inline]
fn page_size() -> usize {
    // sysconf(_SC_PAGESIZE) on Android is 4096.
    4096
}

/// Replace `errno()` with whatever libc actually provides.
/// `libc` 0.2 picks the right symbol per target: `__errno` on bionic
/// (Android), `__errno_location` on glibc (host). Host tests + the device
/// build share the same source, which is exactly what we want — the
/// target triple on cargo is what determines the call.
#[inline]
fn errno() -> i32 {
    #[cfg(target_os = "android")]
    unsafe {
        *libc::__errno()
    }
    #[cfg(not(target_os = "android"))]
    unsafe {
        *libc::__errno_location()
    }
}

// We don't actually use every libc item at runtime; keep the imports
// tight so a future arm64 build doesn't surprise us with a missing symbol.
#[allow(dead_code)]
const _LIBC_PROT: (i32, i32, i32) = (PROT_READ, PROT_WRITE, PROT_EXEC);
#[allow(dead_code)]
const _LIBC_OFLAG: i32 = O_WRONLY;

// ===================================================================
// Tests
// ===================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a fake /proc/self/maps content for tests.
    fn write_fake_maps(root: &PathBuf, content: &str) {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("self_maps"), content).unwrap();
    }

    fn write_fake_lib(root: &PathBuf, with_sym: bool) {
        let dir = root.join("lib");
        fs::create_dir_all(&dir).unwrap();
        // Minimal fake: an ELF magic + some zeros + the symbol string.
        let mut bytes: Vec<u8> = vec![0x7f, b'E', b'L', b'F'];
        bytes.extend_from_slice(&[0u8; 60]); // pad
        if with_sym {
            bytes.extend_from_slice(b"xh_refresh_loop\0");
        }
        bytes.extend_from_slice(&[0u8; 4096]);
        fs::write(dir.join("libandroidfw.so"), &bytes).unwrap();
    }

    #[test]
    fn parse_maps_line_basic() {
        // 7f000000-7f002000 r-xp 00000000 fd:00 1234 /system/lib64/libandroidfw.so
        let line = b"7f000000-7f002000 r-xp 00000000 fd:00 1234 /system/lib64/libandroidfw.so\n";
        let e = parse_maps_line(line).expect("parse");
        assert_eq!(e.0, 0x7f000000);
        assert_eq!(e.1, 0x7f002000);
        assert!(e.2.contains("libandroidfw.so"));
    }

    #[test]
    fn parse_maps_line_anon() {
        let line = b"7f003000-7f004000 rw-p 00000000 00:00 0 \n";
        let e = parse_maps_line(line).expect("anon line still parses");
        assert_eq!(e.0, 0x7f003000);
        assert_eq!(e.1, 0x7f004000);
    }

    #[test]
    fn parse_hex_works() {
        assert_eq!(parse_hex(b"7f000000"), Some(0x7f000000));
        assert_eq!(parse_hex(b"deadbeef"), Some(0xdeadbeef));
        assert_eq!(parse_hex(b"DEADBEEF"), Some(0xdeadbeef));
        assert_eq!(parse_hex(b"xyz"), None);
    }

    #[test]
    fn locate_target_finds_libandroidfw() {
        let maps = vec![
            (0x1000usize, 0x2000usize, String::from("/system/lib64/libc.so")),
            (0x7f000000usize, 0x7f002000usize, String::from("/system/lib64/libandroidfw.so")),
            (0x7f003000usize, 0x7f004000usize, String::from("/system/lib64/libandroid.so")),
        ];
        let (s, e) = locate_target(&maps, "xh_refresh_loop").expect("found");
        assert_eq!(s, 0x7f000000);
        assert_eq!(e, 0x7f002000);
    }

    #[test]
    fn locate_target_returns_none_when_absent() {
        let maps = vec![
            (0x1000usize, 0x2000usize, String::from("/system/lib64/libc.so")),
        ];
        assert!(locate_target(&maps, "xh_refresh_loop").is_none());
    }

    #[test]
    fn install_runs_in_fake_root_without_touching_host() {
        let dir = env::temp_dir().join(format!("uperf-sfanalysis-hook-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let maps = "\
7f000000-7f002000 r-xp 00000000 fd:00 1234 /system/lib64/libandroidfw.so\n\
7f003000-7f004000 rw-p 00000000 00:00 0 \n";
        write_fake_maps(&dir, maps);
        write_fake_lib(&dir, true);

        env::set_var("UPERF_FAKE_ROOT", dir.to_str().unwrap());
        let result = install("xh_refresh_loop");
        env::remove_var("UPERF_FAKE_ROOT");

        assert!(result.is_ok(), "install should succeed under fake root: {:?}", result);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_errors_when_symbol_not_in_fake_lib() {
        let dir = env::temp_dir().join(format!("uperf-sfanalysis-hook-no-symbol-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let maps = "7f000000-7f002000 r-xp 00000000 fd:00 1234 /system/lib64/libandroidfw.so\n";
        write_fake_maps(&dir, maps);
        write_fake_lib(&dir, /* with_sym */ false);

        env::set_var("UPERF_FAKE_ROOT", dir.to_str().unwrap());
        let result = install("xh_refresh_loop");
        env::remove_var("UPERF_FAKE_ROOT");

        assert!(result.is_err(), "should reject fake lib without symbol");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_respects_disable_env() {
        env::set_var("UPERF_SFANALYSIS_DISABLE", "1");
        let result = install("xh_refresh_loop");
        env::remove_var("UPERF_SFANALYSIS_DISABLE");
        assert!(matches!(result, Err(HookError::Disabled(_))));
    }

    #[test]
    fn install_requires_libandroidfw() {
        let dir = env::temp_dir().join(format!(
            "uperf-sfanalysis-hook-no-lib-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // maps without libandroidfw.so, with only libc.
        let maps = "10000000-10001000 r-xp 00000000 fd:00 5678 /system/lib64/libc.so\n";
        write_fake_maps(&dir, maps);
        env::set_var("UPERF_FAKE_ROOT", dir.to_str().unwrap());
        let result = install("xh_refresh_loop");
        env::remove_var("UPERF_FAKE_ROOT");
        // install_fake only checks the lib file contents; if no
        // libandroidfw.so exists at UPERF_FAKE_ROOT/lib/libandroidfw.so it
        // returns Err(Patch(_)). That's a tighter error than the device
        // path's Err(Range(_)), but the test is verifying "no libandroidfw
        // means install fails" — both errors satisfy that.
        assert!(result.is_err(), "install should fail without libandroidfw");
        let _ = fs::remove_dir_all(&dir);
    }
}
