//! `settings put` writer — the production refresh-rate path.
//!
//! Mirrors upstream `dynamic_fps.cpp:284-320 SwitchRefreshRate(int)` with
//! `useSfBackdoor_ = false`: the four-key write from
//! `misc_android.cpp:238-243 SysPeakRefreshRate`:
//!
//! ```text
//! settings put system peak_refresh_rate <hz>
//! settings put system min_refresh_rate  <hz>
//! settings put system miui_refresh_rate <hz>      (Xiaomi only, no-op elsewhere)
//! settings put secure miui_refresh_rate <hz>      (Xiaomi only, no-op elsewhere)
//! ```
//!
//! We write all four so the same binary is correct on both AOSP-derived and
//! stock MIUI ROMs. On crDroid alioth only `peak_refresh_rate` actually
//! drives `dumpsys display` modeId — verified in
//! `docs/m3-standalone-evidence.md` §(c).
//!
//! ## Why `/system/bin/cmd settings` and not `/system/bin/settings`
//!
//! Upstream uses `/system/bin/cmd` (`misc_android.cpp:230 CallSettingsPut`
//! -> `ExecCmd(nullptr, "/system/bin/cmd", "settings", "put", ns, key, val)`).
//! We keep the same binary: on some ROMs `/system/bin/settings` is a shell
//! wrapper that re-execs `cmd`, and on others it does not exist while `cmd`
//! always does (verified on alioth). One less surprise.
//!
//! ## Why `Stdio::null()` on all three fds
//!
//! AGENT.md §8: a daemon that inherits the caller's stdout pipe dies of
//! SIGPIPE when that pipe closes (`service.sh -> webui.sh restart`). The
//! child must not hold the parent's fds either, or the same trap returns
//! through the back door. Every spawn here detaches stdin/stdout/stderr.

use std::process::{Command, Stdio};

/// The four SettingsProvider keys upstream writes, in upstream order.
///
/// `system`/`peak_refresh_rate` first because that is the only one that
/// matters on non-MIUI ROMs, so if the process is killed mid-sequence the
/// effective key has already landed.
const KEYS_SYSTEM: [&str; 3] = ["peak_refresh_rate", "min_refresh_rate", "miui_refresh_rate"];
const KEYS_SECURE: [&str; 1] = ["miui_refresh_rate"];

const CMD_BIN: &str = "/system/bin/cmd";

/// `settings put system|secure <key> <value>` for every key upstream writes.
///
/// Returns the number of writes that spawned a process successfully. A
/// non-zero return only means the *command* ran; `settings put` itself is
/// fire-and-forget (upstream does not read the value back either — the
/// SettingsProvider applies it asynchronously, which is why the M3 evidence
/// waits ~2 s before dumping display state).
pub fn write_peak_refresh_rate(hz: i32) -> usize {
    let value = hz.to_string();
    let mut spawned = 0usize;
    for key in KEYS_SYSTEM {
        if put("system", key, &value) {
            spawned += 1;
        }
    }
    for key in KEYS_SECURE {
        if put("secure", key, &value) {
            spawned += 1;
        }
    }
    spawned
}

fn put(namespace: &str, key: &str, value: &str) -> bool {
    Command::new(CMD_BIN)
        .args(["settings", "put", namespace, key, value])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_tables_match_upstream_order() {
        // Upstream SysPeakRefreshRate writes system.{peak,min,miui} then
        // secure.miui. If this test breaks, the four-key contract changed.
        assert_eq!(
            KEYS_SYSTEM,
            ["peak_refresh_rate", "min_refresh_rate", "miui_refresh_rate"]
        );
        assert_eq!(KEYS_SECURE, ["miui_refresh_rate"]);
    }

    #[test]
    fn cmd_path_is_the_upstream_one() {
        assert_eq!(CMD_BIN, "/system/bin/cmd");
    }
}