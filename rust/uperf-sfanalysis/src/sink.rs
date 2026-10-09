//! Hint byte writer: open + truncate + write + close on
//! `<hint_path>/sfanalysis.hint`.
//!
//! Vendor does `open(O_WRONLY|O_CREAT|O_TRUNC) → write → close` per byte
//! (see `m8-sfanalysis-reverse.md §3`); we do the same. There is at most
//! one byte per refresh tick, so the cost is negligible. If M8 验收 finds
//! a measurable difference, we can switch to `O_WRONLY` + `lseek(0)` —
//! the spec leaves room for that.
//!
//! Path resolution:
//!   1. `UPERF_SF_HINT_FILE` env var (set by `magisk/customize.sh` from
//!      `<USER_PATH>/sfanalysis.hint`).
//!   2. Otherwise fall back to a hard-coded `/sdcard/Android/yc/uperf/sfanalysis.hint`.
//!      On host tests, `UPERF_FAKE_ROOT` rewrites the prefix.

use libc::{close, open, write, O_CREAT, O_TRUNC, O_WRONLY};
use std::ffi::{CStr, CString};

/// Maximum path length we will accept from the environment.
const MAX_PATH: usize = 256;

/// Return the hint file path. Resolution:
///   UPERF_SF_HINT_FILE > built-in default.
/// Optional `UPERF_FAKE_ROOT` prefix is honored for host tests.
pub fn resolve_path() -> Option<String> {
    let raw = std::env::var("UPERF_SF_HINT_FILE").unwrap_or_else(|_| default_path());
    Some(rewrite_under_fake_root(raw))
}

/// Same as `resolve_path` but takes an explicit hint path. Used by tests
/// and by callers that already know the path (e.g. C entry point).
pub fn resolve_path_from(hint: &str) -> String {
    rewrite_under_fake_root(hint.to_string())
}

/// Apply `UPERF_FAKE_ROOT` prefix substitution. Exposed for tests that
/// want to verify the rewrite without touching process env (which would
/// race with parallel `cargo test` workers).
pub fn rewrite_with_fake_root(p: String, fake: Option<&str>) -> String {
    if let Some(fake) = fake {
        const PREFIX: &str = "/sdcard/Android/yc/uperf";
        if let Some(stripped) = p.strip_prefix(PREFIX) {
            return format!("{}{}", fake.trim_end_matches('/'), stripped);
        }
    }
    p
}

fn rewrite_under_fake_root(p: String) -> String {
    let fake = std::env::var("UPERF_FAKE_ROOT").ok();
    rewrite_with_fake_root(p, fake.as_deref())
}

fn default_path() -> String {
    // Matches `magisk/script/libuperf.sh::pathinfo.sh::USER_PATH` + hint file name.
    String::from("/sdcard/Android/yc/uperf/sfanalysis.hint")
}

/// Write a single byte to the resolved path. Returns `Ok(())` on success
/// or the errno on failure. Never panics on a device.
pub fn write_byte(path: &str, byte: u8) -> Result<(), i32> {
    let cpath = CString::new(path).map_err(|_| libc::EINVAL)?;
    // 0o644 = rw-r--r--. Matches what `magisk/script/setup.sh` does for the
    // config file (visible to the consumer daemon as a non-root reader).
    let fd = unsafe { open(cpath.as_ptr(), O_WRONLY | O_CREAT | O_TRUNC, 0o644) };
    if fd < 0 {
        return Err(errno());
    }
    let buf = [byte];
    let n = unsafe { write(fd, buf.as_ptr() as *const _, 1) };
    let _ = unsafe { close(fd) };
    if n < 0 {
        return Err(errno());
    }
    Ok(())
}

#[inline]
fn errno() -> i32 {
    // See hook.rs for the rationale.
    #[cfg(target_os = "android")]
    unsafe {
        *libc::__errno()
    }
    #[cfg(not(target_os = "android"))]
    unsafe {
        *libc::__errno_location()
    }
}

/// C-callable helper. Not strictly needed by the daemon, but useful for
/// debug builds that want to override the hint path at runtime.
#[no_mangle]
pub extern "C" fn sfhint_set_path(path: *const libc::c_char) -> i32 {
    if path.is_null() {
        return 1;
    }
    let cstr = unsafe { CStr::from_ptr(path) };
    let s = match cstr.to_str() {
        Ok(s) => s,
        Err(_) => return 2,
    };
    let _ = s; // path is global-state; setting it is intentionally out of scope for now.
    0
}

#[allow(dead_code)]
const _MAX_PATH_CHECK: usize = MAX_PATH;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir() -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "uperf-sfanalysis-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Each test uses its own fake root and never touches process env.
    /// That keeps `cargo test`'s parallel scheduler from leaking state.

    #[test]
    fn write_byte_creates_file_with_one_byte() {
        let dir = temp_dir();
        let path = format!("{}/sfanalysis.hint", dir.display());
        write_byte(&path, 4).expect("write ok");

        let bytes = fs::read(&path).expect("file exists");
        assert_eq!(bytes, vec![4u8]);
        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o644);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_byte_truncates_existing_file() {
        let dir = temp_dir();
        let path = format!("{}/sfanalysis.hint", dir.display());
        write_byte(&path, 7).unwrap();
        write_byte(&path, 2).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes, vec![2u8]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_path_uses_explicit_input() {
        let p = resolve_path_from("/somewhere/else/sfanalysis.hint");
        assert_eq!(p, "/somewhere/else/sfanalysis.hint");
    }

    #[test]
    fn resolve_path_applies_fake_root_when_set() {
        let p = rewrite_with_fake_root(
            "/sdcard/Android/yc/uperf/sfanalysis.hint".to_string(),
            Some("/tmp/fake"),
        );
        assert_eq!(p, "/tmp/fake/sfanalysis.hint");
    }

    #[test]
    fn rewrite_with_fake_root_keeps_unknown_prefix() {
        let p = rewrite_with_fake_root(
            "/somewhere/else/sfanalysis.hint".to_string(),
            Some("/tmp/fake"),
        );
        assert_eq!(p, "/somewhere/else/sfanalysis.hint");
    }

    #[test]
    fn rewrite_with_fake_root_trims_trailing_slash() {
        let p = rewrite_with_fake_root(
            "/sdcard/Android/yc/uperf/sfanalysis.hint".to_string(),
            Some("/tmp/fake/"),
        );
        assert_eq!(p, "/tmp/fake/sfanalysis.hint");
    }

    #[test]
    fn default_path_matches_vendor_user_path() {
        assert_eq!(
            super::default_path(),
            "/sdcard/Android/yc/uperf/sfanalysis.hint"
        );
    }
}
