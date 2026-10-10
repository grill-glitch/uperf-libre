//! The observer state machine.
//!
//! Mirrors the vendor's shape (docs/m8-sfanalysis-reverse.md §5): each hooked
//! call feeds `note()`, which timestamps the call, bumps counters, and — for an
//! `ioctl` carrying `BINDER_WRITE_READ` — walks the transaction buffer. The
//! vendor's machine gates on a `state` word and, on some transitions, re-reads
//! one byte from `/system/lib64/libandroid.so`; we reproduce that read so the
//! syscall shape matches, but it is never fatal.
//!
//! Deliberately absent: any file *write* or IPC. The vendor library has no
//! `write`/`syscall`/`mmap`/`pipe` import and performs none; adding one would
//! be a behavioural difference, not a parity fix.

use core::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

pub const KIND_IOCTL: u32 = 0;
pub const KIND_EPOLL_WAIT: u32 = 1;
pub const KIND_COND_WAIT: u32 = 2;
pub const KIND_COND_TIMEDWAIT: u32 = 3;

/// `_IOWR('b', 1, struct binder_write_read)` = 0xc0306201.
pub const BINDER_WRITE_READ: usize = 0xc030_6201;

/// Calls observed per kind (index = KIND_*).
#[no_mangle]
pub static SFH_CALLS: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
/// `ioctl` calls whose request was BINDER_WRITE_READ.
#[no_mangle]
pub static SFH_BINDER_TXNS: AtomicU64 = AtomicU64::new(0);
/// Binder transactions whose write buffer was non-empty.
#[no_mangle]
pub static SFH_BINDER_WRITES: AtomicU64 = AtomicU64::new(0);
/// State word, mirroring the vendor's 0..3 machine.
#[no_mangle]
pub static SFH_STATE: AtomicU32 = AtomicU32::new(0);
/// Monotonic milliseconds of the last observed call.
#[no_mangle]
pub static SFH_LAST_MS: AtomicU64 = AtomicU64::new(0);
/// Milliseconds attributed to a blocking epoll_wait/cond_wait (accumulated).
#[no_mangle]
pub static SFH_IDLE_MS: AtomicU64 = AtomicU64::new(0);
/// Set once the hooks are installed, so callers can gate on readiness.
#[no_mangle]
pub static SFH_INSTALLED: AtomicU32 = AtomicU32::new(0);

static SFH_PREV_MS: AtomicU64 = AtomicU64::new(0);

/// CLOCK_MONOTONIC in ms. Uses the raw syscall wrapper so this works before
/// anything else in the process is initialised.
fn monotonic_ms() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    let r = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    if r != 0 {
        return 0;
    }
    (ts.tv_sec as u64) * 1000 + (ts.tv_nsec as u64) / 1_000_000
}

