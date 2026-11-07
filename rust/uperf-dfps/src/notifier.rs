//! Standalone notifier — write the current Hz to DFPS_NOTIFY_PATH.
//!
//! Split out from `task.rs` so the notifier module path stays flat
//! (`crate::notifier`) whether the crate is built standalone
//! (in dfps-rewrite) or mounted as a subtree (in uperf-libre).

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use super::DFPS_NOTIFY_PATH;

/// Write the current Hz to `DFPS_NOTIFY_PATH`.
///
/// Upstream opens with `O_WRONLY|O_NONBLOCK|O_CLOEXEC|O_CREAT|O_TRUNC`
/// (dynamic_fps.cpp:323); `O_NOFOLLOW` is added here because the path lives under
/// `/sdcard/Android/yc/uperf/` — shared, app-writable storage — and a symlink planted there
/// would redirect the truncating write into an arbitrary file.
///
/// The update is atomic: a reader parsing the file (`dfps_cur.txt`) could otherwise observe
/// it truncated, or a partial value (`"12"` while writing `"120"`), and treat that as the
/// current refresh rate.
pub fn write_cur_hz(hz: i32) -> std::io::Result<()> {
    let path = Path::new(DFPS_NOTIFY_PATH);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = Path::new(&tmp);
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(tmp)?;
        f.write_all(hz.to_string().as_bytes())?;
    }
    match std::fs::rename(tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // FUSE can refuse the rename; fall back to a direct write rather than losing the
            // update, and clean the temp file up.
            let _ = std::fs::remove_file(tmp);
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(path)
                .map_err(|_| e)?;
            f.write_all(hz.to_string().as_bytes())
        }
    }
}

/// M1 hook for tests: write the current hz to the notify path.
#[cfg(test)]
pub fn write_task(task: &super::DfpsTask) -> std::io::Result<()> {
    // `-1` is not a refresh rate: propagate the missing state instead of writing a sentinel
    // the reader cannot distinguish from a real value.
    let hz = task.cur_hz().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "DfpsTask has no current hz")
    })?;
    write_cur_hz(hz)
}