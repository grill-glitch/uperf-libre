//! `libsfanalysis_rs` — behaviour-preserving replacement for the vendored
//! `libsfanalysis.so`.
//!
//! What the vendor library does (established by r2 + on-device strace,
//! docs/m8-sfanalysis-reverse.md §5/§5b):
//!
//!   1. injected into `surfaceflinger` via `patchelf --add-needed`;
//!   2. the ctor sets the calling thread to `SCHED_FIFO` priority 3, arms a
//!      POSIX timer whose handler names itself `DelayedWork`, and starts a
//!      worker thread named `xh_refresh_loop`;
//!   3. the worker sleeps 60 s, then repeatedly walks `/proc/self/maps`,
//!      `mprotect`s target pages RWX and patches the entry of four libc
//!      functions: `ioctl`, `epoll_wait`, `pthread_cond_wait`,
//!      `pthread_cond_timedwait`;
//!   4. each replacement calls the original, keeps its return value, feeds an
//!      observer (`ioctl` additionally decodes `BINDER_WRITE_READ`), and
//!      returns the original's value;
//!   5. it never writes a file and never opens an IPC channel.
//!
//! This crate reproduces 1–5. The only intentional deviations are the ones the
//! spec records: `dlsym` instead of a hand-rolled ELF dynsym walk, and the
//! maps scan filters on the `x` permission (the vendor's does not, which makes
//! it patch the ELF-header segment — see docs/m8-sfanalysis-reverse.md §0).
//!
//! Env (documented, not part of the vendor surface):
//!   UPERF_SFANALYSIS_DISABLE=1     do nothing
//!   UPERF_SFANALYSIS_DELAY_SECS=N  worker delay before the first install
//!                                  (default 60, matching the vendor)
//!   UPERF_SFANALYSIS_INTERVAL_SECS=N  re-apply period (default 60)

#![allow(non_snake_case, non_camel_case_types)]
// This crate is the FFI/loader-facing half of the injection library: it walks
// `dl_iterate_phdr`, rewrites relocation slots and calls libc through `dlsym`.
// The `unsafe` therefore cannot be removed — but it must stay minimal, remain
// inside an explicit block even inside an `unsafe fn`, and never become
// decorative (audit: `UNSAFE_AUDIT_REPORT.md`).
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(unused_unsafe)]

pub mod got;
pub mod hook;
pub mod observe;

use std::sync::atomic::Ordering;

/// State snapshot indices for `sfh_stats`.
pub const STAT_IOCTL: usize = 0;
pub const STAT_EPOLL_WAIT: usize = 1;
pub const STAT_COND_WAIT: usize = 2;
pub const STAT_COND_TIMEDWAIT: usize = 3;
pub const STAT_BINDER_TXNS: usize = 4;
pub const STAT_BINDER_WRITES: usize = 5;
pub const STAT_STATE: usize = 6;
pub const STAT_LAST_MS: usize = 7;
pub const STAT_IDLE_MS: usize = 8;
pub const STAT_INSTALLED: usize = 9;

fn env_flag(name: &str) -> bool {
    std::env::var(name).map(|v| v != "0" && !v.is_empty()).unwrap_or(false)
}

fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Debug sink. Appends to `<dir>/sfh.log` only when `UPERF_SFANALYSIS_DEBUG` is
/// set or `<dir>/sfh.debug` exists — the vendor is completely silent, so this
/// stays off by default and is a documented diagnostic deviation.
///
/// A file sink is required because surfaceflinger's stderr goes nowhere: init
/// does not forward it to logcat, so `eprintln!` is invisible in SF.
/// Is the diagnostic sink on? (env var, or the marker file next to the log)
pub(crate) fn debug_enabled() -> bool {
    if std::env::var("UPERF_SFANALYSIS_DEBUG").is_ok() {
        return true;
    }
    let dir = std::env::var("UPERF_SFANALYSIS_LOG_DIR")
        .unwrap_or_else(|_| String::from("/data/misc/surfaceflinger"));
    std::path::Path::new(&format!("{}/sfh.debug", dir)).exists()
}

pub(crate) fn dbg_log(msg: &str) {
    use std::io::Write;
    if !debug_enabled() {
        return;
    }
    let dir = std::env::var("UPERF_SFANALYSIS_LOG_DIR")
        .unwrap_or_else(|_| String::from("/data/misc/surfaceflinger"));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{}/sfh.log", dir))
    {
        let _ = writeln!(f, "{}", msg);
    }
}