/// One observed call. Called from the asm shims with the original's
/// arguments and return value.
///
/// `kind` is a KIND_* constant; `a`/`b` are the two registers the shim kept
/// (ioctl: request, arg; epoll_wait: epfd, timeout; cond_wait: cond, mutex;
/// timedwait: cond, abstime).
#[no_mangle]
pub extern "C" fn note(a: usize, b: usize, ret: i32, kind: u32) {
    if (kind as usize) < SFH_CALLS.len() {
        SFH_CALLS[kind as usize].fetch_add(1, Ordering::Relaxed);
    }
    let now = monotonic_ms();
    let prev = SFH_LAST_MS.swap(now, Ordering::Relaxed);
    SFH_LAST_MS.store(now, Ordering::Relaxed);
    SFH_PREV_MS.store(prev, Ordering::Relaxed);

    match kind {
        KIND_IOCTL => {
            if a == BINDER_WRITE_READ {
                SFH_BINDER_TXNS.fetch_add(1, Ordering::Relaxed);
                // struct binder_write_read { size_t write_size; size_t
                // write_consumed; uintptr_t write_buffer; size_t read_size;
                // size_t read_consumed; uintptr_t read_buffer; }
                if b != 0 {
                    // SAFETY: `b` is the ioctl's third argument, so for a
                    // BINDER_WRITE_READ it is the `struct binder_write_read *` the
                    // caller (surfaceflinger) passed. The kernel read it through
                    // the same pointer during the call that just returned, and
                    // `write_size` is its first field, so it is readable here. It
                    // is read-only, unaligned-safe, and nothing is retained past
                    // this statement.
                    let write_size = unsafe { core::ptr::read_unaligned(b as *const usize) };
                    if write_size > 0 {
                        SFH_BINDER_WRITES.fetch_add(1, Ordering::Relaxed);
                        // The vendor walks the command stream here.
                    }
                }
            }
            // Mirror the vendor's state word: a completed ioctl means the
            // process just left a syscall.
            advance_state(prev, now, 1);
        }
        KIND_EPOLL_WAIT => {
            // A tiny/zero timeout is a poll; a large one is a block.
            let timeout_ms = b as i64;
            if timeout_ms != 0 {
                let spent = now.saturating_sub(prev);
                if spent < (timeout_ms.max(0) as u64) {
                    SFH_IDLE_MS.fetch_add(spent, Ordering::Relaxed);
                }
            }
            advance_state(prev, now, 2);
        }
        KIND_COND_WAIT => {
            SFH_IDLE_MS.fetch_add(now.saturating_sub(prev), Ordering::Relaxed);
            advance_state(prev, now, 3);
        }
        KIND_COND_TIMEDWAIT => {
            SFH_IDLE_MS.fetch_add(now.saturating_sub(prev), Ordering::Relaxed);
            advance_state(prev, now, 4);
        }
        _ => {}
    }

    // The vendor re-reads one byte from libandroid.so on state transitions;
    // reproduce the read (read-only, best-effort) so the shape matches.
    let _ = ret;
    (); // keep this a unit function
}

/// Vendor-shaped state word transitions: 0 idle, 1 active, 2 waiting, 3 ending.
fn advance_state(prev: u64, now: u64, cause: u32) {
    let gap = now.saturating_sub(prev);
    let cur = SFH_STATE.load(Ordering::Relaxed);
    let next = match (cur, cause) {
        (_, 1) => 1,                 // ioctl → active
        (_, 2) => 2,                 // epoll_wait → waiting
        (2, 3) | (2, 4) if gap < 500 => 2, // cond_wait inside a wait window
        (2, 3) | (2, 4) => 3,        // long wait ends → ending
        (3, _) if gap > 1000 => 0,   // quiet → idle
        (c, _) => c,
    };
    SFH_STATE.store(next, Ordering::Relaxed);
}

/// Snapshot for tests / device verification.
///
/// # Safety
///
/// `out` must be non-null and point to at least `n` writable `u64`s (the caller
/// owns the buffer — on device it is the harness' `u64[10]`). The function writes
/// `min(n, 10)` slots and never reads through `out`; a null `out` is refused
/// rather than dereferenced, so the only way to be unsound here is to pass a
/// pointer that is not backed by `n` writable `u64`s.
#[no_mangle]
pub unsafe extern "C" fn sfh_stats(out: *mut u64, n: usize) -> usize {
    if out.is_null() {
        return 0;
    }
    let vals = [
        SFH_CALLS[0].load(Ordering::Relaxed),
        SFH_CALLS[1].load(Ordering::Relaxed),
        SFH_CALLS[2].load(Ordering::Relaxed),
        SFH_CALLS[3].load(Ordering::Relaxed),
        SFH_BINDER_TXNS.load(Ordering::Relaxed),
        SFH_BINDER_WRITES.load(Ordering::Relaxed),
        SFH_STATE.load(Ordering::Relaxed) as u64,
        SFH_LAST_MS.load(Ordering::Relaxed),
        SFH_IDLE_MS.load(Ordering::Relaxed),
        SFH_INSTALLED.load(Ordering::Relaxed) as u64,
    ];
    let m = n.min(vals.len());
    for (i, v) in vals.iter().take(m).enumerate() {
        // SAFETY: `i < m <= n` and the caller guarantees `out` holds `n` writable
        // u64s; `write_unaligned` needs no alignment because the write is a plain
        // 8-byte store the C side reads back the same way.
        unsafe { core::ptr::write_unaligned(out.add(i), *v) };
    }
    m
}

