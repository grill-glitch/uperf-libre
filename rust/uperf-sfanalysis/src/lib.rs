//! `libsfanalysis_rs` — Rust rewrite of yc9559/uperf's `libsfanalysis.so`.
//!
//! Injected into `surfaceflinger` via `patchelf --add-needed` (see
//! `magisk/customize.sh`). Hooks `xh_refresh_loop` inside `libandroidfw.so`
//! by mprotect + inline patch, and writes a single byte to
//! `<USER_PATH>/sfanalysis.hint` on every call. Byte protocol: see
//! `docs/spec/sfanalysis.md §2.3` and AGENT.md §7.4 / §12.1.
//!
//! Layout:
//!   hook.rs — locate libandroidfw.so in /proc/self/maps, mprotect, patch
//!   fsm.rs  — SfHint 6-value FSM (idle/switch/trigger/gesture/touch/junk)
//!   sink.rs — open + truncate + write single byte
//!   lib.rs  — public surface + init entry point
//!
//! The crate is `std` (cdylib always pulls in libc anyway). All syscalls
//! have an error path; nothing panics on a real device. We set a thread
//! name via `prctl` so logcat shows "uperf-sfanalysis" instead of the
//! surfaceflinger-inherited name.
//!
//! Tests live in each submodule and run on the host under `cargo test`.
//! The host has no `libandroidfw.so`; tests inject a fake module under
//! `UPERF_FAKE_ROOT/self_maps` and `UPERF_FAKE_ROOT/lib` to exercise the
//! parser without doing real mprotect on the build machine.

#![allow(non_snake_case, non_camel_case_types)]

mod fsm;
mod hook;
mod sink;

pub use fsm::SfHint;

/// Hint file name, written next to `<USER_PATH>/uperf.json`. Path is
/// supplied by the consumer (`SfAnalysisListener` in `uperf-core`), but
/// `magisk/customize.sh` also exports `UPERF_SF_HINT_FILE` so the producer
/// and consumer agree without an IPC handshake.
pub const SF_HINT_FILE: &str = "sfanalysis.hint";

/// Target function name in `libandroidfw.so`. From vendor `.rodata`:
/// `xh_refresh_loop` (m1-static-reverse.md §1.5 + m8-sfanalysis-reverse §1.2).
pub const HOOK_TARGET_SYM: &str = "xh_refresh_loop";

/// `ctor`-equivalent entry point. `patchelf` puts `libsfanalysis_rs.so` in
/// surfaceflinger's DT_NEEDED, and the dynamic loader runs the library's
/// `DT_INIT` array on load. We register our installer into `.init_array`
/// so it runs **before** surfaceflinger's main thread starts the renderer,
/// giving us the right window to mprotect + patch `xh_refresh_loop`.
///
/// `#[link_section = ".init_array"]` plus `#[used]` makes the symbol survive
/// `--gc-sections`. The leading byte in the section is conventionally a
/// 16-bit priority — `0xffff` = "highest priority, run first".
/// On surfaceflinger startup, this triggers:
///   1. regcomp a benign regex (debug aid)
///   2. mprotect + inline patch xh_refresh_loop
///   3. from then on, every call to xh_refresh_loop runs our handler
///      which writes a SfHint byte to `<hint>`.
#[used]
#[link_section = ".init_array"]
static _INIT_CTOR: unsafe extern "C" fn() -> std::os::raw::c_int = {
    unsafe extern "C" fn wrapper() -> std::os::raw::c_int {
        if let Err(e) = hook::install(HOOK_TARGET_SYM) {
            eprintln!("uperf-sfanalysis: hook install failed: {}", e);
            return 1;
        }
        0
    }
    wrapper
};

// We don't define our own `_init` / `_fini` symbols because the C runtime
// already provides them via `crti.o`. Instead the `.init_array` entry above
// is what runs on library load. We keep `sfhint_install` and
// `sfhint_remove` as the public Rust names for completeness.
#[no_mangle]
pub extern "C" fn sfhint_install() -> std::os::raw::c_int {
    if let Err(e) = hook::install(HOOK_TARGET_SYM) {
        eprintln!("uperf-sfanalysis: hook install failed: {}", e);
        return 1;
    }
    0
}

#[no_mangle]
pub extern "C" fn sfhint_remove() {
    hook::uninstall();
}

/// FFI entry the trampoline jumps to. Original `xh_refresh_loop` body is
/// called first via the trampoline, so we don't need to replicate its
/// semantics — we only observe that it ran and write the byte.
#[no_mangle]
pub unsafe extern "C" fn sfhint_handler(arg: *mut std::ffi::c_void) {
    let _ = arg;
    let hint = fsm::next_hint();
    if let Some(path) = sink::resolve_path() {
        let _ = sink::write_byte(&path, hint as u8);
    }
}

/// Optional C entry point: query the current hint without driving the FSM.
/// Useful for tests and for `SfAnalysisListener`'s dry-run mode.
#[no_mangle]
pub extern "C" fn sfhint_current() -> u8 {
    fsm::current_hint() as u8
}

/// Patch the trampoline back. Provided so a future `uninstall` could exist,
/// but `surfaceflinger` never dlclose's us — kept for symmetry.
#[no_mangle]
pub extern "C" fn _fini_unused() {
    hook::uninstall();
}

/// Optional name helper for diagnostics / logcat. Not strictly needed.
#[no_mangle]
pub extern "C" fn sfhint_name(b: u8, buf: *mut u8, len: usize) -> usize {
    use std::ffi::CStr;
    let name = fsm::SfHint::from_byte(b).as_str();
    let bytes = name.as_bytes();
    let n = bytes.len().min(len.saturating_sub(1));
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
    let _ = CStr::from_bytes_with_nul; // keep import live
    n
}