/// Install all four hooks. Idempotent-ish: a second call re-patches, which is
/// what the vendor's refresh loop does.
fn install_all() -> usize {
    let mut ok = 0;
    for name in hook::TARGETS {
        match hook::install(name) {
            Ok(h) => {
                ok += 1;
                dbg_log(&format!(
                    "hooked {} slots={} orig={:#x}",
                    h.name, h.slots, h.orig
                ));
                eprintln!(
                    "uperf-sfanalysis: hooked {} ({} GOT slots, orig {:#x})",
                    h.name, h.slots, h.orig
                );
            }
            Err(e) => {
                // A slot we cannot patch must never abort the host process.
                dbg_log(&format!("NOT hooked {}: {}", name, e));
                eprintln!("uperf-sfanalysis: {} not hooked: {}", name, e);
            }
        }
    }
    if ok > 0 {
        observe::SFH_INSTALLED.store(1, Ordering::Relaxed);
    }
    ok
}

/// One-line snapshot of the observer counters, for the debug sink.
fn stats_line() -> String {
    use std::sync::atomic::Ordering as O;
    format!(
        "stats ioctl={} epoll={} cw={} ctw={} txns={} writes={} state={} idle_ms={} installed={}",
        observe::SFH_CALLS[0].load(O::Relaxed),
        observe::SFH_CALLS[1].load(O::Relaxed),
        observe::SFH_CALLS[2].load(O::Relaxed),
        observe::SFH_CALLS[3].load(O::Relaxed),
        observe::SFH_BINDER_TXNS.load(O::Relaxed),
        observe::SFH_BINDER_WRITES.load(O::Relaxed),
        observe::SFH_STATE.load(O::Relaxed),
        observe::SFH_IDLE_MS.load(O::Relaxed),
        observe::SFH_INSTALLED.load(O::Relaxed),
    )
}

/// Run the worker: delay, install, then re-apply on a period. Named
/// `xh_refresh_loop` to match the vendor's thread name.
fn worker(delay: u64, interval: u64) {
    std::thread::sleep(std::time::Duration::from_secs(delay));
    let n = install_all();
    if n == 0 {
        return;
    }
    dbg_log(&format!("install done: {}", stats_line()));
    if debug_enabled() {
        // Periodic evidence that the hooks are actually being *called* — the
        // install log alone only proves the GOT slots were rewritten.
        let _ = std::thread::Builder::new()
            .name("sfh_stats_loop".into())
            .spawn(|| loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                dbg_log(&stats_line());
            });
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(interval));
        install_all();
    }
}

/// Set the calling thread to SCHED_FIFO priority 3, matching the vendor ctor.
/// Failure is not fatal (an unprivileged process simply cannot).
fn set_realtime() {
    #[cfg(target_os = "android")]
    unsafe {
        let mut p: libc::sched_param = core::mem::zeroed();
        p.sched_priority = 3;
        let _ = libc::sched_setscheduler(0, libc::SCHED_FIFO, &p);
    }
}

/// Constructor. Registered in `.init_array` so it runs when the dynamic loader
/// loads the library into surfaceflinger.
extern "C" fn sfh_ctor() {
    if env_flag("UPERF_SFANALYSIS_DISABLE") {
        eprintln!("uperf-sfanalysis: disabled by env");
        return;
    }
    set_realtime();
    // Debug mode (env var or the marker file) installs immediately, so a device
    // round does not have to sit through the vendor's 60 s delay.
    let debug = std::env::var("UPERF_SFANALYSIS_DEBUG").is_ok()
        || std::path::Path::new("/data/misc/surfaceflinger/sfh.debug").exists();
    let default_delay = if debug { 0 } else { 60 };
    let delay = env_secs("UPERF_SFANALYSIS_DELAY_SECS", default_delay);
    let interval = env_secs("UPERF_SFANALYSIS_INTERVAL_SECS", 60);
    let _ = std::thread::Builder::new()
        .name("xh_refresh_loop".into())
        .spawn(move || worker(delay, interval));
}

#[used]
#[link_section = ".init_array"]
static SFH_INIT: extern "C" fn() = sfh_ctor;

/// Explicit install entry (used by the device harness and tests).
#[no_mangle]
pub extern "C" fn sfh_install() -> i32 {
    install_all() as i32
}
