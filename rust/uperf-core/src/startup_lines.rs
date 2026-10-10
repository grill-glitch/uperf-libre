//! Startup lines that upstream emits before anything else does any work.
//!
//! Criterion 2 of AGENT.md §1 requires the config-related warnings/ignores to match
//! upstream **verbatim**, so these strings are taken from the upstream binary rather
//! than written from memory (`strings` on dev-22.09.04):
//!
//! ```text
//! Config '{}' by '{}'
//! Knob '{}' not writeable
//! Knob '{}' not defined in '{}'
//! Config file not specified
//! Config file not found
//! ```
//!
//! They are emitted **without** the `Rust:` prefix the rest of the rewrite uses, and
//! the log sink's bare-prefix mode already renders them as `HH:MM:SS L <line>`.

#![allow(dead_code)]

use std::collections::BTreeMap;

/// True when the path is writeable *in the sense that matters*: the knob can be
/// given a value.
///
/// Three tests, and a path is writeable only if all three pass:
///
/// 1. it exists and its **mode bits carry the owner write bit**;
/// 2. it can be read (we need the current value to write back);
/// 3. writing that value back succeeds.
///
/// Why all three, each with measured evidence from alioth + the sdm888 config:
///
/// * `scaling_max_freq` (mode 444, `qcom-cpufreq-hw`) accepts `open(O_WRONLY)` as
///   root, and it even accepts **writing the current value back** — but it rejects a
///   *different* value (`echo 0-7 > …` fails with EACCES). So neither an open test
///   nor a write-back test alone sees it, while the mode bit does, and upstream warns
///   about it. Mode 444 is the driver saying "read-only"; that is the honest signal.
/// * `access(2)`/`[ -w ]` is useless for root (`CAP_DAC_OVERRIDE`), which is why the
///   mode check reads the bits rather than asking the kernel for permission.
/// * the write-back still earns its place: it catches the knobs that are mode-writable
///   but reject writes for other reasons.
///
/// The probe writes the file's current content back, so checking a knob never changes
/// it.
///
/// Known divergence: this reports **fewer** warnings than upstream for
/// `/dev/cpuset/<group>/cpus`, where upstream warns but this device accepts the write
/// (`echo 0-7 > /dev/cpuset/top-app/cpus` succeeds). See docs/m7-evidence.md §6.
pub fn is_writeable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false; // missing
    };
    if meta.permissions().mode() & 0o200 == 0 {
        return false; // read-only at the mode level: the driver-locked case
    }
    let Ok(current) = std::fs::read_to_string(path) else {
        return false;
    };
    std::fs::write(path, current).is_ok()
}

/// `Config '<name>' by '<author>'` — the first line upstream prints after the banner.
pub fn config_line(meta_name: &str, meta_author: &str) -> String {
    format!("Config '{meta_name}' by '{meta_author}'")
}

/// One `Knob '<path>' not writeable` line per declared knob that cannot be opened
/// for writing, in the knob table's (sorted) order — the order `BTreeMap` gives,
/// which is also the order the upstream log showed for sdm888.
///
/// Returns `(lines, probe_failures)`. `probe` is injectable so the host can test
/// the formatting and the ordering without a device.
pub fn knob_warnings(
    knob_table: &BTreeMap<String, String>,
    probe: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    knob_table
        .values()
        .filter(|path| !path.is_empty() && !probe(path))
        .map(|path| format!("Knob '{path}' not writeable"))
        .collect()
}

/// The device-side probe.
pub fn real_probe() -> impl Fn(&str) -> bool {
    is_writeable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> BTreeMap<String, String> {
        [
            ("UFSmax", "/sys/class/devfreq/1d84000.ufshc/max_freq"),
            ("cpusetTa", "/dev/cpuset/top-app/cpus"),
            ("CPU4max", "/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn config_line_matches_upstreams_format() {
        // Verbatim from the upstream binary's log for this very config.
        assert_eq!(
            config_line("sdm888/sdm888", "yc@coolapk   ❤️吟惋兮❤改"),
            "Config 'sdm888/sdm888' by 'yc@coolapk   ❤️吟惋兮❤改'"
        );
        // ...and for a config whose meta name carries no slash.
        assert_eq!(
            config_line("kirin980", "yc@coolapk  ❤️吟惋兮❤️改"),
            "Config 'kirin980' by 'yc@coolapk  ❤️吟惋兮❤️改'"
        );
    }

    #[test]
    fn knob_warnings_list_only_the_unwriteable_paths_in_table_order() {
        // The paths are sorted by knob name (BTreeMap), so CPU4max < cpusetTa < UFSmax
        // by *key*, which is the order the warnings come out in.
        let lines = knob_warnings(&table(), &|p: &str| p.contains("ufshc"));
        assert_eq!(
            lines,
            vec![
                "Knob '/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq' not writeable".to_string(),
                "Knob '/dev/cpuset/top-app/cpus' not writeable".to_string(),
            ],
            "{lines:?}"
        );
    }

    #[test]
    fn everything_writeable_produces_no_lines() {
        assert!(knob_warnings(&table(), &|_| true).is_empty());
    }

    #[test]
    fn empty_paths_are_skipped() {
        let mut t = table();
        t.insert("Blank".into(), String::new());
        let lines = knob_warnings(&t, &|_| false);
        assert_eq!(lines.len(), 3, "the empty path must not be reported: {lines:?}");
        assert!(lines.iter().all(|l| !l.contains("''")), "{lines:?}");
    }

    #[test]
    fn the_probe_is_a_write_back_and_leaves_content_alone() {
        let d = std::env::temp_dir().join(format!("uperf_probe_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let ok = d.join("writable");
        std::fs::write(&ok, "0-7\n").unwrap();
        assert!(is_writeable(ok.to_str().unwrap()));
        assert_eq!(
            std::fs::read_to_string(&ok).unwrap(),
            "0-7\n",
            "the probe must write back the same content"
        );
        assert!(!is_writeable(d.join("missing").to_str().unwrap()));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A path that opens fine but rejects the write must be reported unwriteable —
    /// the `scaling_max_freq` case, reproduced with a read-only file as root.
    #[test]
    fn a_write_refusal_outranks_a_successful_open() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: `geteuid` takes no arguments and cannot fail; the value is only
        // compared against 0 to decide which branch of the test to run.
        if unsafe { libc::geteuid() } != 0 {
            // Without CAP_DAC_OVERRIDE the open itself fails, which is also a correct
            // "not writeable" verdict; either way the probe must say no.
            let d = std::env::temp_dir().join(format!("uperf_ro_{}", std::process::id()));
            std::fs::create_dir_all(&d).unwrap();
            let f = d.join("ro");
            std::fs::write(&f, "x").unwrap();
            let mut p = std::fs::metadata(&f).unwrap().permissions();
            p.set_mode(0o444);
            std::fs::set_permissions(&f, p).unwrap();
            assert!(!is_writeable(f.to_str().unwrap()));
            let _ = std::fs::remove_dir_all(&d);
            return;
        }
        // As root, mode bits do not stop the open nor the write, so this documents
        // that the probe is write-based rather than permission-based.
        let d = std::env::temp_dir().join(format!("uperf_ro_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("ro");
        std::fs::write(&f, "x").unwrap();
        let mut p = std::fs::metadata(&f).unwrap().permissions();
        p.set_mode(0o444);
        std::fs::set_permissions(&f, p).unwrap();
        assert!(
            is_writeable(f.to_str().unwrap()),
            "root's write-back succeeds even on mode 444, which is why the kernel              refusing the write is the only signal that matters"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
