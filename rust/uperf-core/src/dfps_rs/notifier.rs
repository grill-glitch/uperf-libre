//! Standalone notifier — write the current Hz to DFPS_NOTIFY_PATH.
//!
//! Split out from `task.rs` so the notifier module path stays flat
//! (`crate::dfps_rs::notifier`) whether the crate is built standalone
//! (in dfps-rewrite) or mounted as a subtree (in uperf-rewrite).

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use super::DFPS_NOTIFY_PATH;

/// O_WRONLY | O_NONBLOCK | O_CLOEXEC | O_CREAT | O_TRUNC — matches upstream
/// dynamic_fps.cpp:323 byte-for-byte (Linux-specific flags).
pub fn write_cur_hz(hz: i32) -> std::io::Result<()> {
    let path = Path::new(DFPS_NOTIFY_PATH);
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    f.write_all(hz.to_string().as_bytes())?;
    Ok(())
}

/// M1 hook for tests: write the current hz to the notify path.
#[cfg(test)]
pub fn write_task(task: &super::DfpsTask) -> std::io::Result<()> {
    let hz = task.cur_hz().unwrap_or(-1);
    write_cur_hz(hz)
}