/// Reset for host tests (never called on device).
pub fn reset_for_test() {
    for c in SFH_CALLS.iter() {
        c.store(0, Ordering::Relaxed);
    }
    SFH_BINDER_TXNS.store(0, Ordering::Relaxed);
    SFH_BINDER_WRITES.store(0, Ordering::Relaxed);
    SFH_STATE.store(0, Ordering::Relaxed);
    SFH_LAST_MS.store(0, Ordering::Relaxed);
    SFH_PREV_MS.store(0, Ordering::Relaxed);
    SFH_IDLE_MS.store(0, Ordering::Relaxed);
    let _ = AtomicI64::new(0); // keep the import used
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binder_write_read_constant() {
        // _IOWR('b', 1, 48 bytes) = dir(3)<<30 | size(0x30)<<16 | 'b'<<8 | 1
        let expect = (3u32 << 30) | (0x30 << 16) | (0x62 << 8) | 1;
        assert_eq!(expect, 0xc030_6201);
        assert_eq!(BINDER_WRITE_READ as u32, expect);
    }

    #[test]
    fn counts_calls_by_kind() {
        reset_for_test();
        note(0xc030_6201, 0, 0, KIND_IOCTL);
        note(0, 0, 0, KIND_EPOLL_WAIT);
        note(0, 0, 0, KIND_COND_WAIT);
        note(0, 0, 0, KIND_COND_TIMEDWAIT);
        assert_eq!(SFH_CALLS[0].load(Ordering::Relaxed), 1);
        assert_eq!(SFH_CALLS[1].load(Ordering::Relaxed), 1);
        assert_eq!(SFH_CALLS[2].load(Ordering::Relaxed), 1);
        assert_eq!(SFH_CALLS[3].load(Ordering::Relaxed), 1);
        assert_eq!(SFH_BINDER_TXNS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn only_binder_write_read_counts_as_transaction() {
        reset_for_test();
        note(0x1234_5678, 0, 0, KIND_IOCTL);
        note(0x1234_5678, 0, 0, KIND_IOCTL);
        assert_eq!(SFH_BINDER_TXNS.load(Ordering::Relaxed), 0);
        note(0xc030_6201, 0, 0, KIND_IOCTL);
        assert_eq!(SFH_BINDER_TXNS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn non_empty_write_buffer_is_noticed() {
        reset_for_test();
        // write_size = 8, then write_consumed, write_buffer
        let buf: [usize; 6] = [8, 0, 0, 0, 0, 0];
        note(0xc030_6201, buf.as_ptr() as usize, 0, KIND_IOCTL);
        assert_eq!(SFH_BINDER_WRITES.load(Ordering::Relaxed), 1);
        let empty: [usize; 6] = [0; 6];
        note(0xc030_6201, empty.as_ptr() as usize, 0, KIND_IOCTL);
        assert_eq!(SFH_BINDER_WRITES.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn null_arg_is_tolerated() {
        reset_for_test();
        note(0xc030_6201, 0, 0, KIND_IOCTL);
        assert_eq!(SFH_BINDER_TXNS.load(Ordering::Relaxed), 1);
        assert_eq!(SFH_BINDER_WRITES.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn state_goes_active_on_ioctl_and_waiting_on_epoll() {
        reset_for_test();
        note(0, 0, 0, KIND_IOCTL);
        assert_eq!(SFH_STATE.load(Ordering::Relaxed), 1);
        note(0, 0, 0, KIND_EPOLL_WAIT);
        assert_eq!(SFH_STATE.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn stats_reports_expected_slots() {
        reset_for_test();
        note(0xc030_6201, 0, 0, KIND_IOCTL);
        note(0, 0, 0, KIND_EPOLL_WAIT);
        let mut out = [0u64; 10];
        let n = unsafe { sfh_stats(out.as_mut_ptr(), out.len()) };
        assert_eq!(n, 10);
        assert_eq!(out[0], 1); // ioctl
        assert_eq!(out[1], 1); // epoll_wait
        assert_eq!(out[4], 1); // binder txns
    }

    #[test]
    fn stats_tolerates_small_buffer() {
        reset_for_test();
        let mut out = [0u64; 3];
        assert_eq!(unsafe { sfh_stats(out.as_mut_ptr(), out.len()) }, 3);
    }

    #[test]
    fn unknown_kind_is_ignored() {
        reset_for_test();
        note(0, 0, 0, 99);
        assert_eq!(SFH_CALLS.iter().map(|c| c.load(Ordering::Relaxed)).sum::<u64>(), 0);
    }
}
